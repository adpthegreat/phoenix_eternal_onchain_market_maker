pub mod common;

use common::{
    FAIR_PRICE_IN_TICKS, PriceImprovementBehavior, QUOTE_SIZE_IN_QUOTE_LOTS, StrategyParams,
    assert_logs_contain, initialize_ix, setup, size_in_base_lots, state_discriminator,
    strategy_params,
};
use phoenix_rise::ix::types::Side;
use phoenix_rise_litesvm_test::parse_pubkey;

fn size(price: u64) -> u64 {
    size_in_base_lots(QUOTE_SIZE_IN_QUOTE_LOTS, price)
}

#[test]
fn initialize_creates_strategy() {
    let Some(mut h) = setup() else { return };
    let logs = h.send(
        h.initialize_ix(strategy_params(10, PriceImprovementBehavior::Ignore, true)),
        "initialize",
    );
    assert_logs_contain(&logs, "phoenix-eternal-mm: initialized edge_bps=10");

    let state = h.state();
    assert_eq!(state.discriminator, state_discriminator());
    assert_eq!(state.trader, h.maker_key().to_bytes());
    assert_eq!(state.trader_account, h.maker_trader().to_bytes());
    assert_eq!(state.market, h.orderbook().to_bytes());
    assert_eq!(state.quote_edge_in_bps, 10);
    assert_eq!(state.quote_size_in_quote_lots, QUOTE_SIZE_IN_QUOTE_LOTS);
    assert_eq!(state.post_only, 1);
    assert!(!state.bid().is_active() && !state.ask().is_active());
}

#[test]
fn initialize_rejects_invalid_params() {
    let Some(mut h) = setup() else { return };
    assert!(
        h.try_send(h.initialize_ix(strategy_params(0, PriceImprovementBehavior::Ignore, true)))
            .is_err()
    );
    assert!(
        h.try_send(h.initialize_ix(StrategyParams {
            quote_size_in_quote_lots: None,
            ..strategy_params(10, PriceImprovementBehavior::Ignore, true)
        }))
        .is_err()
    );
}

#[test]
fn initialize_rejects_foreign_trader_account() {
    let Some(mut h) = setup() else { return };
    let other = h.context.actor(common::TAKER);
    let ix = initialize_ix(
        h.program_id,
        h.maker_key(),
        parse_pubkey(&other.trader_account).unwrap(),
        h.orderbook(),
        strategy_params(10, PriceImprovementBehavior::Ignore, true),
    );
    let logs = h.try_send(ix).unwrap_err();
    assert_logs_contain(&logs, "trader authority mismatch");
}

#[test]
fn update_quotes_posts_two_sided_market() {
    let Some(mut h) = setup() else { return };
    let params = strategy_params(10, PriceImprovementBehavior::Ignore, true);
    h.send(h.initialize_ix(params), "initialize");
    h.send(
        h.update_quotes_ix(FAIR_PRICE_IN_TICKS, StrategyParams::default()),
        "update-quotes",
    );

    let state = h.state();
    assert_eq!(state.bid().price_in_ticks, 99_900);
    assert_eq!(state.ask().price_in_ticks, 100_100);
    assert_eq!(state.bid().size_in_base_lots, size(99_900));
    assert_eq!(state.ask().size_in_base_lots, size(100_100));

    let book = h.book_orders();
    assert!(book.has_bid(
        state.bid().price_in_ticks,
        state.bid().order_sequence_number
    ));
    assert!(book.has_ask(
        state.ask().price_in_ticks,
        state.ask().order_sequence_number
    ));
}

#[test]
fn update_quotes_keeps_identical_quotes() {
    let Some(mut h) = setup() else { return };
    h.send(
        h.initialize_ix(strategy_params(10, PriceImprovementBehavior::Ignore, true)),
        "initialize",
    );
    h.send(
        h.update_quotes_ix(FAIR_PRICE_IN_TICKS, StrategyParams::default()),
        "update-quotes",
    );
    let before = h.state();

    h.context.svm.warp_to_slot(201);
    let logs = h.send(
        h.update_quotes_ix(FAIR_PRICE_IN_TICKS, StrategyParams::default()),
        "update-quotes-again",
    );
    assert_logs_contain(&logs, "phoenix-eternal-mm: no quotes to place");
    let after = h.state();
    assert_eq!(before.bid(), after.bid());
    assert_eq!(before.ask(), after.ask());
    assert_eq!(after.last_update_slot, 201);
}

#[test]
fn update_quotes_requotes_when_fair_price_moves() {
    let Some(mut h) = setup() else { return };
    h.send(
        h.initialize_ix(strategy_params(10, PriceImprovementBehavior::Ignore, true)),
        "initialize",
    );
    h.send(
        h.update_quotes_ix(FAIR_PRICE_IN_TICKS, StrategyParams::default()),
        "update-quotes",
    );
    let before = h.state();

    let logs = h.send(
        h.update_quotes_ix(100_200, StrategyParams::default()),
        "update-quotes-moved",
    );
    assert_logs_contain(&logs, "phoenix-eternal-mm: cancel stale quotes");
    let after = h.state();
    assert_eq!(after.bid().price_in_ticks, 100_100);
    assert_eq!(after.ask().price_in_ticks, 100_300);

    let book = h.book_orders();
    assert!(!book.has_bid(
        before.bid().price_in_ticks,
        before.bid().order_sequence_number
    ));
    assert!(!book.has_ask(
        before.ask().price_in_ticks,
        before.ask().order_sequence_number
    ));
    assert!(book.has_bid(
        after.bid().price_in_ticks,
        after.bid().order_sequence_number
    ));
    assert!(book.has_ask(
        after.ask().price_in_ticks,
        after.ask().order_sequence_number
    ));
}

#[test]
fn update_quotes_applies_strategy_param_overrides() {
    let Some(mut h) = setup() else { return };
    h.send(
        h.initialize_ix(strategy_params(10, PriceImprovementBehavior::Ignore, true)),
        "initialize",
    );
    h.send(
        h.update_quotes_ix(
            FAIR_PRICE_IN_TICKS,
            StrategyParams {
                quote_edge_in_bps: Some(20),
                quote_size_in_quote_lots: Some(2 * QUOTE_SIZE_IN_QUOTE_LOTS),
                ..StrategyParams::default()
            },
        ),
        "update-quotes",
    );
    let state = h.state();
    assert_eq!(state.quote_edge_in_bps, 20);
    assert_eq!(state.bid().price_in_ticks, 99_800);
    assert_eq!(state.ask().price_in_ticks, 100_200);
    assert_eq!(
        state.bid().size_in_base_lots,
        size_in_base_lots(2 * QUOTE_SIZE_IN_QUOTE_LOTS, 99_800)
    );
}

#[test]
fn join_clamps_to_best_prices_with_limit_orders() {
    let Some(mut h) = setup() else { return };
    h.send(
        h.initialize_ix(strategy_params(1, PriceImprovementBehavior::Join, false)),
        "initialize",
    );
    let logs = h.send(
        h.update_quotes_ix(FAIR_PRICE_IN_TICKS, StrategyParams::default()),
        "update-quotes",
    );
    assert_logs_contain(&logs, "phoenix-eternal-mm: place limit quote");

    let state = h.state();
    assert_eq!(state.bid().price_in_ticks, 99_500);
    assert_eq!(state.ask().price_in_ticks, 100_500);
    assert_eq!(state.bid().size_in_base_lots, size(99_500));
    assert_eq!(state.ask().size_in_base_lots, size(100_500));
}

#[test]
fn dime_improves_best_prices_by_one_tick() {
    let Some(mut h) = setup() else { return };
    h.send(
        h.initialize_ix(strategy_params(1, PriceImprovementBehavior::Dime, false)),
        "initialize",
    );
    let logs = h.send(
        h.update_quotes_ix(FAIR_PRICE_IN_TICKS, StrategyParams::default()),
        "update-quotes",
    );
    assert_logs_contain(&logs, "phoenix-eternal-mm: place post-only quotes");

    let state = h.state();
    assert_eq!(state.bid().price_in_ticks, 99_501);
    assert_eq!(state.ask().price_in_ticks, 100_499);
}

#[test]
fn filled_quote_is_replaced() {
    let Some(mut h) = setup() else { return };
    h.send(
        h.initialize_ix(strategy_params(1, PriceImprovementBehavior::Dime, true)),
        "initialize",
    );
    h.send(
        h.update_quotes_ix(FAIR_PRICE_IN_TICKS, StrategyParams::default()),
        "update-quotes",
    );
    let before = h.state();

    h.taker_market_order(Side::Ask, before.bid().size_in_base_lots);
    assert!(!h.book_orders().has_bid(
        before.bid().price_in_ticks,
        before.bid().order_sequence_number
    ));

    h.send(
        h.update_quotes_ix(FAIR_PRICE_IN_TICKS, StrategyParams::default()),
        "update-quotes-after-fill",
    );
    let after = h.state();
    assert!(after.bid().is_active());
    assert_ne!(
        after.bid().order_sequence_number,
        before.bid().order_sequence_number
    );
    assert_eq!(after.ask(), before.ask());
    assert!(h.book_orders().has_bid(
        after.bid().price_in_ticks,
        after.bid().order_sequence_number
    ));
}

#[test]
fn update_quotes_rejects_foreign_trader() {
    let Some(mut h) = setup() else { return };
    h.send(
        h.initialize_ix(strategy_params(10, PriceImprovementBehavior::Ignore, true)),
        "initialize",
    );
    let other = h.context.actor(common::TAKER);
    let mut ix = h.update_quotes_ix(FAIR_PRICE_IN_TICKS, StrategyParams::default());
    ix.accounts[1].pubkey = parse_pubkey(&other.pubkey).unwrap();
    ix.accounts[5].pubkey = parse_pubkey(&other.trader_account).unwrap();
    let logs = h
        .context
        .try_send_instructions_with_metadata(common::with_compute_budget(ix), &other.seed)
        .unwrap_err()
        .meta
        .logs;
    assert_logs_contain(&logs, "phoenix-eternal-mm: strategy PDA mismatch");
}

#[test]
fn update_quotes_rejects_zero_fair_price() {
    let Some(mut h) = setup() else { return };
    h.send(
        h.initialize_ix(strategy_params(10, PriceImprovementBehavior::Ignore, true)),
        "initialize",
    );
    assert!(
        h.try_send(h.update_quotes_ix(0, StrategyParams::default()))
            .is_err()
    );
}
