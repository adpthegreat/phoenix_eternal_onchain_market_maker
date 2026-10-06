use phoenix_rise::ix::{
    order_packet::CondensedOrder,
    types::{CancelId, FifoOrderId, Side},
};
use pinocchio::{
    ProgramResult,
    account_info::AccountInfo,
    msg,
    program_error::ProgramError,
    pubkey::create_program_address,
    sysvars::{Sysvar, clock::Clock},
};

use crate::{
    STRATEGY_SEED, book,
    error::MarketMakerError,
    market::MarketContext,
    params::{PriceImprovementBehavior, UpdateQuotesParams},
    quote::{apply_price_improvement, client_order_id, quote_prices, size_in_base_lots},
    state::PhoenixStrategyState,
};

pub(crate) fn process(accounts: &[AccountInfo], params: &UpdateQuotesParams) -> ProgramResult {
    let context = MarketContext::load(
        accounts,
        params.global_trader_index_count as usize,
        params.active_trader_buffer_count as usize,
    )?;
    if params.fair_price_in_ticks == 0 {
        return Err(MarketMakerError::InvalidFairPrice.into());
    }

    let mut strategy_data = context.strategy.try_borrow_mut_data()?;
    let phoenix_strategy = PhoenixStrategyState::load_mut(context.strategy, &mut strategy_data)?;
    let expected = create_program_address(
        &[
            STRATEGY_SEED,
            context.trader.key(),
            context.orderbook.key(),
            &[phoenix_strategy.bump],
        ],
        &crate::ID,
    );
    if expected.as_ref() != Ok(context.strategy.key()) {
        msg!("phoenix-eternal-mm: strategy PDA mismatch");
        return Err(ProgramError::InvalidSeeds);
    }
    if &phoenix_strategy.trader != context.trader.key()
        || &phoenix_strategy.trader_account != context.trader_account.key()
        || &phoenix_strategy.market != context.orderbook.key()
    {
        msg!("phoenix-eternal-mm: strategy account mismatch");
        return Err(MarketMakerError::StrategyAccountMismatch.into());
    }

    // Update timestamps
    let clock = Clock::get()?;
    phoenix_strategy.last_update_slot = clock.slot;
    phoenix_strategy.last_update_unix_timestamp = clock.unix_timestamp;

    // Update the strategy parameters
    let strategy_params = &params.strategy;
    if let Some(edge) = strategy_params.quote_edge_in_bps.filter(|edge| *edge > 0) {
        phoenix_strategy.quote_edge_in_bps = edge;
    }
    if let Some(size) = strategy_params.quote_size_in_quote_lots {
        phoenix_strategy.quote_size_in_quote_lots = size;
    }
    if let Some(post_only) = strategy_params.post_only {
        phoenix_strategy.post_only = post_only as u8;
    }
    if let Some(behavior) = strategy_params.price_improvement_behavior {
        phoenix_strategy.price_improvement_behavior = behavior.to_u8();
    }

    // Load market
    let book_data = context.orderbook.try_borrow_data()?;
    let orderbook = book::load(context.orderbook, &book_data)?;
    let header = orderbook.header();
    let tick_size = header.tick_size_in_quote_lots_per_base_lot.as_inner();
    let next_order_sequence_number = header.order_sequence_number.sequence_number;

    // Best bid and ask that are not ours, and whether our quotes are still resting
    let (best_bid, resting_bid) = book::best_excluding(
        orderbook.bids(),
        phoenix_strategy.bid_price_in_ticks,
        phoenix_strategy.bid_order_sequence_number,
    );
    let (best_ask, resting_ask) = book::best_excluding(
        orderbook.asks(),
        phoenix_strategy.ask_price_in_ticks,
        phoenix_strategy.ask_order_sequence_number,
    );

    // Compute quote prices and sizes
    let price_improvement_behavior = phoenix_strategy.price_improvement()?;
    let quotes = apply_price_improvement(
        quote_prices(
            params.fair_price_in_ticks,
            phoenix_strategy.quote_edge_in_bps,
        ),
        price_improvement_behavior,
        best_bid,
        best_ask,
    );
    let bid_price_in_ticks = quotes.bid_price_in_ticks;
    let ask_price_in_ticks = quotes.ask_price_in_ticks;
    let quote_size = phoenix_strategy.quote_size_in_quote_lots;
    let bid_size_in_base_lots = size_in_base_lots(quote_size, bid_price_in_ticks, tick_size);
    let ask_size_in_base_lots = size_in_base_lots(quote_size, ask_price_in_ticks, tick_size);

    msg!(&format!(
        "phoenix-eternal-mm: market {} @ {} quoting {} {} @ {} {}",
        best_bid.unwrap_or(0),
        best_ask.unwrap_or(0),
        bid_size_in_base_lots,
        bid_price_in_ticks,
        ask_price_in_ticks,
        ask_size_in_base_lots
    ));

    // Don't quote a side if the price is invalid or the size is 0
    let mut update_bid = bid_price_in_ticks > 0 && bid_size_in_base_lots > 0;
    let mut update_ask = ask_price_in_ticks < u64::MAX && ask_size_in_base_lots > 0;

    // Keep identical resting quotes; cancel partially filled or stale ones
    let mut orders_to_cancel = [CancelId::new(0, 0); 2];
    let mut cancel_count = 0;
    let mut keep_bid = false;
    let mut keep_ask = false;
    if let Some(remaining) = resting_bid {
        if update_bid
            && remaining == bid_size_in_base_lots
            && phoenix_strategy.bid_price_in_ticks == bid_price_in_ticks
        {
            update_bid = false;
            keep_bid = true;
        } else {
            orders_to_cancel[cancel_count] = cancel_id(
                phoenix_strategy.bid_price_in_ticks,
                phoenix_strategy.bid_order_sequence_number,
            );
            cancel_count += 1;
        }
    }
    if let Some(remaining) = resting_ask {
        if update_ask
            && remaining == ask_size_in_base_lots
            && phoenix_strategy.ask_price_in_ticks == ask_price_in_ticks
        {
            update_ask = false;
            keep_ask = true;
        } else {
            orders_to_cancel[cancel_count] = cancel_id(
                phoenix_strategy.ask_price_in_ticks,
                phoenix_strategy.ask_order_sequence_number,
            );
            cancel_count += 1;
        }
    }
    if !keep_bid {
        set_bid(phoenix_strategy, (0, 0, 0));
    }
    if !keep_ask {
        set_ask(phoenix_strategy, (0, 0, 0));
    }

    // Drop reference prior to invoking
    drop(book_data);

    // Cancel the old orders
    if cancel_count > 0 {
        context.cancel(&orders_to_cancel[..cancel_count])?;
    }

    if !update_bid && !update_ask {
        msg!("phoenix-eternal-mm: no quotes to place");
        return Ok(());
    }

    let client_order_id = client_order_id(context.trader.key());
    if phoenix_strategy.is_post_only()
        || price_improvement_behavior != PriceImprovementBehavior::Join
    {
        // Send both post-only orders in a single instruction
        context.place_post_only(
            update_bid.then(|| condensed(bid_price_in_ticks, bid_size_in_base_lots)),
            update_ask.then(|| condensed(ask_price_in_ticks, ask_size_in_base_lots)),
            client_order_id,
        )?;
    } else {
        if update_bid {
            context.place_limit(
                Side::Bid,
                bid_price_in_ticks,
                bid_size_in_base_lots,
                client_order_id,
            )?;
        }
        if update_ask {
            context.place_limit(
                Side::Ask,
                ask_price_in_ticks,
                ask_size_in_base_lots,
                client_order_id,
            )?;
        }
    }

    // Reload the market and record the orders this instruction placed
    let book_data = context.orderbook.try_borrow_data()?;
    let orderbook = book::load(context.orderbook, &book_data)?;
    if update_bid {
        let placed = book::find_new_order(orderbook.bids(), next_order_sequence_number);
        set_bid(phoenix_strategy, placed);
    }
    if update_ask {
        let placed = book::find_new_order(orderbook.asks(), next_order_sequence_number);
        set_ask(phoenix_strategy, placed);
    }

    msg!(&format!(
        "phoenix-eternal-mm: resting bid {} {} seq={} ask {} {} seq={}",
        phoenix_strategy.initial_bid_size_in_base_lots,
        phoenix_strategy.bid_price_in_ticks,
        phoenix_strategy.bid_order_sequence_number,
        phoenix_strategy.ask_price_in_ticks,
        phoenix_strategy.initial_ask_size_in_base_lots,
        phoenix_strategy.ask_order_sequence_number
    ));
    Ok(())
}

fn cancel_id(price_in_ticks: u64, order_sequence_number: u64) -> CancelId {
    CancelId {
        node_pointer: 0,
        order_id: FifoOrderId {
            price_in_ticks,
            order_sequence_number,
        },
    }
}

fn condensed(price_in_ticks: u64, size_in_base_lots: u64) -> CondensedOrder {
    CondensedOrder {
        price_in_ticks,
        size_in_base_lots,
        last_valid_slot: None,
    }
}

fn set_bid(state: &mut PhoenixStrategyState, (price, sequence_number, size): (u64, u64, u64)) {
    state.bid_price_in_ticks = price;
    state.bid_order_sequence_number = sequence_number;
    state.initial_bid_size_in_base_lots = size;
}

fn set_ask(state: &mut PhoenixStrategyState, (price, sequence_number, size): (u64, u64, u64)) {
    state.ask_price_in_ticks = price;
    state.ask_order_sequence_number = sequence_number;
    state.initial_ask_size_in_base_lots = size;
}
