mod types;
use crate::types::{
    CollateralConfig, CollateralConfigInternal, PriceFeedInternal, StorageKey, TokenId,
    TransferAction, TroveInternal, TroveKey, GAS_FOR_CALLBACK, GAS_FOR_SWAP,
};

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
use near_sdk::collections::{LookupMap, UnorderedMap};
use near_sdk::json_types::{U128, U64};
use near_sdk::store::LazyOption;
use near_sdk::{
    assert_one_yocto, env, ext_contract, log, near, near_bindgen, require, AccountId, NearToken,
    PanicOnDefault, Promise, PromiseOrValue, PromiseResult,
};

mod internal;
mod views;

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

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    owner_id: AccountId,
    intent_router_id: AccountId,
    pyth_oracle_id: AccountId,
    configs: UnorderedMap<TokenId, CollateralConfigInternal>,
    troves: LookupMap<TroveKey, TroveInternal>,
    total_debt: LookupMap<TokenId, Balance>,
    price_feeds: LookupMap<TokenId, PriceFeedInternal>,
    stability_pool_deposits: LookupMap<AccountId, types::StabilityDeposit>,
    collateral_rewards: LookupMap<types::CollateralRewardKey, Balance>,
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
            .unwrap_or_else(|| types::StabilityDeposit::new(self.stability_pool_epoch));
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
            .unwrap_or_else(|| types::StabilityDeposit::new(self.stability_pool_epoch));
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
                / crate::types::BPS_DENOMINATOR;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::StabilityPoolMode;
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

    #[test]
    fn new_deposit_snapshot_prevents_reward_sniping() {
        let mut contract = setup_contract();
        let collateral = collateral_token();
        let alice = alice();

        contract
            .reward_per_share
            .insert(&collateral, &types::REWARD_SCALE);
        contract.stability_pool_total_shares = 1_000;
        contract.stability_pool_total_nusd = 1_000;

        let mut deposit = types::StabilityDeposit::new(contract.stability_pool_epoch);
        deposit.shares = 1_000;
        contract.sync_reward_debt_snapshot(&mut deposit);
        contract.stability_pool_deposits.insert(&alice, &deposit);

        contract.settle_stability_rewards(&alice);

        let reward_after = contract
            .collateral_rewards
            .get(&types::CollateralRewardKey::new(&alice, &collateral))
            .unwrap_or(0);
        assert_eq!(
            reward_after, 0,
            "new deposit should not inherit historical rewards"
        );
    }

    #[test]
    fn accrue_without_deposit_rewards_owner() {
        let mut contract = setup_contract();
        let collateral = collateral_token();

        contract.accrue_reward_per_share(&collateral, 500);

        let owner_reward = contract
            .collateral_rewards
            .get(&types::CollateralRewardKey::new(
                &contract.owner_id,
                &collateral,
            ))
            .unwrap_or(0);
        assert_eq!(owner_reward, 500, "owner should receive direct reward");
    }
}
