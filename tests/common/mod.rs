#![allow(dead_code)]

use std::{
    io::{self, Write},
    path::PathBuf,
};

use phoenix_rise::{
    accounts::orderbook::Orderbook,
    ix::{
        constants::{
            PHOENIX_GLOBAL_CONFIGURATION, PHOENIX_LOG_AUTHORITY, PHOENIX_PROGRAM_ID,
            SYSTEM_PROGRAM_ID, compute_discriminant,
        },
        market_order::{MarketOrderParams, create_place_market_order_ix},
        types::{SelfTradeBehavior, Side},
    },
};
use phoenix_rise_litesvm_test::{
    FixtureActor, FixtureMarket, SdkLocalnetContext, SdkLocalnetProgram,
    default_sdk_localnet_fixture, find_sdk_localnet_program_paths, parse_pubkey, parse_pubkeys,
    sdk_localnet_vm_required,
};
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::{Pubkey, pubkey};
use wincode::{SchemaRead, SchemaWrite, config::DefaultConfig};

const PROGRAM_SO_ENV: &str = "PHOENIX_ETERNAL_MM_SO";
pub const PROGRAM_ID: Pubkey = pubkey!("6jLH6im51XeZw63brqmY6htqQt6ntbGr76SZSqx73BpS");
pub const STRATEGY_SEED: &[u8] = b"phoenix";
pub const TICK_SIZE: u64 = 100;

pub const MAKER: &str = "taker0";
pub const TAKER: &str = "taker1";
pub const BTC: &str = "BTC";
/// $1,000 notional at 1e-6 USD quote lots.
pub const QUOTE_SIZE_IN_QUOTE_LOTS: u64 = 1_000_000_000;
/// $100,000 for BTC (tick = 100 quote lots per base lot, 4 base lot decimals).
pub const FAIR_PRICE_IN_TICKS: u64 = 100_000;

pub struct Harness {
    pub context: SdkLocalnetContext,
    pub program_id: Pubkey,
    pub maker: FixtureActor,
    pub market: FixtureMarket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, SchemaRead, SchemaWrite)]
#[wincode(tag_encoding = "u8")]
pub enum PriceImprovementBehavior {
    Join,
    Dime,
    Ignore,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, SchemaRead, SchemaWrite)]
pub struct StrategyParams {
    pub quote_edge_in_bps: Option<u64>,
    pub quote_size_in_quote_lots: Option<u64>,
    pub price_improvement_behavior: Option<PriceImprovementBehavior>,
    pub post_only: Option<bool>,
}

#[derive(SchemaWrite)]
struct InitializeParams {
    strategy: StrategyParams,
}

#[derive(SchemaWrite)]
struct UpdateQuotesParams {
    fair_price_in_ticks: u64,
    strategy: StrategyParams,
    global_trader_index_count: u8,
    active_trader_buffer_count: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quote {
    pub price_in_ticks: u64,
    pub order_sequence_number: u64,
    pub size_in_base_lots: u64,
}

impl Quote {
    pub fn is_active(&self) -> bool {
        self.price_in_ticks != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, SchemaRead)]
pub struct PhoenixStrategyState {
    pub discriminator: [u8; 8],
    pub trader: [u8; 32],
    pub trader_account: [u8; 32],
    pub market: [u8; 32],
    pub bid_order_sequence_number: u64,
    pub bid_price_in_ticks: u64,
    pub initial_bid_size_in_base_lots: u64,
    pub ask_order_sequence_number: u64,
    pub ask_price_in_ticks: u64,
    pub initial_ask_size_in_base_lots: u64,
    pub last_update_slot: u64,
    pub last_update_unix_timestamp: i64,
    pub quote_edge_in_bps: u64,
    pub quote_size_in_quote_lots: u64,
    pub post_only: u8,
    pub price_improvement_behavior: u8,
    pub bump: u8,
    pub padding: [u8; 5],
}

impl PhoenixStrategyState {
    pub fn bid(&self) -> Quote {
        Quote {
            price_in_ticks: self.bid_price_in_ticks,
            order_sequence_number: self.bid_order_sequence_number,
            size_in_base_lots: self.initial_bid_size_in_base_lots,
        }
    }

    pub fn ask(&self) -> Quote {
        Quote {
            price_in_ticks: self.ask_price_in_ticks,
            order_sequence_number: self.ask_order_sequence_number,
            size_in_base_lots: self.initial_ask_size_in_base_lots,
        }
    }
}

pub const STRATEGY_STATE_LEN: usize = 192;

pub fn state_discriminator() -> [u8; 8] {
    compute_discriminant("account:PhoenixStrategyState")
}

pub fn size_in_base_lots(quote_size_in_quote_lots: u64, price_in_ticks: u64) -> u64 {
    quote_size_in_quote_lots / (price_in_ticks * TICK_SIZE)
}

pub fn setup() -> Option<Harness> {
    let Some(program_paths) = find_sdk_localnet_program_paths() else {
        if sdk_localnet_vm_required() {
            panic!("missing Phoenix/Ember SBF artifacts");
        }
        eprintln!(
            "skipping: Phoenix SBF artifacts missing (set PHOENIX_MAINNET_BPF_PROGRAMS=1 to fetch mainnet builds)"
        );
        return None;
    };
    let Some(program_path) = find_program_path() else {
        if sdk_localnet_vm_required() {
            panic!("missing phoenix_eternal_mm.so; run cargo-build-sbf or set {PROGRAM_SO_ENV}");
        }
        eprintln!("skipping: phoenix_eternal_mm.so missing; run cargo-build-sbf");
        return None;
    };

    let fixture = default_sdk_localnet_fixture().expect("fixture should deserialize");
    let program_id = PROGRAM_ID;
    let mut context = SdkLocalnetContext::new_with_programs(
        fixture,
        program_paths,
        [SdkLocalnetProgram::new(program_id, program_path)],
    );
    context.execute_setup();
    context.svm.warp_to_slot(200);
    context.send_fixture_transaction("oracleSetPrices");
    context.send_fixture_transaction("splineUpdatePrices");
    context.send_fixture_transaction("orderbookPlaceLevels");

    let maker = context.actor(MAKER);
    let market = context.market(BTC);
    Some(Harness {
        context,
        program_id,
        maker,
        market,
    })
}

pub fn strategy_params(
    edge_bps: u64,
    behavior: PriceImprovementBehavior,
    post_only: bool,
) -> StrategyParams {
    StrategyParams {
        quote_edge_in_bps: Some(edge_bps),
        quote_size_in_quote_lots: Some(QUOTE_SIZE_IN_QUOTE_LOTS),
        price_improvement_behavior: Some(behavior),
        post_only: Some(post_only),
    }
}

impl Harness {
    pub fn maker_key(&self) -> Pubkey {
        parse_pubkey(&self.maker.pubkey).unwrap()
    }

    pub fn maker_trader(&self) -> Pubkey {
        parse_pubkey(&self.maker.trader_account).unwrap()
    }

    pub fn orderbook(&self) -> Pubkey {
        parse_pubkey(&self.market.orderbook).unwrap()
    }

    pub fn strategy(&self) -> Pubkey {
        strategy_pda(&self.maker_key(), &self.orderbook())
    }

    pub fn initialize_ix(&self, strategy: StrategyParams) -> Instruction {
        initialize_ix(
            self.program_id,
            self.maker_key(),
            self.maker_trader(),
            self.orderbook(),
            strategy,
        )
    }

    pub fn update_quotes_ix(
        &self,
        fair_price_in_ticks: u64,
        strategy: StrategyParams,
    ) -> Instruction {
        let addresses = &self.context.fixture.addresses;
        update_quotes_ix(
            self.program_id,
            &MarketAccounts {
                trader: self.maker_key(),
                trader_account: self.maker_trader(),
                perp_asset_map: parse_pubkey(&addresses.perp_asset_map).unwrap(),
                orderbook: self.orderbook(),
                spline_collection: parse_pubkey(&self.market.spline).unwrap(),
                global_trader_index: parse_pubkeys(&addresses.global_trader_index),
                active_trader_buffer: parse_pubkeys(&addresses.active_trader_buffer),
            },
            fair_price_in_ticks,
            strategy,
        )
    }

    pub fn send(&mut self, ix: Instruction, label: &str) -> Vec<String> {
        let seed = self.maker.seed.clone();
        let tx =
            self.context
                .send_instructions_with_metadata(with_compute_budget(ix), &seed, label);
        print_logs(label, &tx.logs);
        tx.logs
    }

    pub fn try_send(&mut self, ix: Instruction) -> Result<Vec<String>, Vec<String>> {
        let seed = self.maker.seed.clone();
        self.context
            .try_send_instructions_with_metadata(with_compute_budget(ix), &seed)
            .map(|tx| tx.logs)
            .map_err(|error| error.meta.logs)
    }

    pub fn state(&self) -> PhoenixStrategyState {
        let data = self.context.account_data(&self.strategy());
        assert_eq!(data.len(), STRATEGY_STATE_LEN);
        wincode::deserialize_exact(&data).expect("strategy state should deserialize")
    }

    pub fn book_orders(&self) -> BookOrders {
        let data = self.context.account_data(&self.orderbook());
        let book = Orderbook::try_from_account_bytes(&data).expect("orderbook should deserialize");
        let collect = |iter: phoenix_rise::accounts::orderbook::OrderbookSideIter<'_>| {
            iter.map(|entry| BookOrder {
                price_in_ticks: entry.price_in_ticks().as_inner(),
                order_sequence_number: entry.order_sequence_number(),
                remaining_base_lots: entry.num_base_lots_remaining().as_inner(),
            })
            .collect()
        };
        BookOrders {
            bids: collect(book.bid_orders()),
            asks: collect(book.ask_orders()),
        }
    }

    pub fn taker_market_order(&mut self, side: Side, num_base_lots: u64) {
        let taker = self.context.actor(TAKER);
        let addresses = &self.context.fixture.addresses;
        let params = MarketOrderParams::builder()
            .trader(parse_pubkey(&taker.pubkey).unwrap())
            .trader_account(parse_pubkey(&taker.trader_account).unwrap())
            .perp_asset_map(parse_pubkey(&addresses.perp_asset_map).unwrap())
            .orderbook(self.orderbook())
            .spline_collection(parse_pubkey(&self.market.spline).unwrap())
            .global_trader_index(parse_pubkeys(&addresses.global_trader_index))
            .active_trader_buffer(parse_pubkeys(&addresses.active_trader_buffer))
            .side(side)
            .num_base_lots(num_base_lots)
            .self_trade_behavior(SelfTradeBehavior::CancelProvide)
            .symbol(BTC)
            .build()
            .unwrap();
        let ix = create_place_market_order_ix(params).unwrap().into();
        let tx = self.context.send_instructions_with_metadata(
            with_compute_budget(ix),
            &taker.seed,
            "taker-market-order",
        );
        print_logs("taker-market-order", &tx.logs);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BookOrder {
    pub price_in_ticks: u64,
    pub order_sequence_number: u64,
    pub remaining_base_lots: u64,
}

pub struct BookOrders {
    pub bids: Vec<BookOrder>,
    pub asks: Vec<BookOrder>,
}

impl BookOrders {
    pub fn has_bid(&self, price: u64, seq: u64) -> bool {
        self.bids
            .iter()
            .any(|o| o.price_in_ticks == price && o.order_sequence_number == seq)
    }

    pub fn has_ask(&self, price: u64, seq: u64) -> bool {
        self.asks
            .iter()
            .any(|o| o.price_in_ticks == price && o.order_sequence_number == seq)
    }
}

pub struct MarketAccounts {
    pub trader: Pubkey,
    pub trader_account: Pubkey,
    pub perp_asset_map: Pubkey,
    pub orderbook: Pubkey,
    pub spline_collection: Pubkey,
    pub global_trader_index: Vec<Pubkey>,
    pub active_trader_buffer: Vec<Pubkey>,
}

pub fn strategy_pda(authority: &Pubkey, orderbook: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[STRATEGY_SEED, authority.as_ref(), orderbook.as_ref()],
        &PROGRAM_ID,
    )
    .0
}

pub fn initialize_ix(
    program_id: Pubkey,
    authority: Pubkey,
    trader_account: Pubkey,
    orderbook: Pubkey,
    strategy: StrategyParams,
) -> Instruction {
    Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(strategy_pda(&authority, &orderbook), false),
            AccountMeta::new(authority, true),
            AccountMeta::new_readonly(orderbook, false),
            AccountMeta::new_readonly(trader_account, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
        ],
        data: ix_data("global:initialize", &InitializeParams { strategy }),
    }
}

pub fn update_quotes_ix(
    program_id: Pubkey,
    accounts: &MarketAccounts,
    fair_price_in_ticks: u64,
    strategy: StrategyParams,
) -> Instruction {
    let params = UpdateQuotesParams {
        fair_price_in_ticks,
        strategy,
        global_trader_index_count: accounts.global_trader_index.len() as u8,
        active_trader_buffer_count: accounts.active_trader_buffer.len() as u8,
    };
    let mut metas = vec![
        AccountMeta::new(strategy_pda(&accounts.trader, &accounts.orderbook), false),
        AccountMeta::new_readonly(accounts.trader, true),
        AccountMeta::new_readonly(*PHOENIX_PROGRAM_ID, false),
        AccountMeta::new_readonly(*PHOENIX_LOG_AUTHORITY, false),
        AccountMeta::new(*PHOENIX_GLOBAL_CONFIGURATION, false),
        AccountMeta::new(accounts.trader_account, false),
        AccountMeta::new(accounts.perp_asset_map, false),
        AccountMeta::new(accounts.orderbook, false),
        AccountMeta::new(accounts.spline_collection, false),
    ];
    metas.extend(
        accounts
            .global_trader_index
            .iter()
            .chain(&accounts.active_trader_buffer)
            .map(|pubkey| AccountMeta::new(*pubkey, false)),
    );
    Instruction {
        program_id,
        accounts: metas,
        data: ix_data("global:update_quotes", &params),
    }
}

fn ix_data<P: SchemaWrite<DefaultConfig, Src = P>>(name: &str, params: &P) -> Vec<u8> {
    let mut data = compute_discriminant(name).to_vec();
    data.extend(wincode::serialize(params).expect("params should serialize"));
    data
}

pub fn with_compute_budget(ix: Instruction) -> Vec<Instruction> {
    vec![
        ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
        ix,
    ]
}

pub fn assert_logs_contain(logs: &[String], needle: &str) {
    assert!(
        logs.iter().any(|log| log.contains(needle)),
        "expected logs to contain {needle:?}"
    );
}

fn print_logs(label: &str, logs: &[String]) {
    let mut stderr = io::stderr().lock();
    let _ = writeln!(stderr, "\n=== {label} ===");
    for log in logs {
        let _ = writeln!(stderr, "{log}");
    }
}

fn find_program_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var(PROGRAM_SO_ENV) {
        let path = PathBuf::from(path);
        if path.exists() {
            return Some(path);
        }
    }
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/deploy/phoenix_eternal_mm.so");
    path.exists().then_some(path)
}
