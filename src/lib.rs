mod book;
mod common;
mod cpi;
mod error;
mod initialize;
mod market;
mod params;
mod quote;
mod state;
mod update_quotes;
pub mod withdraw_queue;

use params::{InitializeParams, UpdateQuotesParams};

use pinocchio::{
    ProgramResult, account_info::AccountInfo, entrypoint, msg, program_error::ProgramError,
    pubkey::Pubkey,
};
use wincode::{SchemaRead, config::DefaultConfig};

pinocchio_pubkey::declare_id!("6jLH6im51XeZw63brqmY6htqQt6ntbGr76SZSqx73BpS");

pub(crate) const PHOENIX_PROGRAM_ID: Pubkey =
    pinocchio_pubkey::pubkey!("EtrnLzgbS7nMMy5fbD42kXiUzGg8XQzJ972Xtk1cjWih");

pub(crate) const SYSTEM_PROGRAM_ID: Pubkey =
    pinocchio_pubkey::pubkey!("11111111111111111111111111111111");

pub(crate) const STRATEGY_SEED: &[u8] = b"phoenix";

const INITIALIZE_DISCRIMINANT: [u8; 8] = common::discriminant("global:initialize");
const UPDATE_QUOTES_DISCRIMINANT: [u8; 8] = common::discriminant("global:update_quotes");

entrypoint!(process_instruction);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarketMakerInstruction {
    Initialize,
    UpdateQuotes,
}

impl MarketMakerInstruction {
    fn from_tag(tag: &[u8]) -> Option<Self> {
        if tag == INITIALIZE_DISCRIMINANT {
            return Some(Self::Initialize);
        }
        if tag == UPDATE_QUOTES_DISCRIMINANT {
            return Some(Self::UpdateQuotes);
        }
        None
    }
}

#[inline(always)]
fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    if program_id != &ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    if instruction_data.len() < 8 {
        msg!("phoenix-eternal-mm: missing instruction discriminator");
        return Err(ProgramError::InvalidInstructionData);
    }

    let (tag, data) = instruction_data.split_at(8);
    let instruction = MarketMakerInstruction::from_tag(tag).ok_or_else(|| {
        msg!("phoenix-eternal-mm: unknown instruction discriminator");
        ProgramError::InvalidInstructionData
    })?;

    match instruction {
        MarketMakerInstruction::Initialize => {
            let params = parse_params::<InitializeParams>(
                data,
                "phoenix-eternal-mm: invalid initialize params",
            )?;
            initialize::process(accounts, &params)
        }
        MarketMakerInstruction::UpdateQuotes => {
            let params = parse_params::<UpdateQuotesParams>(
                data,
                "phoenix-eternal-mm: invalid update-quotes params",
            )?;
            update_quotes::process(accounts, &params)
        }
    }
}

fn parse_params<'de, T: SchemaRead<'de, DefaultConfig, Dst = T>>(
    data: &'de [u8],
    invalid_log: &'static str,
) -> Result<T, ProgramError> {
    wincode::deserialize_exact(data).map_err(|_| {
        msg!(invalid_log);
        ProgramError::InvalidInstructionData
    })
}
