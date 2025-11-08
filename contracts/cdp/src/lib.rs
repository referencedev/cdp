use near_contract_standards::fungible_token::core::FungibleTokenCore;
use near_contract_standards::fungible_token::events::{FtBurn, FtMint};
use near_contract_standards::fungible_token::metadata::{
    FungibleTokenMetadata, FungibleTokenMetadataProvider, FT_METADATA_SPEC,
};
use near_contract_standards::fungible_token::receiver::FungibleTokenReceiver;
use near_contract_standards::fungible_token::resolver::FungibleTokenResolver;
use near_contract_standards::fungible_token::{Balance, FungibleToken};
use near_contract_standards::storage_management::{
    StorageBalance, StorageBalanceBounds, StorageManagement,
};
use near_sdk::borsh::{self, BorshDeserialize, BorshSerialize};
use near_sdk::collections::{LookupMap, UnorderedMap};
use near_sdk::json_types::{U128, U64};
use near_sdk::serde_json;
use near_sdk::store::LazyOption;
use near_sdk::{
    assert_one_yocto, env, ext_contract, log, near_bindgen, require, AccountId, BorshStorageKey,
    Gas, NearToken, PanicOnDefault, Promise, PromiseOrValue, PromiseResult,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const BPS_DENOMINATOR: u128 = 10_000;
const GAS_FOR_SWAP: Gas = Gas::from_tgas(50);
const GAS_FOR_CALLBACK: Gas = Gas::from_tgas(25);
const GAS_FOR_FT_TRANSFER: Gas = Gas::from_tgas(10);
const REWARD_SCALE: u128 = 10u128.pow(24);

pub type TokenId = AccountId;

#[derive(BorshSerialize, BorshStorageKey)]
enum StorageKey {
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

#[derive(BorshDeserialize, BorshSerialize, Clone)]
struct CollateralConfigInternal {
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

#[derive(
    BorshDeserialize, BorshSerialize, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema,
)]
#[serde(crate = "near_sdk::serde")]
pub enum StabilityPoolMode {
    Dedicated,
    Shared,
}

impl Default for StabilityPoolMode {
    fn default() -> Self {
        Self::Dedicated
    }
}

#[derive(BorshDeserialize, BorshSerialize)]
struct TroveKey {
    owner_id: AccountId,
    collateral_id: AccountId,
}

#[derive(BorshDeserialize, BorshSerialize, Clone)]
struct TroveInternal {
    owner_id: AccountId,
    collateral_id: AccountId,
    collateral_amount: Balance,
    debt_amount: Balance,
    last_update_timestamp: u64,
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

#[derive(BorshDeserialize, BorshSerialize, Clone)]
struct PriceFeedInternal {
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

#[derive(BorshDeserialize, BorshSerialize, Clone)]
struct CollateralRewardKey {
    account_id: AccountId,
    collateral_id: AccountId,
}

impl CollateralRewardKey {
    fn new(account_id: &AccountId, collateral_id: &AccountId) -> Self {
        Self {
            account_id: account_id.clone(),
            collateral_id: collateral_id.clone(),
        }
    }
}

#[derive(BorshDeserialize, BorshSerialize, Clone)]
struct StabilityDeposit {
    shares: Balance,
    reward_debt: BTreeMap<AccountId, u128>,
    epoch: u64,
}

impl StabilityDeposit {
    fn new(epoch: u64) -> Self {
        Self {
            shares: 0,
            reward_debt: BTreeMap::new(),
            epoch,
        }
    }

    fn amount(&self, total_nusd: Balance, total_shares: Balance) -> Balance {
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

#[derive(Deserialize, Serialize)]
#[serde(crate = "near_sdk::serde", tag = "action", rename_all = "snake_case")]
enum TransferAction {
    DepositCollateral { target_account: Option<AccountId> },
    RepayDebt { collateral_id: AccountId },
}

#[ext_contract(ext_intents)]
pub trait NearIntentsDex {
    fn execute_swap(
        &mut self,
        caller_id: AccountId,
        input_token: AccountId,
        output_token: AccountId,
        amount_in: U128,
        min_out: U128,
        routing_hint: Option<String>,
    );
}

#[ext_contract(ext_ft)]
pub trait ExternalFungibleToken {
    fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>);
}

#[allow(dead_code)]
#[ext_contract(ext_self)]
trait ContractCallbacks {
    fn on_swap_complete(
        &mut self,
        caller_id: AccountId,
        input_token: AccountId,
        amount_in: U128,
    ) -> bool;
}

#[near_bindgen]
#[derive(BorshDeserialize, BorshSerialize, PanicOnDefault)]
pub struct Contract {
    owner_id: AccountId,
    intent_router_id: AccountId,
    pyth_oracle_id: AccountId,
    configs: UnorderedMap<TokenId, CollateralConfigInternal>,
    troves: LookupMap<TroveKey, TroveInternal>,
    total_debt: LookupMap<TokenId, Balance>,
    price_feeds: LookupMap<TokenId, PriceFeedInternal>,
    stability_pool_deposits: LookupMap<AccountId, StabilityDeposit>,
    collateral_rewards: LookupMap<CollateralRewardKey, Balance>,
    reward_per_share: UnorderedMap<TokenId, u128>,
    stability_pool_total_shares: Balance,
    stability_pool_total_nusd: Balance,
    stability_pool_epoch: u64,
    nusd: FungibleToken,
    metadata: LazyOption<FungibleTokenMetadata>,
}

#[near_bindgen]
impl Contract {
    #[init]
    pub fn new(
        owner_id: AccountId,
        intent_router_id: AccountId,
        pyth_oracle_id: AccountId,
        metadata: FungibleTokenMetadata,
    ) -> Self {
        assert!(!env::state_exists(), "Already initialized");

        let mut nusd = FungibleToken::new(StorageKey::FungibleToken);
        let current_id = env::current_account_id();
        nusd.internal_register_account(&current_id);
        if owner_id != current_id {
            nusd.internal_register_account(&owner_id);
        }

        Self {
            owner_id,
            intent_router_id,
            pyth_oracle_id,
            configs: UnorderedMap::new(StorageKey::CollateralConfigs),
            troves: LookupMap::new(StorageKey::Troves),
            total_debt: LookupMap::new(StorageKey::TotalDebt),
            price_feeds: LookupMap::new(StorageKey::PriceFeeds),
            stability_pool_deposits: LookupMap::new(StorageKey::StabilityPoolDeposits),
            collateral_rewards: LookupMap::new(StorageKey::CollateralRewards),
            reward_per_share: UnorderedMap::new(StorageKey::RewardPerShare),
            stability_pool_total_shares: 0,
            stability_pool_total_nusd: 0,
            stability_pool_epoch: 0,
            nusd,
            metadata: LazyOption::new(StorageKey::TokenMetadata, Some(metadata)),
        }
    }

    pub fn owner_id(&self) -> AccountId {
        self.owner_id.clone()
    }

    pub fn intent_router_id(&self) -> AccountId {
        self.intent_router_id.clone()
    }

    pub fn pyth_oracle_id(&self) -> AccountId {
        self.pyth_oracle_id.clone()
    }

    pub fn list_collateral_tokens(&self) -> Vec<AccountId> {
        self.configs.keys_as_vector().to_vec()
    }

    pub fn get_collateral_config(&self, token_id: AccountId) -> Option<CollateralConfig> {
        self.configs.get(&token_id).map(Into::into)
    }

    pub fn get_price(&self, collateral_id: AccountId) -> Option<PriceFeed> {
        self.price_feeds.get(&collateral_id).map(Into::into)
    }

    pub fn get_trove(&self, owner_id: AccountId, collateral_id: AccountId) -> Option<Trove> {
        self.troves
            .get(&Self::trove_key(&owner_id, &collateral_id))
            .map(Into::into)
    }

    pub fn get_total_debt(&self, collateral_id: AccountId) -> U128 {
        U128(self.total_debt.get(&collateral_id).unwrap_or(0))
    }

    pub fn get_stability_pool_balance(&self) -> U128 {
        U128(self.stability_pool_total_nusd)
    }

    pub fn get_stability_pool_deposit(&self, account_id: AccountId) -> U128 {
        self.stability_pool_deposits
            .get(&account_id)
            .filter(|deposit| deposit.epoch == self.stability_pool_epoch)
            .map(|deposit| {
                U128(deposit.amount(
                    self.stability_pool_total_nusd,
                    self.stability_pool_total_shares,
                ))
            })
            .unwrap_or(U128(0))
    }

    pub fn get_claimable_collateral_reward(
        &self,
        account_id: AccountId,
        collateral_id: AccountId,
    ) -> U128 {
        let key = CollateralRewardKey::new(&account_id, &collateral_id);
        let mut total = self.collateral_rewards.get(&key).unwrap_or(0);
        if let Some(deposit) = self.stability_pool_deposits.get(&account_id) {
            if deposit.shares > 0 {
                let global = self.reward_per_share.get(&collateral_id).unwrap_or(0);
                let paid = deposit
                    .reward_debt
                    .get(&collateral_id)
                    .copied()
                    .unwrap_or(0);
                if global > paid {
                    let pending = deposit
                        .shares
                        .checked_mul(global - paid)
                        .expect("View reward overflow")
                        / REWARD_SCALE;
                    total = total.checked_add(pending).expect("Reward overflow");
                }
            }
        }
        U128(total)
    }

    #[payable]
    pub fn register_collateral(&mut self, token_id: AccountId, config: CollateralConfig) {
        assert_one_yocto();
        self.assert_owner();
        require!(
            config.min_collateral_ratio_bps >= 1100,
            "MCR must be >= 110%"
        );
        require!(
            config.recovery_collateral_ratio_bps >= config.min_collateral_ratio_bps,
            "Recovery ratio must be >= MCR"
        );
        let internal: CollateralConfigInternal = config.into();
        self.configs.insert(&token_id, &internal);
    }

    pub fn submit_price(&mut self, collateral_id: AccountId, price: U128, decimals: u8) {
        require!(
            env::predecessor_account_id() == self.pyth_oracle_id,
            "Only oracle contract can submit prices"
        );
        require!(decimals <= 18, "Decimals must be <= 18");
        require!(price.0 > 0, "Price must be positive");
        let feed = PriceFeedInternal {
            price: price.0,
            decimals,
            last_update_timestamp: Self::now_ms(),
        };
        self.price_feeds.insert(&collateral_id, &feed);
    }

    #[payable]
    pub fn borrow(&mut self, collateral_id: AccountId, amount: U128) {
        assert_one_yocto();
        require!(amount.0 > 0, "Amount must be > 0");
        let caller = env::predecessor_account_id();
        let mut trove = self.expect_trove(&caller, &collateral_id);
        let config = self.expect_config(&collateral_id);
        let price = self.expect_price_internal(&collateral_id);

        let new_debt = trove
            .debt_amount
            .checked_add(amount.0)
            .expect("Debt overflow");
        self.ensure_debt_ceiling(&collateral_id, new_debt);
        let ratio = self.collateral_ratio(trove.collateral_amount, new_debt, &price);
        require!(
            ratio >= config.min_collateral_ratio_bps as u128,
            "Insufficient collateral"
        );

        trove.debt_amount = new_debt;
        trove.last_update_timestamp = Self::now_ms();
        self.save_trove(&caller, &collateral_id, &trove);
        self.add_total_debt(&collateral_id, amount.0 as i128);

        self.nusd.internal_deposit(&caller, amount.0);
        FtMint {
            owner_id: &caller,
            amount,
            memo: Some("cdp_borrow"),
        }
        .emit();
    }

    #[payable]
    pub fn repay(&mut self, collateral_id: AccountId, amount: U128) {
        assert_one_yocto();
        require!(amount.0 > 0, "Amount must be > 0");
        let caller = env::predecessor_account_id();
        self.nusd.internal_withdraw(&caller, amount.0);
        FtBurn {
            owner_id: &caller,
            amount,
            memo: Some("cdp_repay"),
        }
        .emit();
        self.internal_repay(&caller, &collateral_id, amount.0);
    }

    #[payable]
    pub fn withdraw_collateral(
        &mut self,
        collateral_id: AccountId,
        amount: U128,
        receiver: Option<AccountId>,
    ) -> Promise {
        assert_one_yocto();
        let caller = env::predecessor_account_id();
        let mut trove = self.expect_trove(&caller, &collateral_id);
        require!(trove.collateral_amount >= amount.0, "Not enough collateral");
        trove.collateral_amount -= amount.0;
        if trove.debt_amount > 0 {
            let price = self.expect_price_internal(&collateral_id);
            let config = self.expect_config(&collateral_id);
            let ratio = self.collateral_ratio(trove.collateral_amount, trove.debt_amount, &price);
            require!(
                ratio >= config.min_collateral_ratio_bps as u128,
                "Would violate MCR"
            );
        }
        trove.last_update_timestamp = Self::now_ms();
        self.save_trove(&caller, &collateral_id, &trove);
        let receiver_id = receiver.unwrap_or(caller.clone());
        self.send_collateral(receiver_id, collateral_id, amount.0)
    }

    #[payable]
    pub fn close_trove(&mut self, collateral_id: AccountId) -> Promise {
        assert_one_yocto();
        let caller = env::predecessor_account_id();
        let key = Self::trove_key(&caller, &collateral_id);
        let trove = self
            .troves
            .get(&key)
            .unwrap_or_else(|| env::panic_str("Trove not found"));
        require!(trove.debt_amount == 0, "Outstanding debt");
        self.troves.remove(&key);
        if trove.collateral_amount == 0 {
            env::panic_str("No collateral to withdraw");
        }
        self.send_collateral(caller, collateral_id, trove.collateral_amount)
    }

    #[payable]
    pub fn deposit_to_stability_pool(&mut self, amount: U128) {
        assert_one_yocto();
        require!(amount.0 > 0, "Amount must be > 0");
        let caller = env::predecessor_account_id();
        self.settle_stability_rewards(&caller);
        let mut deposit = self
            .stability_pool_deposits
            .get(&caller)
            .unwrap_or_else(|| StabilityDeposit::new(self.stability_pool_epoch));
        self.ensure_deposit_epoch(&caller, &mut deposit);
        let shares = self.shares_from_amount(amount.0);
        require!(shares > 0, "Shares must be > 0");
        deposit.shares = deposit
            .shares
            .checked_add(shares)
            .expect("Deposit share overflow");
        self.stability_pool_total_shares = self
            .stability_pool_total_shares
            .checked_add(shares)
            .expect("Pool share overflow");
        self.stability_pool_total_nusd = self
            .stability_pool_total_nusd
            .checked_add(amount.0)
            .expect("Pool balance overflow");
        self.sync_reward_debt_snapshot(&mut deposit);
        self.stability_pool_deposits.insert(&caller, &deposit);

        self.nusd.internal_withdraw(&caller, amount.0);
        self.nusd
            .internal_deposit(&env::current_account_id(), amount.0);
    }

    #[payable]
    pub fn withdraw_from_stability_pool(&mut self, amount: Option<U128>) {
        assert_one_yocto();
        let caller = env::predecessor_account_id();
        self.settle_stability_rewards(&caller);
        let mut deposit = self
            .stability_pool_deposits
            .get(&caller)
            .unwrap_or_else(|| StabilityDeposit::new(self.stability_pool_epoch));
        self.ensure_deposit_epoch(&caller, &mut deposit);
        require!(deposit.shares > 0, "Nothing deposited");
        let available = deposit.amount(
            self.stability_pool_total_nusd,
            self.stability_pool_total_shares,
        );
        require!(available > 0, "Pool depleted");
        let requested = amount.map(|v| v.0).unwrap_or(available);
        require!(requested > 0, "Amount must be > 0");
        require!(requested <= available, "Insufficient balance");
        let shares = self.shares_for_withdraw(requested);
        require!(shares > 0, "Share calculation underflow");

        deposit.shares = deposit
            .shares
            .checked_sub(shares)
            .expect("Withdraw exceeds shares");
        self.stability_pool_total_shares = self
            .stability_pool_total_shares
            .checked_sub(shares)
            .expect("Pool share underflow");
        self.stability_pool_total_nusd = self
            .stability_pool_total_nusd
            .checked_sub(requested)
            .expect("Pool balance underflow");
        self.stability_pool_deposits.insert(&caller, &deposit);

        self.nusd
            .internal_withdraw(&env::current_account_id(), requested);
        self.nusd.internal_deposit(&caller, requested);
    }

    #[payable]
    pub fn claim_collateral_reward(
        &mut self,
        collateral_id: AccountId,
        amount: Option<U128>,
    ) -> Promise {
        assert_one_yocto();
        let caller = env::predecessor_account_id();
        self.settle_stability_rewards(&caller);
        self.claim_collateral(&caller, &collateral_id, amount.map(|v| v.0))
    }

    #[payable]
    pub fn redeem(
        &mut self,
        collateral_id: AccountId,
        trove_owner: AccountId,
        amount: U128,
    ) -> Promise {
        assert_one_yocto();
        require!(amount.0 > 0, "Amount must be > 0");
        let redeemer = env::predecessor_account_id();
        let mut trove = self.expect_trove(&trove_owner, &collateral_id);
        require!(trove.debt_amount >= amount.0, "Redeem exceeds trove debt");

        let price = self.expect_price_internal(&collateral_id);
        let divisor = Self::decimals_factor(price.decimals);
        let collateral_out = amount
            .0
            .checked_mul(divisor)
            .expect("Redeem amount overflow")
            / price.price;
        require!(collateral_out > 0, "Redeem amount too small");
        require!(
            trove.collateral_amount >= collateral_out,
            "Redeem exceeds collateral"
        );

        trove.debt_amount -= amount.0;
        trove.collateral_amount -= collateral_out;
        trove.last_update_timestamp = Self::now_ms();
        if trove.debt_amount == 0 && trove.collateral_amount == 0 {
            self.troves
                .remove(&Self::trove_key(&trove_owner, &collateral_id));
        } else {
            self.save_trove(&trove_owner, &collateral_id, &trove);
        }
        self.add_total_debt(&collateral_id, -(amount.0 as i128));

        self.nusd.internal_withdraw(&redeemer, amount.0);
        FtBurn {
            owner_id: &redeemer,
            amount,
            memo: Some("cdp_redeem"),
        }
        .emit();

        self.enqueue_collateral_reward(&redeemer, &collateral_id, collateral_out);
        // Rewards must be claimed explicitly to receive the collateral tokens.
        Promise::new(env::current_account_id())
    }

    #[payable]
    pub fn liquidate(&mut self, collateral_id: AccountId, owners: Vec<AccountId>) -> U64 {
        assert_one_yocto();
        require!(!owners.is_empty(), "Owners required");
        let price = self.expect_price_internal(&collateral_id);
        let config = self.expect_config(&collateral_id);
        let mut processed = 0u64;
        for owner in owners {
            let key = Self::trove_key(&owner, &collateral_id);
            let trove = match self.troves.get(&key) {
                Some(trove) => trove,
                None => continue,
            };
            if trove.debt_amount == 0 {
                continue;
            }
            let ratio = self.collateral_ratio(trove.collateral_amount, trove.debt_amount, &price);
            if ratio >= config.min_collateral_ratio_bps as u128 {
                continue;
            }
            require!(
                self.stability_pool_total_nusd >= trove.debt_amount,
                "Insufficient stability pool funds"
            );
            let penalty = trove
                .collateral_amount
                .checked_mul(config.liquidation_penalty_bps as u128)
                .expect("Penalty overflow")
                / BPS_DENOMINATOR;
            let distributable = trove
                .collateral_amount
                .checked_sub(penalty)
                .expect("Distributable underflow");
            self.accrue_reward_per_share(&collateral_id, distributable);
            let owner_id = self.owner_id.clone();
            self.enqueue_collateral_reward(&owner_id, &collateral_id, penalty);
            self.burn_from_stability_pool(trove.debt_amount);
            self.add_total_debt(&collateral_id, -(trove.debt_amount as i128));
            self.troves.remove(&key);
            processed += 1;
        }
        U64(processed)
    }

    #[payable]
    pub fn trigger_swap_via_intents(
        &mut self,
        input_token: AccountId,
        output_token: AccountId,
        amount_in: U128,
        min_out: U128,
        routing_hint: Option<String>,
    ) -> Promise {
        self.assert_owner();
        let attached = env::attached_deposit();
        require!(
            attached > NearToken::from_yoctonear(0),
            "Attach deposit for Intents execution"
        );
        require!(amount_in.0 > 0, "Amount must be > 0");
        let caller = env::predecessor_account_id();
        ext_intents::ext(self.intent_router_id.clone())
            .with_attached_deposit(attached)
            .with_static_gas(GAS_FOR_SWAP)
            .execute_swap(
                caller.clone(),
                input_token.clone(),
                output_token,
                amount_in,
                min_out,
                routing_hint,
            )
            .then(
                ext_self::ext(env::current_account_id())
                    .with_static_gas(GAS_FOR_CALLBACK)
                    .on_swap_complete(caller, input_token, amount_in),
            )
    }

    #[private]
    pub fn on_swap_complete(
        &mut self,
        caller_id: AccountId,
        input_token: AccountId,
        amount_in: U128,
    ) -> bool {
        match env::promise_result(0) {
            PromiseResult::Successful(_) => {
                log!(
                    "NEAR Intents swap succeeded: caller={}, token={}, amount={}",
                    caller_id,
                    input_token,
                    amount_in.0
                );
                true
            }
            _ => {
                log!(
                    "NEAR Intents swap failed: caller={}, token={}, amount={}",
                    caller_id,
                    input_token,
                    amount_in.0
                );
                false
            }
        }
    }

    fn internal_repay(&mut self, owner_id: &AccountId, collateral_id: &AccountId, amount: Balance) {
        let mut trove = self.expect_trove(owner_id, collateral_id);
        require!(amount <= trove.debt_amount, "Repay exceeds debt");
        trove.debt_amount -= amount;
        trove.last_update_timestamp = Self::now_ms();
        self.save_trove(owner_id, collateral_id, &trove);
        self.add_total_debt(collateral_id, -(amount as i128));
    }

    fn internal_deposit_collateral(
        &mut self,
        owner_id: AccountId,
        collateral_id: AccountId,
        amount: Balance,
    ) {
        require!(amount > 0, "Amount must be > 0");
        self.expect_config(&collateral_id);
        let key = Self::trove_key(&owner_id, &collateral_id);
        let mut trove = self.troves.get(&key).unwrap_or(TroveInternal {
            owner_id: owner_id.clone(),
            collateral_id: collateral_id.clone(),
            collateral_amount: 0,
            debt_amount: 0,
            last_update_timestamp: Self::now_ms(),
        });
        trove.collateral_amount = trove
            .collateral_amount
            .checked_add(amount)
            .expect("Collateral overflow");
        trove.last_update_timestamp = Self::now_ms();
        self.troves.insert(&key, &trove);
    }

    fn settle_stability_rewards(&mut self, account_id: &AccountId) {
        let mut deposit = self
            .stability_pool_deposits
            .get(account_id)
            .unwrap_or_else(|| StabilityDeposit::new(self.stability_pool_epoch));
        self.ensure_deposit_epoch(account_id, &mut deposit);
        if deposit.shares == 0 || self.stability_pool_total_shares == 0 {
            self.stability_pool_deposits.insert(account_id, &deposit);
            return;
        }
        let keys = self.reward_per_share_keys();
        let mut updated = false;
        for collateral_id in keys {
            let global = self.reward_per_share.get(&collateral_id).unwrap_or(0);
            let paid = deposit
                .reward_debt
                .get(&collateral_id)
                .copied()
                .unwrap_or(0);
            if global > paid {
                let delta = global - paid;
                let pending = deposit
                    .shares
                    .checked_mul(delta)
                    .expect("Reward mul overflow")
                    / REWARD_SCALE;
                if pending > 0 {
                    self.enqueue_collateral_reward(account_id, &collateral_id, pending);
                }
            }
            deposit.reward_debt.insert(collateral_id.clone(), global);
            updated = true;
        }
        if updated {
            self.stability_pool_deposits.insert(account_id, &deposit);
        }
    }

    fn ensure_deposit_epoch(&mut self, account_id: &AccountId, deposit: &mut StabilityDeposit) {
        if deposit.epoch == self.stability_pool_epoch {
            return;
        }
        if deposit.shares > 0 {
            let keys = self.reward_per_share_keys();
            for collateral_id in keys {
                let global = self.reward_per_share.get(&collateral_id).unwrap_or(0);
                let paid = deposit
                    .reward_debt
                    .get(&collateral_id)
                    .copied()
                    .unwrap_or(0);
                if global > paid {
                    let pending = deposit
                        .shares
                        .checked_mul(global - paid)
                        .expect("Epoch reward overflow")
                        / REWARD_SCALE;
                    if pending > 0 {
                        self.enqueue_collateral_reward(account_id, &collateral_id, pending);
                    }
                }
            }
        }
        deposit.reward_debt.clear();
        deposit.shares = 0;
        deposit.epoch = self.stability_pool_epoch;
    }

    fn shares_from_amount(&self, amount: Balance) -> Balance {
        if self.stability_pool_total_shares == 0 || self.stability_pool_total_nusd == 0 {
            amount
        } else {
            amount
                .checked_mul(self.stability_pool_total_shares)
                .expect("Share calc overflow")
                / self.stability_pool_total_nusd
        }
    }

    fn shares_for_withdraw(&self, amount: Balance) -> Balance {
        self.shares_from_amount(amount)
    }

    fn reward_per_share_keys(&self) -> Vec<AccountId> {
        let keys = self.reward_per_share.keys_as_vector();
        let mut collaterals = Vec::with_capacity(keys.len() as usize);
        for idx in 0..keys.len() {
            collaterals.push(keys.get(idx).unwrap());
        }
        collaterals
    }

    fn enqueue_collateral_reward(
        &mut self,
        account_id: &AccountId,
        collateral_id: &AccountId,
        amount: Balance,
    ) {
        if amount == 0 {
            return;
        }
        let key = CollateralRewardKey::new(account_id, collateral_id);
        let mut current = self.collateral_rewards.get(&key).unwrap_or(0);
        current = current.checked_add(amount).expect("Reward overflow");
        self.collateral_rewards.insert(&key, &current);
    }

    fn claim_collateral(
        &mut self,
        account_id: &AccountId,
        collateral_id: &AccountId,
        amount: Option<Balance>,
    ) -> Promise {
        let key = CollateralRewardKey::new(account_id, collateral_id);
        let mut claimable = self.collateral_rewards.get(&key).unwrap_or(0);
        require!(claimable > 0, "Nothing to claim");
        let to_claim = amount.unwrap_or(claimable);
        require!(to_claim > 0, "Amount must be > 0");
        require!(to_claim <= claimable, "Amount exceeds claimable");
        claimable -= to_claim;
        if claimable == 0 {
            self.collateral_rewards.remove(&key);
        } else {
            self.collateral_rewards.insert(&key, &claimable);
        }
        self.send_collateral(account_id.clone(), collateral_id.clone(), to_claim)
    }

    fn accrue_reward_per_share(&mut self, collateral_id: &AccountId, reward_amount: Balance) {
        if reward_amount == 0 {
            return;
        }
        if self.stability_pool_total_shares == 0 {
            let owner_id = self.owner_id.clone();
            self.enqueue_collateral_reward(&owner_id, collateral_id, reward_amount);
            return;
        }
        let mut accrued = self.reward_per_share.get(collateral_id).unwrap_or(0);
        accrued = accrued
            .checked_add(
                reward_amount
                    .checked_mul(REWARD_SCALE)
                    .expect("Reward scaling overflow")
                    / self.stability_pool_total_shares,
            )
            .expect("Reward per share overflow");
        self.reward_per_share.insert(collateral_id, &accrued);
    }

    fn sync_reward_debt_snapshot(&self, deposit: &mut StabilityDeposit) {
        for collateral_id in self.reward_per_share_keys() {
            let global = self.reward_per_share.get(&collateral_id).unwrap_or(0);
            deposit.reward_debt.insert(collateral_id, global);
        }
    }

    fn burn_from_stability_pool(&mut self, amount: Balance) {
        require!(amount > 0, "Amount must be > 0");
        require!(
            self.stability_pool_total_nusd >= amount,
            "Insufficient stability pool balance"
        );
        self.stability_pool_total_nusd -= amount;
        self.nusd
            .internal_withdraw(&env::current_account_id(), amount);
        FtBurn {
            owner_id: &env::current_account_id(),
            amount: U128(amount),
            memo: Some("cdp_liquidation"),
        }
        .emit();
        if self.stability_pool_total_nusd == 0 {
            self.stability_pool_total_shares = 0;
            self.stability_pool_epoch = self.stability_pool_epoch.saturating_add(1);
        }
    }

    fn send_collateral(
        &self,
        receiver_id: AccountId,
        token_id: AccountId,
        amount: Balance,
    ) -> Promise {
        require!(amount > 0, "Nothing to transfer");
        ext_ft::ext(token_id)
            .with_attached_deposit(NearToken::from_yoctonear(1))
            .with_static_gas(GAS_FOR_FT_TRANSFER)
            .ft_transfer(
                receiver_id,
                U128(amount),
                Some("cdp_collateral_withdrawal".to_string()),
            )
    }

    fn expect_config(&self, collateral_id: &AccountId) -> CollateralConfigInternal {
        self.configs
            .get(collateral_id)
            .unwrap_or_else(|| env::panic_str("Collateral not supported"))
    }

    fn expect_price_internal(&self, collateral_id: &AccountId) -> PriceFeedInternal {
        self.price_feeds
            .get(collateral_id)
            .unwrap_or_else(|| env::panic_str("Price not available"))
    }

    fn expect_trove(&self, owner_id: &AccountId, collateral_id: &AccountId) -> TroveInternal {
        self.troves
            .get(&Self::trove_key(owner_id, collateral_id))
            .unwrap_or_else(|| env::panic_str("Trove not found"))
    }

    fn save_trove(
        &mut self,
        owner_id: &AccountId,
        collateral_id: &AccountId,
        trove: &TroveInternal,
    ) {
        self.troves
            .insert(&Self::trove_key(owner_id, collateral_id), trove);
    }

    fn add_total_debt(&mut self, collateral_id: &AccountId, delta: i128) {
        let mut total = self.total_debt.get(collateral_id).unwrap_or(0);
        if delta >= 0 {
            let increased = total
                .checked_add(delta as u128)
                .expect("Total debt overflow");
            self.ensure_debt_ceiling(collateral_id, increased);
            total = increased;
        } else {
            let reduction = (-delta) as u128;
            require!(total >= reduction, "Debt underflow");
            total -= reduction;
        }
        if total == 0 {
            self.total_debt.remove(collateral_id);
        } else {
            self.total_debt.insert(collateral_id, &total);
        }
    }

    fn ensure_debt_ceiling(&self, collateral_id: &AccountId, new_total: Balance) {
        let config = self.expect_config(collateral_id);
        require!(
            new_total <= config.debt_ceiling,
            "Collateral debt ceiling reached"
        );
    }

    fn collateral_ratio(
        &self,
        collateral: Balance,
        debt: Balance,
        price: &PriceFeedInternal,
    ) -> u128 {
        if debt == 0 {
            return u128::MAX;
        }
        let price_value = price.price;
        let divisor = Self::decimals_factor(price.decimals);
        let value = collateral
            .checked_mul(price_value)
            .expect("Collateral value overflow")
            / divisor;
        value.checked_mul(BPS_DENOMINATOR).expect("Ratio overflow") / debt
    }

    fn decimals_factor(decimals: u8) -> u128 {
        10u128.pow(decimals as u32)
    }

    fn trove_key(owner_id: &AccountId, collateral_id: &AccountId) -> TroveKey {
        TroveKey {
            owner_id: owner_id.clone(),
            collateral_id: collateral_id.clone(),
        }
    }

    fn parse_transfer_action(msg: &str) -> TransferAction {
        if msg.trim().is_empty() {
            TransferAction::DepositCollateral {
                target_account: None,
            }
        } else {
            serde_json::from_str(msg).unwrap_or_else(|_| env::panic_str("Invalid transfer msg"))
        }
    }

    fn now_ms() -> u64 {
        env::block_timestamp() / 1_000_000
    }

    fn assert_owner(&self) {
        require!(env::predecessor_account_id() == self.owner_id, "Owner only");
    }
}

#[near_bindgen]
impl FungibleTokenCore for Contract {
    #[payable]
    fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>) {
        self.nusd.ft_transfer(receiver_id, amount, memo)
    }

    #[payable]
    fn ft_transfer_call(
        &mut self,
        receiver_id: AccountId,
        amount: U128,
        memo: Option<String>,
        msg: String,
    ) -> PromiseOrValue<U128> {
        self.nusd.ft_transfer_call(receiver_id, amount, memo, msg)
    }

    fn ft_total_supply(&self) -> U128 {
        self.nusd.ft_total_supply()
    }

    fn ft_balance_of(&self, account_id: AccountId) -> U128 {
        self.nusd.ft_balance_of(account_id)
    }
}

#[near_bindgen]
impl FungibleTokenResolver for Contract {
    #[private]
    fn ft_resolve_transfer(
        &mut self,
        sender_id: AccountId,
        receiver_id: AccountId,
        amount: U128,
    ) -> U128 {
        let (used_amount, _) =
            self.nusd
                .internal_ft_resolve_transfer(&sender_id, receiver_id, amount);
        used_amount.into()
    }
}

#[near_bindgen]
impl StorageManagement for Contract {
    #[payable]
    fn storage_deposit(
        &mut self,
        account_id: Option<AccountId>,
        registration_only: Option<bool>,
    ) -> StorageBalance {
        self.nusd.storage_deposit(account_id, registration_only)
    }

    #[payable]
    fn storage_withdraw(&mut self, amount: Option<NearToken>) -> StorageBalance {
        self.nusd.storage_withdraw(amount)
    }

    #[payable]
    fn storage_unregister(&mut self, force: Option<bool>) -> bool {
        self.nusd.storage_unregister(force)
    }

    fn storage_balance_bounds(&self) -> StorageBalanceBounds {
        self.nusd.storage_balance_bounds()
    }

    fn storage_balance_of(&self, account_id: AccountId) -> Option<StorageBalance> {
        self.nusd.storage_balance_of(account_id)
    }
}

#[near_bindgen]
impl FungibleTokenMetadataProvider for Contract {
    fn ft_metadata(&self) -> FungibleTokenMetadata {
        self.metadata
            .get()
            .clone()
            .unwrap_or(FungibleTokenMetadata {
                spec: FT_METADATA_SPEC.to_string(),
                name: "nUSD".to_string(),
                symbol: "nUSD".to_string(),
                icon: None,
                reference: None,
                reference_hash: None,
                decimals: 24,
            })
    }
}

#[near_bindgen]
impl FungibleTokenReceiver for Contract {
    fn ft_on_transfer(
        &mut self,
        sender_id: AccountId,
        amount: U128,
        msg: String,
    ) -> PromiseOrValue<U128> {
        let token_id = env::predecessor_account_id();
        let action = Self::parse_transfer_action(&msg);

        if token_id == env::current_account_id() {
            match action {
                TransferAction::RepayDebt { collateral_id } => {
                    self.nusd
                        .internal_withdraw(&env::current_account_id(), amount.0);
                    FtBurn {
                        owner_id: &sender_id,
                        amount,
                        memo: Some("cdp_repay_via_ft"),
                    }
                    .emit();
                    self.internal_repay(&sender_id, &collateral_id, amount.0);
                }
                _ => env::panic_str("Unsupported action for nUSD"),
            }
        } else {
            match action {
                TransferAction::DepositCollateral { target_account } => {
                    let owner = target_account.unwrap_or_else(|| sender_id.clone());
                    self.internal_deposit_collateral(owner, token_id, amount.0);
                }
                TransferAction::RepayDebt { .. } => {
                    env::panic_str("Repay action invalid for external tokens")
                }
            }
        }
        PromiseOrValue::Value(U128(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use near_sdk::test_utils::VMContextBuilder;
    use near_sdk::{testing_env, NearToken};

    fn metadata() -> FungibleTokenMetadata {
        FungibleTokenMetadata {
            spec: FT_METADATA_SPEC.to_string(),
            name: "nUSD".to_string(),
            symbol: "nUSD".to_string(),
            icon: None,
            reference: None,
            reference_hash: None,
            decimals: 24,
        }
    }

    fn alice() -> AccountId {
        "alice.testnet".parse().unwrap()
    }

    fn owner() -> AccountId {
        "owner.testnet".parse().unwrap()
    }

    fn intents() -> AccountId {
        "intents.near".parse().unwrap()
    }

    fn oracle() -> AccountId {
        "pyth.near".parse().unwrap()
    }

    fn collateral_token() -> AccountId {
        "usdc.fakes".parse().unwrap()
    }

    fn setup_contract() -> Contract {
        let mut context = VMContextBuilder::new();
        context
            .current_account_id("cdp.testnet".parse().unwrap())
            .signer_account_id(owner())
            .predecessor_account_id(owner());
        testing_env!(context.clone().build());
        let mut contract = Contract::new(owner(), intents(), oracle(), metadata());

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        contract.register_collateral(
            collateral_token(),
            CollateralConfig {
                oracle_price_id: "usdc".to_string(),
                min_collateral_ratio_bps: 1300,
                recovery_collateral_ratio_bps: 1500,
                debt_ceiling: U128(1_000_000_000_000),
                liquidation_penalty_bps: 50,
                stability_pool_mode: StabilityPoolMode::Dedicated,
            },
        );

        testing_env!(context
            .predecessor_account_id(oracle())
            .attached_deposit(NearToken::from_yoctonear(0))
            .build());
        contract.submit_price(collateral_token(), U128(20000), 2);

        contract
    }

    #[test]
    fn borrow_and_repay_flow() {
        let mut contract = setup_contract();
        let mut context = VMContextBuilder::new();
        context
            .current_account_id("cdp.testnet".parse().unwrap())
            .signer_account_id(alice())
            .predecessor_account_id(alice());
        let storage_deposit = contract.storage_balance_bounds().min;
        testing_env!(context.clone().attached_deposit(storage_deposit).build());
        contract.storage_deposit(Some(alice()), None);

        testing_env!(context
            .predecessor_account_id(collateral_token())
            .signer_account_id(collateral_token())
            .attached_deposit(NearToken::from_yoctonear(0))
            .build());
        contract.ft_on_transfer(
            alice(),
            U128(10_000),
            r#"{"action":"deposit_collateral"}"#.to_string(),
        );

        testing_env!(context
            .predecessor_account_id(alice())
            .signer_account_id(alice())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        contract.borrow(collateral_token(), U128(4_000));
        assert_eq!(contract.ft_balance_of(alice()).0, 4_000);

        testing_env!(context
            .predecessor_account_id(alice())
            .signer_account_id(alice())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        contract.repay(collateral_token(), U128(1_000));
        assert_eq!(contract.ft_balance_of(alice()).0, 3_000);
        let trove = contract
            .get_trove(alice(), collateral_token())
            .expect("trove missing");
        assert_eq!(trove.debt_amount.0, 3_000);

        testing_env!(context
            .predecessor_account_id(alice())
            .signer_account_id(alice())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        let _ = contract.withdraw_collateral(collateral_token(), U128(1_000), None);
    }
}
