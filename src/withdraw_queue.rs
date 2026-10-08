//! Locate a trader's request in the Phoenix withdraw queue.
//!
//! There is one exchange-wide queue (`GlobalConfig::withdraw_queue_key`, or
//! `withdrawQueue` from `/v1/view/exchange/keys`) with no per-trader index, so
//! finding a request means walking the queue from its head.

use phoenix_rise::accounts::{
    PhoenixAccountDecodeError,
    withdraw_queue::{WithdrawQueue, WithdrawRequest, WithdrawThrottle},
};
use pinocchio::{account_info::AccountInfo, msg, program_error::ProgramError};

use crate::{PHOENIX_PROGRAM_ID, error::MarketMakerError};

/// Where a request sits in the withdraw queue.
#[derive(Debug, Clone, Copy)]
pub struct WithdrawQueuePosition {
    /// Active requests ahead of this one; 0 means next to be processed.
    pub position: usize,
    pub node_index: u32,
    pub request: WithdrawRequest,
    /// Quote lots requested by active requests ahead of this one.
    pub quote_lots_ahead: u64,
    /// Total number of nodes in the queue.
    pub queue_len: usize,
}

impl WithdrawQueuePosition {
    /// Quote lots of throttle budget needed before this request clears.
    pub fn quote_lots_needed(&self) -> u64 {
        self.quote_lots_ahead
            .saturating_add(self.request.amount().as_inner())
    }

    /// Rough number of slots until enough throttle budget accrues for this
    /// request and everything ahead of it. Assumes strict FIFO processing and
    /// that cranks keep up; `None` if the throttle never replenishes.
    pub fn estimated_slots_remaining(
        &self,
        throttle: &WithdrawThrottle,
        current_slot: u64,
    ) -> Option<u64> {
        let budget = budget_at_slot(throttle, current_slot);
        let needed = self.quote_lots_needed();
        if needed <= budget {
            return Some(0);
        }
        let replenish = throttle.replenish_amount_per_slot().as_inner();
        if replenish == 0 {
            return None;
        }
        Some((needed - budget).div_ceil(replenish))
    }
}

/// Throttle budget available at `current_slot`, including replenishment since
/// the throttle was last written.
pub fn budget_at_slot(throttle: &WithdrawThrottle, current_slot: u64) -> u64 {
    let elapsed = current_slot.saturating_sub(throttle.last_update_slot());
    throttle
        .remaining_budget()
        .as_inner()
        .saturating_add(elapsed.saturating_mul(throttle.replenish_amount_per_slot().as_inner()))
        .min(throttle.max_budget().as_inner())
}

/// Finds the first active request whose trader account or wallet is `key`.
pub fn find_position(
    queue: &WithdrawQueue<'_>,
    key: &[u8; 32],
) -> Result<Option<WithdrawQueuePosition>, PhoenixAccountDecodeError> {
    let mut position = 0;
    let mut quote_lots_ahead = 0u64;
    for entry in queue.iter() {
        let entry = entry?;
        let request = entry.request;
        if !request.is_active() {
            continue;
        }
        if &request.trader_key() == key || &request.wallet_key() == key {
            return Ok(Some(WithdrawQueuePosition {
                position,
                node_index: entry.node_index,
                request,
                quote_lots_ahead,
                queue_len: queue.len(),
            }));
        }
        position += 1;
        quote_lots_ahead = quote_lots_ahead.saturating_add(request.amount().as_inner());
    }
    Ok(None)
}

/// Decodes the withdraw queue from an account owned by Phoenix.
pub fn load<'a>(
    withdraw_queue: &AccountInfo,
    data: &'a [u8],
) -> Result<WithdrawQueue<'a>, ProgramError> {
    if !withdraw_queue.is_owned_by(&PHOENIX_PROGRAM_ID) {
        msg!("phoenix-eternal-mm: withdraw queue not owned by Phoenix");
        return Err(MarketMakerError::InvalidPhoenixAccount.into());
    }
    WithdrawQueue::try_from_account_bytes(data).map_err(|error| {
        msg!(&format!(
            "phoenix-eternal-mm: failed to deserialize withdraw queue: {error}"
        ));
        MarketMakerError::InvalidPhoenixAccount.into()
    })
}

#[cfg(test)]
mod tests {
    use phoenix_rise::accounts::PhoenixAccount;

    use super::*;

    const PREFIX_LEN: usize = 144;
    const NODE_LEN: usize = 104;
    const MAX_SIZE: usize = 2048;
    const ACTIVE: u8 = 1;
    const PROCESSED: u8 = 4;

    /// `(trader, wallet, amount, state)` per node, linked head to tail.
    fn queue_bytes(requests: &[([u8; 32], [u8; 32], u64, u8)]) -> Vec<u8> {
        let mut data = vec![0u8; PREFIX_LEN + NODE_LEN * MAX_SIZE];
        data[..8].copy_from_slice(&PhoenixAccount::WithdrawQueueHeader.discriminant());
        let count = requests.len() as u32;
        if count > 0 {
            data[120..124].copy_from_slice(&1u32.to_le_bytes());
            data[124..128].copy_from_slice(&count.to_le_bytes());
        }
        data[128..136].copy_from_slice(&(count as u64).to_le_bytes());
        data[136..140].copy_from_slice(&(count + 1).to_le_bytes());
        for (i, (trader, wallet, amount, state)) in requests.iter().enumerate() {
            let index = i as u32 + 1;
            let node = &mut data[PREFIX_LEN + i * NODE_LEN..][..NODE_LEN];
            let next = if index == count { 0 } else { index + 1 };
            node[0..4].copy_from_slice(&(index - 1).to_le_bytes());
            node[4..8].copy_from_slice(&next.to_le_bytes());
            node[8..40].copy_from_slice(trader);
            node[40..72].copy_from_slice(wallet);
            node[72..80].copy_from_slice(&amount.to_le_bytes());
            node[98] = *state;
        }
        data
    }

    #[test]
    fn finds_position_by_trader_or_wallet() {
        let data = queue_bytes(&[
            ([1; 32], [11; 32], 100, ACTIVE),
            ([2; 32], [12; 32], 50, PROCESSED),
            ([3; 32], [13; 32], 200, ACTIVE),
            ([4; 32], [14; 32], 75, ACTIVE),
        ]);
        let queue = WithdrawQueue::try_from_account_bytes(&data).unwrap();

        let by_trader = find_position(&queue, &[4; 32]).unwrap().unwrap();
        assert_eq!(by_trader.position, 2);
        assert_eq!(by_trader.node_index, 4);
        assert_eq!(by_trader.quote_lots_ahead, 300);
        assert_eq!(by_trader.quote_lots_needed(), 375);
        assert_eq!(by_trader.queue_len, 4);

        let by_wallet = find_position(&queue, &[11; 32]).unwrap().unwrap();
        assert_eq!(by_wallet.position, 0);
        assert_eq!(by_wallet.quote_lots_ahead, 0);

        assert!(find_position(&queue, &[2; 32]).unwrap().is_none());
        assert!(find_position(&queue, &[9; 32]).unwrap().is_none());
    }

    #[test]
    fn empty_queue_has_no_position() {
        let data = queue_bytes(&[]);
        let queue = WithdrawQueue::try_from_account_bytes(&data).unwrap();
        assert!(queue.is_empty());
        assert!(find_position(&queue, &[1; 32]).unwrap().is_none());
    }

    #[test]
    fn estimates_slots_from_throttle() {
        // max 1_000, remaining 100, replenish 10/slot, last update slot 50.
        let mut data = queue_bytes(&[
            ([1; 32], [11; 32], 400, ACTIVE),
            ([2; 32], [12; 32], 200, ACTIVE),
        ]);
        for (offset, value) in [(24, 1_000u64), (32, 100), (40, 10), (48, 50)] {
            data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        let queue = WithdrawQueue::try_from_account_bytes(&data).unwrap();
        let throttle = queue.header().withdraw_throttle();
        let position = find_position(&queue, &[2; 32]).unwrap().unwrap();

        assert_eq!(budget_at_slot(&throttle, 50), 100);
        assert_eq!(budget_at_slot(&throttle, 60), 200);
        assert_eq!(budget_at_slot(&throttle, 1_000), 1_000);
        // Needs 600; 200 available at slot 60, so 40 more slots.
        assert_eq!(position.estimated_slots_remaining(&throttle, 60), Some(40));
        assert_eq!(position.estimated_slots_remaining(&throttle, 100), Some(0));
    }
}
