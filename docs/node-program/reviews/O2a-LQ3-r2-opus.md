# O2a-LQ3 oracle lock verdicts (ABI 25): Opus-h review, round 2 (narrow)

**Ranges:**
- rdc `node/o2a-lq3-oracle`: `74abb9c42..ea84aaa0e` (1 commit: ea84aaa0e, test-only).
- Core: pin `808f56f7d2` is unchanged. The comment-only side branch `node/o2a-lq3-oracle-v24.0-nits` is at
  `3978ecfdabb`.

**Core reference:** Dash Core v24.0.0-rc.3. Line numbers are rc.3 unless marked "pin".
**Reviewer:** Opus 5.5, high effort (independent; no edits, commits or pushes to either branch).
**Scope:** the r1 findings (F1–F7, N1–N6) against the fix commit, and new D21–D28 with amended D6, D10 and D14.
Code the fix did not touch is not re-reviewed.

**Verdict: ACCEPT WITH NITS**

Both required findings are closed, and I reproduced each kill:
- **F2:** the active window's upper edge is now compared with Core on a real chain.
- **F3:** the ChainLock sign offset is now compared at its boundaries. Every failing pass's quorum is now
  compared with the quorum our selection names.

The minor findings are handled as follows:
- F4, F5 and F7 are fixed.
- N2 is fixed.
- The F1 and F6 deferrals match the schedules r1 gave.

None of the new checks is vacuous. I counted what each one compares (scope 3). The fix broke nothing I could find.
The three nits below are optional.

## Findings

### R2-N1. Nit (test): the `isdlock` test infers equal pass statuses from equal quorums

- **Where:** `node-l1/tests/l1_llmq.rs:2461`, D23.
- **What it does:**
  - For a non-rotated type, D23 asserts that both passes' quorums equal the named quorum. From that it
    concludes that "the last pass invalid" equals "any pass invalid".
  - The conclusion holds. At the pin, the non-rotated branch is `m_qman.GetQuorum(type, islock->cycleHash)` in
    both passes, and `signOffset` is ignored (`instantsend/net_instantsend.cpp`, pin, after the `nSignHeight`
    computation). The same quorum, message and signature give the same status.
- **Problem:** the property LK1 will rely on is pass-status equality. The test asserts it only by implication.
- **Optional fix:** carry `pass_status` in `CoreLock` and assert `passes[0].status == passes[1].status` next to
  the quorum check. This makes D23's premise explicit, and a rotated capture (D17) will then fail loudly at exactly
  the point where the premise stops holding.

### R2-N2. Nit (test strength, informational): only two of D22's three boundary pairs change the selected quorum

- **Where:** `node-l1/tests/l1_llmq.rs:2274` (boundaries), D22.
- **What I measured** (instrumented copy, `selected_quorum` per boundary height, all three captured ChainLocks,
  both schemes):

| Height | Selected quorum | Effect |
|---|---|---|
| 162 | none | first quorum not yet in the scan |
| 163 | Q155 | outcome flips (`invalid: false` → `true`) |
| 186 | Q155 | |
| 187 | Q179 | selection switches |
| 210 | Q179 | |
| 211 | Q179 | **no change**: the 203 quorum enters the scan but does not win for these request ids |

- **Assessment:**
  - The 162/163 pair is the one that kills M5± and SEL7/SEL9.
  - The 186/187 pair independently discriminates a selection drift.
  - The third pair still adds compared cases but does not discriminate. This needs no action, since the uniform
    offset mutants are already killed twice over.
  - D22's wording ("puts the quorum's mining block on either side of the start height") is accurate about scan
    contents, not about the selected quorum. A clause would avoid over-reading it.

### R2-N3. Nit (tracking, for the manager): the deferred items live only in the design notes

- **Where:** D17 + D25 (one follow-up capture row: rotation plus a late-V19 legacy accept), D23 (LK1 must use the
  last pass's status before banning on an `isdlock`), D28 (F1 squash with the nits side branch, N4, N6 at
  stacking).
- **Problem:** I found no BACKLOG or PLAN row for the follow-up capture or for D23's LK1 consequence.
  - BL-209, the LQ3a "before LK1 calls the lock checks" row, is the natural home for D23.
  - The capture row has no home yet.
- **Fix:** file the capture row and add D23 to BL-209. Make sure the stacking items (F1 squash including
  `3978ecfdabb`, N4, N6) are written into the train driver's bundle D-row when this rider stacks.

## Scope answers

### 1. Required findings

**F2 → D21: closed.**
- `recovered_sigs_of_every_mined_quorum_match_cores_active_window` (`:2608`) loads `llmq-regtest-v24.0` (tip 452).
- Non-rotated quorums: five `llmq_test` quorums, mined at 155, 179, 395, 419 and 443. Bases 144 and 168 are past
  the `keepOldConnections` = 3 window.
- The test compares 10 cases (5 quorums × 2 schemes). Core's outcomes are:
  - `{NotVerified { invalid: true }, Refused(InactiveQuorum), Refused(MalformedSignature)}`;
  - inactive per type `{100: 2}`, which equals `n − keepOldConnections`.
- Under the legacy scheme, the inactive quorums give `InactiveQuorum` and the active ones `MalformedSignature`. So
  the test also confirms, on a real chain, that `IsQuorumActive` runs before the signature read (`llmq/signing.cpp`
  pin :429, `IsQuorumActive` at :415).
- **M1** (`keep_old_connections + 1`) is **killed** (rerun by me):
  - "qsigrec type 100 quorum 168 … left: NotVerified { invalid: true }, right: Refused(InactiveQuorum)".
  - This matches the owner's log.
- `windows > 0` and the per-type equality keep the test from passing on a chain that mines no excess quorums.
- `lock_chain_of` now skips rotated commitments. `lq3-regtest-v24.0` mines none (D17), so the existing four tests
  see the same quorum set as before.

**F3 → D22: closed, both parts.**
- **(a) Boundary heights.** Each `mined + 7` / `mined + 8` for the ChainLocks type is added: 162/163, 186/187,
  210/211.
- **(b) Failing passes.** `core_lock_outcome` now keeps `pass_quorum` on every outcome (`CoreLock`). On every
  `NotVerified`:
  - `clsig`: the pass quorum must equal `select_start_height` + `select_from_scan` over Core's scan (`:2184`,
    `:2317`);
  - `isdlock`: the named quorum or none, for both passes (`:2461`);
  - `qsigrec`: the named quorum (`:2566`).
- **Counts** (instrumented copy):
  - 79 `clsig` pass-quorum comparisons have a quorum (`selections`), plus the no-quorum ones;
  - the `isdlock` test has 12 named and 3 not named.
- **Mutants:**
  - **M5−** (`SIGN_HEIGHT_OFFSET as i32 - 1` in `locks/mod.rs`) is **killed** (rerun by me): "clsig 0 height 162
    … left: NotVerified { invalid: true }, right: NotVerified { invalid: false }".
  - The owner's log also shows M5+ killed at 163, and SEL7/SEL9 (the `selection.rs` constant) killed by the new
    pass-quorum assert at 162/163.
  - I did not rerun those three. Their failure sites and values are consistent with the boundary table above: the
    first quorum enters the scan at 163.
- **SEL7/SEL9 check:** the test's own `offset` reads the mutated constant, so the boundaries shift with it.
  - The kill still comes from comparing against Core's `pass_quorum`, which uses Core's 8.
  - This is a genuine differential kill, not a self-consistency check.

### 2. Minor findings and deferrals

- **F4 → D23: handled.**
  - `NotVerified { invalid }` is now the last pass's status (`:2104`).
  - At the pin, Core runs pass 0 with `ban=false` and retries only the bad locks with `ban=true` at `dkgInterval`
    (`net_instantsend.cpp`). So the last pass carries the ban decision, as D23 says.
  - The non-rotated premise is enforced by the both-passes quorum assert (R2-N1 suggests asserting the statuses
    as well).
  - The rotated consequence is recorded for LK1 (R2-N3: give it a row).
- **F5 → D24: handled.**
  - **M2** is killed by `islock_signs_at_the_tip_when_the_tip_is_the_cycle_end` (`dash-evo/src/locks/tests.rs:748`;
    rerun by me: it panics at `:762`, "pass 1 at the tip").
  - The test's edge matches Core at the pin, `cycleHeight + dkgInterval < tipHeight`.
  - M2 still survives the node-l1 differentials, as expected: there is no rotated capture (D17), and r1 scheduled
    M2's oracle coverage with D17.
  - **M6** is killed by "isdlock 0 no inputs, unknown cycle" (owner's log: `Refused(UnknownCycle)` against Core's
    `Refused(TriviallyInvalid)`). The four paired cases (`:2402`) are the ones r1 asked for. "Unknown cycle, zero
    sig" also pins cycle-before-signature.
  - The new dash-evo test touches LQ3a's `tests.rs`. LQ3a's later head (`a3088713d`) changes only docs past this
    base, so there is no conflict.
- **F6 → D25: acceptable as recorded.** It matches r1's schedule ("with the D17 follow-up capture"), and LQ3a
  D20–D21 cover legacy acceptance in unit tests meanwhile. It needs a tracking row (R2-N3).
- **F7 → D26: handled.**
  - Each of the three lock tests now asserts set equality over the full `Debug` outcome (`assert_outcomes`,
    `:2175`). The expected sets match what r1's instrumentation found, including `NotVerified { invalid: false }`
    for `clsig` and `isdlock`.
  - Leaving `qsigrec`'s no-quorum pass out of the expected set is justified in a comment (LQ3a D6).
  - The D21 test uses `contains`, which is right: its legacy outcomes depend on the borrowed signature.
- **N2 → D27: handled.** The "a byte after" case is gone. `Trailing` now falls through to `panic!("Core: …")`, so
  a future case that reaches it fails loudly instead of comparing two refusals. D6 says the refusal is the shim's.
- **N1, N3, N5 (Core side branch `3978ecfdabb`):**
  - The diff from the pin is comment-only. Every changed line is a comment; there are no code tokens.
  - It fixes the three citations (`quorumsman.cpp:790`; `commitment.cpp:301` via `specialtxman.cpp:258`;
    `blockprocessor.cpp:267-283`).
  - It adds the ChainLocks-enabled and unrequested-CLSIG wording, and the hash-order comment.
  - The rdc mirror (`dash-oracle-sys/src/llmq.rs` `verify_chainlock` doc, and D10) matches.
- **F1, N4, N6 (D28): acceptable as recorded.** r1 allowed the train driver to squash at stacking. The side branch
  must be included in that squash (R2-N3).

### 3. Regressions and vacuity

- **Nothing broken.** The fix commit is test- and doc-only. No re-pin; ABI 25 is unchanged.
- **New guards** (each confirmed non-zero above):
  - `selections > 0` in the `clsig` and `isdlock` tests;
  - `windows > 0` and the per-type count in D21.
- **Boundary heights:** every one is ≤ the `lq3-regtest-v24.0` tip (214), so all are reached. Two of the three
  pairs discriminate (R2-N2).
- **Set equality:** the sets are non-empty and exact.
- **`selected_quorum`** panics if the view does not hold the scan, rather than skipping. Heights whose start is
  negative or past the tip give `None` on both sides, consistently with Core's `SelectQuorumForSigning` (-1 reads
  as the tip on both sides).

## What I ran

All runs used `CARGO_BUILD_JOBS=12` and `DASH_ORACLE_CACHE=/work/cache/oracle` (pinned archives), with
`DASH_ORACLE_ALLOW_UNPINNED` unset. Each run used its own detached worktree at `ea84aaa0e` and its own
`CARGO_TARGET_DIR` under `/work/scratch/o2a-lq3-r2-opus`, seeded by reflink from the owner's target with every
`.rs` and `Cargo.toml` touched. Both were removed afterwards. Load was 50–180.

- **Baseline:**
  - `cargo test -p node-l1 --features oracle --test l1_llmq`: 26/26, including the four O2a-LQ3 differentials and
    the new D21 test.
  - `cargo test -p dash-evo --lib locks::`: 44/44.
- **Mutants**, applied to the scratch copy and restored after each:
  - M1: killed (D21 test).
  - M5−: killed (`clsig` test, height 162).
  - M2: killed (dash-evo unit test).
  - The owner's log (`/work/scratch/o2a-lq3/mut/run.log`) also covers M5+, SEL7, SEL9 and M6. All are killed, and
    M2 survives node-l1 as expected. I read the runner: it used the pinned cache and unset `ALLOW_UNPINNED`.
- **Instrumentation** (scratch copy only):
  - the D21 mined list, inactive counts and outcome set;
  - the `clsig` pass-quorum comparisons by height and selected quorum;
  - the `isdlock` named and unnamed counts.
- **Gate:** I read the GATE-FAST tier 1 run `1018-174352-gate-ea84aaa0e-NmK`. All 16 steps exit 0
  (`sb12a-captures` skip), and bench_inputs are all SAME (BENCH-EXIT 0).
- **Static checks:**
  - the `74abb9c42..ea84aaa0e` diff;
  - the `808f56f7d2..3978ecfdabb` Core diff;
  - Core at the pin: `instantsend/net_instantsend.cpp` (sign height, non-rotated `GetQuorum`, the two-pass ban)
    and `llmq/signing.cpp` (`IsQuorumActive` call sites);
  - the `llmq-regtest-v24.0` fixture's quorum list.
