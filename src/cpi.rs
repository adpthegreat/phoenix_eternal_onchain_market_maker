//! Allocation-free CPI helpers, adapted from `phoenix-rise-ix`'s `cpi` module.

// https://github.com/Ellipsis-Labs/rise-public/blob/0aa44d1c763fccca2c81951ad995071362c76da0/rust/ix/src/cpi.rs#L80

#![allow(dead_code)]

use core::array;

use phoenix_rise::ix::types::Instruction as RiseInstruction;
use pinocchio::{
    account_info::AccountInfo,
    cpi::{ReturnData, get_return_data, slice_invoke, slice_invoke_signed},
    instruction::{AccountMeta, Instruction, Signer},
    program_error::ProgramError,
};
use wincode::{SchemaWrite, config::DefaultConfig};

/// Stack-allocated account infos, metas and instruction data for one CPI.
pub(crate) struct CpiScratch<'a, const MAX_ACCOUNTS: usize, const MAX_DATA_LEN: usize> {
    account_infos: [&'a AccountInfo; MAX_ACCOUNTS],
    account_metas: [AccountMeta<'a>; MAX_ACCOUNTS],
    data: [u8; MAX_DATA_LEN],
}

impl<'a, const MAX_ACCOUNTS: usize, const MAX_DATA_LEN: usize>
    CpiScratch<'a, MAX_ACCOUNTS, MAX_DATA_LEN>
{
    /// `fill_account` is only a placeholder; every used slot is overwritten.
    pub(crate) fn new(fill_account: &'a AccountInfo) -> Self {
        Self {
            account_infos: array::from_fn(|_| fill_account),
            account_metas: array::from_fn(|_| AccountMeta::readonly(fill_account.key())),
            data: [0; MAX_DATA_LEN],
        }
    }

    pub(crate) fn buffers(&mut self) -> CpiBuffers<'a, '_> {
        CpiBuffers {
            account_infos: &mut self.account_infos,
            account_metas: &mut self.account_metas,
            data: &mut self.data,
        }
    }
}

pub(crate) struct CpiBuffers<'a, 'b> {
    pub(crate) account_infos: &'b mut [&'a AccountInfo],
    pub(crate) account_metas: &'b mut [AccountMeta<'a>],
    pub(crate) data: &'b mut [u8],
}

impl<'a, 'b> CpiBuffers<'a, 'b> {
    pub(crate) fn new(
        account_infos: &'b mut [&'a AccountInfo],
        account_metas: &'b mut [AccountMeta<'a>],
        data: &'b mut [u8],
    ) -> Self {
        Self {
            account_infos,
            account_metas,
            data,
        }
    }
}

pub(crate) fn account_metas<'a>(
    accounts: &'a [&'a AccountInfo],
    out: &mut [AccountMeta<'a>],
) -> Result<usize, ProgramError> {
    if out.len() < accounts.len() {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    for (meta, account) in out.iter_mut().zip(accounts.iter().copied()) {
        *meta = AccountMeta::new(account.key(), account.is_writable(), account.is_signer());
    }
    Ok(accounts.len())
}

/// Builds metas from an off-chain instruction, validating program id and account order.
pub(crate) fn instruction_account_metas<'a>(
    ix: &RiseInstruction,
    program: &AccountInfo,
    accounts: &'a [&'a AccountInfo],
    out: &mut [AccountMeta<'a>],
) -> Result<usize, ProgramError> {
    if program.key() != &ix.program_id.to_bytes() {
        return Err(ProgramError::IncorrectProgramId);
    }
    if ix.accounts.len() != accounts.len() || out.len() < accounts.len() {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    for ((out_meta, expected), account) in out
        .iter_mut()
        .zip(ix.accounts.iter())
        .zip(accounts.iter().copied())
    {
        if account.key() != &expected.pubkey.to_bytes() {
            return Err(ProgramError::InvalidAccountData);
        }
        *out_meta = AccountMeta::new(account.key(), expected.is_writable, expected.is_signer);
    }
    Ok(accounts.len())
}

/// Orders `available_accounts` to match `ix.accounts`. Nested scan: avoid on CU-hot paths.
pub(crate) fn instruction_account_infos<'info, I>(
    ix: &RiseInstruction,
    available_accounts: I,
    out: &mut [AccountInfo],
) -> Result<usize, ProgramError>
where
    I: Clone + IntoIterator<Item = &'info AccountInfo>,
{
    let account_count = ix.accounts.len();
    if out.len() < account_count {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    for (index, expected) in ix.accounts.iter().enumerate() {
        let expected_pubkey = expected.pubkey.to_bytes();
        let Some(account) = available_accounts
            .clone()
            .into_iter()
            .find(|account| account.key() == &expected_pubkey)
        else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };
        out[index] = *account;
    }
    Ok(account_count)
}

pub(crate) fn instruction_account_info_refs<'storage, 'info, I>(
    ix: &RiseInstruction,
    available_accounts: I,
    account_storage: &'storage mut [AccountInfo],
    out: &mut [&'storage AccountInfo],
) -> Result<usize, ProgramError>
where
    I: Clone + IntoIterator<Item = &'info AccountInfo>,
{
    if out.len() < ix.accounts.len() {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let len = instruction_account_infos(ix, available_accounts, account_storage)?;
    for (out_account, account) in out.iter_mut().zip(account_storage.iter()).take(len) {
        *out_account = account;
    }
    Ok(len)
}

pub(crate) fn invoke<'a>(
    program: &AccountInfo,
    accounts: &'a [&'a AccountInfo],
    account_metas: &mut [AccountMeta<'a>],
    data: &[u8],
) -> Result<(), ProgramError> {
    let len = self::account_metas(accounts, account_metas)?;
    let instruction = Instruction {
        program_id: program.key(),
        accounts: &account_metas[..len],
        data,
    };
    slice_invoke(&instruction, accounts)
}

pub(crate) fn invoke_instruction<'a>(
    ix: &RiseInstruction,
    program: &AccountInfo,
    accounts: &'a [&'a AccountInfo],
    account_metas: &mut [AccountMeta<'a>],
) -> Result<(), ProgramError> {
    let len = instruction_account_metas(ix, program, accounts, account_metas)?;
    let instruction = Instruction {
        program_id: program.key(),
        accounts: &account_metas[..len],
        data: &ix.data,
    };
    slice_invoke(&instruction, accounts)
}

pub(crate) fn invoke_signed<'a>(
    program: &AccountInfo,
    accounts: &'a [&'a AccountInfo],
    account_metas: &mut [AccountMeta<'a>],
    data: &[u8],
    signer_seeds: &[Signer<'_, '_>],
) -> Result<(), ProgramError> {
    let len = self::account_metas(accounts, account_metas)?;
    let instruction = Instruction {
        program_id: program.key(),
        accounts: &account_metas[..len],
        data,
    };
    slice_invoke_signed(&instruction, accounts, signer_seeds)
}

pub(crate) fn invoke_with_return_data<'a>(
    program: &AccountInfo,
    accounts: &'a [&'a AccountInfo],
    account_metas: &mut [AccountMeta<'a>],
    data: &[u8],
) -> Result<Option<ReturnData>, ProgramError> {
    invoke(program, accounts, account_metas, data)?;
    Ok(get_return_data())
}

#[inline(always)]
pub(crate) fn require_accounts(
    buffers: &CpiBuffers<'_, '_>,
    account_count: usize,
) -> Result<(), ProgramError> {
    if buffers.account_infos.len() < account_count || buffers.account_metas.len() < account_count {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    Ok(())
}

#[inline(always)]
pub(crate) fn require_data(out: &[u8], data_len: usize) -> Result<(), ProgramError> {
    if out.len() < data_len {
        return Err(ProgramError::InvalidInstructionData);
    }
    Ok(())
}

#[inline(always)]
pub(crate) fn write_account<'a>(
    buffers: &mut CpiBuffers<'a, '_>,
    index: usize,
    account: &'a AccountInfo,
    is_writable: bool,
    is_signer: bool,
) {
    buffers.account_infos[index] = account;
    buffers.account_metas[index] = AccountMeta::new(account.key(), is_writable, is_signer);
}

#[inline(always)]
pub(crate) fn write_bytes(
    out: &mut [u8],
    offset: &mut usize,
    bytes: &[u8],
) -> Result<(), ProgramError> {
    let end = offset
        .checked_add(bytes.len())
        .ok_or(ProgramError::InvalidInstructionData)?;
    require_data(out, end)?;
    out[*offset..end].copy_from_slice(bytes);
    *offset = end;
    Ok(())
}

#[inline(always)]
pub(crate) fn write_u8(out: &mut [u8], offset: &mut usize, value: u8) -> Result<(), ProgramError> {
    write_bytes(out, offset, &[value])
}

#[inline(always)]
pub(crate) fn write_bool(
    out: &mut [u8],
    offset: &mut usize,
    value: bool,
) -> Result<(), ProgramError> {
    write_u8(out, offset, u8::from(value))
}

#[inline(always)]
pub(crate) fn write_u32(
    out: &mut [u8],
    offset: &mut usize,
    value: u32,
) -> Result<(), ProgramError> {
    write_bytes(out, offset, &value.to_le_bytes())
}

#[inline(always)]
pub(crate) fn write_u64(
    out: &mut [u8],
    offset: &mut usize,
    value: u64,
) -> Result<(), ProgramError> {
    write_bytes(out, offset, &value.to_le_bytes())
}

#[inline(always)]
pub(crate) fn write_option_u64(
    out: &mut [u8],
    offset: &mut usize,
    value: Option<u64>,
) -> Result<(), ProgramError> {
    match value {
        Some(value) => {
            write_u8(out, offset, 1)?;
            write_u64(out, offset, value)
        }
        None => write_u8(out, offset, 0),
    }
}

#[inline(always)]
pub(crate) fn write_option_u8(
    out: &mut [u8],
    offset: &mut usize,
    value: Option<u8>,
) -> Result<(), ProgramError> {
    match value {
        Some(value) => {
            write_u8(out, offset, 1)?;
            write_u8(out, offset, value)
        }
        None => write_u8(out, offset, 0),
    }
}

/// Bincode layout; matches Borsh for primitives/options/arrays but not for
/// sequence length prefixes (u64 vs u32).
#[inline(always)]
pub(crate) fn write_wincode<T: SchemaWrite<DefaultConfig, Src = T> + ?Sized>(
    out: &mut [u8],
    offset: &mut usize,
    value: &T,
) -> Result<(), ProgramError> {
    let len =
        wincode::serialized_size(value).map_err(|_| ProgramError::InvalidInstructionData)? as usize;
    let end = offset
        .checked_add(len)
        .ok_or(ProgramError::InvalidInstructionData)?;
    require_data(out, end)?;
    wincode::serialize_into(&mut out[*offset..end], value)
        .map_err(|_| ProgramError::InvalidInstructionData)?;
    *offset = end;
    Ok(())
}

#[inline(always)]
pub(crate) fn invoke_prepared(
    program: &AccountInfo,
    account_count: usize,
    data_len: usize,
    buffers: &mut CpiBuffers<'_, '_>,
) -> Result<(), ProgramError> {
    let instruction = Instruction {
        program_id: program.key(),
        accounts: &buffers.account_metas[..account_count],
        data: &buffers.data[..data_len],
    };
    slice_invoke(&instruction, &buffers.account_infos[..account_count])
}

#[inline(always)]
pub(crate) fn invoke_prepared_signed(
    program: &AccountInfo,
    account_count: usize,
    data_len: usize,
    signer_seeds: &[Signer<'_, '_>],
    buffers: &mut CpiBuffers<'_, '_>,
) -> Result<(), ProgramError> {
    let instruction = Instruction {
        program_id: program.key(),
        accounts: &buffers.account_metas[..account_count],
        data: &buffers.data[..data_len],
    };
    slice_invoke_signed(
        &instruction,
        &buffers.account_infos[..account_count],
        signer_seeds,
    )
}
