use crate::types::{CollateralConfig, CollateralRewardKey, PriceFeed, Trove, REWARD_SCALE};
use crate::{Contract, ContractExt};
use near_sdk::json_types::U128;
use near_sdk::{near_bindgen, AccountId};

#[near_bindgen]
impl Contract {
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
}
