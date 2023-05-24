use cosmwasm_std::{
    ensure_eq, entry_point, from_slice, to_binary, Addr, Binary, Decimal, DepsMut, Env, Reply,
    Response, SubMsg, SubMsgResponse, WasmMsg,
};
use cw2::set_contract_version;
use cw_storage_plus::{Item, Map};
use cw_utils::{must_pay, parse_instantiate_response_data};
use sylvia::types::{ExecCtx, InstantiateCtx, QueryCtx};
use sylvia::{contract, schemars};

use mesh_apis::local_staking_api::{self, LocalStakingApi, MaxSlashResponse};
use mesh_apis::vault_api::VaultApiHelper;
use mesh_native_staking_proxy::native_staking_callback::{self, NativeStakingCallback};

use crate::error::ContractError;
use crate::msg::{ConfigResponse, OwnerByProxyResponse, OwnerMsg, ProxyByOwnerResponse, StakeMsg};
use crate::state::Config;

pub const CONTRACT_NAME: &str = env!("CARGO_PKG_NAME");
pub const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub const REPLY_ID_INSTANTIATE: u64 = 2;

// TODO: Hardcoded for now. Revisit for v1.
pub const MAX_SLASH_PERCENTAGE: u64 = 10;

pub struct NativeStakingContract<'a> {
    config: Item<'a, Config>,
    /// Map of proxy contract address by owner address
    proxy_by_owner: Map<'a, &'a Addr, Addr>,
    /// Reverse map of owner address by proxy contract address
    owner_by_proxy: Map<'a, &'a Addr, Addr>,
}

#[contract]
#[error(ContractError)]
#[messages(local_staking_api as LocalStakingApi)]
#[messages(native_staking_callback as NativeStakingCallback)]
impl NativeStakingContract<'_> {
    pub const fn new() -> Self {
        Self {
            config: Item::new("config"),
            proxy_by_owner: Map::new("proxies"),
            owner_by_proxy: Map::new("owners"),
        }
    }

    /// The caller of the instantiation will be the vault contract
    #[msg(instantiate)]
    pub fn instantiate(
        &self,
        ctx: InstantiateCtx,
        denom: String,
        proxy_code_id: u64,
    ) -> Result<Response, ContractError> {
        let config = Config {
            denom,
            proxy_code_id,
            vault: ctx.info.sender,
        };
        self.config.save(ctx.deps.storage, &config)?;
        set_contract_version(ctx.deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;
        Ok(Response::new())
    }

    #[msg(query)]
    fn config(&self, ctx: QueryCtx) -> Result<ConfigResponse, ContractError> {
        self.config.load(ctx.deps.storage).map_err(Into::into)
    }

    fn reply_init_callback(
        &self,
        deps: DepsMut,
        reply: SubMsgResponse,
    ) -> Result<Response, ContractError> {
        let init_data = parse_instantiate_response_data(&reply.data.unwrap())?;

        // Associate staking proxy with owner address
        let proxy_addr = Addr::unchecked(init_data.contract_address);
        let owner_data: OwnerMsg =
            from_slice(&init_data.data.ok_or(ContractError::NoInstantiateData {})?)?;
        let owner_addr = deps.api.addr_validate(&owner_data.owner)?;
        self.proxy_by_owner
            .save(deps.storage, &owner_addr, &proxy_addr)?;
        self.owner_by_proxy
            .save(deps.storage, &proxy_addr, &owner_addr)?;

        Ok(Response::new())
    }

    #[msg(query)]
    fn proxy_by_owner(
        &self,
        ctx: QueryCtx,
        owner: String,
    ) -> Result<ProxyByOwnerResponse, ContractError> {
        let owner_addr = ctx.deps.api.addr_validate(&owner)?;
        let proxy_addr = self.proxy_by_owner.load(ctx.deps.storage, &owner_addr)?;
        Ok(ProxyByOwnerResponse {
            proxy: proxy_addr.to_string(),
        })
    }

    #[msg(query)]
    fn owner_by_proxy(
        &self,
        ctx: QueryCtx,
        proxy: String,
    ) -> Result<OwnerByProxyResponse, ContractError> {
        let proxy_addr = ctx.deps.api.addr_validate(&proxy)?;
        let owner_addr = self.owner_by_proxy.load(ctx.deps.storage, &proxy_addr)?;
        Ok(OwnerByProxyResponse {
            owner: owner_addr.to_string(),
        })
    }
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn reply(deps: DepsMut, _env: Env, reply: Reply) -> Result<Response, ContractError> {
    match reply.id {
        REPLY_ID_INSTANTIATE => {
            NativeStakingContract::new().reply_init_callback(deps, reply.result.unwrap())
        }
        _ => Err(ContractError::InvalidReplyId(reply.id)),
    }
}

#[contract]
impl LocalStakingApi for NativeStakingContract<'_> {
    type Error = ContractError;

    /// Receives stake (info.funds) from vault contract on behalf of owner and performs the action
    /// specified in msg with it.
    /// Msg is custom to each implementation of the staking contract and opaque to the vault
    #[msg(exec)]
    fn receive_stake(
        &self,
        ctx: ExecCtx,
        owner: String,
        msg: Binary,
    ) -> Result<Response, Self::Error> {
        // Can only be called by the vault
        let cfg = self.config.load(ctx.deps.storage)?;
        ensure_eq!(cfg.vault, ctx.info.sender, ContractError::Unauthorized {});

        // Assert funds are passed in
        let _paid = must_pay(&ctx.info, &cfg.denom)?;

        // Parse message to find validator to stake on
        let StakeMsg { validator } = from_slice(&msg)?;

        let owner_addr = ctx.deps.api.addr_validate(&owner)?;

        // Look up if there is a proxy to match. Instantiate or call stake on existing
        match self
            .proxy_by_owner
            .may_load(ctx.deps.storage, &owner_addr)?
        {
            None => {
                // Instantiate proxy contract and send funds to stake, with reply handling on success
                let msg = to_binary(&mesh_native_staking_proxy::contract::InstantiateMsg {
                    denom: cfg.denom,
                    owner: owner.clone(),
                    validator,
                })?;
                let wasm_msg = WasmMsg::Instantiate {
                    admin: Some(ctx.env.contract.address.into()),
                    code_id: cfg.proxy_code_id,
                    msg,
                    funds: ctx.info.funds,
                    label: format!("LSP for {owner}"),
                };
                let sub_msg = SubMsg::reply_on_success(wasm_msg, REPLY_ID_INSTANTIATE);
                Ok(Response::new().add_submessage(sub_msg))
            }
            Some(proxy_addr) => {
                // Send stake message with funds to the proxy contract
                let msg =
                    to_binary(&mesh_native_staking_proxy::contract::ExecMsg::Stake { validator })?;
                let wasm_msg = WasmMsg::Execute {
                    contract_addr: proxy_addr.into(),
                    msg,
                    funds: ctx.info.funds,
                };
                Ok(Response::new().add_message(wasm_msg))
            }
        }
    }

    /// Returns the maximum percentage that can be slashed
    /// TODO: Any way to query this from the chain? Or we just pass in InstantiateMsg?
    #[msg(query)]
    fn max_slash(&self, _ctx: QueryCtx) -> Result<MaxSlashResponse, Self::Error> {
        Ok(MaxSlashResponse {
            max_slash: Decimal::percent(MAX_SLASH_PERCENTAGE),
        })
    }
}

#[contract]
impl NativeStakingCallback for NativeStakingContract<'_> {
    type Error = ContractError;

    /// This sends tokens back from the proxy to native-staking. (See info.funds)
    /// The native-staking contract can determine which user it belongs to via an internal Map.
    /// The native-staking contract will then send those tokens back to vault and release the claim.
    #[msg(exec)]
    fn release_proxy_stake(&self, ctx: ExecCtx) -> Result<Response, Self::Error> {
        let cfg = self.config.load(ctx.deps.storage)?;

        // Assert funds are passed in
        let _paid = must_pay(&ctx.info, &cfg.denom)?;

        // Look up account owner by proxy address (info.sender). This asserts the caller is a valid
        // proxy
        let owner_addr = self
            .owner_by_proxy
            .load(ctx.deps.storage, &ctx.info.sender)?;

        // Send the tokens to the vault contract
        let msg = VaultApiHelper(cfg.vault)
            .release_local_stake(owner_addr.to_string(), ctx.info.funds)?;

        Ok(Response::new().add_message(msg))
    }
}
