# NEAR CDP Stablecoin – Architecture & Research Notes

## Project Goal
Implement a Liquity-inspired, multi-collateral CDP (Collateralized Debt Position) system on NEAR Protocol that can mint a native over-collateralized stablecoin (`nUSD`). Unlike the original Liquity (single ETH collateral on Ethereum), this design targets any NEP-141 fungible token as collateral while preserving core safety properties: minimum collateral ratios, instant liquidations, decentralized price feeds, and capital-efficient redemption mechanisms.

---

## Liquity Reference Model

| Component | Liquity Behavior | Relevance for NEAR Adaptation |
| --- | --- | --- |
| **Troves** | Isolated vaults backed by ETH; minimum collateral ratio (MCR) = 110%; per-trove debt includes borrowing fee + `LUSD`. | Mirror concept with per-collateral parameters. Need storage-efficient representation for potentially thousands of troves. |
| **Stability Pool** | Users deposit `LUSD`; liquidations burn `LUSD` for discounted ETH, distributing rewards pro-rata. | Keep pool-based liquidation to avoid order book reliance. Need to support multiple collateral tokens → either shared pool with auto-swaps or per-collateral sub-pools. |
| **Redemptions** | `LUSD` holders can redeem directly for collateral at face value when price < $1, prioritizing lowest collateral ratio troves. Creates backstop arbitrage. | Maintain deterministic ordering by collateral ratio and price to avoid gas blow-ups on NEAR. |
| **Recovery Mode** | Triggers when TVL collateral ratio < 150%. Enforces stricter rules (e.g., higher MCR, limited borrowing) to recapitalize. | Keep chain-wide health metric to guard against oracle failures and black swan volatility. |
| **Fees & Incentives** | Borrowing fee (decays), redemption fee, `LQTY` inflation rewards for Stability Pool & front ends, staking rewards. | Replace with governance token or protocol fee sink depending on long-term roadmap. For now, design with configurable fee receiver. |

Key takeaways:
1. Instant, algorithmic liquidations + deterministic ordering maintain solvency without external keepers.
2. Stability Pool handles the majority of liquidations; user deposits absorb bad debt.
3. Redemption path ensures soft peg by letting arbitrageurs extract collateral at par.

---

## NEAR-Specific Considerations

1. **Account & Storage Model**
   - Each contract maintains its own state; users must attach NEAR for storage (“storage staking”).
   - Need lightweight trove representation to minimize per-user storage costs. Consider packing trove state (owner, collateral_id, collateral_amount, debt, status) and refunding storage on closure.
2. **Asynchronous Cross-Contract Calls**
   - Price feeds, multi-token transfers, and potential DEX trades happen via async promises.
   - Design must handle optimistic execution followed by callbacks; use state machines or optimistic transfers with rollback guards.
3. **Fungible Token Standard (NEP-141)**
   - Collateral tokens follow `ft_transfer_call`. Contract must implement `ft_on_transfer` to accept deposits.
   - Need registry that whitelists NEP-141 contracts with per-token risk params (MCR, liquidation penalty, debt ceiling).
4. **Gas Constraints**
   - 300 Tgas per transaction; liquidation/redemption loops must be bounded. Consider batch sizes or priority queues stored on-chain.
5. **Oracle Access**
   - NEAR lacks native price feeds; rely on existing oracle networks (e.g., Pyth, Flux, RedStone) via dedicated adapter contracts.
6. **Custody / Access Control**
   - Prefer DAO-controlled upgrades via NEAR’s access keys or multi-sig. Initial deployment can use guarded launch (upgrade key retained, but plan to transfer to DAO).

---

## Proposed Architecture

```
+-------------------+       +----------------+        +----------------+
|  Collateral Token | ----> | CollateralPool | <----> |   TroveManager |
+-------------------+       +----------------+        +----------------+
                                         |
                                         v
                                +----------------+
                                | StabilityPool  |
                                +----------------+
                                         |
                                         v
        +-------------+     +----------------+      +----------------+
        | PriceOracle | --> | RiskEngine     | ---> | LiquidationMgr |
        +-------------+     +----------------+      +----------------+
                                         |
                                         v
                                +----------------+
                                | RedemptionMgr  |
                                +----------------+
                                         |
                                         v
                                +----------------+
                                | nUSD Token     |
                                +----------------+
```

### Core Contracts/Modules

1. **Collateral Registry**
   - Stores whitelisted NEP-141 token addresses with parameters: MCR, liquidation penalty, debt ceiling, `stability_pool_mode` (shared vs. dedicated), price feed id, bootstrap discounts.
   - Admin-managed via DAO; emits events for front ends.

2. **Trove Manager**
   - Maintains trove structs keyed by `(owner, collateral_id)`.
   - Tracks normalized debt (principal + cumulative fees) using per-collateral base rates to avoid updating every trove.
   - Supports operations: open, adjust, close, transfer ownership. Enforces MCR and Recovery Mode rules.

3. **Stability Pool**
   - Users deposit `nUSD`; receives liquidation events from `LiquidationMgr`.
   - When trove liquidated, pool burns equivalent `nUSD` debt and receives collateral at discount, distributing pro-rata.
   - For multi-collateral: either
     - **Option A (Preferred):** Single pool; protocol swaps collateral to base asset via DEX aggregator before distributing (requires liquidity + price risk).
     - **Option B:** Per-collateral sub-pools; depositors choose risk/asset. Simpler and deterministic, recommended for MVP.

4. **Liquidation Manager**
   - Periodically checks troves below MCR. On liquidation:
     1. Pulls price from `RiskEngine`.
     2. Uses Stability Pool funds first.
     3. If pool insufficient, falls back to redistribution across healthy troves (like Liquity’s “redistribution”).
   - Supports batch operations to liquidate top-K riskiest troves.

5. **Redemption Manager**
   - Allows `nUSD` holders to redeem for collateral at oracle price, starting from lowest-collateralized troves.
   - Applies redemption fees to discourage abuse; increases base rate similar to Liquity.

6. **nUSD Token**
   - NEP-141 compliant stablecoin minted/burned exclusively by TroveManager & Redemption flows.
   - Optional hooks for compliance lists or bridging.

7. **Risk Engine / Oracle Adapter**
   - Normalizes external oracle feeds to common units.
   - Maintains time-weighted prices and sanity checks (e.g., confidence intervals).
   - Provides system-wide metrics: Total Collateral Ratio (TCR), Recovery Mode flag.

8. **Fee & Treasury Module**
   - Accumulates protocol fees (borrow, redemption, liquidation).
   - Routes to DAO treasury, buyback module, or reward contract.

---

## Key Flows

### 1. Open / Adjust Trove
1. User registers collateral token if not already (ensures storage deposit).
2. User calls `ft_transfer_call` to send collateral to `CollateralPool`.
3. Within callback, TroveManager increases collateral balance, calculates max borrowable `nUSD`.
4. User specifies target debt; TroveManager mints `nUSD` to user, updates individual debt with current base rate, records storage footprint.

### 2. Liquidation
1. Off-chain watcher (or anyone) calls `liquidate(collateral_id, trove_ids[], max_count)`.
2. RiskEngine verifies price freshness & TCR to determine Recovery Mode.
3. For each trove under thresholds:
   - Debt canceled using Stability Pool `nUSD`.
   - Collateral distributed to pool depositors (pro-rata) or redistributed to healthy troves if pool drained.
   - Liquidation penalty allocated between depositors and protocol fee sink.

### 3. Stability Pool Rewards
1. Depositor transfers `nUSD` to pool, receiving a position token (accounting share).
2. Rewards accumulate as collateral deposits + optional incentive token emissions.
3. Withdrawals return remaining `nUSD` plus accrued collateral (claimable via `claim_rewards`).

### 4. Redemption
1. `redeem(nUSD_amount, collateral_id, max_hints)` selects lowest-CR troves for that collateral using on-chain hint mechanism (e.g., binary tree or skip list).
2. Burns `nUSD`, reduces trove debt, transfers collateral to redeemer minus fee.
3. Updates base rate to throttle repeated redemptions.

---

## Risk Parameters & Governance Hooks

| Parameter | Purpose | Notes |
| --- | --- | --- |
| `MCR` per collateral | Minimum ratio (e.g., 130% for volatile assets, 110% for stables). | Configurable via DAO; Recovery Mode raises effective MCR. |
| `CCR` (Critical Collateralization Ratio) | System-wide threshold to trigger Recovery Mode (e.g., 150%). | Computed using total collateral/debt per asset. |
| `Debt Ceiling` per collateral | Caps exposure to any single asset. | Prevents concentration risk. |
| `Liquidation Penalty` | Incentive for pool depositors/redistributors. | Could vary by asset volatility. |
| `Borrow Fee` | Recovers oracle costs, funds treasury; decays over time. | Similar to Liquity base rate. |
| `Redemption Fee` | Discourages gaming; scales with recent redemption volume. | Shares base rate logic. |
| `Oracle Staleness Threshold` | Max age of price data before disabling borrowing. | Disable trove adjustments when stale. |

Governance can be enforced via:
- **DAO contract** controlling registry updates, fee parameters, oracle configs.
- **Time-lock or multi-sig** for critical upgrades.

---

## Open Questions & Next Steps

1. **Oracle Source** – Decide on concrete oracle provider(s) and interface contract. Need plan for resiliency (e.g., medianizer).
2. **DEX Liquidity for Collateral Swaps** – If opting for single Stability Pool with swaps, integrate with REF/Spin/PembRock to convert rewards; otherwise implement per-collateral pools.
3. **Front-End Incentives** – Liquity leverages “front end kickbacks” to decentralize UX. Determine if similar mechanism is required.
4. **Bridging / Interop** – Consider future compatibility with Ethereum L2s or Aurora for liquidity.
5. **Testing & Formal Verification** – Outline invariant tests (no under-collateralized system, debt conservation) and consider formal methods (e.g., Model checking Trove state machine).

**Immediate tasks:**
1. Choose contract language (Rust vs. TypeScript SDK) → Rust recommended for predictable gas and mature tooling.
2. Scaffold workspace: workspace-level Cargo project with core contracts + integration tests.
3. Implement Collateral Registry + nUSD token as foundational modules.
4. Draft specification for oracle adapter and storage accounting to avoid griefing via `ft_transfer_call`.

---

## Implementation Status (Rust Workspace)

- `contracts/cdp`: NEAR smart contract built with `near-sdk` 5.x that houses the `nUSD` NEP-141 token, collateral registry, trove storage, Pyth oracle syncing, and a NEAR Intents swap hook (for liquidation routing or treasury operations).
- Fungible-token functionality is provided via `near-contract-standards`; storage management is already wired up so users must call `storage_deposit` before receiving `nUSD`.
- Collateral onboarding, borrowing, repaying, redemption prep, and multi-token withdrawals are expressed as explicit methods, enabling incremental delivery of Liquity-like flows.
- Oracle data is pushed via `submit_price` and restricted to the configured Pyth adapter contract; latest prices are cached per collateral asset for ratio checks.
- NEAR Intents integration is encapsulated in `trigger_swap_via_intents`, which forwards parameters to the configured router contract and handles the callback bookkeeping.

### Key Contract Methods

| Method | Purpose |
| --- | --- |
| `register_collateral` | Owner-only registration of NEP-141 collateral with risk parameters. |
| `submit_price` | Oracle adapter pushes scaled USD price data for a collateral token. |
| `ft_on_transfer` | Handles collateral deposits (any NEP-141 via `DepositCollateral`) and optional `nUSD` repayments (`RepayDebt`). |
| `borrow` / `repay` | Mutate trove debt while minting/burning `nUSD`; enforce collateral ratios and debt ceilings. |
| `withdraw_collateral` / `close_trove` | Return collateral via token transfers once safety constraints hold. |
| `trigger_swap_via_intents` | Owner-controlled call into the NEAR Intents aggregator for liquidation routing or treasury operations. |

Message formats for `ft_on_transfer` are JSON payloads with `action`:

```json
{ "action": "deposit_collateral", "target_account": "optional-account.near" }
{ "action": "repay_debt", "collateral_id": "asset.token.near" }
```

### Building & Testing

```bash
cargo fmt        # format the whole workspace
cargo test       # runs unit tests under contracts/cdp
```

> `cargo test` requires network access the first time to pull crates from crates.io.

---

This README will evolve into a full technical spec as components are implemented, including state diagrams, storage layouts, and exact API signatures. Contributions and feedback welcome.
