# Brief A1: Foundational architecture of the Rust Dash node

## Context
Read ../CHARTER.md (the program goal), ../research/R1-rust-dashcore-state.md (rust-dashcore today), and ../research/R3-base-sdk.md (base-sdk and its integration status). Two more reports are in progress: ../research/R2-dashcore-inventory.md (+R2b RPC inventory, which exists already), covering Dash Core subsystems, RPCs and user stories, and ../research/R4-consensus-and-verification.md, covering the consensus surface and verification strategy. Read them if they exist when you start, and check again before you finalize.

Repos: rust-dashcore /work/home/agent/workspace/rust-dashcore (branch dev, read-only for you), base-sdk /work/home/agent/workspace/base-sdk (develop), Dash Core /work/home/agent/workspace/dash (develop).

Constraints that shape the design:
- The program is local. We are the git server and nothing goes upstream unless pasta asks. We MAY change base-sdk itself (in a local branch, consumed via a path or git pin), rust-dashcore, and Dash Core (for test hooks).
- Consensus must be bit-for-bit identical to Dash Core, proven by differential tests and differential fuzzing. Performance must never be slower than dashd on any metric, with a target of 2x where the node's own work is the bottleneck (IBD, connect latency, sig validation, mempool, GBT, RSS, disk, RPC, LLMQ latencies).
- rust-dashcore is consumed by the mobile wallets (via FFI), by dashpay/platform (about 2,100 `dashcore::` uses), and by the desktop wallet (via platform-wallet). Do not break those consumers gratuitously; the node must coexist with SPV/wallet use of the same crates.
- Follow rust-dashcore patterns and crate structure. Extend the existing wallet (key-wallet / key-wallet-manager); do not replace it.
- Program instruction: integrate Base SDK first if it isn't integrated (R3: mostly not).

## Questions you must decide (with reasoning and alternatives considered)
1. **Type stack.** What do the node crates build on: rust-dashcore `dashcore` types, base-sdk types, or a migration path? How far must base-sdk integration go before node work starts, and what can proceed in parallel? Address bincode, hash display order, the Network enum/Devnet, the v24 payloads (upstream PRs #1136–#1138), the BlockHash X11 feature footgun, and the one-codec-vs-two problem. Give a concrete, ordered "Base SDK milestone" made of PR-sized tasks, and mark which ones change base-sdk itself.
2. **Node crate architecture.** The new crates (names, responsibilities, dependency graph), and where they live (same workspace). Cover: consensus (script interpreter, tx/block validation, params/deployments, subsidy/payments), chainstate (block index, UTXO/coins cache, undo data, reorgs, assumevalid), storage engine choice (RocksDB / LMDB / redb / MDBX / custom flat files; justify with the performance targets), block files, mempool and policy, P2P (inbound+outbound, peer manager, addrman, relay, compact blocks, BIP324 #264, the in-flight network rewrite #902, and fingerprint-equivalence with dashd), the evo/masternode list state machine, LLMQ (DKG, sigshares, recovered sigs, IS, CL), governance, CoinJoin server, sporks, indexes, the RPC server (JSON-RPC, dashd-compatible), the CLI binary, the daemon binary, config/datadir, and wallet integration. What from dash-spv is reused vs replaced?
3. **Concurrency and performance architecture.** Parallel script/sig validation, the caching strategy, the async vs threads split, and DB write batching. How we beat dashd 2x on IBD / connect / GBT / RSS.
4. **Verification architecture.** How the design makes differential testing and fuzzing natural (e.g. a pure `consensus` crate with no I/O; a dashd differential oracle; Core JSON vectors; chain-replay comparison hooks; an RPC surface compatible enough for Dash Core's Python functional tests).
5. **Sequencing.** A milestone-level build order from "Base SDK done" to "masternode on mainnet", with the critical path and the parallelizable workstreams (agents can work in parallel; assume 6–10 concurrent implementers).

## Deliverable
A single design doc (path given in your task) with sections matching the 5 questions, plus a "Decisions table" (decision | choice | main alternative | why) and "Open questions for pasta" (only true product or irreversible decisions). Be concrete: crate names, key traits/types, data layouts. Target 1,500–3,000 lines of dense Markdown. Read-only except for your output file. Don't commit.
