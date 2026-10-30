# ST3a checkpoint 1, round 2: closure review (Opus)

**Revision:** `990e776c` (`node/st3a-power-loss`). Round 1 was at `c89e7ad6`. The fixes are in `58010d90` (checker and MicroLog) and `990e776c` (regressions). I read `a5b93c5a` (ToyChain core) only where the fixes touch it: `has_continuation`, D1's cost in recovery, and the refresh fix for the ToyChain false positive.

**Inputs:** my round-1 review (`ST3a-cp1-opus.md`), the GPT round-1 review, the implementer's dispositions, and design rev5 §5.3–§5.4 and §8.2.

**Worktree:** fresh and detached at `/work/scratch/st3a-cp1-r2-opus/wt` (now removed). Its only local edits were:
- the probe file;
- two temporary experiments, both reverted: D1 turned off in `boot_cfg`, and `faulty` added to `dash-coreimport`'s normal dependencies.

**Evidence** (all in `/work/scratch/st3a-cp1-r2-opus/`):
- `st3a_cp1_r2_opus_probe.rs`: 6 probe tests;
- `probe.log`: their output;
- `suites.log`: 3 timed rounds of every checkpoint-1 suite under `systemd-run --user -p CPUQuota=400%`, with `TMPDIR=/dev/shm/st3a-r2-opus`;
- `build.log`.

## Verdict

**APPROVED, with three minors for Phase C.** Every round-1 major is closed, and I re-probed each one. The suites are green: model 24/24, micro 15/15, checkers 14/14, toy 14/14, feature scope 1/1.

The fixes introduced one regression, Minor A below. To make byte-identical `make_private` clean, the M4 fix made D1 compare bytes, but the comparison ignores sizes, and it also exempts **copy-over**, which is not atomic. In a contrived case (the referenced bytes are all zeros), D1 is now fully silent on an unsafe copy-over. R1 still catches it after a crash, and ST2 never copies over a referenced file. So it does not block checkpoint 1, but fix it before S10/S10b rely on D1(d).

The refresh fix for the ToyChain false positive cannot hide a stale range for any presence-based requirement (probe N4). There is one gap next to it, Minor B: requirements whose resolution reads *other* undecided records are not tracked. No checkpoint-1 extractor does that, but Phase C's locator extractor will.

## Round-1 items

| # | Round-1 finding | Status | Evidence |
|---|---|---|---|
| M1 | D1 ignores pending directory operations on a referenced name | **Closed** | `d1_pending_rebinding_of_a_referenced_name` (D1(a); the `sync_dir` control is clean); `d1_rename_away_from_a_referenced_name` (D1(e) at the rename, D1(a) at the next barrier); `make_private`'s identical replacement is clean. The "every possible binding holds the same durable bytes" rule is sound for rename. See Minor A for sizes. |
| M2 | Required set empty after boot | **Closed** | D1 runs at boot (the extractor travels in the image) and at `RecoveryEnd`. `d1_after_boot_before_the_first_barrier` gives D1(d)+D1(e) under both crash kinds. My round-1 X3 is now caught exhaustively (below). |
| M3 / GPT M1 | D1 skips decided cells after a crash | **Closed** | `WorldDurableView` carries `ctx.decisions()`. `durable_get`, `durable_binding`, `Len::peek` and `Unit::peek` read decided cells, and the result is Durable, NotDurable or Unknown, with Unknown never certified. Both GPT controls pass, and so does `d1_recreated_name_without_sync_dir`. |
| M4 | D1(d) false positive on content-preserving rename-over | **Closed for rename; regression for copy (Minor A)** | N0 with 2 and 3 snapshots is clean exhaustively (29 and 40 images). |
| Minor 1 | MicroLog blind to post-read damage; R12 silently skipped | **Closed** | `check` re-reads through a fresh `recover()`; there is a `Continue` op; an empty log is `tip(0)`. Exhaustive X3 finds **R1, R12, D1a, D1d** (29 images, 86 branches; round 1 found nothing). Asking for R12 with no continuation is now a Precondition violation. The X1–X3 repros are checked in. |
| Minor 2 | Zero-image windows pass | **Closed** | My round-1 `AfterMark` window that never opens now reports `Precondition: the crash window captured no images` and `explored 0 images, the scenario expects at least 1`. A fault or recovery fault that never fires also fails. |
| Minor 3 | §5.4 always-on checks missing | **Partly closed; acceptable for checkpoint 1** | "A binding never names a missing inode" is now an `assert!` in `lookup`. "A synced value never changes" is still offline only (M1), and I found no deviation record on the branch: `grep` finds nothing outside the design's own §5.4 line. It is cheap to add where a barrier collapses a unit's history (assert the new `h[0]` equals the version it synced) and where a crash converts it. Add that check in Phase C, or record the deviation in the design's errata. |
| Minor 4 | M-suite budget | **Closed for checkpoint 1; see Minor C for the toy** | `power_loss_model` took 3.04, 3.31 and 4.45 user CPU-s at load 72.8, 62.7 and 59.2. Micro took 0.13–0.20 and checkers about 0.00. Together that is 3.2–4.7 CPU-s, inside the 5.0 line even at roughly twice the calibration load. M10 is 40 seeds in the PR gate, 400 nightly. |
| Minor 5 | D1(b)–(f) untested | **Closed** | `d1f_scopes` (all five cases), `d1b_and_d1c`. |
| Nit | Feature-scope test vacuous | **Closed** | It now uses `cargo tree --workspace -e features,no-dev` plus a dev-edge control. Mutation check: adding `features = ["faulty"]` to `dash-coreimport`'s normal `dashd-store` dependency fails the test with `a workspace member builds dashd-store feature "faulty"`. |
| Nit | `check.rs` overclaimed completeness | **Closed** | The module doc now states the boot/barrier/end schedule and the Unknown/pending semantics. |
| Nit | No fault after the crash | **Closed** | `Scenario::recovery_faults`. N0 runs one read fault in recovery, and it fires. |
| Nit | `InsideWrite` covers only `write_all_at`; `copy` with a lazy source size | **Unchanged (accepted)** | Neither is documented in code yet. A one-line comment on `VarKind::InsideWrite` and on `copy`'s `(None, None)` arm would close them. |
| — | M18 restated `Ctx::decide` | **Closed** | M18 now compares the set of reachable `(k in epoch 1, k in epoch 2, x in epoch 2)` against a closed-form reference, for both inner kinds and with and without a read before the inner crash. Because the tuple forces k₁ = k₂, deciding a cell twice would show up. |

## New findings

### Minor A: the byte-identity exemption ignores sizes, and copy-over is not atomic (regression from the M4 fix)

**Where:**
- `fs.rs` `d1_replace`: copy calls it, as does rename;
- `fs.rs` `inode_range`: checks only `inode.len[0]`, the durable size;
- `fs.rs` `bytes_at`: a missing unit reads as zeros, whatever the size.

**The defect.** `copy` runs `set_len(dst, 0)` and then pushes every sector-aligned size up to `n`. A power loss before the destination's `sync_data` can therefore leave it at any of those sizes, including 0.
- `d1_replace` compares only the current bytes over the referenced range, so a byte-identical copy over a referenced file is not D1(d). Before the fix every copy-over was D1(d), as §5.3 lists it.
- At the next barrier, D1(a) does not see the pending shrink either, because `inode_range` never looks at pending sizes. It fires only incidentally, when the shrink's zeroed sector differs from the referenced bytes.

**Probe** (`n1_copy_over_identical_is_silent_but_unsafe`). Setup: `b` holds 512 synced bytes, and a Durable record needs `b[0, 512)`. Then `copy(c, b)` with identical content runs, followed by `sync_wal`.

| Content | D1 at the copy | D1 after `sync_wal` | Power-loss branches (record present, `len(b)`) |
|---|---|---|---|
| ones | none | D1(a): "unit 0 has a pending write" | `{(true, 0), (true, 512)}` |
| **zeros** | none | **none** | `{0, 512}` |

Controls:
- a copy with different bytes is D1(d);
- `truncate(b, 0)` is D1(d).

**The same root cause on rename** (`n2_rename_over_shorter_source`). `b` is 256 ones followed by 256 zeros and is referenced over `[0, 512)`. Renaming a synced 256-byte file of ones over it is **not** D1(d). D1(a) fires only at the `sync_dir` ("its durable size is 256"). Here the D1(a) backstop is reliable, because the pending rename makes the source a candidate, and the source's durable size is checked.

**Impact:**
- R1 catches these after a crash, and ST2's `make_private` copies to `.unlink` and renames, never copying over in place.
- But a planted "make_private copies in place" bug would get past D1(d).
- The zero case shows that D1(a) does not fully implement §5.3's "its length".

**Fix:**
1. Keep copy-over as unconditional D1(d) on referenced ranges: it truncates first, so it is never atomic.
2. In `inode_range`, treat any pending size below `offset + len` (`len[1..]`, peeked) as NotDurable.
3. In `d1_replace` for rename, also require the source's current size (peeked) to cover the range.
4. Add N1 (both rows) and N2 as regressions.

### Minor B: refresh and `pending` follow record presence only, not undecided records an extractor reads through the view

**Where:** `check.rs` `standing_check`. `known` and `conservative` come only from the record's own presence. `WorldDurableView::get` returns `Peek::Unknown` but does not report that it did.

**The defect.** §5.3's locator extractor resolves its block requirements through `durable_view`: G, the marker's `new` or `meta.tip`, and the index positions. After a power loss those records can be undecided while the locator itself is known present. The extractor then has three choices, and each is wrong (`n3_extractor_view_unknown_is_not_tracked`). The setup:
- `loc` needs a block at the position `idx` names;
- `idx` is `f1` durably, then `f2` in a Buffered batch;
- after a power loss, recovery reads `idx` (decided `f2`) and truncates `f2`, then `f1`.

| On `Unknown` the extractor… | boot | `truncate(f2)` (live) | `truncate(f1)` (stale) |
|---|---|---|---|
| returns no ranges | — | **missed** (D1(a) only at the next barrier) | — |
| returns `Err` | **D1(b) false positive**, kept | — | — |
| returns every candidate (via `records`) | — | D1(d) | **D1(d) false positive**: no refresh, because `conservative` is false |

**Impact:** none at checkpoint 1, because `ToyRefs` ignores the view. It does affect Phase C's locator requirements (T10, T13, T15, T16, T19, T28, T30 and T33 all rely on D1(b) or standing ranges) on post-crash epochs.

**Fix:**
- Have `WorldDurableView` record whether any lookup during `refs`/`resolve` hit an undecided cell.
- If one did, treat that record's results like an undecided record's: failures go to `pending`, ranges are protected, and `conservative = true`.
- Either give `get` a candidate list, or document that extractors return every candidate's ranges on `Unknown`.

**The refresh fix itself is sound** (`n4_refresh_cannot_drop_a_live_range`). The setup: `r` is `f` durably, then `g` Buffered; after a power loss, truncate an unrelated file (an unrelated decision), then read `r`, then truncate `f` and `g`.
- With `g` surviving, only `g` is D1(d).
- With `g` lost, only `f` is.
- Left undecided, both are.

Decisions only ever remove candidates from `durable_records`, so a stale `required` errs toward protecting too much, never too little. The only exception is the extractor-view path above.

### Minor C: D1 in recovery doubles the toy's cost, and the toy suite is already well past its rows

Per test, at load ≈ 60–67:

| Test | With D1 in recovery | `boot_cfg.d1 = false` | Forecast (§8.2) |
|---|---|---|---|
| `n1_s1` | 7.06 | 3.13 | S1 row 2.11 |
| `t2_index_write_without_the_files_barrier` | 5.53 | 3.47 | witness ≈ 0.06 (narrowed) |
| `n1_s4b` | 1.28 | 0.84 | S4b 0.72 |

The whole toy binary took 15.7–17.6 user CPU-s over 3 rounds.

- CPU time does not gate (§8.3). But §8.2 forecasts the whole PR gate at ≈ 43.8 CPU-s, and Phase C still has about 20 scenarios to add. S1 alone is now 3.3× its row.
- `t2` is the only witness without `at_flush` narrowing, which is the forecast's assumption. Most of its 5.5 s is that, not D1.

**Recommendations:**
- **Evaluate boot D1 once per image and share it across branches.** In a non-nested boot it depends on no new decisions, and every branch repeats it.
- **Make refresh incremental**: re-check only the records whose presence was decided since the last evaluation.
- **Narrow `t2`.**
- **Re-derive the S1 and S4b rows** in `Work` units before Phase C's budget check.

So the 14 CPU-s rise "matters" as a trend, not as a checkpoint-1 blocker.

### Nits

- **MicroLog's R1 marker exemption ignores position** (`micro.rs` `check`: `(Some(got), _) if *got == marker`). A marker is accepted at any index. That is harmless, because workloads cannot append it (an `assert_ne!` in `MicroRef::apply`), but it is tighter to accept it only at index ≥ the pre-continuation count.
- **Two paths can drop a `pending` failure, from code reading only (not probed):**
  - a nested crash resets `D1State`;
  - a newer Durable write to the same key makes `durable_get` return `Known` without consulting the older undecided bit, so `standing_check` drops the pending failure.
  - In both cases the immediate D1(d)/D1(e) on protected ranges already fires for the mutations that cause such failures, so I found no real escape. Note it in the `Pending` doc, though.

## Re-run summary

| Probe | Round 1 | Round 2 |
|---|---|---|
| Replace-rename without `sync_dir` | missed | D1(a) |
| Rename away from a referenced name | missed until `sync_dir` | D1(e) at once, D1(a) at the barrier |
| Truncate/unlink before recovery's first barrier (both kinds) | missed | D1(d), D1(e) |
| Recreated name with no `sync_dir` after a crash | missed | D1(a) |
| Two-snapshot N0 | D1(d) false positive | clean (29 images); three snapshots clean (40) |
| X1 | R1, D1(a) | R1, R12, D1(a) |
| X2 / X2 with two snapshots | R1, D1(a) (+ false positive) | R1, D1(a) / R1, D1(a) |
| X3 (recovery trims the committed tail, unsynced) | **missed** | R1, R12, D1(a), D1(d) |
| `AfterMark` window that never opens | clean with 0 images | Precondition |
| Byte-identical copy-over (new) | D1(d) | **silent** (Minor A) |
