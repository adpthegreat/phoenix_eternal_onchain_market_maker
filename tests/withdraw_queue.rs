//! Live check: print the mainnet Phoenix withdraw queue and where a wallet
//! sits in it.
//!
//! cargo test --test withdraw_queue
//! Override the wallet with PHOENIX_WITHDRAW_WALLET and the RPC with
//! PHOENIX_MAINNET_RPC_URL. The report is written straight to the terminal,
//! so `--nocapture` is not needed.

use std::{
    fmt::Arguments,
    io::{self, Write},
    str::FromStr,
};

use phoenix_eternal_mm::withdraw_queue::{budget_at_slot, find_position};
use phoenix_rise::{
    accounts::withdraw_queue::{WithdrawQueue, WithdrawRequest},
    ix::constants::PHOENIX_PROGRAM_ID,
};
use serde_json::Value;
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;

const PHOENIX_API: &str = "https://perp-api.phoenix.trade";
const DEFAULT_DATASOURCE_RPC: &str = "https://api.mainnet-beta.solana.com";
/// Read-only demo account from phoenix.trade.
const DEFAULT_WALLET: &str = "72de7tXDEEZSAuFaw3QkdhXtC8Zph1rdF3aWV4v4WGCA";
/// Queue entries listed in the report.
const MAX_LISTED_ENTRIES: usize = 20;

/// Writes past libtest's output capture so the report always shows.
fn out(args: Arguments<'_>) {
    let mut stderr = io::stderr().lock();
    stderr.write_fmt(args).unwrap();
    stderr.write_all(b"\n").unwrap();
}

macro_rules! report {
    () => { out(format_args!("")) };
    ($($arg:tt)*) => { out(format_args!($($arg)*)) };
}

fn trader_pda(authority: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"trader", authority.as_ref(), &[0, 0]],
        &PHOENIX_PROGRAM_ID,
    )
    .0
}

fn fetch_withdraw_queue_address() -> Result<Pubkey, String> {
    let body = ureq::get(format!("{PHOENIX_API}/v1/view/exchange/keys"))
        .call()
        .map_err(|error| format!("exchange keys request: {error}"))?
        .into_body()
        .read_to_string()
        .map_err(|error| format!("exchange keys body: {error}"))?;
    let keys: Value = serde_json::from_str(&body).map_err(|error| error.to_string())?;
    let address = keys["withdrawQueue"]
        .as_str()
        .ok_or("exchange keys missing withdrawQueue")?;
    Pubkey::from_str(address).map_err(|error| error.to_string())
}

fn describe(request: &WithdrawRequest) -> String {
    format!(
        "trader {}  wallet {}  amount {}  state {:?}  reason {:?}  submitted slot {}",
        Pubkey::new_from_array(request.trader_key()),
        Pubkey::new_from_array(request.wallet_key()),
        request.amount().as_inner(),
        request.state(),
        request.last_transition_reason(),
        request.submission_slot(),
    )
}

#[test]
fn prints_wallet_position_in_mainnet_withdraw_queue() {
    let wallet = std::env::var("PHOENIX_WITHDRAW_WALLET").unwrap_or_else(|_| DEFAULT_WALLET.into());
    let wallet = Pubkey::from_str(&wallet).expect("wallet pubkey");
    let rpc_url =
        std::env::var("PHOENIX_MAINNET_RPC_URL").unwrap_or_else(|_| DEFAULT_DATASOURCE_RPC.into());

    let address = match fetch_withdraw_queue_address() {
        Ok(address) => address,
        Err(error) => {
            report!("skipping: could not reach the Phoenix API ({error})");
            return;
        }
    };
    let rpc = RpcClient::new(rpc_url.clone());
    let (account, slot) = match rpc.get_account(&address).and_then(|account| {
        let slot = rpc.get_slot()?;
        Ok((account, slot))
    }) {
        Ok(fetched) => fetched,
        Err(error) => {
            report!("skipping: could not fetch the withdraw queue from {rpc_url} ({error})");
            return;
        }
    };

    assert_eq!(
        account.owner.to_string(),
        PHOENIX_PROGRAM_ID.to_string(),
        "withdraw queue owner"
    );
    let queue =
        WithdrawQueue::try_from_account_bytes(&account.data).expect("decode withdraw queue");
    let header = queue.header();
    let throttle = header.withdraw_throttle();

    report!();
    report!("Phoenix withdraw queue");
    report!("  address              {address}");
    report!("  owner                {}", account.owner);
    report!("  account size         {} bytes", account.data.len());
    report!("  rpc                  {rpc_url}");
    report!("  current slot         {slot}");
    report!(
        "  requests             {} / {}",
        queue.len(),
        queue.capacity()
    );
    report!(
        "  total queued         {} quote lots",
        header.total_queued_amount().as_inner()
    );
    report!(
        "  withdrawal fee       {} quote lots",
        header.withdrawal_fee().as_inner()
    );
    report!(
        "  enqueueing fee       {} quote lots",
        header.enqueueing_fee().as_inner()
    );
    report!();
    report!("Withdraw throttle");
    report!(
        "  max budget           {} quote lots",
        throttle.max_budget().as_inner()
    );
    report!(
        "  remaining (stored)   {} quote lots",
        throttle.remaining_budget().as_inner()
    );
    report!(
        "  available now        {} quote lots",
        budget_at_slot(&throttle, slot)
    );
    report!(
        "  replenish per slot   {} quote lots",
        throttle.replenish_amount_per_slot().as_inner()
    );
    report!("  last update slot     {}", throttle.last_update_slot());
    report!();
    report!("Lookup");
    report!("  wallet               {wallet}");
    report!(
        "  trader account       {} (pda 0, subaccount 0)",
        trader_pda(&wallet)
    );

    match find_position(&queue, &wallet.to_bytes()).expect("walk withdraw queue") {
        Some(position) => {
            let eta = position
                .estimated_slots_remaining(&throttle, slot)
                .map_or("never (throttle does not replenish)".into(), |slots| {
                    format!("~{slots} slots (~{:.1}s at 400ms/slot)", slots as f64 * 0.4)
                });
            report!(
                "  position             {} ({} requests ahead)",
                position.position + 1,
                position.position
            );
            report!("  node index           {}", position.node_index);
            report!("  quote lots ahead     {}", position.quote_lots_ahead);
            report!("  quote lots needed    {}", position.quote_lots_needed());
            report!("  estimated wait       {eta}");
            report!("  request              {}", describe(&position.request));
        }
        None => report!("  position             not in queue (no active withdraw request)"),
    }

    report!();
    if queue.is_empty() {
        report!("Queue entries: none");
    } else {
        report!("Queue entries (first {MAX_LISTED_ENTRIES}, head first)");
        for (position, entry) in queue.iter().take(MAX_LISTED_ENTRIES).enumerate() {
            let entry = entry.expect("decode withdraw queue entry");
            report!(
                "  #{:<4} node {:<5} {}",
                position + 1,
                entry.node_index,
                describe(&entry.request)
            );
        }
    }
    report!();
}
