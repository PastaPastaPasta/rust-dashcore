**Verdict: ACCEPT-WITH-NITS — 0 blocker, 0 major, 1 minor, 2 nit**

# LQ3b r1 review (Opus, independent): `bb8a22792..89c6b40c4` on `node/lq3b`

Reviewer: Opus, fresh. Scope: correctness only. Core reference: `v24.0.0-rc.4` (`llmq/quorumsman.cpp`, `options.cpp`, `unordered_lru_cache.h`, `llmq/params.h`).
Worktree `~/workspace/rdc-wt/lq3b` was read only. Builds and mutants ran in the reflink copy `/work/scratch/cp/reviews/lq3b-opus-copy` (its `.git` neutralized), target `/work/scratch/cp/reviews/lq3b-opus-target`. Unit `cp-lq3b-opus-mutants` ran under `heavy.lock`. Logs and the table are in `/work/scratch/cp/reviews/lq3b-opus-run/`.

## What I checked against Core (line by line), and what holds

### ScanQuorums, both overloads (`scan.rs:259` `scan_quorums`, plus the pre-existing `scan_store_block` and `scan_quorums_from`)

| Core rc.4 | Rust | Holds |
|---|---|---|
| `:174` `nCountRequested == 0 \|\| !IsQuorumTypeEnabled(type, pindexStart)` → `{}` | `scan_store_block` → `None`; `Scanned { [], event: None }` | yes |
| `:184-196` the store block: the previous cycle's `miningEnd`, the genesis bail at `:191`, or the past-window end | `scan_store_block:61-77` | yes |
| `:203`/`:237` the cache is read only for `chain == nullptr` | `Cached` → `cache.get(type, hash(store))`; `ChainBound` → `(None, None)` → `ScanCache::Bypass`, with no hash read and no write | yes |
| `:215` the key is `pindexStore->GetBlockHash()` | `ctx.hash(store)` (not `start`) | yes (my mutant A3 is killed) |
| `:218-224` hit with size ≥ count → the first `count`, no write | `Hit(c) if c.len() >= count` → `cached[..count]`, `cache_write: None` | yes |
| `:227-232` hit with size < count: `nScanCommitments -= size`, the genesis-parent bail, scan from the oldest base's `pprev` | `from = prev(oldest.base())`, `None` → `ContinuationBail` (no write), scan `count - cached.len()` | yes |
| `:235`/`:238` a miss or the chain overload → `max(count, keepOldConnections)` | `count.max(keep)` for `Miss` and `Bypass` | yes |
| `:242-245` indexed versus plain reader by `useRotation` | same | yes |
| `:251` `populate_cache = vecResultQuorums.size() < keep`, counting the cached prefix | `result.len() < keep`, where `result` starts as the cached prefix | yes |
| `:264-268` a missing quorum → `{}` with no write; earlier builds keep their effects | `ScanOutcome::empty(use_cache)` | yes |
| `:274-282` write iff `nCountResult > 0 && chain == nullptr`, the first `min(n, keep)` including the cached prefix; HitContinued writes too (a re-stamp when nothing new is found) | `cache_write` is `Some` iff `use_cache != Bypass && !result.is_empty()`; the composition inserts iff `store_hash` is `Some` (Cached) | yes |
| `:284-285` truncate to `nCountRequested` | `result.truncate(count)` | yes |

- `ScanCacheStore::get` stamps on a hit and `insert` stamps on an overwrite. That matches `unordered_lru_cache::get`/`_emplace`.
- The harness host (`PerTypeCache` over `LruCache`) keeps Core's 2×`maxSize` truncation threshold and `nth_element` keep-most-recent rule.
- A type with no configured cache misses and drops writes, as `PerLlmqTypeCache` does.

### `scan_cache_size` (`scan.rs:222`) against `:211-214`

- It computes `max_cycles(keepOldConnections) * (dkgMiningWindowEnd - dkgMiningWindowStart)`.
- `max_cycles` (base-sdk `params/src/llmq.rs:60`) is `useRotation ? n / signingActiveQuorumCount : n`, as in `llmq/params.h:122-125`.
- `i32` arithmetic is used, as in Core's `int`. `LlmqCtx::new` (`ctx.rs:62`) refuses tables whose product is outside `1..=i32::MAX`.
- **Holds.**

### `select_quorum_for_signing` (`selection.rs:110`)

- **Start block.**
  - `select_start_height(tip_height, sign_height, sign_offset)` follows `:711-722`: −1 means the tip, and it returns `None` when the start is above the tip or below 0.
  - `ancestor_below(tip, start)` is `active_chain[start]` on the chain ending at `tip`.
  - No start gives no scan (`Selection { None, None }`), as `:727-729` does.
- **`SIGN_HEIGHT_OFFSET`.** It is 8 (`quorumsman.h:210`). The caller passes `sign_offset`, as Core's `signOffset` parameter.
- **Rotation flag.** `is_quorum_rotation_enabled(ctx, params, start)` is evaluated at `pindexStart`, as `:733`. Its body (`params.rs:93`) matches `options.cpp:50-61`.
- **Count.** `signingActiveQuorumCount` (`:730`) is passed to the Cached overload (`:734`/`:758`).
- **Rotated choice** (`select_from_scan`, pre-existing):
  - `n = log2(count)`;
  - `b = GetUint64(3)`, the little-endian bytes 24..32;
  - `signer = ((1<<n)-1) & (b >> (64-n-1))`;
  - `signer > size` → null (not `>=`; my A10 is killed);
  - the first quorum whose `uint64_t(quorumIndex)` equals `signer`.
- **Non-rotated choice:**
  - the hash is `SHA256d(type u8 ‖ quorumHash ‖ selectionHash)`;
  - the minimum is taken by memcmp of the 32 bytes, with ties broken by position (as `std::sort` on `(uint256, size_t)`).
  - My A9 (max) and A11 (reversed byte order) are killed by the Core-anchored captured/legacy/regtest ChainLock tests.

### `mined` (LQ3b-1)

- `VerifiedQuorum::mined` is `VerifiedQuorum::of(qc)` (`commitment.rs:729-736`).
- It carries `qc.quorum_public_key.value().clone()` verbatim, with `quorum_hash`, `llmq_type`, and `quorum_index.unwrap_or(0)` (Core's default).
- It reads no members and no BLS. This is exactly `quorum->qc->quorumPublicKey` at `:795-796`, after `BuildQuorumFromCommitment` (`:58-105`) took `GetMinedCommitment`'s qc as is.
- `ScannedQuorum::mined` wraps it as `Verified`.
- It is gated `cfg(any(test, feature = "mined"))`, and only `dashd-chainstate` turns it on.

**Guard (`tests/feature_gates.rs`).** I ran the built guard binary against manifest mutants in my copy. Each one was restored and checked by `cmp` against the worktree.

| Mutant | Guard |
|---|---|
| G0: none | pass |
| G1: dash-mnlist's dep features `["reload", "mined"]` | **FAIL** `["dashd-chainstate", "dash-mnlist"]` |
| G2: dashd-mining feature `x = ["dash-evo/mined"]` | **FAIL** `["dashd-chainstate", "dashd-mining"]` |
| G3: dash-evo's own `reference = ["mined"]` | **FAIL** `["dash-evo", "dashd-chainstate"]` |
| G4: dash-llmq renamed target-specific dev-dep `evo2 = { package = "dash-evo", features = ["mined"] }` | **FAIL** `["dashd-chainstate", "dash-llmq"]` |
| G5: `[workspace.dependencies] dash-evo` with `mined`, inherited by dash-llmq via `workspace = true` | **FAIL** `["dashd-chainstate", "dash-llmq"]` |

The guard really fails on a second owner in every manifest form I tried.

- Reachability by unification: callers that do not depend on dashd-chainstate cannot compile a call, because CI tests each crate with `cargo test -p <crate> --all-features` (`.github/scripts/ci_config.py:189-195`). So a stray call in, for example, dash-mnlist fails CI.
- What remains is call sites in dashd-chainstate's own dependents (nit N2).

### The twin (LQ3b-3) and the differential (LQ3b-2)

- The twin compares the selected quorum and the full SCAN trace event (store, cache use, written, n) with the live Core oracle. It does this call by call, over one persistent cache, with tip = coinbase parent.
- The oracle's process-wide cache evolves in the same order, so a per-call reset or a wrong key would show up. My A3 (wrong key) is killed here.
- It asserts that both Miss and HitSatisfied occur, and that there are at least 10 CLs.
- `MinedIndex::recorded` refuses broken recordings, as stated.

## Mutants (mine; `LQ3BOP=n`, one build; dash-evo lib + node-l1 `l1_llmq` full with `oracle`)

| # | Mutant | dash-evo | node-l1 | Status |
|---|---|---|---|---|
| none | — | pass | pass (30/30) | — |
| A1 | HitContinued scan count = `count` (not `count - cached.len()`) (`scan.rs:166`) | fail | pass | killed (`a_hit_is_cut_or_continued_below_its_oldest_quorum` only) |
| A2 | HitContinued scan count = `count.max(keep)` | fail | pass | killed (same unit test only) |
| A3 | cache keyed by `hash(start)` | fail | fail | killed (differential + twin) |
| A4 | cache write = the truncated result | fail | pass | killed (differential) |
| **A5** | **rotation flag read at `tip`, not `start` (`selection.rs:135`)** | **pass** | **pass** | **SURVIVES** → m1 |
| A6 | `select_quorum_for_signing` offset + 1 | fail | fail | killed |
| A7 | ChainBound reads the cache (no write) | fail | pass | killed (differential) |
| A8 | `populate_cache` `<=` | fail | fail | killed (incl. Core `scan_boundary_rows`) |
| A9 | non-rotated takes the max hash | fail | fail | killed |
| A10 | rotated bound `>=` | fail | pass | killed (unit) |
| A11 | non-rotated hash compared in reversed byte order | fail | fail | killed (Core CL tests) |

---

## Findings

### m1 (minor): the rotation flag's block in `select_quorum_for_signing` is untested; mutant A5 survives
**File:** `dash-evo/src/llmq/selection.rs:135` (`is_quorum_rotation_enabled(ctx, params, start)`), together with the test `dash-evo/src/llmq/tests.rs:1820` (`the_production_rotated_scan_and_selection_equal_the_model`, `rotated_fixture()` = `Fixture::new(NEVER, 2, 0)`).

The code is right: it reads the flag at `start`, as Core's `IsQuorumRotationEnabled(llmq_params, pindexStart)` (`quorumsman.cpp:733`). But nothing pins it.

- The rotated differential's DIP0024 height is 2, so the flag is the same at every block of the fixture.
- The node-l1 twin and the ChainLock tests use a type that never rotates.

Replacing `start` with `tip`, both `BlockId`s in scope, passes the whole dash-evo lib suite and the whole `l1_llmq` suite. That slip is easy to make, and later callers (CH7b-3, then LK1b's IS selection and the quorum RPCs) would inherit it.

**Failure scenario:**

- Take a rotated type (IS `LLMQ_60_75`; regtest/devnet `LLMQ_TEST_DIP0024`) with DIP0024 active after the tip's cycle base − 1 but not after the start block's cycle base − 1.
- That happens when the cycle boundary lies between `signHeight − 8` and the tip: on regtest/devnet with `-testactivationheight=dip0024@h`, or when replaying at mainnet's DIP0024 activation.
- Core selects by the least `SHA256d(type, quorumHash, id)`. The mutant selects by the signer bits and a `quorumIndex` match. The two give a different quorum or none, so `NoQuorum` or `Invalid` where Core says `Valid`.

**Requested change:** add one differential case, with no production change. In a fixture whose DIP0024 height falls between the start's cycle base and the tip's:

- with `TIP = 120`, `select(ROTATED, s, -1, 8)` starts at 112, so cycle base 96 reads block 95, and the tip's cycle base 120 reads block 119;
- pick a height with DIP0024 inactive after 95 and active after 119;
- run `select(ROTATED, …)` over several selection hashes, with mined rows that make the hash-min choice and the signer-bit choice differ;
- assert that the model and the production side agree, and assert the expected base explicitly, so the case also catches a mutant applied to both sides.

Re-run A5 to confirm it dies.

### N1 (nit): `VerifiedQuorum`'s first doc sentence now overstates the invariant
**File:** `dash-evo/src/llmq/commitment.rs:652`.

**Failure scenario:** a reader of the type relies on "A quorum whose commitment passed `verify_commitment` with signatures" and treats a `VerifiedQuorum` as evidence that the commitment's signatures were checked. After LQ3b-1 that is false for `mined` (and was already false for `reloaded`), for example for quorums mined under `-assumevalid`. The added trailing clause names `mined`, but the lead sentence still states the old contract.

**Requested change:** reword the lead to say the key locks are verified against. It came from a commitment that passed `verify_commitment`, or (behind `reload`/`mined`) from a recorded verdict or a commitment the node's validation accepted.

### N2 (nit): no call-site check for `mined` in dashd-chainstate's dependents
**Files:** `dash-evo/tests/feature_gates.rs:18` (guard scope); `dash-evo/src/llmq/commitment.rs:719-721` (doc contract).

**Failure scenario:**

- Every crate that depends on dashd-chainstate gets `mined` by unification. Those are node-l1, dashd-node, rdashd, dashd-mining, conformance, node-fuzz and dash-testkit.
- Such a crate can call `VerifiedQuorum::mined` on a commitment that validation never accepted, for example a `qfcommit` from the wire before LQ2's verdict. That would build a `Verified` key from unvalidated bytes.
- The feature guard checks declarations only, and per-crate CI cannot catch it because those crates legitimately see the feature.
- The doc states the contract. Nothing enforces it.

**Requested change (optional, may ride CH7b-3):** extend `feature_gates.rs` with a source scan. It should list every `VerifiedQuorum::mined(` / `ScannedQuorum::mined(` call outside `dashd-chainstate/src` and `node-l1/tests`, and do the same for `::reloaded(` outside `dash-mnlist/src`, and fail on any new one. Alternatively, record the accepted call-site set in OD-CP-2.

---

## Notes (no change requested in LQ3b)

- **Continuation count (`nScanCommitments -= size`, `:228`).**
  - The code is right, at pre-existing `scan.rs:166`.
  - My A1 (`count`) and A2 (`count.max(keep)`) are killed only by the dash-evo unit test `a_hit_is_cut_or_continued_below_its_oldest_quorum` (`scan.rs:589`).
  - The Core-anchored LQ1a scan rows, which do include HitContinued (`l1_llmq.rs:431,456`), pass under both, and so do the LQ3b differential and twin.
  - The twin cannot reach a continuation (RESULTS says so).
  - So Core fidelity of the continuation count rests on one hand-written expectation. If CP wants it Core-anchored, a follow-up BL could add an oracle scan row where the continuation finds fewer quorums below the oldest cached base than `count` but at least `count - cached`.
- **The LQ3b-2 differential is a composition check, not an independent model.**
  - `reference.rs:973-1046` composes the same `scan_store_block`/`scan_quorums_from`/`select_from_scan` almost line for line (it differs only in reading `ctx.hash(store)` under the chain overload too, which Core does not need).
  - RESULTS m3 states this. Core fidelity comes from the twin and the LQ1a oracle rows, as the doc now says.
  - Mutants A4 and A7 are caught only there, which is adequate for composition-level slips.
- `select_start_height`'s Core cite (`selection.rs:27`, ":738") and `scan_store_block`'s (":171") are v24-generic line numbers, not rc.4's (:711, :170). This is pre-existing and outside the range.
- I found no Core divergence in LQ3b's production code.
