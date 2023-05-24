use cosmwasm_std::{
    coin, coins, ensure, Addr, BankMsg, Binary, Coin, Decimal, DepsMut, Order, Reply, Response,
    SubMsg, SubMsgResponse, Uint128, WasmMsg,
};
use cw2::set_contract_version;
use cw_storage_plus::{Item, Map};
use cw_utils::{must_pay, nonpayable, parse_instantiate_response_data};

use mesh_apis::cross_staking_api::CrossStakingApiHelper;
use mesh_apis::local_staking_api::{
    LocalStakingApiHelper, LocalStakingApiQueryMsg, MaxSlashResponse,
};
use mesh_apis::vault_api::{self, VaultApi};
use sylvia::types::{ExecCtx, InstantiateCtx, QueryCtx, ReplyCtx};
use sylvia::{contract, schemars};

use crate::error::ContractError;
use crate::msg::{AccountResponse, ConfigResp, LienInfo, StakingInitInfo};
use crate::state::{Config, Lien, LocalStaking, UserInfo};

pub const CONTRACT_NAME: &str = env!("CARGO_PKG_NAME");
pub const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub const REPLY_ID_INSTANTIATE: u64 = 1;

pub struct VaultContract<'a> {
    /// General contract configuration
    config: Item<'a, Config>,
    /// Local staking info
    local_staking: Item<'a, LocalStaking>,
    /// All liens in the protocol
    ///
    /// Liens are indexed with (user, creditor), as this pair has to be unique
    liens: Map<'a, (&'a Addr, &'a Addr), Lien>,
    /// Per-user information
    users: Map<'a, &'a Addr, UserInfo>,
}

#[cfg_attr(not(feautre = "library"), sylvia::entry_points)]
#[contract]
#[error(ContractError)]
#[messages(vault_api as VaultApi)]
impl VaultContract<'_> {
    pub const fn new() -> Self {
        Self {
            config: Item::new("config"),
            local_staking: Item::new("local_staking"),
            liens: Map::new("liens"),
            users: Map::new("users"),
        }
    }

    #[msg(instantiate)]
    pub fn instantiate(
        &self,
        ctx: InstantiateCtx,
        denom: String,
        local_staking: StakingInitInfo,
    ) -> Result<Response, ContractError> {
        nonpayable(&ctx.info)?;

        let config = Config { denom };
        self.config.save(ctx.deps.storage, &config)?;
        set_contract_version(ctx.deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

        // instantiate local_staking and handle reply
        let msg = WasmMsg::Instantiate {
            admin: local_staking.admin,
            code_id: local_staking.code_id,
            msg: local_staking.msg,
            funds: vec![],
            label: local_staking
                .label
                .unwrap_or_else(|| "Mesh Security Local Staking".to_string()),
        };
        let sub_msg = SubMsg::reply_on_success(msg, REPLY_ID_INSTANTIATE);
        Ok(Response::new().add_submessage(sub_msg))
    }

    #[msg(exec)]
    fn bond(&self, ctx: ExecCtx) -> Result<Response, ContractError> {
        let denom = self.config.load(ctx.deps.storage)?.denom;
        let amount = must_pay(&ctx.info, &denom)?;

        let mut user = self
            .users
            .may_load(ctx.deps.storage, &ctx.info.sender)?
            .unwrap_or_default();
        user.collateral += amount;
        self.users.save(ctx.deps.storage, &ctx.info.sender, &user)?;

        let resp = Response::new()
            .add_attribute("action", "bond")
            .add_attribute("sender", ctx.info.sender)
            .add_attribute("amount", amount.to_string());

        Ok(resp)
    }

    #[msg(exec)]
    fn unbond(&self, ctx: ExecCtx, amount: Coin) -> Result<Response, ContractError> {
        nonpayable(&ctx.info)?;

        let coin = amount;
        let amount = coin.amount;

        let mut user = self
            .users
            .may_load(ctx.deps.storage, &ctx.info.sender)?
            .unwrap_or_default();

        let free_collateral = user.free_collateral();
        ensure!(
            user.free_collateral() >= amount,
            ContractError::ClaimsLocked(free_collateral)
        );

        user.collateral -= amount;
        self.users.save(ctx.deps.storage, &ctx.info.sender, &user)?;

        let msg = BankMsg::Send {
            to_address: ctx.info.sender.to_string(),
            amount: vec![coin],
        };

        let resp = Response::new()
            .add_message(msg)
            .add_attribute("action", "unbond")
            .add_attribute("sender", ctx.info.sender)
            .add_attribute("amount", amount.to_string());

        Ok(resp)
    }

    /// This assigns a claim of amount tokens to the remote contract, which can take some action with it
    #[msg(exec)]
    fn stake_remote(
        &self,
        mut ctx: ExecCtx,
        // address of the contract to virtually stake on
        contract: String,
        // amount to stake on that contract
        amount: Coin,
        // action to take with that stake
        msg: Binary,
    ) -> Result<Response, ContractError> {
        nonpayable(&ctx.info)?;

        let contract = ctx.deps.api.addr_validate(&contract)?;
        let contract = CrossStakingApiHelper(contract);
        let slashable = contract.max_slash(ctx.deps.as_ref())?;

        let denom = self.config.load(ctx.deps.storage)?.denom;
        ensure!(denom == amount.denom, ContractError::UnexpectedDenom(denom));

        let amount = amount.amount;

        self.stake(&mut ctx, &contract.0, slashable.max_slash, amount)?;

        let stake_msg = contract.receive_virtual_stake(
            ctx.info.sender.to_string(),
            coin(amount.u128(), denom),
            msg,
            vec![],
        )?;

        let resp = Response::new()
            .add_message(stake_msg)
            .add_attribute("action", "stake_remote")
            .add_attribute("sender", ctx.info.sender)
            .add_attribute("amount", amount.to_string());

        Ok(resp)
    }

    /// This sends actual tokens to the local staking contract
    #[msg(exec)]
    fn stake_local(
        &self,
        mut ctx: ExecCtx,
        // amount to stake on that contract
        amount: Coin,
        // action to take with that stake
        msg: Binary,
    ) -> Result<Response, ContractError> {
        let denom = self.config.load(ctx.deps.storage)?.denom;
        ensure!(denom == amount.denom, ContractError::UnexpectedDenom(denom));

        let amount = amount.amount;

        let local_staking = self.local_staking.load(ctx.deps.storage)?;

        self.stake(
            &mut ctx,
            &local_staking.contract.0,
            local_staking.max_slash,
            amount,
        )?;

        let stake_msg = local_staking.contract.receive_stake(
            ctx.info.sender.to_string(),
            msg,
            coins(amount.u128(), denom),
        )?;

        let resp = Response::new()
            .add_message(stake_msg)
            .add_attribute("action", "stake_local")
            .add_attribute("sender", ctx.info.sender)
            .add_attribute("amount", amount.to_string());

        Ok(resp)
    }

    #[msg(query)]
    fn account(&self, ctx: QueryCtx, account: String) -> Result<AccountResponse, ContractError> {
        let denom = self.config.load(ctx.deps.storage)?.denom;

        let account = Addr::unchecked(account);

        let claims = self
            .liens
            .prefix(&account)
            .range(ctx.deps.storage, None, None, Order::Ascending)
            .map(|lien| {
                lien.map(|(lienholder, lien)| LienInfo {
                    lienholder: lienholder.into(),
                    amount: lien.amount,
                })
            })
            .collect::<Result<_, _>>()?;

        let user = self
            .users
            .may_load(ctx.deps.storage, &account)?
            .unwrap_or_default();

        let resp = AccountResponse {
            denom,
            bonded: user.collateral,
            free: user.free_collateral(),
            claims,
        };

        Ok(resp)
    }

    #[msg(query)]
    fn config(&self, ctx: QueryCtx) -> Result<ConfigResp, ContractError> {
        let config = self.config.load(ctx.deps.storage)?;
        let local_staking = self.local_staking.load(ctx.deps.storage)?;

        let resp = ConfigResp {
            denom: config.denom,
            local_staking: local_staking.contract.0.into(),
        };

        Ok(resp)
    }

    #[msg(reply)]
    fn reply(&self, ctx: ReplyCtx, reply: Reply) -> Result<Response, ContractError> {
        match reply.id {
            REPLY_ID_INSTANTIATE => self.reply_init_callback(ctx.deps, reply.result.unwrap()),
            _ => Err(ContractError::InvalidReplyId(reply.id)),
        }
    }

    fn reply_init_callback(
        &self,
        deps: DepsMut,
        reply: SubMsgResponse,
    ) -> Result<Response, ContractError> {
        let init_data = parse_instantiate_response_data(&reply.data.unwrap())?;
        let local_staking = Addr::unchecked(init_data.contract_address);

        // As we control the local staking contract it might be better to just raw-query it
        // on demand instead of duplicating the data.
        let query = LocalStakingApiQueryMsg::MaxSlash {};
        let MaxSlashResponse { max_slash } =
            deps.querier.query_wasm_smart(&local_staking, &query)?;

        let local_staking = LocalStaking {
            contract: LocalStakingApiHelper(local_staking),
            max_slash,
        };

        self.local_staking.save(deps.storage, &local_staking)?;

        Ok(Response::new())
    }

    /// Updates the local stake for staking on any contract
    ///
    /// Stake (both local and remote) is always called by the tokens owner, so the `sender` is
    /// ued as an owner address.
    fn stake(
        &self,
        ctx: &mut ExecCtx,
        lienholder: &Addr,
        slashable: Decimal,
        amount: Uint128,
    ) -> Result<(), ContractError> {
        let mut lien = self
            .liens
            .may_load(ctx.deps.storage, (&ctx.info.sender, lienholder))?
            .unwrap_or(Lien {
                amount: Uint128::zero(),
                slashable,
            });
        lien.amount += amount;

        let mut user = self
            .users
            .may_load(ctx.deps.storage, &ctx.info.sender)?
            .unwrap_or_default();
        user.max_lien = user.max_lien.max(lien.amount);
        user.total_slashable += amount * lien.slashable;

        ensure!(user.verify_collateral(), ContractError::InsufficentBalance);

        self.liens
            .save(ctx.deps.storage, (&ctx.info.sender, lienholder), &lien)?;

        self.users.save(ctx.deps.storage, &ctx.info.sender, &user)?;

        Ok(())
    }

    /// Updates the local stake for unstaking from any contract
    ///
    /// The unstake (both local and remote) is always called by the staking contract
    /// (aka leinholder), so the `sender` address is used for that.
    fn unstake(&self, ctx: &mut ExecCtx, owner: String, amount: Coin) -> Result<(), ContractError> {
        let denom = self.config.load(ctx.deps.storage)?.denom;
        ensure!(amount.denom == denom, ContractError::UnexpectedDenom(denom));
        let amount = amount.amount;

        let owner = Addr::unchecked(owner);
        let mut lien = self
            .liens
            .may_load(ctx.deps.storage, (&owner, &ctx.info.sender))?
            .ok_or(ContractError::UnknownLienholder)?;

        ensure!(lien.amount >= amount, ContractError::InsufficientLien);
        lien.amount -= amount;
        self.liens
            .save(ctx.deps.storage, (&owner, &ctx.info.sender), &lien)?;

        let mut user = self.users.load(ctx.deps.storage, &owner)?;

        // Max lien has to be recalculatd from scratch; the just released lien
        // is already decreased in cache
        user.max_lien = self
            .liens
            .prefix(&owner)
            .range(ctx.deps.storage, None, None, Order::Ascending)
            .try_fold(Uint128::zero(), |max_lien, lien| {
                lien.map(|(_, lien)| max_lien.max(lien.amount))
            })?;

        user.total_slashable -= amount * lien.slashable;
        self.users.save(ctx.deps.storage, &owner, &user)?;

        Ok(())
    }
}

#[contract]
#[messages(vault_api as VaultApi)]
impl VaultApi for VaultContract<'_> {
    type Error = ContractError;

    /// This must be called by the remote staking contract to release this claim
    #[msg(exec)]
    fn release_cross_stake(
        &self,
        mut ctx: ExecCtx,
        // address of the user who originally called stake_remote
        owner: String,
        // amount to unstake on that contract
        amount: Coin,
    ) -> Result<Response, ContractError> {
        nonpayable(&ctx.info)?;

        self.unstake(&mut ctx, owner.clone(), amount.clone())?;

        let resp = Response::new()
            .add_attribute("action", "release_cross_stake")
            .add_attribute("sender", ctx.info.sender)
            .add_attribute("owner", owner)
            .add_attribute("amount", amount.amount.to_string());

        Ok(resp)
    }

    /// This must be called by the local staking contract to release this claim
    /// Amount of tokens unstaked are those included in ctx.info.funds
    #[msg(exec)]
    fn release_local_stake(
        &self,
        mut ctx: ExecCtx,
        // address of the user who originally called stake_remote
        owner: String,
    ) -> Result<Response, ContractError> {
        let denom = self.config.load(ctx.deps.storage)?.denom;
        let amount = must_pay(&ctx.info, &denom)?;

        self.unstake(&mut ctx, owner.clone(), coin(amount.u128(), denom))?;

        let resp = Response::new()
            .add_attribute("action", "release_cross_stake")
            .add_attribute("sender", ctx.info.sender)
            .add_attribute("owner", owner)
            .add_attribute("amount", amount.to_string());

        Ok(resp)
    }
}
