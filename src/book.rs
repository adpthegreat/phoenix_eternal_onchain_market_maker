use phoenix_rise::accounts::orderbook::{Orderbook, OrderbookSide, Side};
use pinocchio::{account_info::AccountInfo, msg, program_error::ProgramError};

use crate::{PHOENIX_PROGRAM_ID, error::MarketMakerError};

pub(crate) fn load<'a>(
    orderbook: &AccountInfo,
    data: &'a [u8],
) -> Result<Orderbook<'a>, ProgramError> {
    if !orderbook.is_owned_by(&PHOENIX_PROGRAM_ID) {
        msg!("phoenix-eternal-mm: orderbook not owned by Phoenix");
        return Err(MarketMakerError::InvalidPhoenixAccount.into());
    }
    Orderbook::try_from_account_bytes(data).map_err(|error| {
        msg!(&format!(
            "phoenix-eternal-mm: failed to deserialize orderbook: {error}"
        ));
        MarketMakerError::InvalidPhoenixAccount.into()
    })
}

/// Best price on `side` excluding our order, and our order's remaining base
/// lots if it is still resting.
pub(crate) fn best_excluding(
    side: &OrderbookSide<'_>,
    price_in_ticks: u64,
    order_sequence_number: u64,
) -> (Option<u64>, Option<u64>) {
    let tracked = price_in_ticks != 0;
    let mut best = None;
    let mut resting = None;
    for entry in side.iter() {
        let price = entry.price_in_ticks().as_inner();
        if tracked
            && price == price_in_ticks
            && entry.order_sequence_number() == order_sequence_number
        {
            resting = Some(entry.num_base_lots_remaining().as_inner());
        } else if best.is_none() {
            best = Some(price);
        }
        let past_ours = match side.side() {
            Side::Bid => price < price_in_ticks,
            Side::Ask => price > price_in_ticks,
        };
        if best.is_some() && (resting.is_some() || !tracked || past_ours) {
            break;
        }
    }
    (best, resting)
}

/// First order on `side` created at or after `min_raw_sequence_number`, as
/// `(price_in_ticks, order_sequence_number, remaining_base_lots)`.
pub(crate) fn find_new_order(
    side: &OrderbookSide<'_>,
    min_raw_sequence_number: u64,
) -> (u64, u64, u64) {
    side.iter()
        .find(|entry| entry.raw_sequence_number() >= min_raw_sequence_number)
        .map(|entry| {
            (
                entry.price_in_ticks().as_inner(),
                entry.order_sequence_number(),
                entry.num_base_lots_remaining().as_inner(),
            )
        })
        .unwrap_or_default()
}
