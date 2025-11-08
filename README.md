# NEAR CDP Stablecoin – Smart Contract Logic

The repository contains a single Rust smart contract that mints an
over‑collateralised NEP‑141 stablecoin (`nUSD`).  The contract combines a trove
manager, a stability pool, and the `nUSD` token itself.  This document explains
how the contract behaves from every participant’s perspective, which methods
they call, which assets they provide or receive, and which rewards and risks are
associated with each role.

---

## System Overview

1. **Collateral registry** – the owner registers NEP‑141 tokens together with a
   minimum collateral ratio (MCR), recovery ratio, debt ceiling, liquidation
   penalty, and oracle id.  Only registered collateral can be deposited.
2. **Troves (vaults)** – each `(borrower, collateral_id)` pair has a trove that
   tracks deposited collateral, outstanding debt, and the last update timestamp.
3. **Price feeds** – a designated oracle account calls `submit_price` to push the
   latest USD price per collateral.  All risk checks depend on this feed.
4. **Borrowing / redemption** – borrowers lock collateral through
   `ft_transfer_call`, mint `nUSD` with `borrow`, and can reduce debt with
   `repay` or `redeem` (burning `nUSD` against another trove’s collateral).
5. **Stability pool** – `nUSD` holders can deposit their tokens.  When an unsafe
   trove is liquidated, the pool’s `nUSD` is burnt to cancel the debt, and the
   pool depositors receive the collateral (minus a penalty that goes to the
   protocol owner).  The pool tracks per-share rewards so depositors earn only
   the liquidation events that happen while they are staked.
6. **Owner utilities** – the owner can trigger swaps through a NEAR Intents
   router (`trigger_swap_via_intents`) to rebalance reserves or route treasury
   assets.

---

## Roles, Interactions, Rewards & Risks

### 1. Borrowers (Trove Owners)
- **How they interact**
  - Call `storage_deposit` once to register their account with the `nUSD`
    fungible token.
  - Transfer NEP‑141 collateral via `ft_transfer_call` with
    `{"action":"deposit_collateral"}` to increase their trove balance.
  - Mint `nUSD` with `borrow(collateral_id, amount)` as long as the trove’s
    collateral ratio stays above the configured MCR.
  - Reduce debt using `repay` (burning their `nUSD`) or `redeem` against another
    trove’s collateral when they want to arbitrage the peg.
  - Withdraw surplus collateral with `withdraw_collateral` or close the trove
    entirely with `close_trove` after repaying all debt.
- **What they provide / receive**
  - Provide volatile collateral tokens.
  - Receive freshly minted `nUSD` that can be sold, swapped, or deposited into
    the stability pool.
- **Rewards**
  - Cheap leverage when the collateral price appreciates.
  - The ability to capture redemption arbitrage by burning `nUSD` against their
    own trove.
- **Risks**
  - If the collateral price drops and their ratio falls below MCR, anyone can
    liquidate their trove.  The entire collateral (minus the liquidation penalty)
    is redistributed to stability pool depositors.
  - They must trust the oracle feed; stale or incorrect prices can still trigger
    a liquidation.

### 2. Stability Pool Depositors
- **How they interact**
  - Call `deposit_to_stability_pool(amount)` to move `nUSD` into the pool.
  - Optionally withdraw partially or fully using `withdraw_from_stability_pool`;
    shares are converted back to `nUSD` using the pool’s share accounting.
  - Claim accrued collateral rewards with `claim_collateral_reward` and receive
    real NEP‑141 tokens.
- **What they provide / receive**
  - Provide `nUSD` liquidity that stands ready to cancel bad debt during
    liquidations.
  - Receive collateral from liquidated troves pro‑rata.  All rewards are tracked
    via reward-per-share accumulators so new depositors cannot steal past
    rewards.
- **Rewards**
  - Instant access to discounted collateral whenever a trove is liquidated.
  - Compounding yield if the seized collateral appreciates.
- **Risks**
  - Deposited `nUSD` cannot be withdrawn during the liquidation transaction; if
    the pool covers a large liquidation, the `nUSD` balance shrinks immediately.
  - Rewards depend entirely on liquidation volume; in calm markets, deposits may
    sit idle.

### 3. Liquidators / Arbitrageurs
- **How they interact**
  - Monitor troves and call `liquidate(collateral_id, owners[])` on any trove
    whose collateral ratio is below MCR.
  - Optionally call `redeem` to burn `nUSD` against the weakest troves when `nUSD`
    trades below the peg.
- **What they provide / receive**
  - Provide orchestration: they spend gas to keep the system solvent.
  - Receive no direct payout for the liquidation call itself (rewards go to the
    stability pool), but can arbitrage by buying discounted collateral or by
    acquiring `nUSD` cheaply and redeeming it.
- **Rewards**
  - Access to system-wide arbitrage opportunities.
- **Risks**
  - Need to front gas and handle asynchronous NEAR execution; if the oracle price
    moves during the transaction, the call may fail.

### 4. Oracle Operators
- **How they interact**
  - The address configured as `pyth_oracle_id` calls `submit_price` to push fresh
    prices for each collateral.  Every state-changing method that touches troves
    consults the cached price.
- **What they provide / receive**
  - Provide timely, accurate price data (no direct in-contract reward).
  - Receive governance trust or off-chain compensation.
- **Risks**
  - An incorrect price can liquidate healthy troves or block borrowing.  The
    operator must maintain infrastructure and SLAs.

### 5. Governance
- **How they interact**
  - Registers collateral through `register_collateral` and manages the list of
    trusted oracles and the NEAR Intents router.
  - Can trigger swaps via `trigger_swap_via_intents` to recycle treasury assets
    or fund future rewards.
- **What they provide / receive**
  - Provide stewardship and upgrades (initially through an owner account, later
    ideally through a DAO).
  - Receive the liquidation penalty portion that is not distributed to the pool
    (recorded as pending collateral rewards for the owner account).
- **Risks**
  - Misconfiguration (too low MCR or too high debt ceiling) can render the
    system unsafe.
  - Owning the admin key is a security liability; it should migrate to a DAO once
    the system is hardened.

---

## Key Flows Summarised

1. **Deposit & Borrow**
   1. User calls `ft_transfer_call` on a collateral token → contract credits the
      trove.
   2. User borrows `nUSD` → trove debt increases, `nUSD` is minted to the
      borrower.
   3. The `nUSD` balance can be spent, swapped, or deposited into the stability
      pool.
2. **Repay / Close**
   1. Borrower transfers `nUSD` back (either via `repay` or `ft_transfer_call`
      with a repay action).
   2. Debt decreases; once it reaches zero, borrower can withdraw all collateral
      and delete the trove.
3. **Liquidation**
   1. Anyone calls `liquidate` with a list of unsafe troves.
   2. The contract burns `nUSD` from the stability pool, cancels the debt, and
      redistributes the collateral minus the penalty.
   3. Pool depositors can claim the collateral immediately; the penalty portion
      accrues to the owner.
4. **Redemption**
   1. A user burns `nUSD` via `redeem`, targeting a specific trove.
   2. Debt decreases and collateral is queued as a reward for the redeemer.
5. **Oracle Update**
   - The designated oracle account periodically calls `submit_price`; borrowing
     and withdrawals always read the cached price to enforce safety guarantees.

---

## Operational Considerations

- **Storage staking** – every participant (borrower, pool depositor, owner) must
  attach enough NEAR when calling `storage_deposit`; collateral transfers also
  need the underlying FT contract to have storage for both the sender and this
  contract.
- **Gas** – external calls (`ft_transfer`, `trigger_swap_via_intents`) specify
  static gas budgets; integration tests rely on `max_gas()` to avoid “Exceeded
  prepaid gas” errors.
- **Security** – the contract has no upgrade hooks inside the business logic, so
  safe parameter choices and a trustworthy owner/oracle are essential.
- **Extensibility** – the module split (`types.rs`, `views.rs`, `internal.rs`)
  keeps pure view methods isolated from state mutations, making auditing easier
  and enabling future components (e.g., multiple stability pools) to plug in.

---

With these mechanics, the contract delivers a fully on-chain borrowing and
liquidation process on NEAR.  Borrowers gain capital efficiency, stability pool
contributors earn collateral rewards, and arbitrageurs keep `nUSD` near its peg
—all orchestrated by the owner-configured parameters and the oracle feed.
