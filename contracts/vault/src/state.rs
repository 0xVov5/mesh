use cosmwasm_schema::cw_serde;
use cosmwasm_std::{Addr, Decimal, Uint128};
use mesh_apis::local_staking_api::LocalStakingApiHelper;

#[cw_serde]
pub struct Config {
    /// The denom we accept for staking (only native tokens)
    pub denom: String,

    /// info about the local staking contract (where actual tokens go)
    pub local_staking: LocalStaking,
}

#[cw_serde]
pub struct LocalStaking {
    /// Local staking address
    pub contract: LocalStakingApiHelper,

    /// Max slashing on local staking
    pub max_slash: Decimal,
}

/// Single Lien description
#[cw_serde]
pub struct Lien {
    /// Credit amount (denom is in `Config::denom`)
    pub amount: Uint128,
    /// Slashable part - restricted to [0; 1] range
    pub slashable: Decimal,
}

impl Lien {
    /// Calculates collateral slashable for this lien
    pub fn slashable_collateral(&self) -> Uint128 {
        self.amount * self.slashable
    }
}

/// All values are in Config.denom
#[cw_serde]
pub struct Balance {
    pub bonded: Uint128,
    pub claims: Vec<LienAddr>,
}

#[cw_serde]
pub struct LienAddr {
    pub lienholder: Addr,
    pub amount: Uint128,
}
