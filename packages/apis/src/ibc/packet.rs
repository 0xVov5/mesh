use cosmwasm_schema::cw_serde;
use cosmwasm_std::Coin;

/// These are messages sent from provider -> consumer
/// ibc_packet_receive in converter must handle them all.
/// Each one has a different ack to be used in the reply.
#[cw_serde]
pub enum ProviderPacket {
    /// This should be called when we lock more tokens to virtually stake on a given validator
    Stake {
        validator: String,
        /// This is the local (provider-side) denom that is held in the vault.
        /// It will be converted to the consumer-side staking token in the converter with help
        /// of the price feed.
        stake: Coin,
    },
    /// This should be called when we begin the unbonding period of some more tokens previously virtually staked
    Unstake {
        validator: String,
        /// This is the local (provider-side) denom that is held in the vault.
        /// It will be converted to the consumer-side staking token in the converter with help
        /// of the price feed.
        unstake: Coin,
    },
}

/// Ack sent for ProviderPacket::Stake
#[cw_serde]
pub struct StakeAck {}

/// Ack sent for ProviderPacket::Unstake
#[cw_serde]
pub struct UnstakeAck {}

/// These are messages sent from consumer -> provider
/// ibc_packet_receive in external-staking must handle them all.
#[cw_serde]
pub enum ConsumerPacket {
    /// This is sent when a new validator registers and is available to receive
    /// delegations. This is also sent when a validator changes pubkey.
    /// One such packet is sent right after the channel is opened to sync initial state
    AddValidators(Vec<AddValidator>),
    /// This is sent when a validator is tombstoned. Not just leaving the active state,
    /// but when it is no longer a valid target to delegate to.
    /// It contains a list of `valoper_address` to be removed
    RemoveValidators(Vec<String>),
}

#[cw_serde]
pub struct AddValidator {
    /// This is the validator operator (valoper) address used for delegations and rewards
    pub valoper: String,

    // TODO: is there a better type for this? what encoding is used
    /// This is the *Tendermint* public key, used for signing blocks.
    /// This is needed to detect slashing conditions
    pub pub_key: String,

    /// This is the first height the validator was active.
    /// It is used to detect slashing conditions, eg which header heights are punishable.
    pub start_height: u64,

    /// This is the timestamp of the first block the validator was active.
    /// It may be used for unbonding_period issues, maybe just for informational purposes.
    /// Stored as unix seconds.
    pub start_time: u64,
}

/// Ack sent for ConsumerPacket::AddValidators
#[cw_serde]
pub struct AddValidatorsAck {}

/// Ack sent for ConsumerPacket::RemoveValidators
#[cw_serde]
pub struct RemoveValidatorsAck {}
