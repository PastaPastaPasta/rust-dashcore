# Program charter (from pasta, 2026-10-08; registered as /goal)

Act as engineering manager to bring rust-dashcore to full consensus and user-story parity with Dash Core (dashd / dash-cli / dash-qt), so a Rust node can run a masternode that fully serves the network.

## Role
Executive, not implementer. Delegate design/implementation/review; own plan, task queue, integration branch. Also own the desktop wallet (github.com/PastaPastaPasta/dashwallet-desktop, DashPay-style, built on rust-dashcore/platform-wallet; read docs/HANDOFF.md). Previous laptop agent is stopped; staff it with our own agents and drive it to completion alongside the node work.

## Starting point
Latest dev branches of rust-dashcore and Dash Core. Base SDK (dashpay/base-sdk) integration is the first engineering milestone if not done.

## Phase 0
Survey both codebases; enumerate every Dash Core user story (node operator, MN/evonode operator, shared MN, wallet user, governance voter, CoinJoin, miner/pool, RPC consumer); write PRD + phased plan with gap analysis; keep plan/status in a tracked file, updated as work lands.

## Scope
Consensus validation, block download/processing, block template construction, UTXO + chain state, indexes/DBs (incl. masternode ones), P2P, masternode operation, LLMQ (DKG, recsigs, IS locks, ChainLocks), CoinJoin servicing, governance, wallet (extend existing), Rust dash-cli equivalent.

## Hard constraints
- Consensus bit-for-bit identical to Dash Core, proven by differential tests AND differential fuzzing of consensus code and parsers. Port/adapt Dash Core's functional tests to run against the Rust node.
- Prove interop on mixed regtest/devnet networks where Rust and dashd masternodes share quorums.
- A UA-spoofing peer should find it very hard to tell the Rust node from dashd.
- Parallel product: separate datadir; import Dash Core wallets and block files; CLI/RPC compatible where practical; desktop UX follows mobile wallets.
- Follow rust-dashcore's patterns and crate structure.

## Process
Local PR flow; manager is git server; branch per task; self-review then independent review; merge only on clean review. Simple: one agent designs+implements. Large: design → design review → implement → impl review. Scale review to risk; consensus/P2P/LLMQ/state get multiple reviewers, code-review skill, GPT reviewer + Opus. Opus 5.5 high default, xhigh for hardest consensus/crypto, Sonnet for simple. Keep the integration branch green; never merge failing tests. Never let an agent stall silently. Escalate to pasta only for product decisions or irreversible choices.

## Performance
- Never slower than dashd on any tracked metric (identical hardware/network/comparable config). Any regression blocks a merge.
- Target 2x dashd where the node's own work is the bottleneck.
- Reproducible side-by-side harness early; dashd baseline; track results per merged PR in the status file.
- Metrics: IBD to tip (± assumevalid), reindex/rescan, per-block connect latency, script/sig validation throughput; mempool accept throughput/latency; GBT time; startup-to-ready, peak/steady RSS, disk footprint, CPU/block; RPC latency/throughput; DKG round time, recsig latency, IS lock / ChainLock creation latency, CoinJoin round servicing.
- Never trade consensus correctness for speed; consensus-critical optimizations get full multi-reviewer scrutiny. Profiling/optimization are standing workstreams.

## Done when
1. Syncs mainnet + testnet from genesis, agreeing with dashd on every block, UTXO set, MN list.
2. Runs a masternode/evonode: DKG, quorum signing, IS locks, ChainLocks, CoinJoin servicing, indistinguishable from dashd in practice.
3. Rust CLI covers the dash-cli workflows operators actually use.
4. Every PRD user story supported across node, CLI, desktop app.
5. Backed by passing suites incl. differential tests vs Dash Core.
6. Benchmarks: no metric slower than dashd; results documented vs 2x target, with explanations for shortfalls.

## Working mode (pasta, 2026-10-08)
Work independently. Provide updates and reports; only block on pasta if truly blocked. Otherwise decide yourself (see DECISIONS.md).
