use wincode::{SchemaRead, SchemaWrite};

#[derive(Debug, Clone, Copy, PartialEq, Eq, SchemaRead, SchemaWrite)]
#[wincode(tag_encoding = "u8")]
pub enum PriceImprovementBehavior {
    Join,
    Dime,
    Ignore,
}

impl PriceImprovementBehavior {
    pub const fn to_u8(self) -> u8 {
        self as u8
    }

    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Join),
            1 => Some(Self::Dime),
            2 => Some(Self::Ignore),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, SchemaRead, SchemaWrite)]
pub struct StrategyParams {
    pub quote_edge_in_bps: Option<u64>,
    pub quote_size_in_quote_lots: Option<u64>,
    pub price_improvement_behavior: Option<PriceImprovementBehavior>,
    pub post_only: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, SchemaRead, SchemaWrite)]
pub struct InitializeParams {
    pub strategy: StrategyParams,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, SchemaRead, SchemaWrite)]
pub struct UpdateQuotesParams {
    pub fair_price_in_ticks: u64,
    pub strategy: StrategyParams,
    pub global_trader_index_count: u8,
    pub active_trader_buffer_count: u8,
}
