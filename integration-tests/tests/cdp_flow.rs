use anyhow::{ensure, Context, Result};
use near_token::NearToken;
use near_workspaces::{network::Sandbox, sandbox, Account, Contract, Worker};
use serde_json::{json, Value};
use serial_test::serial;
use std::{
    path::{Path, PathBuf},
    process::Command,
};
use tokio::fs;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn contract_project_dir() -> PathBuf {
    workspace_root().join("contracts").join("cdp")
}

fn wasm_artifact_path() -> PathBuf {
    workspace_root()
        .join("target")
        .join("near")
        .join("cdp")
        .join("cdp.wasm")
}

fn mock_token_wasm_path() -> PathBuf {
    workspace_root()
        .join("target")
        .join("near")
        .join("mock_token")
        .join("mock_token.wasm")
}

fn build_contract_wasm() -> Result<()> {
    let status = Command::new("cargo")
        .args(["near", "build", "non-reproducible-wasm"])
        .current_dir(contract_project_dir())
        .status()
        .context("failed to run `cargo near build`")?;
    ensure!(
        status.success(),
        "`cargo near build` exited with non-zero status"
    );
    Ok(())
}

fn build_mock_token_wasm() -> Result<()> {
    let status = Command::new("cargo")
        .args(["near", "build", "non-reproducible-wasm"])
        .current_dir(workspace_root().join("contracts").join("mock-token"))
        .status()
        .context("failed to run `cargo near build` for mock token")?;
    ensure!(status.success(), "`cargo build -p mock-token` failed");
    Ok(())
}

async fn load_contract_wasm() -> Result<Vec<u8>> {
    if !wasm_artifact_path().exists() {
        build_contract_wasm()?;
    }
    fs::read(wasm_artifact_path())
        .await
        .context("unable to read compiled CDP wasm")
}

async fn load_mock_token_wasm() -> Result<Vec<u8>> {
    if !mock_token_wasm_path().exists() {
        build_mock_token_wasm()?;
    }
    fs::read(mock_token_wasm_path())
        .await
        .context("unable to read compiled mock token wasm")
}

struct TestEnv {
    #[allow(dead_code)]
    worker: Worker<Sandbox>,
    contract: Contract,
    owner: Account,
    oracle: Account,
    collateral_token: Contract,
    borrower: Account,
}

async fn setup_borrow_env() -> Result<TestEnv> {
    let worker = sandbox().await?;
    let wasm = load_contract_wasm().await?;
    let contract = worker.dev_deploy(&wasm).await?;

    let owner = worker.dev_create_account().await?;
    let oracle = worker.dev_create_account().await?;
    let borrower = worker.dev_create_account().await?;
    let collateral_wasm = load_mock_token_wasm().await?;
    let collateral_token = worker.dev_deploy(&collateral_wasm).await?;

    collateral_token
        .call("new")
        .args_json(json!({
            "owner_id": owner.id(),
            "metadata": {
                "spec": "ft-1.0.0",
                "name": "Mock USDC",
                "symbol": "mUSDC",
                "icon": null,
                "reference": null,
                "reference_hash": null,
                "decimals": 24
            }
        }))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    contract
        .call("new")
        .args_json(json!({
            "owner_id": owner.id(),
            "intent_router_id": owner.id(),
            "pyth_oracle_id": oracle.id(),
            "metadata": {
                "spec": "ft-1.0.0",
                "name": "nUSD",
                "symbol": "nUSD",
                "icon": null,
                "reference": null,
                "reference_hash": null,
                "decimals": 24
            }
        }))
        .transact()
        .await?
        .into_result()?;

    owner
        .call(contract.id(), "register_collateral")
        .args_json(json!({
            "token_id": collateral_token.id(),
            "config": {
                "oracle_price_id": "usdc",
                "min_collateral_ratio_bps": 1300,
                "recovery_collateral_ratio_bps": 1500,
                "debt_ceiling": "1000000000000",
                "liquidation_penalty_bps": 50,
                "stability_pool_mode": "Dedicated"
            }
        }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    ensure_token_storage(&collateral_token, &owner).await?;
    ensure_token_storage(&collateral_token, contract.as_account()).await?;

    oracle
        .call(contract.id(), "submit_price")
        .args_json(json!({
            "collateral_id": collateral_token.id(),
            "price": "20000",
            "decimals": 2
        }))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    let env = TestEnv {
        worker,
        contract,
        owner,
        oracle,
        collateral_token,
        borrower,
    };

    open_trove_for(&env, &env.borrower, "10000", "4000").await?;
    Ok(env)
}

#[tokio::test]
#[serial]
async fn borrow_flow_smoke_test() -> Result<()> {
    let env = setup_borrow_env().await?;

    let balance: String = env
        .contract
        .view("ft_balance_of")
        .args_json(json!({ "account_id": env.borrower.id() }))
        .await?
        .json()?;

    assert_eq!(balance, "4000", "expected nUSD minted to borrower");

    let trove: Value = env
        .contract
        .view("get_trove")
        .args_json(json!({
            "owner_id": env.borrower.id(),
            "collateral_id": env.collateral_token.id()
        }))
        .await?
        .json()?;

    assert!(trove != Value::Null, "trove should exist after borrowing");
    assert_eq!(
        trove
            .get("debt_amount")
            .and_then(|v| v.as_str())
            .unwrap_or_default(),
        "4000"
    );

    Ok(())
}

#[tokio::test]
#[serial]
async fn liquidation_guard_prevents_withdraw_after_price_drop() -> Result<()> {
    let env = setup_borrow_env().await?;

    env.oracle
        .call(env.contract.id(), "submit_price")
        .args_json(json!({
            "collateral_id": env.collateral_token.id(),
            // "5" with 2 decimals => price of 0.05, enough to breach the MCR after withdrawal
            "price": "5",
            "decimals": 2
        }))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    let attempt = env
        .borrower
        .call(env.contract.id(), "withdraw_collateral")
        .args_json(json!({
            "collateral_id": env.collateral_token.id(),
            "amount": "1000",
            "receiver": Option::<String>::None
        }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?;

    let err = attempt.into_result().expect_err("withdraw should fail");
    assert!(
        format!("{err:?}").contains("Would violate MCR"),
        "error should mention MCR breach"
    );

    Ok(())
}

#[tokio::test]
#[serial]
async fn stability_pool_liquidates_underwater_trove() -> Result<()> {
    let env = setup_borrow_env().await?;
    let liquidated = env.worker.dev_create_account().await?;

    open_trove_for(&env, &liquidated, "10000", "4000").await?;

    env.borrower
        .call(env.contract.id(), "deposit_to_stability_pool")
        .args_json(json!({ "amount": "4000" }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    env.oracle
        .call(env.contract.id(), "submit_price")
        .args_json(json!({
            "collateral_id": env.collateral_token.id(),
            // Drop collateral value to trigger liquidation
            "price": "5",
            "decimals": 2
        }))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    let liquidator = env.worker.dev_create_account().await?;
    liquidator
        .call(env.contract.id(), "liquidate")
        .args_json(json!({
            "collateral_id": env.collateral_token.id(),
            "owners": [liquidated.id()]
        }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    let trove: Value = env
        .contract
        .view("get_trove")
        .args_json(json!({
            "owner_id": liquidated.id(),
            "collateral_id": env.collateral_token.id()
        }))
        .await?
        .json()?;
    assert_eq!(
        trove,
        Value::Null,
        "trove should be removed after liquidation"
    );

    let pool_balance: String = env
        .contract
        .view("get_stability_pool_deposit")
        .args_json(json!({ "account_id": env.borrower.id() }))
        .await?
        .json()?;
    assert_eq!(pool_balance, "0", "depositor balance should be depleted");

    let depositor_reward: String = env
        .contract
        .view("get_claimable_collateral_reward")
        .args_json(json!({
            "account_id": env.borrower.id(),
            "collateral_id": env.collateral_token.id()
        }))
        .await?
        .json()?;
    assert_eq!(
        depositor_reward, "9950",
        "stability pool depositor should receive collateral minus penalty"
    );

    let owner_reward: String = env
        .contract
        .view("get_claimable_collateral_reward")
        .args_json(json!({
            "account_id": env.owner.id(),
            "collateral_id": env.collateral_token.id()
        }))
        .await?
        .json()?;
    assert_eq!(
        owner_reward, "50",
        "owner should receive liquidation penalty"
    );

    env.borrower
        .call(env.contract.id(), "claim_collateral_reward")
        .args_json(json!({
            "collateral_id": env.collateral_token.id(),
            "amount": Option::<String>::None
        }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    let borrower_collateral = ft_balance(&env.collateral_token, &env.borrower).await?;
    assert_eq!(
        borrower_collateral, "9950",
        "claim should transfer seized collateral to depositor"
    );

    env.owner
        .call(env.contract.id(), "claim_collateral_reward")
        .args_json(json!({
            "collateral_id": env.collateral_token.id(),
            "amount": Option::<String>::None
        }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    let owner_collateral = ft_balance(&env.collateral_token, &env.owner).await?;
    assert_eq!(
        owner_collateral, "50",
        "owner should receive penalty collateral"
    );

    Ok(())
}

#[tokio::test]
#[serial]
async fn stability_pool_new_deposit_does_not_get_past_rewards() -> Result<()> {
    let env = setup_borrow_env().await?;
    let liquidated = env.worker.dev_create_account().await?;
    let late_depositor = env.worker.dev_create_account().await?;

    open_trove_for(&env, &liquidated, "10000", "4000").await?;
    open_trove_for(&env, &late_depositor, "10000", "1000").await?;

    env.borrower
        .call(env.contract.id(), "deposit_to_stability_pool")
        .args_json(json!({ "amount": "4000" }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    env.oracle
        .call(env.contract.id(), "submit_price")
        .args_json(json!({
            "collateral_id": env.collateral_token.id(),
            "price": "5",
            "decimals": 2
        }))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    env.worker
        .dev_create_account()
        .await?
        .call(env.contract.id(), "liquidate")
        .args_json(json!({
            "collateral_id": env.collateral_token.id(),
            "owners": [liquidated.id()]
        }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    let borrower_pending: String = env
        .contract
        .view("get_claimable_collateral_reward")
        .args_json(json!({
            "account_id": env.borrower.id(),
            "collateral_id": env.collateral_token.id()
        }))
        .await?
        .json()?;
    assert_eq!(
        borrower_pending, "9950",
        "existing depositor should own liquidation rewards"
    );

    let late_pending_before: String = env
        .contract
        .view("get_claimable_collateral_reward")
        .args_json(json!({
            "account_id": late_depositor.id(),
            "collateral_id": env.collateral_token.id()
        }))
        .await?
        .json()?;
    assert_eq!(
        late_pending_before, "0",
        "non-depositor should have no rewards before joining"
    );

    late_depositor
        .call(env.contract.id(), "deposit_to_stability_pool")
        .args_json(json!({ "amount": "10" }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    let late_pending_after: String = env
        .contract
        .view("get_claimable_collateral_reward")
        .args_json(json!({
            "account_id": late_depositor.id(),
            "collateral_id": env.collateral_token.id()
        }))
        .await?
        .json()?;
    assert_eq!(
        late_pending_after, "0",
        "new deposit should not inherit historical rewards"
    );

    let borrower_pending_after: String = env
        .contract
        .view("get_claimable_collateral_reward")
        .args_json(json!({
            "account_id": env.borrower.id(),
            "collateral_id": env.collateral_token.id()
        }))
        .await?
        .json()?;
    assert_eq!(
        borrower_pending_after, "9950",
        "existing depositor's rewards must remain intact"
    );

    Ok(())
}

#[tokio::test]
#[serial]
async fn redeem_reduces_trove_and_awards_collateral() -> Result<()> {
    let env = setup_borrow_env().await?;
    let target = env.worker.dev_create_account().await?;

    open_trove_for(&env, &target, "10000", "4000").await?;

    env.borrower
        .call(env.contract.id(), "redeem")
        .args_json(json!({
            "collateral_id": env.collateral_token.id(),
            "trove_owner": target.id(),
            "amount": "1000"
        }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    let trove: Value = env
        .contract
        .view("get_trove")
        .args_json(json!({
            "owner_id": target.id(),
            "collateral_id": env.collateral_token.id()
        }))
        .await?
        .json()?;
    let debt = trove
        .get("debt_amount")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert_eq!(debt, "3000", "trove debt should drop by redeemed amount");
    let collateral_after = trove
        .get("collateral_amount")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert_eq!(
        collateral_after, "9995",
        "collateral should be reduced by conversion of redeemed nUSD"
    );

    let claimable: String = env
        .contract
        .view("get_claimable_collateral_reward")
        .args_json(json!({
            "account_id": env.borrower.id(),
            "collateral_id": env.collateral_token.id()
        }))
        .await?
        .json()?;
    assert_eq!(
        claimable, "5",
        "redeemer should accrue equivalent collateral"
    );

    env.borrower
        .call(env.contract.id(), "claim_collateral_reward")
        .args_json(json!({
            "collateral_id": env.collateral_token.id(),
            "amount": Option::<String>::None
        }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    let borrower_collateral = ft_balance(&env.collateral_token, &env.borrower).await?;
    assert_eq!(
        borrower_collateral, "5",
        "claiming after redemption should transfer collateral"
    );

    let total_debt: String = env
        .contract
        .view("get_total_debt")
        .args_json(json!({ "collateral_id": env.collateral_token.id() }))
        .await?
        .json()?;
    assert_eq!(
        total_debt, "7000",
        "system debt should reflect redemption burn"
    );

    Ok(())
}

#[tokio::test]
#[serial]
async fn stability_pool_withdraw_returns_balance() -> Result<()> {
    let env = setup_borrow_env().await?;

    env.borrower
        .call(env.contract.id(), "deposit_to_stability_pool")
        .args_json(json!({ "amount": "3000" }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    env.borrower
        .call(env.contract.id(), "withdraw_from_stability_pool")
        .args_json(json!({ "amount": "1000" }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    let remaining: String = env
        .contract
        .view("get_stability_pool_deposit")
        .args_json(json!({ "account_id": env.borrower.id() }))
        .await?
        .json()?;
    assert_eq!(remaining, "2000", "partial withdraw should leave the rest");

    let borrower_balance = nusd_balance(&env.contract, &env.borrower).await?;
    assert_eq!(
        borrower_balance, "2000",
        "withdrawn funds should return to borrower balance"
    );

    env.borrower
        .call(env.contract.id(), "withdraw_from_stability_pool")
        .args_json(json!({ "amount": Option::<String>::None }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    let final_balance: String = env
        .contract
        .view("get_stability_pool_deposit")
        .args_json(json!({ "account_id": env.borrower.id() }))
        .await?
        .json()?;
    assert_eq!(
        final_balance, "0",
        "withdrawing without amount should drain deposit"
    );

    Ok(())
}

async fn open_trove_for(
    env: &TestEnv,
    borrower: &Account,
    collateral_amount: &str,
    debt_amount: &str,
) -> Result<()> {
    borrower
        .call(env.contract.id(), "storage_deposit")
        .args_json(json!({
            "account_id": borrower.id(),
            "registration_only": Option::<bool>::None
        }))
        .deposit(NearToken::from_near(1))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    ensure_token_storage(&env.collateral_token, borrower).await?;
    mint_collateral(
        &env.collateral_token,
        &env.owner,
        borrower,
        collateral_amount,
    )
    .await?;

    let deposit_msg =
        json!({ "action": "deposit_collateral", "target_account": borrower.id() }).to_string();

    borrower
        .call(env.collateral_token.id(), "ft_transfer_call")
        .args_json(json!({
            "receiver_id": env.contract.id(),
            "amount": collateral_amount,
            "msg": deposit_msg
        }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    borrower
        .call(env.contract.id(), "borrow")
        .args_json(json!({
            "collateral_id": env.collateral_token.id(),
            "amount": debt_amount
        }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    Ok(())
}

async fn ensure_token_storage(token: &Contract, account: &Account) -> Result<()> {
    account
        .call(token.id(), "storage_deposit")
        .args_json(json!({
            "account_id": account.id(),
            "registration_only": Option::<bool>::None
        }))
        .deposit(NearToken::from_near(1))
        .max_gas()
        .transact()
        .await?
        .into_result()?;
    Ok(())
}

async fn mint_collateral(
    token: &Contract,
    owner: &Account,
    receiver: &Account,
    amount: &str,
) -> Result<()> {
    owner
        .call(token.id(), "mint")
        .args_json(json!({
            "account_id": receiver.id(),
            "amount": amount
        }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?
        .into_result()?;
    Ok(())
}

async fn ft_balance(token: &Contract, account: &Account) -> Result<String> {
    Ok(token
        .view("ft_balance_of")
        .args_json(json!({ "account_id": account.id() }))
        .await?
        .json()?)
}

async fn nusd_balance(contract: &Contract, account: &Account) -> Result<String> {
    Ok(contract
        .view("ft_balance_of")
        .args_json(json!({ "account_id": account.id() }))
        .await?
        .json()?)
}
