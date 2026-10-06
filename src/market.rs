use phoenix_rise::ix::{
    PhoenixInstruction,
    order_packet::CondensedOrder,
    types::{CancelId, OrderFlags, SelfTradeBehavior, Side},
};
use pinocchio::{ProgramResult, account_info::AccountInfo, msg, program_error::ProgramError};

use crate::{
    PHOENIX_PROGRAM_ID,
    common::{check_program_id, dynamic_tail, require_signer},
    cpi::{
        CpiBuffers, CpiScratch, invoke_prepared, require_accounts, require_data, write_account,
        write_bool, write_bytes, write_option_u64, write_u8, write_u32, write_u64,
    },
};

pub(crate) const MAX_CPI_ACCOUNTS: usize = 16;
const MARKET_ACTION_MIN_ACCOUNT_COUNT: usize = 8;
const MAX_CANCEL_ORDER_IDS: usize = 2;
const CANCEL_ORDERS_BY_ID_MAX_DATA_LEN: usize = 8 + 4 + MAX_CANCEL_ORDER_IDS * 20;
const CONDENSED_ORDER_MAX_LEN: usize = 8 + 8 + 9;
const PLACE_MULTI_LIMIT_ORDER_MAX_DATA_LEN: usize = 8 + 2 * (4 + CONDENSED_ORDER_MAX_LEN) + 17 + 1;
const PLACE_LIMIT_ORDER_MAX_DATA_LEN: usize = 63;

pub(crate) struct MarketContext<'a> {
    pub(crate) strategy: &'a AccountInfo,
    pub(crate) trader: &'a AccountInfo,
    pub(crate) phoenix_program: &'a AccountInfo,
    pub(crate) log_authority: &'a AccountInfo,
    pub(crate) global_config: &'a AccountInfo,
    pub(crate) trader_account: &'a AccountInfo,
    pub(crate) perp_asset_map: &'a AccountInfo,
    pub(crate) orderbook: &'a AccountInfo,
    pub(crate) spline_collection: &'a AccountInfo,
    pub(crate) global_trader_index: &'a [AccountInfo],
    pub(crate) active_trader_buffer: &'a [AccountInfo],
}

impl<'a> MarketContext<'a> {
    const FIXED_ACCOUNT_COUNT: usize = 9;

    pub(crate) fn load(
        accounts: &'a [AccountInfo],
        global_trader_index_count: usize,
        active_trader_buffer_count: usize,
    ) -> Result<Self, ProgramError> {
        let (global_trader_index, active_trader_buffer) = dynamic_tail(
            accounts,
            Self::FIXED_ACCOUNT_COUNT,
            global_trader_index_count,
            active_trader_buffer_count,
        )?;
        let context = Self {
            strategy: &accounts[0],
            trader: &accounts[1],
            phoenix_program: &accounts[2],
            log_authority: &accounts[3],
            global_config: &accounts[4],
            trader_account: &accounts[5],
            perp_asset_map: &accounts[6],
            orderbook: &accounts[7],
            spline_collection: &accounts[8],
            global_trader_index,
            active_trader_buffer,
        };
        check_program_id(context.phoenix_program, &PHOENIX_PROGRAM_ID, "Phoenix")?;
        require_signer(context.trader, "trader")?;
        if global_trader_index.is_empty() || active_trader_buffer.is_empty() {
            return Err(ProgramError::NotEnoughAccountKeys);
        }
        Ok(context)
    }

    pub(crate) fn cancel(&self, order_ids: &[CancelId]) -> ProgramResult {
        msg!("phoenix-eternal-mm: cancel stale quotes");
        let mut scratch = CpiScratch::<MAX_CPI_ACCOUNTS, CANCEL_ORDERS_BY_ID_MAX_DATA_LEN>::new(
            self.phoenix_program,
        );
        self.invoke(&mut scratch.buffers(), |out| {
            write_cancel_orders_by_id_data(order_ids, out)
        })
    }

    pub(crate) fn place_post_only(
        &self,
        bid: Option<CondensedOrder>,
        ask: Option<CondensedOrder>,
        client_order_id: u128,
    ) -> ProgramResult {
        msg!("phoenix-eternal-mm: place post-only quotes");
        let mut scratch = CpiScratch::<MAX_CPI_ACCOUNTS, PLACE_MULTI_LIMIT_ORDER_MAX_DATA_LEN>::new(
            self.phoenix_program,
        );
        self.invoke(&mut scratch.buffers(), |out| {
            write_place_multi_limit_order_data(
                bid.as_slice(),
                ask.as_slice(),
                Some(client_order_id),
                true,
                out,
            )
        })
    }

    pub(crate) fn place_limit(
        &self,
        side: Side,
        price_in_ticks: u64,
        num_base_lots: u64,
        client_order_id: u128,
    ) -> ProgramResult {
        msg!("phoenix-eternal-mm: place limit quote");
        let mut scratch = CpiScratch::<MAX_CPI_ACCOUNTS, PLACE_LIMIT_ORDER_MAX_DATA_LEN>::new(
            self.phoenix_program,
        );
        self.invoke(&mut scratch.buffers(), |out| {
            write_place_limit_order_data(side, price_in_ticks, num_base_lots, client_order_id, out)
        })
    }

    fn invoke(
        &self,
        buffers: &mut CpiBuffers<'a, '_>,
        write_data: impl FnOnce(&mut [u8]) -> Result<usize, ProgramError>,
    ) -> ProgramResult {
        let account_count = self.write_accounts(buffers)?;
        let data_len = write_data(buffers.data)?;
        invoke_prepared(self.phoenix_program, account_count, data_len, buffers)
    }

    fn write_accounts(&self, buffers: &mut CpiBuffers<'a, '_>) -> Result<usize, ProgramError> {
        let account_count = MARKET_ACTION_MIN_ACCOUNT_COUNT
            + self.global_trader_index.len()
            + self.active_trader_buffer.len();
        require_accounts(buffers, account_count)?;

        write_account(buffers, 0, self.phoenix_program, false, false);
        write_account(buffers, 1, self.log_authority, false, false);
        write_account(buffers, 2, self.global_config, true, false);
        write_account(buffers, 3, self.trader, false, true);
        write_account(buffers, 4, self.trader_account, true, false);
        write_account(buffers, 5, self.perp_asset_map, true, false);
        let mut index = 6;
        for account in self
            .global_trader_index
            .iter()
            .chain(self.active_trader_buffer)
        {
            write_account(buffers, index, account, true, false);
            index += 1;
        }
        write_account(buffers, index, self.orderbook, true, false);
        write_account(buffers, index + 1, self.spline_collection, true, false);
        Ok(account_count)
    }
}

fn write_cancel_orders_by_id_data(
    order_ids: &[CancelId],
    out: &mut [u8],
) -> Result<usize, ProgramError> {
    if order_ids.is_empty() || order_ids.len() > MAX_CANCEL_ORDER_IDS {
        return Err(ProgramError::InvalidInstructionData);
    }
    require_data(out, 8 + 4 + order_ids.len() * 20)?;
    let mut offset = 0;
    write_bytes(
        out,
        &mut offset,
        &PhoenixInstruction::CancelOrdersById.discriminant(),
    )?;
    write_u32(out, &mut offset, order_ids.len() as u32)?;
    for cancel_id in order_ids {
        write_u32(out, &mut offset, cancel_id.node_pointer)?;
        write_u64(out, &mut offset, cancel_id.order_id.price_in_ticks)?;
        write_u64(out, &mut offset, cancel_id.order_id.order_sequence_number)?;
    }
    Ok(offset)
}

fn write_place_multi_limit_order_data(
    bids: &[CondensedOrder],
    asks: &[CondensedOrder],
    client_order_id: Option<u128>,
    slide: bool,
    out: &mut [u8],
) -> Result<usize, ProgramError> {
    let mut offset = 0;
    write_bytes(
        out,
        &mut offset,
        &PhoenixInstruction::PlaceMultiLimitOrder.discriminant(),
    )?;
    write_condensed_order_slice(out, &mut offset, bids)?;
    write_condensed_order_slice(out, &mut offset, asks)?;
    match client_order_id {
        Some(client_order_id) => {
            write_u8(out, &mut offset, 1)?;
            write_bytes(out, &mut offset, &client_order_id.to_le_bytes())?;
        }
        None => write_u8(out, &mut offset, 0)?,
    }
    write_bool(out, &mut offset, slide)?;
    Ok(offset)
}

fn write_condensed_order_slice(
    out: &mut [u8],
    offset: &mut usize,
    orders: &[CondensedOrder],
) -> Result<(), ProgramError> {
    write_u32(out, offset, orders.len() as u32)?;
    for order in orders {
        write_u64(out, offset, order.price_in_ticks)?;
        write_u64(out, offset, order.size_in_base_lots)?;
        write_option_u64(out, offset, order.last_valid_slot)?;
    }
    Ok(())
}

fn write_place_limit_order_data(
    side: Side,
    price_in_ticks: u64,
    num_base_lots: u64,
    client_order_id: u128,
    out: &mut [u8],
) -> Result<usize, ProgramError> {
    let mut offset = 0;
    write_bytes(
        out,
        &mut offset,
        &PhoenixInstruction::PlaceLimitOrder.discriminant(),
    )?;
    write_u8(out, &mut offset, 1)?;
    write_u8(out, &mut offset, side as u8)?;
    write_u64(out, &mut offset, price_in_ticks)?;
    write_u64(out, &mut offset, num_base_lots)?;
    write_u8(out, &mut offset, SelfTradeBehavior::DecrementTake as u8)?;
    write_option_u64(out, &mut offset, None)?;
    write_bytes(out, &mut offset, &client_order_id.to_le_bytes())?;
    write_option_u64(out, &mut offset, None)?;
    write_u8(out, &mut offset, OrderFlags::None as u8)?;
    write_bool(out, &mut offset, false)?;
    Ok(offset)
}
