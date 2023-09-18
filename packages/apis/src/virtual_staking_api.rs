use cosmwasm_schema::cw_serde;
use cosmwasm_std::{Coin, Response, StdError, Validator};
use sylvia::types::ExecCtx;
use sylvia::{interface, schemars};

/// The Virtual Staking API is called from the converter contract to bond and (instantly) unbond tokens.
/// The Virtual Staking contract is responsible for interfacing with the native SDK module, while the converter
/// manages the IBC connection.
#[interface]
pub trait VirtualStakingApi {
    type Error: From<StdError>;

    /// Requests to bond tokens to a validator. This will be actually handled at the next epoch.
    /// If the virtual staking module is over the max cap, it will trigger a rebalance.
    /// If the max cap is 0, then this will immediately return an error.
    #[msg(exec)]
    fn bond(&self, ctx: ExecCtx, validator: String, amount: Coin) -> Result<Response, Self::Error>;

    /// Requests to unbond tokens from a validator. This will be actually handled at the next epoch.
    /// If the virtual staking module is over the max cap, it will trigger a rebalance in addition to unbond.
    /// If the virtual staking contract doesn't have at least amount tokens staked to the given validator, this will return an error.
    #[msg(exec)]
    fn unbond(
        &self,
        ctx: ExecCtx,
        validator: String,
        amount: Coin,
    ) -> Result<Response, Self::Error>;
}

#[cw_serde]
pub enum SudoMsg {
    /// SudoMsg::Rebalance{} should be called once per epoch by the sdk (in EndBlock).
    /// It allows the virtual staking contract to bond or unbond any pending requests, as well
    /// as to perform a rebalance if needed (over the max cap).
    ///
    /// It should also withdraw all pending rewards here, and send them to the converter contract.
    Rebalance {},
    /// SudoMsg::ValsetUpdate{} should be called every time there's a validator set update:
    ///  - Addition of a new validator to the active validator set.
    ///  - Temporary removal of a validator from the active set. (i.e. `unbonded` state).
    ///  - Update of validator data.
    ///  - Temporary removal of a validator from the active set due to jailing. Implies slashing.
    ///  - Addition of an existing validator to the active validator set.
    ///  - Permanent removal (i.e. tombstoning) of a validator from the active set. Implies slashing
    ValsetUpdate {
        additions: Vec<Validator>,
        removals: Vec<String>,
        updated: Vec<Validator>,
        jailed: Vec<String>,
        unjailed: Vec<String>,
        tombstoned: Vec<String>,
    },
    /// SudoMsg::Slash{} should be called to execute a validator (cross-) slashing event.
    /// If the validator is already tombstoned at the slashing height, the slashing will be ignored.
    /// `tombstone` can be set to true, to tombstone the validator in passing after slashing.
    Slash {
        /// Validator address. The first 20 bytes of SHA256(public key)
        validator: String,
        /// Height at which the offense occurred
        height: u64,
        /// Time at which the offense occurred, in nanoseconds
        time: u64,
        /// Tombstone the validator
        tombstone: bool,
    },
}
