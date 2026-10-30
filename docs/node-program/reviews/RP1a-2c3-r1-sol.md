# RP1a-2c-3 code review, round 1

**Verdict: ACCEPT-WITH-NITS**

Reviewed branch `node/rp1a-2c3` at `119526c2d` against `49196c094` in read-only worktree `/work/home/agent/workspace/rdc-wt/rp1a-2c3`. Counts: **0 major, 1 minor, 2 nits**.

## Findings

### m-1 — The six Core whitespace characters are not all regression-tested

**Location:** `dashd-rpc/src/blockchain/merkle.rs:35`; tests at `dashd-rpc/src/blockchain/merkle/tests.rs:31-38` and the DIFF case at `rdashd/tests/rpc_diff.rs:2665`.

Core's `IsSpace` accepts space, tab, newline, vertical tab (`\\v`), form feed (`\\f`), and carriage return (`\\r`). The implementation lists all six, but the unit test exercises only space/tab/newline and the DIFF's spaced filter uses only spaces.

**Concrete surviving mutant/input:** Replacing the implementation's whitespace arm with

```rust
matches!(c, b' ' | b'\t' | b'\n')
```

still passes all three focused merkle unit tests and the live `getmerkleblocks_match_dashd_v24` DIFF. The input `["03\u000b000000010000000000000000", "<genesis>", 1]` (and the analogous \f or \r forms) is accepted by Core, which skips the control character and parses the filter, but the mutant treats it as malformed and returns `DataStream::read(): end of data: iostream error`.

**Fix:** Add unit and DIFF cases containing \v, \f, and \r between filter-byte pairs. Keep the existing implementation, which already matches Core.

### n-1 — The node-side start-block pruned branch is only tested through a fake seam

**Location:** `dashd-node/src/rpc.rs:615`; D45 at `docs/node-program/designs/RP1a-notes.md:355`; the mapping-only test is `dashd-rpc/src/blockchain/merkle/tests.rs:122-130`.

The unit test injects `BlockReadError::Pruned` as the already-computed seam result. It does not execute `Writer::merkle_blocks` with `have_pruned` and therefore would stay green if the node's `if pruned { ... }` branch were deleted. The node currently rejects pruning, so this is unreachable in production for this slice.

**Concrete surviving mutant/input:** Delete `if pruned { return Err(BlockReadError::Pruned); }` and run the current library/DIFF suites; they still pass because no production test starts a pruned node and the fake returns the error after the seam call.

**Fix:** Reword D45 as “the RPC mapping is unit-tested; the node's pruning check is unreachable until CH8,” and add a real `have_pruned`/missing-data node test (and a pruned-node DIFF) when CH8 enables `-prune`.

### n-2 — New test-only dependency declarations duplicate the normal edges

**Location:** `dashd-node/Cargo.toml:60-61` and the existing dev-dependencies at `:77` and `:80`; D45's dependency note at `docs/node-program/designs/RP1a-notes.md:337-355`.

The new normal dependencies on `dash-p2p-core` and `dash-types` are required by `dashd-node/src/rpc.rs`, but the same packages remain in `[dev-dependencies]` with no additional features. They add no graph capability and leave comments describing edges that are now normal dependencies. The layer check passes, so this is maintenance rather than a runtime defect.

**Concrete maintenance input:** A future contributor updates only one of the two declarations (for example, changes the feature set or path while debugging a test) and gets a test build that resolves through the other declaration while the production dependency remains different. This can hide an intended dependency change from review and makes `Cargo.toml`'s ownership/comments misleading.

**Fix:** Remove the duplicate dev-dependency entries (or explain a deliberately distinct test-only feature), and add a one-line D45 note recording that `check-layers.py` passes with the three new edges: `dashd-node -> dash-p2p-core`, `dashd-node -> dash-types`, and `rdashd -> dashd-p2p` (dev-only).

## What I checked

* Compared the handler and seam with Dash Core v24.0.0-rc.4:
  * `~/workspace/dash/src/rpc/blockchain.cpp:827-924` for argument order, defaults, limits, errors, active-chain walk, count cap, and serialized array/empty reply.
  * `~/workspace/dash/src/merkleblock.cpp:30-72` and `common/bloom.{h,cpp}` for allowed special transaction types, matching, bloom update flags, and partial-tree bytes.
* Verified the Rust handler's lenient hex behavior, compact-size precheck, filter/hash/count order, exact RPC texts, count semantics, empty result, and use of the shared `dashd-p2p` bloom/merkle implementation.
* Checked the DIFF harness's byte-for-byte HTTP comparison (masking only `Date`), error cases, count limits, off-chain/unknown/header-only cases, and the uncompressed P2PK fixture. The fixture is signed by Core's `signrawtransactionwithkey` using the uncompressed WIF and then mined by Core before comparison; it is not tailored only to rdashd.
* Checked the read order: the V query captures a coherent chain snapshot and immutable block positions; append-only block files let reads after that turn represent the query's linearization point even if a later reorg disconnects those blocks. This matches the established snapshot model and is not a finding.
* Reviewed the two test-file edits in `blockchain/tests.rs` and `mining/tests.rs`; they only add the required `Chainman::merkle_blocks` fake implementations and state.
* Read D45 in `docs/node-program/designs/RP1a-notes.md`; `python3 contrib/node/check-layers.py` passes with the documented baseline exception and the new dependency edges.
* Reviewed the recorded 12-mutant run in `/work/scratch/rp1a-2c3/mut-r2.log`: all 12 mutants fail either the focused library tests or the live DIFF (mutants 9–12 are killed by the DIFF). Independently reproduced the whitespace mutant above in `/work/scratch/rp1a-2c3-r1-sol`; its 3/3 merkle unit tests and the live DIFF both passed.
* Ran:
  * `cargo test -p dashd-rpc --lib blockchain::merkle --all-features` — **3 passed**.
  * `cargo test -p dashd-node --lib --all-features` — **192 passed, 0 failed, 3 ignored**.
  * `cargo test -p rdashd --test rpc_diff getmerkleblocks_match_dashd_v24 -- --nocapture` — **1 passed** (live Dash Core comparison).
  * `cargo fmt --all -- --check` — passed.
  * `git diff --check 49196c094 119526c2d` — passed.
  * `python3 contrib/node/check-layers.py` — passed.
