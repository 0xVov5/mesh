use cosmwasm_std::{coin, ensure, ensure_eq, Addr, Coin, Order, Response, Uint128};
use cw2::set_contract_version;
use cw_storage_plus::{Bounder, Item, Map, PrefixBound};
use mesh_apis::cross_staking_api;
use mesh_apis::vault_api::VaultApiHelper;
use sylvia::contract;
use sylvia::types::{ExecCtx, InstantiateCtx, QueryCtx};

use crate::error::ContractError;
use crate::msg::{ConfigResponse, StakeInfo, StakesResponse, UserInfo, UsersResponse};
use crate::state::{Config, PendingUnbond, Stake};

pub const CONTRACT_NAME: &str = env!("CARGO_PKG_NAME");
pub const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub const DEFAULT_PAGE_LIMIT: u32 = 10;
pub const MAX_PAGE_LIMIT: u32 = 30;

/// Aligns pagination limit
fn clamp_page_limit(limit: Option<u32>) -> usize {
    limit.unwrap_or(DEFAULT_PAGE_LIMIT).max(MAX_PAGE_LIMIT) as usize
}

pub struct ExternalStakingContract<'a> {
    pub config: Item<'a, Config>,
    pub stakes: Map<'a, (&'a Addr, &'a str), Stake>,
}

#[cfg_attr(not(feature = "library"), sylvia::entry_points)]
#[contract]
#[error(ContractError)]
#[messages(cross_staking_api as CrossStakingApi)]
impl ExternalStakingContract<'_> {
    pub const fn new() -> Self {
        Self {
            config: Item::new("config"),
            stakes: Map::new("stakes"),
        }
    }

    #[msg(instantiate)]
    pub fn instantiate(
        &self,
        ctx: InstantiateCtx,
        denom: String,
        vault: String,
        unbonding_period: u64,
    ) -> Result<Response, ContractError> {
        let vault = ctx.deps.api.addr_validate(&vault)?;
        let vault = VaultApiHelper(vault);

        let config = Config {
            denom,
            vault,
            unbonding_period,
        };

        self.config.save(ctx.deps.storage, &config)?;

        set_contract_version(ctx.deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

        Ok(Response::new())
    }

    /// Schedules tokens for release, adding them to the pending unbonds. After `unbonding_period`
    /// passes, funds are ready to be released with `withdraw_unbonded` call by the user
    #[msg(exec)]
    pub fn unstake(
        &self,
        ctx: ExecCtx,
        validator: String,
        amount: Coin,
    ) -> Result<Response, ContractError> {
        let config = self.config.load(ctx.deps.storage)?;

        ensure_eq!(
            amount.denom,
            config.denom,
            ContractError::InvalidDenom(config.denom)
        );

        let mut stake = self
            .stakes
            .may_load(ctx.deps.storage, (&ctx.info.sender, &validator))?
            .unwrap_or_default();

        ensure!(
            stake.stake >= amount.amount,
            ContractError::NotEnoughStake(stake.stake)
        );

        stake.stake -= amount.amount;

        let release_at = ctx.env.block.time.plus_seconds(config.unbonding_period);
        let unbond = PendingUnbond {
            amount: amount.amount,
            release_at,
        };
        stake.pending_unbonds.push(unbond);

        self.stakes
            .save(ctx.deps.storage, (&ctx.info.sender, &validator), &stake)?;

        // TODO:
        //
        // Probably some more communication with remote via IBC should happen here?
        // Or maybe this contract should be called via IBC here? To be specified
        let resp = Response::new()
            .add_attribute("action", "unstake")
            .add_attribute("owner", ctx.info.sender.into_string())
            .add_attribute("amount", amount.amount.to_string());

        Ok(resp)
    }

    /// Withdraws all released tokens to the sender.
    ///
    /// Tokens to be claimed has to be unbond before by calling the `unbond` message and
    /// waiting the `unbond_period`
    #[msg(exec)]
    pub fn withdraw_unbonded(&self, ctx: ExecCtx) -> Result<Response, ContractError> {
        let config = self.config.load(ctx.deps.storage)?;

        let stakes: Vec<_> = self
            .stakes
            .prefix(&ctx.info.sender)
            .range(ctx.deps.storage, None, None, Order::Ascending)
            .collect::<Result<_, _>>()?;

        let released: Uint128 = stakes
            .into_iter()
            .map(|(validator, mut stake)| {
                let released = stake.release_pending(&ctx.env.block);
                self.stakes
                    .save(ctx.deps.storage, (&ctx.info.sender, &validator), &stake)
                    .map(|_| released)
            })
            .fold(Ok(Uint128::zero()), |acc, released| {
                let acc = acc?;
                released.map(|released| released + acc)
            })?;

        let release_msg = config.vault.release_cross_stake(
            ctx.info.sender.to_string(),
            coin(released.u128(), &config.denom),
            vec![],
        )?;

        let resp = Response::new()
            .add_message(release_msg)
            .add_attribute("action", "withdraw_unbonded")
            .add_attribute("owner", ctx.info.sender.into_string())
            .add_attribute("amount", released.to_string());

        Ok(resp)
    }

    /// Queries for contract configuration
    #[msg(query)]
    pub fn config(&self, ctx: QueryCtx) -> Result<ConfigResponse, ContractError> {
        let resp = self.config.load(ctx.deps.storage)?.into();
        Ok(resp)
    }

    /// Queries for stake info
    ///
    /// If stake is not existing in the system is queried, the default "nothing staken" is returned
    #[msg(query)]
    pub fn stake(
        &self,
        ctx: QueryCtx,
        user: String,
        validator: String,
    ) -> Result<Stake, ContractError> {
        let user = ctx.deps.api.addr_validate(&user)?;
        let stake = self
            .stakes
            .may_load(ctx.deps.storage, (&user, &validator))?
            .unwrap_or_default();
        Ok(stake)
    }

    /// Paginate list of users
    ///
    /// `start_after` is the last user address of previous page
    #[msg(query)]
    pub fn users(
        &self,
        ctx: QueryCtx,
        start_after: Option<String>,
        limit: Option<u32>,
    ) -> Result<UsersResponse, ContractError> {
        let limit = clamp_page_limit(limit);

        let start_after = start_after.map(Addr::unchecked);
        let bound = start_after.as_ref().map(PrefixBound::exclusive);

        let users = self
            .stakes
            .prefix_range(ctx.deps.storage, bound, None, Order::Ascending)
            // Makes items into `Some(addr)` for first occurence of an address, or `None` for others
            // States is the previous returned address
            // Double-option wrapping, as top-level `None` breaks iteration in scan
            .scan(None, |last, item| {
                let item = item.map(|((addr, _), _)| match (last, addr) {
                    (last @ None, addr) => {
                        *last = Some(addr.clone());
                        Some(addr)
                    }
                    (Some(l), addr) if *l != addr => {
                        *l = addr.clone();
                        Some(addr)
                    }
                    _ => None,
                });
                Some(item.transpose())
            })
            .flatten()
            .map(|addr| addr.map(|addr| UserInfo { addr: addr.into() }))
            .take(limit)
            .collect::<Result<_, _>>()?;

        let users = UsersResponse { users };

        Ok(users)
    }

    /// Paginated list of user stakes.
    ///
    /// `start_after` is the last validator of previous page
    #[msg(query)]
    pub fn stakes(
        &self,
        ctx: QueryCtx,
        user: String,
        start_after: Option<String>,
        limit: Option<u32>,
    ) -> Result<StakesResponse, ContractError> {
        let limit = clamp_page_limit(limit);
        let user = Addr::unchecked(user);

        let bound = start_after.as_deref().and_then(Bounder::exclusive_bound);

        let stakes = self
            .stakes
            .prefix(&user)
            .range(ctx.deps.storage, bound, None, Order::Ascending)
            .map(|item| {
                item.map(|(validator, stake)| StakeInfo {
                    owner: user.to_string(),
                    validator,
                    stake: stake.stake,
                })
            })
            .take(limit)
            .collect::<Result<_, _>>()?;

        let resp = StakesResponse { stakes };

        Ok(resp)
    }
}
