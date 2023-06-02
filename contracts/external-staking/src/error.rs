use cosmwasm_std::{ConversionOverflowError, StdError, Uint128};
use cw_utils::PaymentError;
use thiserror::Error;

#[derive(Error, Debug, PartialEq)]
pub enum ContractError {
    #[error("{0}")]
    Std(#[from] StdError),

    #[error("{0}")]
    Payment(#[from] PaymentError),

    #[error("{0}")]
    Conversion(#[from] ConversionOverflowError),

    #[error("Unauthorized")]
    Unauthorized,

    #[error("Invalid denom, {0} expected")]
    InvalidDenom(String),

    #[error("Not enough tokens staked, up to {0} can be unbond")]
    NotEnoughStake(Uint128),

    #[error("Not enough tokens released, up to {0} can be claimed")]
    NotEnoughRelease(Uint128),

    #[error("Validator for user missmatch, {0} expected")]
    InvalidValidator(String),
}
