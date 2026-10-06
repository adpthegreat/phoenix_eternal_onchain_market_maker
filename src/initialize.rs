use phoenix_rise::accounts::trader::TraderHeader;
use pinocchio::{
    ProgramResult,
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    msg,
    program_error::ProgramError,
    pubkey::find_program_address,
    sysvars::{Sysvar, clock::Clock, rent::Rent},
};

use crate::{
    PHOENIX_PROGRAM_ID, STRATEGY_SEED, SYSTEM_PROGRAM_ID, book,
    common::{check_program_id, require_exact_accounts, require_signer},
    cpi::{CpiScratch, invoke_prepared_signed, write_account, write_bytes, write_u32, write_u64},
    error::MarketMakerError,
    params::InitializeParams,
    state::PhoenixStrategyState,
};

const ACCOUNT_COUNT: usize = 5;

pub(crate) fn process(accounts: &[AccountInfo], params: &InitializeParams) -> ProgramResult {
    require_exact_accounts(accounts, ACCOUNT_COUNT)?;
    let [
        strategy,
        authority,
        orderbook,
        trader_account,
        system_program,
    ] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    require_signer(authority, "authority")?;
    check_program_id(system_program, &SYSTEM_PROGRAM_ID, "System")?;

    let strategy_params = params.strategy;
    let (Some(quote_edge_in_bps), Some(quote_size_in_quote_lots), Some(behavior)) = (
        strategy_params.quote_edge_in_bps,
        strategy_params.quote_size_in_quote_lots,
        strategy_params.price_improvement_behavior,
    ) else {
        return Err(MarketMakerError::InvalidStrategyParams.into());
    };
    if quote_edge_in_bps == 0 {
        return Err(MarketMakerError::EdgeMustBeNonZero.into());
    }

    book::load(orderbook, &orderbook.try_borrow_data()?)?;
    validate_trader_account(trader_account, authority)?;

    let (expected, bump) = find_program_address(
        &[STRATEGY_SEED, authority.key(), orderbook.key()],
        &crate::ID,
    );
    if strategy.key() != &expected {
        msg!("phoenix-eternal-mm: strategy PDA mismatch");
        return Err(ProgramError::InvalidSeeds);
    }

    let bump_seed = [bump];
    let seeds = [
        Seed::from(STRATEGY_SEED),
        Seed::from(authority.key().as_ref()),
        Seed::from(orderbook.key().as_ref()),
        Seed::from(&bump_seed),
    ];
    create_account(
        system_program,
        authority,
        strategy,
        Rent::get()?.minimum_balance(PhoenixStrategyState::LEN),
        PhoenixStrategyState::LEN as u64,
        &[Signer::from(&seeds)],
    )?;

    let clock = Clock::get()?;
    let state = PhoenixStrategyState {
        discriminator: PhoenixStrategyState::DISCRIMINATOR,
        trader: *authority.key(),
        trader_account: *trader_account.key(),
        market: *orderbook.key(),
        bid_order_sequence_number: 0,
        bid_price_in_ticks: 0,
        initial_bid_size_in_base_lots: 0,
        ask_order_sequence_number: 0,
        ask_price_in_ticks: 0,
        initial_ask_size_in_base_lots: 0,
        last_update_slot: clock.slot,
        last_update_unix_timestamp: clock.unix_timestamp,
        quote_edge_in_bps,
        quote_size_in_quote_lots,
        post_only: strategy_params.post_only.unwrap_or(false) as u8,
        price_improvement_behavior: behavior.to_u8(),
        bump,
        padding: [0; 5],
    };
    let mut data = strategy.try_borrow_mut_data()?;
    wincode::serialize_into(&mut data[..], &state).map_err(|_| ProgramError::InvalidAccountData)?;
    msg!(&format!(
        "phoenix-eternal-mm: initialized edge_bps={quote_edge_in_bps} size_quote_lots={quote_size_in_quote_lots}"
    ));
    Ok(())
}

fn validate_trader_account(trader_account: &AccountInfo, authority: &AccountInfo) -> ProgramResult {
    if !trader_account.is_owned_by(&PHOENIX_PROGRAM_ID) {
        msg!("phoenix-eternal-mm: trader account not owned by Phoenix");
        return Err(MarketMakerError::InvalidPhoenixAccount.into());
    }
    let data = trader_account.try_borrow_data()?;
    let header = TraderHeader::try_from_account_bytes(&data)
        .map_err(|_| ProgramError::from(MarketMakerError::InvalidPhoenixAccount))?;
    if &header.authority != authority.key() {
        msg!("phoenix-eternal-mm: trader authority mismatch");
        return Err(MarketMakerError::TraderAuthorityMismatch.into());
    }
    Ok(())
}

const CREATE_ACCOUNT_DATA_LEN: usize = 4 + 8 + 8 + 32;

fn create_account<'a>(
    system_program: &'a AccountInfo,
    from: &'a AccountInfo,
    to: &'a AccountInfo,
    lamports: u64,
    space: u64,
    signers: &[Signer<'_, '_>],
) -> ProgramResult {
    let mut scratch = CpiScratch::<2, CREATE_ACCOUNT_DATA_LEN>::new(system_program);
    let mut buffers = scratch.buffers();
    write_account(&mut buffers, 0, from, true, true);
    write_account(&mut buffers, 1, to, true, true);
    let mut offset = 0;
    write_u32(buffers.data, &mut offset, 0)?;
    write_u64(buffers.data, &mut offset, lamports)?;
    write_u64(buffers.data, &mut offset, space)?;
    write_bytes(buffers.data, &mut offset, &crate::ID)?;
    invoke_prepared_signed(system_program, 2, offset, signers, &mut buffers)
}
