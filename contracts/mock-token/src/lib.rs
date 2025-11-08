use near_contract_standards::fungible_token::core::FungibleTokenCore;
use near_contract_standards::fungible_token::metadata::{
    FungibleTokenMetadata, FungibleTokenMetadataProvider, FT_METADATA_SPEC,
};
use near_contract_standards::fungible_token::receiver::FungibleTokenReceiver;
use near_contract_standards::fungible_token::resolver::FungibleTokenResolver;
use near_contract_standards::fungible_token::FungibleToken;
use near_contract_standards::storage_management::{
    StorageBalance, StorageBalanceBounds, StorageManagement,
};
use near_sdk::borsh::{BorshDeserialize, BorshSerialize};
use near_sdk::store::LazyOption;
use near_sdk::{
    assert_one_yocto, env, near_bindgen, AccountId, NearToken, PanicOnDefault, PromiseOrValue,
};
use near_sdk::{json_types::U128, require};

#[near_bindgen]
#[derive(BorshDeserialize, BorshSerialize, PanicOnDefault)]
pub struct MockToken {
    owner_id: AccountId,
    token: FungibleToken,
    metadata: LazyOption<FungibleTokenMetadata>,
}

#[near_bindgen]
impl MockToken {
    #[init]
    pub fn new(owner_id: AccountId, metadata: FungibleTokenMetadata) -> Self {
        assert!(!env::state_exists(), "Already initialized");
        let mut token = FungibleToken::new(b"t".to_vec());
        token.internal_register_account(&owner_id);
        Self {
            owner_id,
            token,
            metadata: LazyOption::new(b"m".to_vec(), Some(metadata)),
        }
    }

    #[payable]
    pub fn mint(&mut self, account_id: AccountId, amount: U128) {
        assert_one_yocto();
        self.assert_owner();
        if !self.token.accounts.contains_key(&account_id) {
            self.token.internal_register_account(&account_id);
        }
        self.token.internal_deposit(&account_id, amount.0);
    }

    fn assert_owner(&self) {
        require!(env::predecessor_account_id() == self.owner_id, "Owner only");
    }
}

#[near_bindgen]
impl FungibleTokenCore for MockToken {
    #[payable]
    fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>) {
        self.token.ft_transfer(receiver_id, amount, memo)
    }

    #[payable]
    fn ft_transfer_call(
        &mut self,
        receiver_id: AccountId,
        amount: U128,
        memo: Option<String>,
        msg: String,
    ) -> PromiseOrValue<U128> {
        self.token.ft_transfer_call(receiver_id, amount, memo, msg)
    }

    fn ft_total_supply(&self) -> U128 {
        self.token.ft_total_supply()
    }

    fn ft_balance_of(&self, account_id: AccountId) -> U128 {
        self.token.ft_balance_of(account_id)
    }
}

#[near_bindgen]
impl FungibleTokenResolver for MockToken {
    #[private]
    fn ft_resolve_transfer(
        &mut self,
        sender_id: AccountId,
        receiver_id: AccountId,
        amount: U128,
    ) -> U128 {
        let (unused_amount, _) =
            self.token
                .internal_ft_resolve_transfer(&sender_id, receiver_id, amount);
        unused_amount.into()
    }
}

#[near_bindgen]
impl FungibleTokenReceiver for MockToken {
    fn ft_on_transfer(
        &mut self,
        _sender_id: AccountId,
        _amount: U128,
        _msg: String,
    ) -> PromiseOrValue<U128> {
        PromiseOrValue::Value(U128(0))
    }
}

#[near_bindgen]
impl StorageManagement for MockToken {
    #[payable]
    fn storage_deposit(
        &mut self,
        account_id: Option<AccountId>,
        registration_only: Option<bool>,
    ) -> StorageBalance {
        self.token.storage_deposit(account_id, registration_only)
    }

    #[payable]
    fn storage_withdraw(&mut self, amount: Option<NearToken>) -> StorageBalance {
        self.token.storage_withdraw(amount)
    }

    #[payable]
    fn storage_unregister(&mut self, force: Option<bool>) -> bool {
        self.token.storage_unregister(force)
    }

    fn storage_balance_bounds(&self) -> StorageBalanceBounds {
        self.token.storage_balance_bounds()
    }

    fn storage_balance_of(&self, account_id: AccountId) -> Option<StorageBalance> {
        self.token.storage_balance_of(account_id)
    }
}

#[near_bindgen]
impl FungibleTokenMetadataProvider for MockToken {
    fn ft_metadata(&self) -> FungibleTokenMetadata {
        match self.metadata.get() {
            Some(metadata) => metadata.clone(),
            None => FungibleTokenMetadata {
                spec: FT_METADATA_SPEC.to_string(),
                name: "Mock Token".to_string(),
                symbol: "MOCK".to_string(),
                icon: None,
                reference: None,
                reference_hash: None,
                decimals: 24,
            },
        }
    }
}
