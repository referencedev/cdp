use crate::types::{
    CollateralConfigInternal, CollateralRewardKey, PriceFeedInternal, StabilityDeposit,
    TransferAction, TroveInternal, TroveKey, BPS_DENOMINATOR, GAS_FOR_FT_TRANSFER, REWARD_SCALE,
};
use crate::{ext_ft, Contract};
use near_contract_standards::fungible_token::events::FtBurn;
use near_contract_standards::fungible_token::Balance;
use near_sdk::json_types::U128;
use near_sdk::serde_json;
use near_sdk::{env, require, AccountId, NearToken, Promise};

impl Contract {
    pub(crate) fn settle_stability_rewards(&mut self, account_id: &AccountId) {
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

    pub(crate) fn ensure_deposit_epoch(
        &mut self,
        account_id: &AccountId,
        deposit: &mut StabilityDeposit,
    ) {
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

    pub(crate) fn shares_from_amount(&self, amount: Balance) -> Balance {
        if self.stability_pool_total_shares == 0 || self.stability_pool_total_nusd == 0 {
            amount
        } else {
            amount
                .checked_mul(self.stability_pool_total_shares)
                .expect("Share calc overflow")
                / self.stability_pool_total_nusd
        }
    }

    pub(crate) fn shares_for_withdraw(&self, amount: Balance) -> Balance {
        self.shares_from_amount(amount)
    }

    pub(crate) fn reward_per_share_keys(&self) -> Vec<AccountId> {
        let keys = self.reward_per_share.keys_as_vector();
        let mut collaterals = Vec::with_capacity(keys.len() as usize);
        for idx in 0..keys.len() {
            collaterals.push(keys.get(idx).unwrap());
        }
        collaterals
    }

    pub(crate) fn enqueue_collateral_reward(
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

    pub(crate) fn claim_collateral(
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

    pub(crate) fn accrue_reward_per_share(
        &mut self,
        collateral_id: &AccountId,
        reward_amount: Balance,
    ) {
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

    pub(crate) fn burn_from_stability_pool(&mut self, amount: Balance) {
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

    pub(crate) fn sync_reward_debt_snapshot(&self, deposit: &mut StabilityDeposit) {
        for collateral_id in self.reward_per_share_keys() {
            let global = self.reward_per_share.get(&collateral_id).unwrap_or(0);
            deposit.reward_debt.insert(collateral_id, global);
        }
    }
    pub(crate) fn internal_deposit_collateral(
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

    pub(crate) fn send_collateral(
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

    pub(crate) fn expect_config(&self, collateral_id: &AccountId) -> CollateralConfigInternal {
        self.configs
            .get(collateral_id)
            .unwrap_or_else(|| env::panic_str("Collateral not supported"))
    }

    pub(crate) fn expect_price_internal(&self, collateral_id: &AccountId) -> PriceFeedInternal {
        self.price_feeds
            .get(collateral_id)
            .unwrap_or_else(|| env::panic_str("Price not available"))
    }

    pub(crate) fn expect_trove(
        &self,
        owner_id: &AccountId,
        collateral_id: &AccountId,
    ) -> TroveInternal {
        self.troves
            .get(&Self::trove_key(owner_id, collateral_id))
            .unwrap_or_else(|| env::panic_str("Trove not found"))
    }

    pub(crate) fn save_trove(
        &mut self,
        owner_id: &AccountId,
        collateral_id: &AccountId,
        trove: &TroveInternal,
    ) {
        self.troves
            .insert(&Self::trove_key(owner_id, collateral_id), trove);
    }

    pub(crate) fn add_total_debt(&mut self, collateral_id: &AccountId, delta: i128) {
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

    pub(crate) fn ensure_debt_ceiling(&self, collateral_id: &AccountId, new_total: Balance) {
        let config = self.expect_config(collateral_id);
        require!(
            new_total <= config.debt_ceiling,
            "Collateral debt ceiling reached"
        );
    }

    pub(crate) fn collateral_ratio(
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

    pub(crate) fn decimals_factor(decimals: u8) -> u128 {
        10u128.pow(decimals as u32)
    }

    pub(crate) fn trove_key(owner_id: &AccountId, collateral_id: &AccountId) -> TroveKey {
        TroveKey {
            owner_id: owner_id.clone(),
            collateral_id: collateral_id.clone(),
        }
    }

    pub(crate) fn parse_transfer_action(msg: &str) -> TransferAction {
        if msg.trim().is_empty() {
            TransferAction::DepositCollateral {
                target_account: None,
            }
        } else {
            serde_json::from_str(msg).unwrap_or_else(|_| env::panic_str("Invalid transfer msg"))
        }
    }

    pub(crate) fn now_ms() -> u64 {
        env::block_timestamp() / 1_000_000
    }

    pub(crate) fn assert_owner(&self) {
        require!(env::predecessor_account_id() == self.owner_id, "Owner only");
    }
}
