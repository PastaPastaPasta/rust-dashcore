# O2a review r2 (Opus): closure check, oracle shim v2 (ABI 6)

**Verdict: APPROVE-WITH-NITS.** My round-1 items and GPT's three majors are closed in code, and I verified each one independently. The pins reproduce: I rebuilt v23.1 plain myself and got the pinned bytes. The listdiff claim holds on the final archives. I found no regression in the shim or the bindings.

**One merge blocker, outside the O2a branch.** Merging `node/o2a-oracle-v2-merge` into `node/integration` has no textual conflicts, but it does not compile:
- `node-l1/tests/recorded_fixtures.rs:98` assigns the new `Rejection.result: ResultCode` to an `i32` field (finding N1).
- The fix is five lines. With it, node-l1 passes 26/26 on the ABI 6 archives and the recorded fixtures are unchanged.
- The fix must go into the merge (see the merge steps below).

The remaining items are documentation nits.

**Heads under review.**
- Core `node/o2a-oracle-v2-v24.0` a9a483e51e and `-v23.1` 55fc676e02. At both pins, `oracle.cpp` and `oracle.h` are byte-identical (`git diff`).
- rdc `node/o2a-oracle-v2` e0ed7276, and `node/o2a-oracle-v2-merge` fd1330c4.
- Evidence is in `/work/scratch/o2a-r2-opus/out/`.

## What I ran

| Check | Result |
|---|---|
| `cargo test -p dash-oracle-sys --features oracle --all-targets` at e0ed7276 on the pinned archives (no `DASH_ORACLE_ALLOW_UNPINNED`; TMPDIR=/dev/shm/o2a-r2-opus, CARGO_BUILD_JOBS=6) | **62/62 pass**: archive_check 8, contexts 9, contextual 15, script 12, smoke 18 |
| Installed archives vs pins.txt, all 4 (v23.1 plain b911a23b, fuzz 635d3d0b; v24.0 plain af924b63, fuzz 790dc62e) | sha256 equal. Every MANIFEST names its commit and recipe sha256 `428e540f…`, which equals `build-oracle.sh` at e0ed7276 |
| **Independent rebuild**: `build-oracle.sh --force --flavor plain --cache /work/scratch/o2a-r2-opus/repro-root-opus-r2 --jobs 6 v23.1`. This is a third cache root, with its own path length and a fresh depends tree; only the depends downloads are shared | **sha256 b911a23b…a6 = pins.txt**. The MANIFEST equals the installed one apart from `branch` (`repro-v23.1-plain-MANIFEST`) |
| Method of `check-oracle-repro.sh` (repro-r1b.log) | Sound: two fresh roots whose paths differ in length, separate depends caches, shared downloads only; both roots must agree with each other before pins.txt is recorded. All four archives reproduced in both roots. Limit: one host and toolchain, both recorded in the MANIFEST. That is acceptable for this pin's claim |
| **Replay of the implementer's 1,000 saved round-1 samples** (`dml-listdiff-1000-r1-samples.jsonl.gz`) on the pinned archives | **1,000/1,000 match at both pins** (`replay-1000.jsonl`) |
| **Collateral negative control**: all 18 samples that carry an external ProRegTx collateral, coins removed, view still at the tip | **18/18 `bad-protx-collateral` (Block(1)) at both pins** (`nocoins.jsonl`). The implementer checked 2; I checked every one. The at-tip re-check runs |
| **Fresh read-only RPC sample** (`--count 20 --seed 20261010`, one request at a time) | **20/20 at both pins**, heights 1,030,853–1,868,232, which extends round 1's 1.52M almost to V19. The sample included 2 non-null commitments, 1 of them punishing |
| My round-1 probe: the 7 punishing-commitment heights, fetched again from the RPC | **7/7 at both pins** |
| My round-1 probe: concurrency. `contextual` 8× at `--test-threads=32`, then `contextual`, `smoke` and my probes run at once | 0 failures |
| New probes for the context-above-tip class (`zz_r2_probe.rs`, scratch only) | All as expected at both pins; see "Context-above-tip class" below |
| Test merge of `node/o2a-oracle-v2-merge` into `node/integration` (7c4448ff) in a scratch worktree | Merges without conflicts. `cargo check` passes for cs2-diff, dde-probe/oracle and node-fuzz. **node-l1 fails** (N1). With the fix, `cargo test -p node-l1 --features oracle` passes **26/26** |
| node-fuzz with the fuzz flavor on the merged tree (`run.sh`'s exact RUSTFLAGS, `--locked`, `diff_tx_decode` and `diff_block_decode`) | Builds and links against the ABI 6 fuzz archives. I did not run the targets: they refuse to run outside `run.sh` (`NODE_FUZZ_RUN`), and I left that guard alone |

## Status per item

| Item | Status | Verified how |
|---|---|---|
| Opus 1: §22 provenance and re-run | **Closed** | §22 now states the round-0 facts: the 978/22 split, that the 22 were reclassified rather than re-run, and the unchecked coins. My replay of all 1,000 saved samples, my 18/18 negative control, 20 fresh blocks and the 7 PoSe heights all agree |
| Opus 2: `Context: Default` | **Closed** | `Default` is gone. `Context::new` takes every field. `CoinsView::{Unchecked, AtTip}`, `Checkpoints::{Disabled, Enabled{best_header_height}}` and `MnList::{Absent, Supplied}` make every choice explicit. Quorum members are framed one `CDeterministicMN` per entry; my probes show a member with a trailing byte, or one byte short, gives ORACLE_E_ARG. Residual, accepted: `adjusted_time = 0` still means unset, and only the header check reads it; it refuses 0 |
| Opus 3: MNHF/asset-unlock wording | **Closed** | oracle.h now says "every transaction of those two types once DIP0003 is active … (those reject strings get no verdict here)". COVERAGE E4, E6 and N7 say the same and point to O2a-LQ1 |
| Opus 4 (round-1 findings 4–10) | **Closed except 10b and 10c (nits) and 4c (accepted)** | Itemized below |
| 4a: silent truncation | Closed | `reject_reason_len`/`debug_message_len` (u32, saturating). The Rust flags are `len >= cap`, which matches `CopyString`'s cap-1 |
| 4b: untyped result | Closed | `ResultCode::{Tx, Block, Bool}`, set per entry point: `check_special_tx`, `check_tx_inputs` and `contextual_check_tx` give Tx; `dml_apply`, `contextual_check_header` and `contextual_check_block` give Block; `verify_commitment` gives Bool. This is the change behind N1 |
| 4c: lossy UTF-8 | Not done; accepted | Core's reject reasons are ASCII. Raw bytes can wait for a real need |
| 5: BLS flag restore | Closed | `ScopedBlsScheme` (RAII, exchange and store) in `payload_decode` |
| 6: thin smoke coverage | Closed | Release-dashd regtest verdicts (`contextual-regtest-<pin>.json`) for `bad-diffbits`, `time-too-old`, `time-too-new`, `bad-version(0x00000003)`, `bad-txns-nonfinal`, `bad-cb-height`, `bad-txns-cb-type`, `bad-txns-type` and (v24) `bad-protx-payload`, compared as the first rejection in TestBlockValidity order. Height and time relative locks on both sides of the boundary, before and after DIP0003. `Coverage::require` pins the counts |
| 7: wrap check | Closed | `build-oracle.sh` requires the `__real_` references and the `__wrap_` definitions to be equal sets. `nm` runs without `\|\| true` and filters in awk |
| 8: per-pin depends cache | Closed | `depends_cache <git rev-parse commit:depends>` is in the MANIFEST, with a lock per id. My rebuild used a fresh one and matched |
| 9: branch notes | Closed | README lines 14–17 describe the review-branch and fast-forward flow |
| 10a: ID clashes | Closed | Contradictions are now X1–X11 and the ChainLock rows CL1–CL3 |
| 10b: G1 owner | **Open (nit)** | COVERAGE:209 still says "`oracle_block_value_payee` … (O2a, **not written**)". It should say O2a-GV, as Y5 and N21 do |
| 10c: spork row | **Open (nit)** | §14 has no spork row (my proposed V5: SPORK_19 ChainLock enforcement, SPORK_3 IS filtering, SPORK_9 superblocks). Add it, or record in DEC-70 why not |
| 10d–10h | Closed | M1 now says reload order and canonical bytes in O2a-EV; E4/E6/N7 wording fixed; §22 caveats present; header says DEC-60; N13 is owned by BS12 |
| Opus 5: fast-forward prep | **Closed** | `node/oracle-shim-v24.0` (23769258) and `-v23.1` (350ce242) are ancestors of the new heads, and no other branch in `~/workspace/dash` contains them. Both `dash-wt/oracle-shim-*` worktrees are clean. `node/o2a-oracle-v2-merge` = e0ed7276 + one commit (fd1330c4) that changes **only pins.txt column 3** of both rows; commit and sha256 columns are unchanged, so the cached archives stay valid |
| GPT 1: absent MN list | **Closed** | `mn_list_present` is explicit. `MissingRead()` reads `CDeterministicMNManager::m_initial_snapshot_index` through private access. That member is set exactly where Core bootstraps an empty "initial snapshot" for a DIP3-active block with no snapshot and no diff: v24 deterministicmns.cpp:883, v23 :814. Pre-DIP3 reads return before evodb at both pins, as a node's do. The check runs on success too, so an absent list can no longer cause a false accept (`duplicate_registration_needs_the_list`). v24's dual-chainstate throw is unreachable: the in-memory evodb has no marker, and the v24 test passes |
| GPT 2: later checkpoints | **Closed** | Checkpoints in (tip, best_header_height] become block-map entries. v24 uses `ChainstateManager::Options.checkpoints_enabled`; v23 uses the `fCheckpointsEnabled` global, saved in a member and restored in the destructor. It is set last in the constructor, so a throw can't leak it. The only other reader in v23 is validation.cpp:4127, inside ContextualCheckBlockHeader, which only contextual calls reach, under `ProcessStateMutex` and `cs_main`. A best header below the tip is ORACLE_E_ARG (probed) |
| GPT 3: overflow | **Closed** | `CheckListState` guards the tip list before Core's int arithmetic. I re-derived the bounds: PoSePunish adds `CalcPenalty(66)` ≤ (INT_MAX/200)·66/100 to a penalty ≤ INT_MAX/2, then clamps to CalcMaxPoSePenalty, so repeated punishments in one block cannot accumulate. Member states never reach arithmetic: v24 specialtxman.cpp:283-297 reads only `proTxHash` after `HasMN`. The UBSan evidence is real: the positive control aborts on X11's misaligned store without suppressions, and the 1,000 replay plus the boundary run show 0 `runtime error` lines |
| Self-found: checkpoint as quorum base | **Closed** | `oracle_verify_commitment` now also requires `Tip()->GetAncestor(base->nHeight) == base` |

### Context-above-tip class (new probes, both pins)

A checkpoint entry has no `pprev`, so any path that calls `GetAncestor` on it, or treats it as ancestry, is a hazard. I listed every `LookupBlockIndex` in `src/llmq` and `src/evo` at both pins and checked each path an adapter reaches:

- **`verify_commitment`.** Fixed (self-found); the test covers it.
- **`check_special_tx`, QcTx.** `CheckLLMQCommitment` (commitment.cpp v24:281-290, v23:274-283) already rejects a non-ancestor base with `bad-qc-quorum-hash` before `Verify`. My probe, a QcTx at tip + 1 whose quorumHash is checkpoint 1,850,400:
  - checkpoints disabled, or best header = tip: ORACLE_E_CONTEXT (`QuorumBaseInContext`);
  - best header = 1,850,400 or INT_MAX: **`bad-qc-quorum-hash`, Tx(1)**. That is a node's verdict, whether or not the node knows the header.
  - In every mode the mined QcTx is still accepted.
  - So the answer is either Core's verdict or a conservative refusal, never a wrong verdict.
- **`dml_apply`.** RebuildListFromBlock takes the base from `pindexPrev->GetAncestor(quorumHeight)` (v24:715, v23:448), so it can never reach a checkpoint entry.
- **`sequence_locks`.** Already refuses prev heights above tip + 1.
- **MNHF and asset unlocks**, the other `LookupBlockIndex` users, are refused (E4 and E6).
- **`ContextualCheckBlockHeader`** reads only `GetLastCheckpoint`'s height.

Other probes:
- `MnList::Supplied(vec![])` (present, but no bytes) is ORACLE_E_ARG.
- Bytes with an absent flag cannot be expressed from Rust; the shim refuses them.

## New findings

### N1 (blocker for the merge, not for the branch): ABI 6's `ResultCode` breaks node-l1 on node/integration

- **Where:** `node-l1/tests/recorded_fixtures.rs:98` on node/integration (7c4448ff). It feeds `CheckBlockCase.result: i32` (`dash-consensus/tests/support/recorded.rs:40`), which dash-consensus shares.
- **Repro:** in a worktree of node/integration, run `git merge --no-ff node/o2a-oracle-v2-merge && cargo check -p node-l1 --features oracle --all-targets`. The result is `error[E0308]: mismatched types … expected i32, found ResultCode`. A branch gate would fail at compile time.
- **Fix:** apply `/work/scratch/o2a-r2-opus/out/node-l1-resultcode-fix.patch`. It maps `ResultCode::Block(code) => code` and panics on any other variant. With it:
  - `cargo test -p node-l1 --features oracle` passes 26/26 on the ABI 6 archives (boundaries 9, coin_boundaries 4, coin_records 1, twins 6, private_seed_replay 4, recorded_fixtures 2);
  - `recorded_fixtures_match_the_oracle` still equals the committed JSON, so the recorded verdict is unchanged.
- **Other in-flight branches:** eight branches carry the same line only through node/integration (bs5-adapt, bs7-p2p-core, ev0-evo-interfaces, st3a-power-loss and others). They pick up the fix when they next merge node/integration. None adds a new `Rejection.result`-as-`i32` use.
- **Other consumers:** cs2-diff, dde-probe/oracle and node-fuzz compile unchanged. CS0's `node-fuzz/run.sh` and O3's `fuzzctl` read only pins.txt columns 1, 2, 4 and 5 for provenance (`oracle_commit`, `oracle_archive_sha256`); neither has a hard-coded pin or ABI. CS1's recorder links only libbitcoin_{consensus,common,util} of `dash-wt/oracle-shim-*`, which the fast-forward does not change (it touches oracle.* and validation.cpp). `record.py --check` reports, but does not fail on, the provenance naming a new commit.

### N2 (minor, docs): ABI 6 is not reflected everywhere

- `dash-oracle-sys/README.md:35` is titled "Entry points (ABI 5)".
- The README's context paragraph lacks the checkpoint setting and list presence.
- The README's "`ErrorKind::Context` is the one deliberate domain restriction: a header window shorter than…" is now wrong. ErrorKind::Context also covers:
  - an absent masternode list, or a read of another block's list;
  - members missing from the seam;
  - every MNHF and asset-unlock transaction after DIP0003;
  - a commitment's base outside the headers.
- COVERAGE.md §1:
  - line 40 says "ABI 5" and "not merged yet";
  - line 45 says "format 1", and its field list lacks the checkpoints and the list-presence flag;
  - line 54 says "(oracle.h, ABI 5)".
  - Line 3 already says ABI 6.
- Fix these in the merge, or in a docs commit right after it.

### N3 (nit): §22's UBSan sentence

"…replay with no report and all equal, and so do the 7 blocks…" reads as if the boundary run were also all equal. `pose-boundary-ubsan.log` records 7/7 **mismatch**, which is by design: penalties were raised to INT_MAX/2, and `pose-boundary.py` says so. Reword it to "the 7 boundary variants run with no report (their listdiffs differ by design; the punished member is clamped and banned)". I checked that clamp and ban in the oracle output (`PoSeBanHeight` 1,099,043).

### N4 (note, procedure): pins.txt names the branch, so order the merge steps

After fd1330c4, pins.txt's branch column says `node/oracle-shim-*`. `check-oracle-repro.sh --update-pins` and `build-oracle.sh --update-pins` resolve that branch to a commit. If either runs before the Core fast-forward, it silently moves the pins back to 23769258/350ce242 and blanks the sha256 columns. Fast-forward Core first, and check that the branches resolve to the new heads before merging the rdc side.

### N5 (note, outside O2a's scope)

- `contrib/node/cs2-diff/Cargo.lock` is stale on node/integration itself: `cargo metadata --locked` fails at 7c4448ff without O2a, and a plain `cargo check` adds secp256k1 and others. Mentioned so the gate isn't blamed on O2a.
- The `~/workspace/rdc-wt/integration` worktree has uncommitted files from other threads (`designs/CH2-coins-flush.md`, a DW review). They don't overlap with O2a's paths, but don't sweep them into the merge commit.

## Recommended merge steps

1. **Check the heads:**
   ```
   git -C ~/workspace/dash rev-parse node/o2a-oracle-v2-v24.0 node/o2a-oracle-v2-v23.1
   #  a9a483e51efdadeee37cf40ff8697da62b3cd012 / 55fc676e02c63828c6ec61ff92e4851223f26d44
   git -C ~/workspace/rust-dashcore rev-parse node/o2a-oracle-v2 node/o2a-oracle-v2-merge
   #  e0ed7276… / fd1330c4…
   git -C ~/workspace/dash-wt/oracle-shim-v24.0 status --porcelain   # empty
   git -C ~/workspace/dash-wt/oracle-shim-v23.1 status --porcelain   # empty
   ```
2. **Fast-forward Core** (the branches are checked out, so use `merge --ff-only` in their worktrees):
   ```
   git -C ~/workspace/dash-wt/oracle-shim-v24.0 merge --ff-only node/o2a-oracle-v2-v24.0
   git -C ~/workspace/dash-wt/oracle-shim-v23.1 merge --ff-only node/o2a-oracle-v2-v23.1
   git -C ~/workspace/dash rev-parse node/oracle-shim-v24.0 node/oracle-shim-v23.1   # = the two heads above
   ```
3. **Merge rdc with the node-l1 fix in the merge commit**, so every commit on the first-parent line builds:
   ```
   cd ~/workspace/rdc-wt/integration            # on node/integration; leave others' uncommitted files alone
   git merge --no-ff --no-commit node/o2a-oracle-v2-merge
   git apply /work/scratch/o2a-r2-opus/out/node-l1-resultcode-fix.patch
   # optional, same commit: the N2 doc fixes (README "ABI 6" and the ErrorKind::Context paragraph;
   # COVERAGE §1 lines 40/45/54, "merged"), N3's wording, G1 → O2a-GV (10b)
   git add node-l1/tests/recorded_fixtures.rs [docs…]
   cargo test -p dash-oracle-sys --features oracle        # pinned archives, no ALLOW_UNPINNED
   cargo test -p node-l1 --features oracle
   # + the usual branch gate
   git commit   # "merge: O2a oracle shim v2, ABI 6 contextual entry points + COVERAGE.md (…; branch gate green N tests)"
   ```
   Alternative: merge `node/integration` into `node/o2a-oracle-v2-merge`, commit the fix there, run the gate on that branch, then `merge --no-ff` it, the same way O3 was "gated at b59e6525".
4. **After the merge:**
   - `contrib/node/build-oracle.sh --flavor plain` and `--flavor fuzz`, without `--force`: both pins should log "cached … (sha256 verified)".
   - One `fuzzctl` smoke on the fuzz flavor, so a `run.sh` verdict records `oracle_commit` a9a483e51e1f / 55fc676e02c6.
   - Open divergence clusters keep their old provenance; replay them once against the new archives during the next triage.
5. **Keep for rollback:** `/work/cache/oracle/{23769258…,350ce242…}`. Delete the `node/o2a-oracle-v2*` branches only after the merge commit lands.

## Summary

- **Fix round 1 holds.** Every round-1 item from both reviewers is closed and independently verified.
- **The pins reproduce:** my rebuild of v23.1 plain gives the pinned bytes. 62/62 crate tests pass on the pinned archives.
- **The listdiff claim holds:** the 1,000-sample replay is 1,000/1,000 at both pins, 18/18 collateral controls reject, and 27 fresh RPC blocks up to 1.868M match.
- **The context-above-tip class is closed** on every path an adapter reaches.
- **Before or in the merge:** fix N1 (a five-line semantic conflict in node-l1 that only appears on node/integration).
- **Ideally in the same merge:** N2, N3, 10b and 10c (docs).
- **Order the merge steps** as N4 says.
