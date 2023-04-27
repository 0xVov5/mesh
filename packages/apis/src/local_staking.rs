use cosmwasm_schema::cw_serde;
use cosmwasm_std::{Binary, Decimal, Response, StdError};
use sylvia::types::{ExecCtx, QueryCtx};
use sylvia::{interface, schemars};

#[cw_serde]
pub struct MaxSlashResponse {
    pub max_slash: Decimal,
}

// TODO: question - staking should know which is vault, vault should know what is local staking...
// How to best handle the chicken and egg problem (2 step init with Option?)

/// This is the interface to any local staking contract needed by the vault contract.
/// Users will need to use the custom methods to actually manage funds
#[interface]
pub trait LocalStakingApi {
    type Error: From<StdError>;

    /// Receives stake (info.funds) from vault contract on behalf of owner and performs the action
    /// specified in msg with it.
    /// Msg is custom to each implementation of the staking contract and opaque to the vault
    #[msg(exec)]
    fn receive_stake(
        &self,
        ctx: ExecCtx,
        owner: String,
        msg: Binary,
    ) -> Result<Response, Self::Error>;

    /// Returns the maximum percentage that can be slashed
    #[msg(query)]
    fn max_slash(&self, ctx: QueryCtx) -> Result<MaxSlashResponse, Self::Error>;
}
