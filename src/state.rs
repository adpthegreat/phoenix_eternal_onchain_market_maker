use pinocchio::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey};
use wincode::{SchemaRead, SchemaWrite};

use crate::{common::discriminant, error::MarketMakerError, params::PriceImprovementBehavior};

#[derive(Debug, Clone, Copy, PartialEq, Eq, SchemaRead, SchemaWrite)]
#[repr(C)]
#[wincode(assert_zero_copy)]
pub struct PhoenixStrategyState {
    pub discriminator: [u8; 8],
    pub trader: Pubkey,
    pub trader_account: Pubkey,
    pub market: Pubkey,
    // Order parameters; a zero price means no resting order.
    pub bid_order_sequence_number: u64,
    pub bid_price_in_ticks: u64,
    pub initial_bid_size_in_base_lots: u64,
    pub ask_order_sequence_number: u64,
    pub ask_price_in_ticks: u64,
    pub initial_ask_size_in_base_lots: u64,
    pub last_update_slot: u64,
    pub last_update_unix_timestamp: i64,
    // Strategy parameters
    pub quote_edge_in_bps: u64,
    pub quote_size_in_quote_lots: u64,
    pub post_only: u8,
    pub price_improvement_behavior: u8,
    pub bump: u8,
    pub padding: [u8; 5],
}

impl PhoenixStrategyState {
    pub const DISCRIMINATOR: [u8; 8] = discriminant("account:PhoenixStrategyState");
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub fn load_mut<'a>(
        account: &AccountInfo,
        data: &'a mut [u8],
    ) -> Result<&'a mut Self, ProgramError> {
        if !account.is_owned_by(&crate::ID) {
            return Err(ProgramError::InvalidAccountOwner);
        }
        if data.len() != Self::LEN {
            return Err(ProgramError::InvalidAccountData);
        }
        let state: &mut Self =
            wincode::deserialize_mut(data).map_err(|_| ProgramError::InvalidAccountData)?;
        if state.discriminator != Self::DISCRIMINATOR {
            return Err(MarketMakerError::InvalidStrategyAccount.into());
        }
        Ok(state)
    }

    pub fn price_improvement(&self) -> Result<PriceImprovementBehavior, ProgramError> {
        PriceImprovementBehavior::from_u8(self.price_improvement_behavior)
            .ok_or_else(|| MarketMakerError::InvalidStrategyParams.into())
    }

    pub const fn is_post_only(&self) -> bool {
        self.post_only != 0
    }
}

const _: () = assert!(PhoenixStrategyState::LEN == 192);
