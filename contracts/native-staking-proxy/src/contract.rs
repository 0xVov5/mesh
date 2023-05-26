use cosmwasm_std::{
    coin, ensure_eq, Coin, DistributionMsg, GovMsg, Order, Response, StakingMsg, StdResult,
    Storage, Uint128, VoteOption, WeightedVoteOption,
};
use cw2::set_contract_version;
use cw_storage_plus::{Item, Map};

use cw_utils::must_pay;
use sylvia::types::{ExecCtx, InstantiateCtx, QueryCtx};
use sylvia::{contract, schemars};

use crate::error::ContractError;
use crate::types::{ClaimsResponse, Config, ConfigResponse};

pub const CONTRACT_NAME: &str = env!("CARGO_PKG_NAME");
pub const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct NativeStakingProxyContract<'a> {
    config: Item<'a, Config>,
    /// Map of delegated amounts per validator
    delegations: Map<'a, &'a str, Uint128>,
}

#[contract]
#[error(ContractError)]
impl NativeStakingProxyContract<'_> {
    pub const fn new() -> Self {
        Self {
            config: Item::new("config"),
            delegations: Map::new("delegations"),
        }
    }

    /// The caller of the instantiation will be the native-staking contract.
    /// We stake `funds.info` on the given validator
    #[msg(instantiate)]
    pub fn instantiate(
        &self,
        ctx: InstantiateCtx,
        denom: String,
        owner: String,
        validator: String,
    ) -> Result<Response, ContractError> {
        let config = Config {
            denom,
            parent: ctx.info.sender.clone(),
            owner: ctx.deps.api.addr_validate(&owner)?,
        };
        self.config.save(ctx.deps.storage, &config)?;
        set_contract_version(ctx.deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

        // Stake info.funds on validator
        let res = self.stake(ctx, validator)?;

        // Set owner as recipient of future withdrawals
        let set_withdrawal = DistributionMsg::SetWithdrawAddress {
            address: config.owner.into_string(),
        };
        Ok(res.add_message(set_withdrawal))
    }

    /// Stakes the tokens from `info.funds` to the given validator.
    /// Can only be called by the parent contract
    #[msg(exec)]
    fn stake(&self, ctx: ExecCtx, validator: String) -> Result<Response, ContractError> {
        let cfg = self.config.load(ctx.deps.storage)?;
        ensure_eq!(cfg.parent, ctx.info.sender, ContractError::Unauthorized {});

        let amount = must_pay(&ctx.info, &cfg.denom)?;

        // Update validator delegation
        self.increase_validator_delegation(ctx.deps.storage, &validator, amount)?;

        let amount = coin(amount.u128(), cfg.denom);
        let msg = StakingMsg::Delegate { validator, amount };

        Ok(Response::new().add_message(msg))
    }

    /// Re-stakes the given amount from the one validator to another on behalf of the calling user.
    /// Returns an error if the user doesn't have such stake
    #[msg(exec)]
    fn restake(
        &self,
        ctx: ExecCtx,
        src_validator: String,
        dst_validator: String,
        amount: Coin,
    ) -> Result<Response, ContractError> {
        let cfg = self.config.load(ctx.deps.storage)?;
        ensure_eq!(cfg.owner, ctx.info.sender, ContractError::Unauthorized {});
        ensure_eq!(
            amount.denom,
            cfg.denom,
            ContractError::InvalidDenom(amount.denom)
        );

        // Update src and dst validator delegations
        self.decrease_validator_delegation(ctx.deps.storage, &src_validator, amount.amount)?;
        self.increase_validator_delegation(ctx.deps.storage, &dst_validator, amount.amount)?;

        let msg = StakingMsg::Redelegate {
            src_validator,
            dst_validator,
            amount,
        };
        Ok(Response::new().add_message(msg))
    }

    fn increase_validator_delegation(
        &self,
        storage: &mut dyn Storage,
        validator: &str,
        amount: Uint128,
    ) -> Result<Uint128, ContractError> {
        self.delegations
            .update::<_, ContractError>(storage, validator, |old| {
                Ok(old.unwrap_or_default() + amount)
            })
    }

    fn decrease_validator_delegation(
        &self,
        storage: &mut dyn Storage,
        validator: &str,
        amount: Uint128,
    ) -> Result<Uint128, ContractError> {
        // FIXME?: Remove zero amount delegations
        self.delegations.update(storage, validator, |old| {
            let old_amount = old.unwrap_or_default();
            if old_amount >= amount {
                Ok(old_amount - amount)
            } else {
                Err(ContractError::InsufficientDelegation(
                    validator.to_string(),
                    old_amount,
                ))
            }
        })
    }

    /// Vote with the user's stake (over all delegations)
    #[msg(exec)]
    fn vote(
        &self,
        ctx: ExecCtx,
        proposal_id: u64,
        vote: VoteOption,
    ) -> Result<Response, ContractError> {
        let cfg = self.config.load(ctx.deps.storage)?;
        ensure_eq!(cfg.owner, ctx.info.sender, ContractError::Unauthorized {});

        let msg = GovMsg::Vote { proposal_id, vote };
        Ok(Response::new().add_message(msg))
    }

    /// Vote with the user's stake (over all delegations)
    #[msg(exec)]
    fn vote_weighted(
        &self,
        ctx: ExecCtx,
        proposal_id: u64,
        vote: Vec<WeightedVoteOption>,
    ) -> Result<Response, ContractError> {
        let cfg = self.config.load(ctx.deps.storage)?;
        ensure_eq!(cfg.owner, ctx.info.sender, ContractError::Unauthorized {});

        let msg = GovMsg::VoteWeighted {
            proposal_id,
            options: vote,
        };
        Ok(Response::new().add_message(msg))
    }

    /// If the caller has any delegations, withdraw all rewards from those delegations and
    /// send the tokens to the caller.
    /// NOTE: must make sure not to release unbonded tokens
    #[msg(exec)]
    fn withdraw_rewards(&self, ctx: ExecCtx) -> Result<Response, ContractError> {
        let cfg = self.config.load(ctx.deps.storage)?;
        ensure_eq!(cfg.owner, ctx.info.sender, ContractError::Unauthorized {});

        let validators = self
            .delegations
            .range(ctx.deps.storage, None, None, Order::Ascending)
            .filter(|item| {
                if let Ok((_, amount)) = item {
                    !amount.is_zero()
                } else {
                    true
                }
            })
            .map(|item| item.map(|(validator, _)| validator))
            .collect::<StdResult<Vec<_>>>()?;

        // Withdraw all delegations to the owner (already set as withdrawal address in instantiate)
        let msgs = validators
            .into_iter()
            .map(|validator| DistributionMsg::WithdrawDelegatorReward { validator });
        let res = Response::new().add_messages(msgs);
        Ok(res)
    }

    /// Unstakes the given amount from the given validator on behalf of the calling user.
    /// Returns an error if the user doesn't have such stake.
    /// After the unbonding period, it will allow the user to claim the tokens (returning to vault)
    #[msg(exec)]
    fn unstake(
        &self,
        ctx: ExecCtx,
        validator: String,
        amount: Coin,
    ) -> Result<Response, ContractError> {
        let cfg = self.config.load(ctx.deps.storage)?;
        ensure_eq!(cfg.owner, ctx.info.sender, ContractError::Unauthorized {});
        ensure_eq!(
            amount.denom,
            cfg.denom,
            ContractError::InvalidDenom(amount.denom)
        );

        // Reduce validator delegation
        self.decrease_validator_delegation(ctx.deps.storage, &validator, amount.amount)?;

        let msg = StakingMsg::Undelegate { validator, amount };
        Ok(Response::new().add_message(msg))
    }

    /// Releases any tokens that have fully unbonded from a previous unstake.
    /// This will go back to the parent via `release_proxy_stake`.
    /// Errors if the proxy doesn't have any liquid tokens
    #[msg(exec)]
    fn release_unbonded(&self, ctx: ExecCtx) -> Result<Response, ContractError> {
        let cfg = self.config.load(ctx.deps.storage)?;
        ensure_eq!(cfg.owner, ctx.info.sender, ContractError::Unauthorized {});

        todo!()
    }

    #[msg(query)]
    fn config(&self, ctx: QueryCtx) -> Result<ConfigResponse, ContractError> {
        Ok(self.config.load(ctx.deps.storage)?)
    }

    /// Returns all pending unbonding
    /// TODO: can we do that with contract API?
    /// Or better they use cosmjs native delegation queries with this proxy address
    #[msg(query)]
    fn unbonding(&self, _ctx: QueryCtx) -> Result<ClaimsResponse, ContractError> {
        todo!()
    }
}
