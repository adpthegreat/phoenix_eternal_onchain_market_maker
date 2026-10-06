use pinocchio::{
    ProgramResult, account_info::AccountInfo, msg, program_error::ProgramError, pubkey::Pubkey,
};
use sha2_const_stable::Sha256;

pub(crate) const fn discriminant(name: &str) -> [u8; 8] {
    let hash = Sha256::new().update(name.as_bytes()).finalize();
    [
        hash[0], hash[1], hash[2], hash[3], hash[4], hash[5], hash[6], hash[7],
    ]
}

pub(crate) fn dynamic_tail<'a>(
    accounts: &'a [AccountInfo],
    fixed_account_count: usize,
    global_trader_index_count: usize,
    active_trader_buffer_count: usize,
) -> Result<(&'a [AccountInfo], &'a [AccountInfo]), ProgramError> {
    let gti_end = fixed_account_count
        .checked_add(global_trader_index_count)
        .ok_or(ProgramError::InvalidInstructionData)?;
    let expected = gti_end
        .checked_add(active_trader_buffer_count)
        .ok_or(ProgramError::InvalidInstructionData)?;
    require_exact_accounts(accounts, expected)?;
    Ok((
        &accounts[fixed_account_count..gti_end],
        &accounts[gti_end..expected],
    ))
}

pub(crate) fn require_exact_accounts(accounts: &[AccountInfo], expected: usize) -> ProgramResult {
    if accounts.len() != expected {
        msg!("phoenix-eternal-mm: invalid account count");
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    Ok(())
}

pub(crate) fn check_program_id(
    account: &AccountInfo,
    expected: &Pubkey,
    label: &'static str,
) -> ProgramResult {
    if account.key() != expected {
        msg!(&format!("phoenix-eternal-mm: {label} program id mismatch"));
        return Err(ProgramError::IncorrectProgramId);
    }
    Ok(())
}

pub(crate) fn require_signer(account: &AccountInfo, label: &'static str) -> ProgramResult {
    if !account.is_signer() {
        msg!(&format!("phoenix-eternal-mm: {label} must sign"));
        return Err(ProgramError::MissingRequiredSignature);
    }
    Ok(())
}
