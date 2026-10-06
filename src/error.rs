use pinocchio::program_error::ProgramError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum MarketMakerError {
    InvalidStrategyParams = 6000,
    EdgeMustBeNonZero,
    InvalidPhoenixAccount,
    InvalidStrategyAccount,
    StrategyAccountMismatch,
    TraderAuthorityMismatch,
    InvalidFairPrice,
}

impl From<MarketMakerError> for ProgramError {
    fn from(error: MarketMakerError) -> Self {
        ProgramError::Custom(error as u32)
    }
}
