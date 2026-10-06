//! Mainnet-fork test: surfpool lazily clones Phoenix Eternal and the live BTC
//! market from mainnet, and only this program is loaded into the fork.

pub mod common;

use std::{
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use common::{
    MarketAccounts, PROGRAM_ID, PhoenixStrategyState, PriceImprovementBehavior, StrategyParams,
    initialize_ix, strategy_params, strategy_pda, update_quotes_ix, with_compute_budget,
};
use phoenix_rise::{
    accounts::{
        orderbook::Orderbook,
        perp_asset_map::PerpAssetMap,
        trader::{TraderHeader, capabilities::TraderCapabilityFlags},
    },
    ix::{
        constants::{PHOENIX_GLOBAL_CONFIGURATION, PHOENIX_PROGRAM_ID, SPL_TOKEN_PROGRAM_ID},
        deposit_funds::{DepositFundsParams, create_deposit_funds_ix},
        register_trader::{RegisterTraderParams, create_register_trader_ix},
    },
};
use serde_json::{Value, json};
use solana_commitment_config::CommitmentConfig;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use solana_rpc_client_api::request::RpcRequest;
use solana_signer::Signer;
use solana_transaction::Transaction;

const SURFPOOL_BIN_ENV: &str = "SURFPOOL_BIN";
const DATASOURCE_RPC_ENV: &str = "PHOENIX_MAINNET_RPC_URL";
const DEFAULT_DATASOURCE_RPC: &str = "https://api.mainnet-beta.solana.com";
const PHOENIX_API: &str = "https://perp-api.phoenix.trade";
const SYMBOL: &str = "BTC";
const BPF_LOADER: Pubkey = solana_pubkey::pubkey!("BPFLoader2111111111111111111111111111111111");
const LAST_RESTART_SLOT_SYSVAR: Pubkey =
    solana_pubkey::pubkey!("SysvarLastRestartS1ot1111111111111111111111");
const TRADER_FLAGS_OFFSET: usize = 96;
/// 100,000 PhUSD (6 decimals).
const DEPOSIT_AMOUNT: u64 = 100_000_000_000;
const ATA_PROGRAM: Pubkey = solana_pubkey::pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

struct Surfpool {
    child: Child,
    rpc: RpcClient,
    work_dir: PathBuf,
}

impl Drop for Surfpool {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.work_dir);
    }
}

impl Surfpool {
    fn start() -> Option<Self> {
        let bin = std::env::var(SURFPOOL_BIN_ENV).unwrap_or_else(|_| "surfpool".into());
        let datasource = datasource_url();
        let rpc_port = free_port();
        let ws_port = free_port();
        // Run outside the repo so surfpool doesn't auto-register target/deploy keypairs.
        let work_dir = std::env::temp_dir().join(format!("phoenix-eternal-mm-surfpool-{rpc_port}"));
        std::fs::create_dir_all(&work_dir).unwrap();
        let child = Command::new(&bin)
            .current_dir(&work_dir)
            .args(["start", "--no-tui", "--no-studio", "--no-deploy", "--ci"])
            .args(["--rpc-url", &datasource])
            .args(["-p", &rpc_port.to_string(), "-w", &ws_port.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let Ok(child) = child else {
            eprintln!("skipping: `{bin}` not found (set {SURFPOOL_BIN_ENV})");
            return None;
        };
        let rpc = RpcClient::new_with_commitment(
            format!("http://127.0.0.1:{rpc_port}"),
            CommitmentConfig::confirmed(),
        );
        let surfpool = Self {
            child,
            rpc,
            work_dir,
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        while surfpool.rpc.get_health().is_err() {
            assert!(Instant::now() < deadline, "surfpool did not become healthy");
            thread::sleep(Duration::from_millis(250));
        }
        Some(surfpool)
    }

    fn cheatcode(&self, method: &'static str, params: Value) {
        self.rpc
            .send::<Value>(RpcRequest::Custom { method }, params)
            .unwrap_or_else(|error| panic!("{method} failed: {error}"));
    }

    /// `surfnet_writeProgram` keeps executing its placeholder stub from the
    /// program cache, so load the ELF as a BPFLoader2 executable instead.
    fn load_program(&self, program_id: &Pubkey, elf: &[u8]) {
        self.cheatcode(
            "surfnet_setAccount",
            json!([
                program_id.to_string(),
                {
                    "lamports": 1_000_000_000u64,
                    "data": hex(elf),
                    "owner": BPF_LOADER.to_string(),
                    "executable": true,
                }
            ]),
        );
    }

    fn fund(&self, address: &Pubkey, lamports: u64) {
        self.cheatcode(
            "surfnet_setAccount",
            json!([address.to_string(), { "lamports": lamports }]),
        );
    }

    /// Surfpool serves its own LastRestartSlot (0); Phoenix gates on the
    /// mainnet value acknowledged in the global config, so mirror it.
    fn sync_last_restart_slot(&self, datasource: &RpcClient) {
        let sysvar = datasource.get_account(&LAST_RESTART_SLOT_SYSVAR).unwrap();
        self.cheatcode(
            "surfnet_setAccount",
            json!([
                LAST_RESTART_SLOT_SYSVAR.to_string(),
                {
                    "lamports": sysvar.lamports,
                    "data": hex(&sysvar.data),
                    "owner": sysvar.owner.to_string(),
                }
            ]),
        );
    }

    /// Onboarding grants cold capabilities via an off-chain-gated signer on
    /// mainnet; on the fork we set them directly. The first deposit then
    /// activates the trader into the global trader index.
    fn activate_trader(&self, trader_account: &Pubkey) {
        let mut data = self.rpc.get_account_data(trader_account).unwrap();
        data[TRADER_FLAGS_OFFSET..TRADER_FLAGS_OFFSET + 4]
            .copy_from_slice(&TraderCapabilityFlags::cold().as_u32().to_le_bytes());
        self.cheatcode(
            "surfnet_setAccount",
            json!([trader_account.to_string(), { "data": hex(&data) }]),
        );
        let data = self.rpc.get_account_data(trader_account).unwrap();
        let header = TraderHeader::try_read_from_account_bytes(&data).unwrap();
        assert!(header.trader_state.is_ready());
    }

    fn mint_tokens(&self, owner: &Pubkey, mint: &Pubkey, amount: u64) -> Pubkey {
        self.cheatcode(
            "surfnet_setTokenAccount",
            json!([owner.to_string(), mint.to_string(), { "amount": amount }]),
        );
        associated_token_address(owner, mint)
    }

    fn send(&self, payer: &Keypair, ix: Instruction) {
        let blockhash = self.rpc.get_latest_blockhash().unwrap();
        let tx = Transaction::new_signed_with_payer(
            &with_compute_budget(ix),
            Some(&payer.pubkey()),
            &[payer],
            blockhash,
        );
        let simulation = self.rpc.simulate_transaction(&tx).unwrap().value;
        if std::env::var_os("SURFPOOL_LOGS").is_some() {
            eprintln!("{:#?}", simulation.logs);
        }
        if let Some(error) = simulation.err {
            panic!("simulation failed: {error:?}\n{:#?}", simulation.logs);
        }
        self.rpc.send_and_confirm_transaction(&tx).unwrap();
    }

    fn state(&self, strategy: &Pubkey) -> PhoenixStrategyState {
        let data = self.rpc.get_account_data(strategy).unwrap();
        wincode::deserialize_exact(&data).unwrap()
    }

    /// The fork syncs its clock once at startup but clones accounts lazily at
    /// mainnet's later state, so a cloned price can be stamped ahead of the
    /// fork's slot. Clone everything Phoenix reads, then jump the clock to
    /// mainnet's current slot.
    fn align_clock(&self, datasource: &RpcClient, accounts: &[Pubkey]) {
        for chunk in accounts.chunks(100) {
            self.rpc.get_multiple_accounts(chunk).unwrap();
        }
        let slot = datasource.get_slot().unwrap();
        self.cheatcode("surfnet_timeTravel", json!([{ "absoluteSlot": slot }]));
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.processed_slot() < slot {
            assert!(
                Instant::now() < deadline,
                "fork clock did not reach slot {slot}"
            );
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn processed_slot(&self) -> u64 {
        self.rpc
            .get_slot_with_commitment(CommitmentConfig::processed())
            .unwrap()
    }

    fn assert_prices_not_ahead_of_clock(&self, perp_asset_map: &Pubkey) {
        let data = self.rpc.get_account_data(perp_asset_map).unwrap();
        let map = PerpAssetMap::try_from_account_bytes(&data).unwrap();
        let mark_slot = map
            .find_by_symbol(SYMBOL)
            .unwrap()
            .expect("BTC market should exist")
            .metadata
            .oracle_price()
            .mark_price
            .price
            .slot;
        let fork_slot = self.processed_slot();
        assert!(
            mark_slot <= fork_slot,
            "mark price slot {mark_slot} is ahead of fork slot {fork_slot}"
        );
    }

    fn mid_price_in_ticks(&self, orderbook: &Pubkey) -> u64 {
        let data = self.rpc.get_account_data(orderbook).unwrap();
        let book = Orderbook::try_from_account_bytes(&data).unwrap();
        let bid = book.best_bid().expect("live BTC book should have bids");
        let ask = book.best_ask().expect("live BTC book should have asks");
        (bid.price_in_ticks().as_inner() + ask.price_in_ticks().as_inner()) / 2
    }

    fn resting(&self, orderbook: &Pubkey, price: u64, seq: u64) -> bool {
        let data = self.rpc.get_account_data(orderbook).unwrap();
        let book = Orderbook::try_from_account_bytes(&data).unwrap();
        book.bid_orders().chain(book.ask_orders()).any(|entry| {
            entry.price_in_ticks().as_inner() == price && entry.order_sequence_number() == seq
        })
    }
}

struct MainnetMarket {
    canonical_mint: Pubkey,
    global_vault: Pubkey,
    perp_asset_map: Pubkey,
    orderbook: Pubkey,
    spline_collection: Pubkey,
    global_trader_index: Vec<Pubkey>,
    active_trader_buffer: Vec<Pubkey>,
}

impl MainnetMarket {
    fn fetch() -> Self {
        let body = ureq::get(format!("{PHOENIX_API}/v1/exchange/snapshot"))
            .call()
            .expect("exchange snapshot request")
            .into_body()
            .read_to_string()
            .unwrap();
        let snapshot: Value = serde_json::from_str(&body).unwrap();
        let exchange = &snapshot["exchange"];
        let market = snapshot["markets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|market| market["symbol"] == SYMBOL)
            .expect("BTC market in snapshot");
        let key = |value: &Value| value.as_str().unwrap().parse::<Pubkey>().unwrap();
        let keys = |value: &Value| value.as_array().unwrap().iter().map(key).collect();
        assert_eq!(key(&exchange["programId"]), *PHOENIX_PROGRAM_ID);
        Self {
            canonical_mint: key(&exchange["canonicalMint"]),
            global_vault: key(&exchange["globalVault"]),
            perp_asset_map: key(&exchange["perpAssetMap"]),
            orderbook: key(&market["marketPubkey"]),
            spline_collection: key(&market["splinePubkey"]),
            global_trader_index: keys(&exchange["globalTraderIndex"]),
            active_trader_buffer: keys(&exchange["activeTraderBuffer"]),
        }
    }

    fn phoenix_accounts(&self) -> Vec<Pubkey> {
        [
            *PHOENIX_GLOBAL_CONFIGURATION,
            self.canonical_mint,
            self.global_vault,
            self.perp_asset_map,
            self.orderbook,
            self.spline_collection,
        ]
        .into_iter()
        .chain(self.global_trader_index.iter().copied())
        .chain(self.active_trader_buffer.iter().copied())
        .collect()
    }

    fn accounts(&self, trader: Pubkey, trader_account: Pubkey) -> MarketAccounts {
        MarketAccounts {
            trader,
            trader_account,
            perp_asset_map: self.perp_asset_map,
            orderbook: self.orderbook,
            spline_collection: self.spline_collection,
            global_trader_index: self.global_trader_index.clone(),
            active_trader_buffer: self.active_trader_buffer.clone(),
        }
    }
}

fn datasource_url() -> String {
    std::env::var(DATASOURCE_RPC_ENV).unwrap_or_else(|_| DEFAULT_DATASOURCE_RPC.into())
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0xf) as usize] as char);
    }
    out
}

fn trader_pda(authority: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"trader", authority.as_ref(), &[0, 0]],
        &PHOENIX_PROGRAM_ID,
    )
    .0
}

fn associated_token_address(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), SPL_TOKEN_PROGRAM_ID.as_ref(), mint.as_ref()],
        &ATA_PROGRAM,
    )
    .0
}

fn program_elf() -> Option<Vec<u8>> {
    let path = std::env::var("PHOENIX_ETERNAL_MM_SO")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/deploy/phoenix_eternal_mm.so")
        });
    std::fs::read(path).ok()
}

#[test]
fn quotes_live_btc_market_on_mainnet_fork() {
    let Some(elf) = program_elf() else {
        eprintln!("skipping: phoenix_eternal_mm.so missing; run cargo-build-sbf");
        return;
    };
    let market = MainnetMarket::fetch();
    let Some(surfpool) = Surfpool::start() else {
        return;
    };
    surfpool.load_program(&PROGRAM_ID, &elf);
    let datasource = RpcClient::new(datasource_url());
    surfpool.sync_last_restart_slot(&datasource);
    surfpool.align_clock(&datasource, &market.phoenix_accounts());
    surfpool.assert_prices_not_ahead_of_clock(&market.perp_asset_map);

    let maker = Keypair::new();
    let trader_account = trader_pda(&maker.pubkey());
    surfpool.fund(&maker.pubkey(), 100_000_000_000);

    let register = RegisterTraderParams::builder()
        .payer(maker.pubkey())
        .trader(maker.pubkey())
        .trader_account(trader_account)
        .max_positions(128)
        .trader_pda_index(0)
        .subaccount_index(0)
        .build()
        .unwrap();
    surfpool.send(&maker, create_register_trader_ix(register).unwrap().into());
    surfpool.activate_trader(&trader_account);

    let token_account =
        surfpool.mint_tokens(&maker.pubkey(), &market.canonical_mint, DEPOSIT_AMOUNT);
    let deposit = DepositFundsParams::builder()
        .trader(maker.pubkey())
        .trader_account(trader_account)
        .canonical_mint(market.canonical_mint)
        .global_vault(market.global_vault)
        .trader_token_account(token_account)
        .global_trader_index(market.global_trader_index.clone())
        .active_trader_buffer(market.active_trader_buffer.clone())
        .amount(DEPOSIT_AMOUNT)
        .build()
        .unwrap();
    surfpool.send(&maker, create_deposit_funds_ix(deposit).unwrap().into());

    let accounts = market.accounts(maker.pubkey(), trader_account);
    let strategy = strategy_pda(&maker.pubkey(), &market.orderbook);
    let fair = surfpool.mid_price_in_ticks(&market.orderbook);
    assert!(fair > 0);

    surfpool.send(
        &maker,
        initialize_ix(
            PROGRAM_ID,
            maker.pubkey(),
            trader_account,
            market.orderbook,
            strategy_params(50, PriceImprovementBehavior::Ignore, true),
        ),
    );
    surfpool.send(
        &maker,
        update_quotes_ix(PROGRAM_ID, &accounts, fair, StrategyParams::default()),
    );

    let quoted = surfpool.state(&strategy);
    let (bid, ask) = (quoted.bid(), quoted.ask());
    assert!(bid.is_active() && ask.is_active());
    assert!(bid.price_in_ticks < fair && ask.price_in_ticks > fair);
    assert!(surfpool.resting(
        &market.orderbook,
        bid.price_in_ticks,
        bid.order_sequence_number
    ));
    assert!(surfpool.resting(
        &market.orderbook,
        ask.price_in_ticks,
        ask.order_sequence_number
    ));

    let moved = fair + fair / 1_000;
    surfpool.send(
        &maker,
        update_quotes_ix(PROGRAM_ID, &accounts, moved, StrategyParams::default()),
    );
    let requoted = surfpool.state(&strategy);
    assert!(requoted.bid().price_in_ticks > bid.price_in_ticks);
    assert!(requoted.ask().price_in_ticks > ask.price_in_ticks);
    assert!(!surfpool.resting(
        &market.orderbook,
        bid.price_in_ticks,
        bid.order_sequence_number
    ));
    assert!(!surfpool.resting(
        &market.orderbook,
        ask.price_in_ticks,
        ask.order_sequence_number
    ));
    assert!(surfpool.resting(
        &market.orderbook,
        requoted.bid().price_in_ticks,
        requoted.bid().order_sequence_number
    ));
    assert!(surfpool.resting(
        &market.orderbook,
        requoted.ask().price_in_ticks,
        requoted.ask().order_sequence_number
    ));
}
