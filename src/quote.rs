use crate::params::PriceImprovementBehavior;

const BPS: u128 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Quotes {
    pub bid_price_in_ticks: u64,
    pub ask_price_in_ticks: u64,
}

pub(crate) fn quote_prices(fair_price_in_ticks: u64, edge_in_bps: u64) -> Quotes {
    let edge = (fair_price_in_ticks as u128 * edge_in_bps as u128 / BPS).max(1) as u64;
    Quotes {
        bid_price_in_ticks: fair_price_in_ticks.saturating_sub(edge),
        ask_price_in_ticks: fair_price_in_ticks.saturating_add(edge),
    }
}

pub(crate) fn apply_price_improvement(
    quotes: Quotes,
    behavior: PriceImprovementBehavior,
    best_bid: Option<u64>,
    best_ask: Option<u64>,
) -> Quotes {
    let Quotes {
        mut bid_price_in_ticks,
        mut ask_price_in_ticks,
    } = quotes;
    match behavior {
        PriceImprovementBehavior::Join => {
            if let Some(best_bid) = best_bid {
                bid_price_in_ticks = bid_price_in_ticks.min(best_bid);
            }
            if let Some(best_ask) = best_ask {
                ask_price_in_ticks = ask_price_in_ticks.max(best_ask);
            }
        }
        PriceImprovementBehavior::Dime => {
            if let Some(best_bid) = best_bid {
                bid_price_in_ticks = bid_price_in_ticks.min(best_bid.saturating_add(1));
            }
            if let Some(best_ask) = best_ask {
                ask_price_in_ticks = ask_price_in_ticks.max(best_ask.saturating_sub(1));
            }
        }
        PriceImprovementBehavior::Ignore => {}
    }
    Quotes {
        bid_price_in_ticks,
        ask_price_in_ticks,
    }
}

pub(crate) fn size_in_base_lots(
    quote_size_in_quote_lots: u64,
    price_in_ticks: u64,
    tick_size_in_quote_lots_per_base_lot: u64,
) -> u64 {
    let quote_lots_per_base_lot =
        price_in_ticks as u128 * tick_size_in_quote_lots_per_base_lot as u128;
    if quote_lots_per_base_lot == 0 {
        return 0;
    }
    (quote_size_in_quote_lots as u128 / quote_lots_per_base_lot).min(u64::MAX as u128) as u64
}

pub(crate) fn client_order_id(authority: &[u8; 32]) -> u128 {
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&authority[..16]);
    u128::from_le_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_is_symmetric_and_at_least_one_tick() {
        assert_eq!(
            quote_prices(100_000, 10),
            Quotes {
                bid_price_in_ticks: 99_900,
                ask_price_in_ticks: 100_100,
            }
        );
        assert_eq!(
            quote_prices(50, 1),
            Quotes {
                bid_price_in_ticks: 49,
                ask_price_in_ticks: 51,
            }
        );
    }

    #[test]
    fn price_improvement_behaviors() {
        let quotes = quote_prices(100_000, 1);
        let best = (Some(99_500), Some(100_500));
        let join = apply_price_improvement(quotes, PriceImprovementBehavior::Join, best.0, best.1);
        assert_eq!(
            (join.bid_price_in_ticks, join.ask_price_in_ticks),
            (99_500, 100_500)
        );
        let dime = apply_price_improvement(quotes, PriceImprovementBehavior::Dime, best.0, best.1);
        assert_eq!(
            (dime.bid_price_in_ticks, dime.ask_price_in_ticks),
            (99_501, 100_499)
        );
        let ignore =
            apply_price_improvement(quotes, PriceImprovementBehavior::Ignore, best.0, best.1);
        assert_eq!(ignore, quotes);
        let empty = apply_price_improvement(quotes, PriceImprovementBehavior::Join, None, None);
        assert_eq!(empty, quotes);
    }

    #[test]
    fn quote_notional_converts_to_base_lots() {
        assert_eq!(size_in_base_lots(1_000_000_000, 100_000, 100), 100);
        assert_eq!(size_in_base_lots(1_000_000_000, 100_100, 100), 99);
        assert_eq!(size_in_base_lots(1, 100_000, 100), 0);
        assert_eq!(size_in_base_lots(1, 0, 100), 0);
    }
}
