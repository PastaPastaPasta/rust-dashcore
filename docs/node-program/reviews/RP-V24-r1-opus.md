# RP-V24: first Opus high review

- **Reviewed:** `node/rp-v24` at `618428f0`.
- **Diff base:** `9d7e6c11`, the merge base with `node/integration`.
- **Order:** this pass runs after Sol r1 (APPROVE-WITH-NITS, `RP-V24-r1-sol.md`), per DEC-92. Sol's two minors are not repeated here: the stale profile docs and the release-guard bypasses (`Release::V24_0 == r`, `matches!`).
- **Context read:** REVIEW-COMMON, DEC-82, the O17 method note, and the in-flight branches `node/cs1-phase-b` (3733566d), `node/cs3-checks` (c4f1ffa3), `node/cs5b-inputs` (3e9a0660) and `node/bs15-x11-blockhash` (b8bc9160).
- **Setup:** detached worktrees under `/work/scratch/rp-v24-review-opus/`, `TMPDIR=/dev/shm/rp-v24-opus-tmp` (0700). The branch under review was not modified.
- **Scope of this file:** must-fix items first, then the checks that came back clean.

## Must-fix

### 1. Major: the DEC-82 guard runs in no gate that merges code, and CS1-b would bring the v23 axis back

`check-release-refs.sh` is the only thing that stops per-release code from coming back. The branch wires it into one place, `.github/workflows/rust.yml:110-111`, in the `pure-crates` job. That job is `if: needs.plan.outputs.hosted == 'true'`, so it is skipped while the base-sdk pin is `file://`, which is the state today (PROCESS "Gates on node/integration"; `BASE-SDK-FORK.md`). The agentbox gate does not list it either:

- PROCESS.md:41 "Node checks" names check-base-sdk-pins, deny, check-r2, check-pure and verify-groups only.
- The gate copies the in-flight tasks use do not run it: `/work/scratch/{cs1-b,cs3,cs5b,bs15-keep}/gate.sh`.
- RP-V24's own `/work/scratch/rp-v24/gate.sh` does not run it.
- Only the old CS0 gate copy did.

The failure scenario is concrete. `node/cs1-phase-b` modifies the file this branch deletes:

- `dash-consensus/src/release.rs` gains `mandatory_script_flags: ScriptFlags::MANDATORY_V23_1` in `UngatedRules::V23_1`.
- `release.golden` changes to match.
- Its consumers call `ScriptPolicy::of_release(rules(pin).mandatory_script_flags)` and `node_l1::rules(pin)`.

A rebase gives a modify/delete conflict on `release.rs`. The shortest route back to green is to keep the file and restore `pub mod release`: the "keep theirs" resolution, followed by re-adding `node_l1::rules`. That compiles and passes every gate step, and it brings back:

- `Release::V23_1`;
- `TimeSource::AdjustedTime`;
- the v23 `MANDATORY` mask as a selectable value.

CS3 and CS5b are in the same position with less to restore. They call `tx.txid(&UngatedRules::V24_0)` and `block.merkle(rules)`, and carry `ConsensusCtx { rules: &UngatedRules::V24_0 }` in many places. A rebaser who re-adds a one-variant `UngatedRules` stub to save edits re-creates the axis. Run against the CS1-b tree as it stands, the guard reports about 100 lines, so it would catch every one of these. It just never runs.

**Fix:**
- Add `contrib/node/pin-delta/check-release-refs.sh --selftest` and `contrib/node/pin-delta/check-release-refs.sh` to PROCESS.md's "Node checks", and to the gate copy the manager uses for the RP-V24 merge and every merge after it.
- In the CS1-b, CS3 and CS5b rebase briefs, say so explicitly:
  - resolve the `release.rs` modify/delete conflict by deleting the file;
  - fold `mandatory_script_flags` into `ScriptFlags::MANDATORY_V24_0` at the call site, which is CS1-b's `ScriptPolicy`;
  - drop the `rules` arguments; don't stub `UngatedRules`.

### 2. Major: under the documented gate environment, the live v24 scanner differential now passes vacuously

`dash-coreimport/tests/scanner_parity.rs:304-325` (`environment()`) now filters the dashd binaries to v24 and returns `None` ("skipped") when none is left. The test then passes. The documented gate environment is `eval $(python3 contrib/setup-dashd.py)` (PROCESS.md:40, and every gate copy). In it, `contrib/setup-dashd.py:26` still defaults `DASHVERSION` to 23.1.0, which commit b5f4d3d4 deliberately kept for the SDK's DKG tests. So `DASHD_PATH` is a v23.1.0 dashd, and `ST2_DASHD` is unset.

| Revision | What `scanner_parity` does in that environment |
|---|---|
| Base | Runs the real differential: damaged external block files through dashd and through `BlockFiles`/`load_external_block_file`, byte-compared blk/rev (`release_of` mapped v23 to the v23 finalize rule). |
| Head | Prints `skipped: … not v24` and reports `ok`. |

So the gate has lost its only live check of the ST2 import/scanner path against Core, and nothing reports it. RP-V24's acceptance asks for no v24 behaviour change "vectors + oracle diff". This differential is part of that evidence: the scanner and the rev-file finalize rule are the code this branch changed.

Only the implementer's private gate copy kept the coverage, because it exports `ST2_DASHD=…/dashcore-24.0.0-rc.3/bin/dashd` (`/work/scratch/rp-v24/gate.sh:7`). Sol's "2 passed" was likewise run with a v24 binary. The same applies to `blockfiles_parity`'s manifest cases (`manifest_case` returns `None` for non-v24). Those need `ST2_PARITY_DATADIRS`, which no gate sets, so that one is not a regression.

**Fix (both parts):**
- **(a)** Make the gate provide v24. Add `export ST2_DASHD=$HOME/.rust-dashcore-test/dashcore-24.0.0-rc.3/bin/dashd` to PROCESS.md's dashd line and the gate copies, or have `setup-dashd.py` emit a second variable for the node's v24 binary.
- **(b)** Make a skip visible. When `ST2_DASHD` is set and contains no v24 binary, fail rather than skip. Have the gate check the test's stderr for `skipped` the way it checks `receipt PASS`. Optional: also fail when `DASHD_PATH` is the only dashd and the gate sets something like `NODE_GATE=1`.

Run result in the documented environment (`eval $(python3 contrib/setup-dashd.py)`, no `ST2_DASHD`) at head: `cargo test --locked -p dash-coreimport --test scanner_parity` printed `skipped /…/dashcore-23.1.0/bin/dashd: not v24` and `skipped: no v24 dashd in ST2_DASHD or DASHD_PATH` for both tests, then `test result: ok. 2 passed; 0 failed` in 0.24 s, exit 0. A green result with nothing compared.

## Not must-fix (for the fix round's judgement)

- **Minor: the CS1 script recorder still needs v23.** `contrib/node/cs1-script-recorder/record.py:30-33, 80-85` still needs a v23.1.8 build tree to record v24 script vectors, and refuses to write if the two disagree ("an O17 PIN-DELTA finding"). CS1-b re-records through it, so v24 vectors still depend on v23 agreeing, and on the retired `oracle-shim-v23.1` worktree existing. The branch only added a comment. Make v24 the only required tree, and keep v23 as an opt-in check for history.
- **Nit: weaker checks on the private corpus's v23.1 cases.** `dash-consensus/tests/quirk_01_embargo_1.rs:90-97` stops checking txid, merkle root, mutation flag and txids on every `v23.1` case. For a case with no version-2+ asset unlock, those values are the same under both rules, so they could still be checked: skip them only when some transaction has `txid() != instance_hash()`. The receipt counts are unchanged (522), so this is check strength, not case count. Whether any v23.1 input lacks a v24.0 twin was not inspected (PLAN §7).
- **Nit: no fixture asserts a stable-txid unlock is present.** `conformance/tests/fixtures.rs:487-498` dropped the `stable == 1` assertion. The comment still says the stable-txid unlock is included, but nothing checks that a fixture with one is present. node-l1's `assembled_block_root` (oracle feature) and `conformance/tests/txid_rule.rs` still assert the shape exists. A one-line `assert!(… any tx with txid() != instance_hash())` in the fixture loop would restore it for `cargo test --workspace`.
- **Nit: Python fuzz tests in CS1-b still build v23.1 legs.** CS1-b's `node-fuzz/fuzzctl_test.py` additions build `Target(..., ("v23.1", "v24.0"), ...)` in memory and expect `diff_tx_decode@v23.1` legs. The registry check (`fuzzctl.py:117`) rejects v23.1 in `targets.toml`, so nothing ships, but those tests will keep testing two-pin plans after the rebase. Mention it in the CS1-b rebase brief.

## Checked and clean

- **O17 `profile` rows with real behaviour now take v24's side.**
  - SIG-01: `Tx::txid()` is `stable_txid.unwrap_or(instance_hash)`, and `Block::new` builds leaves from `txid()`. This is v24 `CTransaction::ComputeHash`: `IsAssetUnlockWithStableTxid` is shape-only (payload version ≥ 2), not gated by a deployment. Checked at `primitives/transaction.{h:392,cpp:93-127}` on tag v24.0.0-rc.3.
  - CONS-07/MPIX-01: the mask is `MANDATORY_V24_0 = 0x615`, and `block_script_flags(all) == MANDATORY_V24_0` is still asserted.
  - CONS-04: `header_check_time` is documented as NodeClock. No consumer at head computes adjusted time; CS3 and CS5b's `header.rs` read `ctx.header_check_time` only.
  - Rev-file finalize: only `height_last == undo_height_in_last_file` (bitcoin#27746) remains. The `tip_height` parameter is gone from every caller, including benches.
  - WAL-45 growth and CONS-28 unlink are unchanged.
  - The test rewrite `restart_before_a_rollover_leaves_the_rev_file_unfinalized` keeps the v24 expectation (`UNDOFILE_CHUNK_SIZE`).
  - No v23 default survived: `ChainParamsBuilder::new(_, Release::V24_0)` is the only release passed in node code.
- **No dead `#[cfg]`.** The only oracle cfg is `dash_oracle_unpinned`; `ffi.rs` instantiates `pin_module!` for v24_0 alone, and `Pin`, `network()` and `api()` have no v23 arms.
- **Fuzz runner and triage handle the dropped pin.**
  - `run.sh` refuses `--pin v23.1`.
  - The registry rejects v23.1 legs.
  - Legacy v23.1 runs are counted but never clustered.
  - Open clusters that name only v23.1 fall back to replay on the target's v24 leg (`fuzzctl.py:834`), so they can still close.
  - `legs_with_open_findings` ignores v23.1 keys because they are not plan legs.
  - Six legs were removed, not halved, as Sol's table shows; I agree with it.
- **Oracle coverage minimums.** The `require_per_pin` minimums match one pin. `bad-protx-payload` stays at 1. `next_work_matches_core_pow_test` is now stricter: it requires exactly `records × pins` runs.
- **In-flight branches beyond item 1.**
  - BS15 touches none of this.
  - The new `targets.toml` entries in CS3, CS5b and CS1-b are already `pins = ["v24.0"]`.
  - Their oracle tests compare on `Pin::V24_0`.
  - Their `if pin != Pin::V24_0` guards become dead after the rebase, which is harmless.
  - Every API they use that this branch removed (`UngatedRules`, `txid(rules)`, `merkle(rules)`, `node_l1::rules`) fails to compile, so the only silent route back is the stub or keep-the-file resolution in item 1.
- **Guard.** `check-release-refs.sh --selftest` and the full check pass at head.

## Verdict

**APPROVE-WITH-CHANGES.** Items 1 and 2 are both gate-side: wire the guard into the gate, and give the gate a v24 dashd with skips made visible. Neither is a consensus defect. No v24 behaviour was lost in the collapse.
