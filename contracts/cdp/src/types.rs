use near_contract_standards::fungible_token::Balance;
use near_sdk::borsh::{BorshDeserialize, BorshSerialize};
use near_sdk::json_types::{U128, U64};
use near_sdk::{near, AccountId, BorshStorageKey, Gas};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const BPS_DENOMINATOR: u128 = 10_000;
pub const GAS_FOR_SWAP: Gas = Gas::from_tgas(50);
pub const GAS_FOR_CALLBACK: Gas = Gas::from_tgas(25);
pub const GAS_FOR_FT_TRANSFER: Gas = Gas::from_tgas(10);
pub const REWARD_SCALE: u128 = 10u128.pow(24);

pub type TokenId = AccountId;

#[derive(BorshStorageKey)]
#[near]
pub enum StorageKey {
    FungibleToken,
    TokenMetadata,
    CollateralConfigs,
    Troves,
    TotalDebt,
    PriceFeeds,
    StabilityPoolDeposits,
    CollateralRewards,
    RewardPerShare,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(crate = "near_sdk::serde")]
pub struct CollateralConfig {
    pub oracle_price_id: String,
    pub min_collateral_ratio_bps: u16,
    pub recovery_collateral_ratio_bps: u16,
    #[schemars(with = "String")]
    pub debt_ceiling: U128,
    pub liquidation_penalty_bps: u16,
    pub stability_pool_mode: StabilityPoolMode,
}

#[derive(Clone)]
#[near(serializers=[borsh])]
pub struct CollateralConfigInternal {
    pub oracle_price_id: String,
    pub min_collateral_ratio_bps: u16,
    pub recovery_collateral_ratio_bps: u16,
    pub debt_ceiling: Balance,
    pub liquidation_penalty_bps: u16,
    pub stability_pool_mode: StabilityPoolMode,
}

impl From<CollateralConfigInternal> for CollateralConfig {
    fn from(value: CollateralConfigInternal) -> Self {
        Self {
            oracle_price_id: value.oracle_price_id,
            min_collateral_ratio_bps: value.min_collateral_ratio_bps,
            recovery_collateral_ratio_bps: value.recovery_collateral_ratio_bps,
            debt_ceiling: U128(value.debt_ceiling),
            liquidation_penalty_bps: value.liquidation_penalty_bps,
            stability_pool_mode: value.stability_pool_mode,
        }
    }
}

impl From<CollateralConfig> for CollateralConfigInternal {
    fn from(value: CollateralConfig) -> Self {
        Self {
            oracle_price_id: value.oracle_price_id,
            min_collateral_ratio_bps: value.min_collateral_ratio_bps,
            recovery_collateral_ratio_bps: value.recovery_collateral_ratio_bps,
            debt_ceiling: value.debt_ceiling.0,
            liquidation_penalty_bps: value.liquidation_penalty_bps,
            stability_pool_mode: value.stability_pool_mode,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(crate = "near_sdk::serde")]
#[near(serializers=[borsh])]
pub enum StabilityPoolMode {
    Dedicated,
    Shared,
}

impl Default for StabilityPoolMode {
    fn default() -> Self {
        Self::Dedicated
    }
}

#[derive(Clone)]
#[near(serializers=[borsh])]
pub struct TroveKey {
    pub owner_id: AccountId,
    pub collateral_id: AccountId,
}

#[derive(Clone)]
#[near(serializers=[borsh])]
pub struct TroveInternal {
    pub owner_id: AccountId,
    pub collateral_id: AccountId,
    pub collateral_amount: Balance,
    pub debt_amount: Balance,
    pub last_update_timestamp: u64,
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(crate = "near_sdk::serde")]
pub struct Trove {
    #[schemars(with = "String")]
    pub owner_id: AccountId,
    #[schemars(with = "String")]
    pub collateral_id: AccountId,
    #[schemars(with = "String")]
    pub collateral_amount: U128,
    #[schemars(with = "String")]
    pub debt_amount: U128,
    #[schemars(with = "String")]
    pub last_update_timestamp: U64,
}

impl From<TroveInternal> for Trove {
    fn from(value: TroveInternal) -> Self {
        Self {
            owner_id: value.owner_id,
            collateral_id: value.collateral_id,
            collateral_amount: U128(value.collateral_amount),
            debt_amount: U128(value.debt_amount),
            last_update_timestamp: U64(value.last_update_timestamp),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(crate = "near_sdk::serde")]
pub struct PriceFeed {
    #[schemars(with = "String")]
    pub price: U128,
    pub decimals: u8,
    #[schemars(with = "String")]
    pub last_update_timestamp: U64,
}

#[derive(Clone)]
#[near(serializers=[borsh])]
pub struct PriceFeedInternal {
    pub price: Balance,
    pub decimals: u8,
    pub last_update_timestamp: u64,
}

impl From<PriceFeedInternal> for PriceFeed {
    fn from(value: PriceFeedInternal) -> Self {
        Self {
            price: U128(value.price),
            decimals: value.decimals,
            last_update_timestamp: U64(value.last_update_timestamp),
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(crate = "near_sdk::serde", tag = "action", rename_all = "snake_case")]
pub enum TransferAction {
    DepositCollateral { target_account: Option<AccountId> },
    RepayDebt { collateral_id: AccountId },
}

#[derive(Clone)]
#[near(serializers=[borsh])]
pub struct CollateralRewardKey {
    pub account_id: AccountId,
    pub collateral_id: AccountId,
}

impl CollateralRewardKey {
    pub fn new(account_id: &AccountId, collateral_id: &AccountId) -> Self {
        Self {
            account_id: account_id.clone(),
            collateral_id: collateral_id.clone(),
        }
    }
}

#[derive(Clone)]
#[near(serializers=[borsh])]
pub struct StabilityDeposit {
    pub shares: Balance,
    pub reward_debt: BTreeMap<AccountId, u128>,
    pub epoch: u64,
}

impl StabilityDeposit {
    pub fn new(epoch: u64) -> Self {
        Self {
            shares: 0,
            reward_debt: BTreeMap::new(),
            epoch,
        }
    }

    pub fn amount(&self, total_nusd: Balance, total_shares: Balance) -> Balance {
        if self.shares == 0 || total_shares == 0 || total_nusd == 0 {
            0
        } else {
            self.shares
                .checked_mul(total_nusd)
                .expect("Share amount overflow")
                / total_shares
        }
    }
}
