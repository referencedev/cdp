use anyhow::{ensure, Context, Result};
use near_token::NearToken;
use near_workspaces::{network::Sandbox, sandbox, Account, Contract, Worker};
use serde_json::{json, Value};
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

async fn load_contract_wasm() -> Result<Vec<u8>> {
    if !wasm_artifact_path().exists() {
        build_contract_wasm()?;
    }
    fs::read(wasm_artifact_path())
        .await
        .context("unable to read compiled CDP wasm")
}

struct TestEnv {
    #[allow(dead_code)]
    worker: Worker<Sandbox>,
    contract: Contract,
    oracle: Account,
    collateral_token: Account,
    borrower: Account,
}

async fn setup_borrow_env() -> Result<TestEnv> {
    let worker = sandbox().await?;
    let wasm = load_contract_wasm().await?;
    let contract = worker.dev_deploy(&wasm).await?;

    let owner = worker.dev_create_account().await?;
    let oracle = worker.dev_create_account().await?;
    let collateral_token = worker.dev_create_account().await?;
    let borrower = worker.dev_create_account().await?;

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
        .transact()
        .await?
        .into_result()?;

    oracle
        .call(contract.id(), "submit_price")
        .args_json(json!({
            "collateral_id": collateral_token.id(),
            "price": "20000",
            "decimals": 2
        }))
        .transact()
        .await?
        .into_result()?;

    borrower
        .call(contract.id(), "storage_deposit")
        .args_json(json!({
            "account_id": borrower.id(),
            "registration_only": Option::<bool>::None
        }))
        .deposit(NearToken::from_near(1))
        .transact()
        .await?
        .into_result()?;

    let deposit_msg =
        json!({ "action": "deposit_collateral", "target_account": borrower.id() }).to_string();

    collateral_token
        .call(contract.id(), "ft_on_transfer")
        .args_json(json!({
            "sender_id": borrower.id(),
            "amount": "10000",
            "msg": deposit_msg
        }))
        .transact()
        .await?
        .into_result()?;

    borrower
        .call(contract.id(), "borrow")
        .args_json(json!({
            "collateral_id": collateral_token.id(),
            "amount": "4000"
        }))
        .deposit(NearToken::from_yoctonear(1))
        .transact()
        .await?
        .into_result()?;

    Ok(TestEnv {
        worker,
        contract,
        oracle,
        collateral_token,
        borrower,
    })
}

#[tokio::test]
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
