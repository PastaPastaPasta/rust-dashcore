**Verdict: ACCEPT — 0 blocker, 0 major, 0 minor, 0 nit (m1 closed, N1 closed, N2 stays no-change as agreed)**

# LQ3b r2 re-review (Opus, narrow): `89c6b40c4..3733246f9` on `node/lq3b`

Scope: correctness only. I checked closure of my r1 m1 and N1, and looked for defects introduced by `0718d28ff`, `cc1b723c1` and `3733246f9`.
- Core reference: `v24.0.0-rc.4`.
- Worktree `~/workspace/rdc-wt/lq3b` (HEAD 3733246f9, clean) was read only.
- Mutants ran in `/work/scratch/cp/reviews/lq3b-opus-copy`. I refreshed it by `rsync --delete` (excluding `.git` and `target`), checked it with `diff -rq` as identical to the worktree, kept its `.git` as `gitdir: /nonexistent-mutant-copy`, and ran no git in it. After the run I restored it, and it is again identical.
- Target: `/work/scratch/cp/reviews/lq3b-opus-target`.
- Unit `cp-lq3b-opus2-a5` ran under `heavy.lock`, after load5 84.9 and 234G free were confirmed. Logs, script and table are in `/work/scratch/cp/reviews/lq3b-opus-run2/`.

## m1 (rotation flag read at the start block): CLOSED

**A5 rerun** (the same textual mutant as r1, `is_quorum_rotation_enabled(ctx, params, if opmut(5) { tip } else { start })` at `selection.rs:135`):

| run | `cargo test -p dash-evo --lib` | failing |
|---|---|---|
| none (also `--test feature_gates`) | 165/165 ok; feature_gates 3/3 ok | — |
| A5 (`LQ3BOP=5`) | 164 passed, **1 failed** | `llmq::tests::the_production_selection_reads_rotation_at_the_start_block` |

The kill is at `Select { LlmqTestDip0024, selection: 2, sign_height: -1, offset: 8 }`:
- the mutant returns base 96, index 0 (the signer bit);
- the model, and Core's score, return base 97, index 1.

**The test also catches a mutant applied to both sides.** It asserts `bases == expected`, where `expected` is computed in the test itself. That computation is a `sha256d(type u8 ‖ quorumHash ‖ selectionHash)` byte-order minimum over the two scanned quorums (96, 97). It does not reuse `select_from_scan`. So a slip that reached both the model and production would still fail.

**The test cannot pass vacuously.** It asserts that at least one selection gives a different quorum by score than by signer bit. `selection_hash` forces bit 62 of `GetUint64(3)` to `s & 1`. With `signingActiveQuorumCount = 2` (`n = 1`), Core's `signer = 1 & (b >> 62)` is exactly that bit, so "odd selections choose index 1" holds.

**The precondition is real, at the heights Core reads:**
- Core rc.4 `quorumsman.cpp:733` evaluates `IsQuorumRotationEnabled(llmq_params, pindexStart)`. `options.cpp:50-61` reads `DeploymentActiveAfter(pindex->GetAncestor(base - 1), DIP0024)` with `base = h - h % dkgInterval`.
- Regtest `llmq_test_dip0024` has `dkgInterval = 24` and `signingActiveQuorumCount = 2` (rc.4 `llmq/params.h:226-239`).
- The fixture sets `-testactivationheight=dip0024@110`. Its `Heights::active_after` is Core's buried rule, `prev.nHeight + 1 >= height` (`tests.rs:200-204`).
- At the start block (`TIP − 8 = 112`): the base is 96 and Core reads block 95. 95 + 1 = 96 < 110, so rotation is **off**.
- At the tip (120): the base is 120 and Core reads block 119. 119 + 1 = 120 ≥ 110, so rotation is **on**.
- The test asserts both values (`!rotation(TIP - 8) && rotation(TIP)`) before it selects.
- The state is reachable in Core. DIP0024 is active after 112 (113 ≥ 110), so the rotated type is enabled at `pindexStart` and ScanQuorums proceeds. Only the rotation flag is off. A regtest or devnet chain with DIP0024 activating inside a cycle is in exactly this state.

The implementer's table (`/work/scratch/cp/lq3b/RESULTS.md`, mutant 11) agrees: A5 is killed only by this test.

## N1 (`VerifiedQuorum`'s lead overstated the invariant): CLOSED

`commitment.rs:652-659` now opens with "The identity and public key of a quorum, the only key locks are verified against". It then names all three sources: a `verify_commitment` pass with signatures, a recorded verdict (`reload`), and a validation-accepted mined commitment (`mined`). It also says outright that "not every value attests that its signatures were checked". That is the requested wording. It is doc only, with no code change.

## N2: no change requested (unchanged; CP agreed in r1)

## Commits introduced in the round: no defect found

**`0718d28ff` (feature guard, TOML workspace discovery).** I read the full diff.
- Roots are now every manifest whose parsed TOML has a `workspace` key. That catches `[workspace] # …`, `workspace = {…}` and dotted forms, which the literal-line match missed.
- A manifest with a `package` table that no root's read returns is read through its own manifest. Then `assert!(… is in no read)` runs, so no package escapes silently.
- A stray manifest that is neither a member nor excluded makes `cargo metadata` fail, and the guard fails loudly with it. That is fail-closed, which is acceptable for a guard.
- `check` requires each enabling set to be exactly `[owner]`. So dropping the old "root found / >20 packages" sanity asserts cannot make the real-tree test pass vacuously: a guard that read nothing would fail.
- The probe writes under `CARGO_TARGET_TMPDIR`, which is set for integration tests, and only uses `--no-deps` with path deps, so it is offline-safe.
- `toml = "0.9"` is a dev-dep only. The lockfile delta is the single dependency-list line (0.9.12 was already locked).
- The clean run passes feature_gates 3/3.
- My r1 manifest mutants G1–G5 are not affected by this change, because they test `enables()`, which is unchanged. I did not re-run them.

**`cc1b723c1`.** Doc only; see N1.

**`3733246f9`.** Tests only.
- The start-block test is covered above.
- `the_production_scan_cache_keeps_types_apart` puts `LlmqTest` and `LlmqTestPlatform` (whose windows it asserts equal, so the store block is the same) in one `PerTypeCache`, in both orders. It expects Miss, Miss, HitSatisfied, HitSatisfied, with distinct bases per type: [96, 72] versus [96, 48].
- A cache keyed without the type would return the other type's entry on the second Miss, or the wrong bases on the hits. The implementer's mutant 12 confirms this.
- No production line changed in this commit.

## Verdict

**ACCEPT.** 0 blocker, 0 major, 0 minor, 0 nit. m1 and N1 are closed, and the round introduced no correctness defect.
