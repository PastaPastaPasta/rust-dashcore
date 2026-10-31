# ST3a checkpoint 1 review — Opus

**Revision:** `c89e7ad6` (`node/st3a-power-loss`, 14 commits on integration after ST2), phases A and B.

**Inputs:** REVIEW-COMMON and design rev5 (DEC-74): §2, §3, §5, §6.3–§6.4, §7, §8 and §10, plus rev1 §6.3 for the M1–M18 table.

**Worktree:** fresh detached, at `/work/scratch/st3a-cp1-opus/wt`. Its only local edits are the probes below, which are uncommitted and never pushed.

**Evidence** (all in `/work/scratch/st3a-cp1-opus/`):
- `probe.log`: the output of every probe;
- `st3a_cp1_opus_probe.rs`: the probe tests (9 of them);
- `probes-micro.patch`: the planted MicroLog bugs X1–X3b;
- `build.log`.

**Independence.** I wrote this review independently and read `ST3a-cp1-gpt.md` only after my probes had run. Where GPT and I overlap, the overlap is marked.

## Findings

### Major 1: D1(a) ignores pending directory operations on a referenced name, even on the forward run

**Where:** `dashd-store/src/faulty/fs.rs:279-291` (`durable_binding`), `:883-895` (`durable_lookup`), `:837-842` (`range_durable`); `:1321-1336` (`rename`, which checks only `to`).

**The defect.** `durable_binding` walks only `ops[..durable_len]`. So a name that a **pending** operation rebinds or unbinds still resolves to its old durable inode, and D1(a) then checks that old inode's bytes. Design §5.3 says D1(a) fires when "a required byte range is not durable: its binding, …". A name with a pending operation on it does not have a durable binding.

`rename` calls `d1_touch` only for its destination, and neither D1(d) nor D1(e) looks at its source.

**Probes** (`d1_pending_rebinding_of_a_durable_name`, `d1_rename_away_from_a_referenced_name`):
1. **Replace-rename with no `sync_dir`, then a Durable record over the new content.**
   - Setup: `b` holds 1s and is synced, including its directory. Then `tmp` gets 2s and `sync_data`, `rename(tmp, b)` runs, and the `sync_dir` is missing. Finally a Durable record needs `b[0, 512)`.
   - D1 reports **nothing**.
   - After a power loss, `b[0]` can be either 1 or 2: the record survives over the wrong bytes.
   - Control: with a new name (`c`), D1(a) does fire.
2. **Rename a durably referenced file away, then `sync_wal`.**
   - D1 reports nothing at the rename, nothing at the engine barrier, and nothing at the next `sync_data`.
   - D1(a) fires only once a `sync_dir` makes the damage durable. Without that `sync_dir` the rename can still survive a crash, because each directory operation survives on its own, and D1 never fires.

**Why it matters.** ST2's `make_private` (`flatfile/seq.rs:134-149`) is copy, `sync_data`, rename over, then `sync_dir`. A planted "make_private without `sync_dir`" (S10/S10b) would get past D1. R1 or R10 would catch it only after a crash, and W3/R10 are Phase D.

**Fix:**
- In `durable_lookup` and `durable_lookup_dir`, if any operation at index `>= durable_len` changes the name (or an ancestor), return "not durable".
- Make D1(d) and D1(e) check `rename`'s source as well as its destination.
- Add both probes as regressions.

### Major 2: D1's required set is empty after every boot, so D1(d) and D1(e) are blind in recovery until its first barrier

**Where:** `world.rs:795`, where `Ctx::new` starts with `D1State::default()`, and `world.rs:1190`, where `boot` calls `Ctx::new`. `required` is set only by `World::after_barrier`.

**The defect.** Durable records that existed before the crash are still durable at boot, but D1 does not know what they reference until something in the new epoch triggers a barrier. Until then, every `touched` and `referenced` lookup returns `None`.

**Probe** (`d1_after_boot_before_first_barrier`). A durable record references `f[0, 1000)`.
- In epoch 0, `truncate(f, 10)` reports D1(d).
- After a crash of either kind, the same `truncate` followed by `remove(f)` reports **nothing** under both ProcessKill and PowerLoss.

This differs from GPT's Major 1. Here the referenced record sits in the engine's concrete base, so no lazy cell is involved: the state was simply reset.

**Combined with Minor 1, a planted recovery bug escapes completely** (X3 below).

**Fix:** at boot, after the crash conversion, run `standing_check` on the booted stores. It must only peek at cells, never decide them. Seed `required` from the result, and treat any requirement still unknown as "unknown", not as "empty".

### Major 3: D1 skips cells the branch has already decided after a crash (concurs with GPT Major 1)

I reproduced this independently with `d1_after_crash_recreated_name_not_durable`:
1. A file's create was pending at the crash, and the branch decides it was lost (recovery observes that the file is absent).
2. Recovery recreates the file, writes it and runs `sync_data`. It then commits a Durable record over it with **no `sync_dir`**.
3. D1 reports nothing.
4. Control: the same sequence in a fresh world reports D1(a).

The cause: `durable_get` (`engine.rs:288-302`) and `durable_binding` (`fs.rs:279-291`) take "not `Present`" to mean "unknown", whatever the branch has decided, and `range_durable` then passes anything unknown (`fs.rs:841`).

So the leniency is wider than the stated deviation ("skips records whose presence is undecided"). It also skips requirements on files, sizes and sectors that were touched in the last pre-crash window, including cells that are already decided.

**Fix:** as GPT says, carry the branch's decisions into `WorldDurableView` and use the decided values. Keep "undecided" as a distinct third state that a later barrier re-checks. Never certify it as durable.

### Major 4: D1(d) false positive on content-preserving rename-over, and N0's workload hides it

**Where:** `fs.rs:1328` (`d1_touch(…, to, 0..u64::MAX, "rename over")`).

**The defect.** Any rename over a path whose bytes a durable record references is flagged, even when the new inode holds byte-identical, synced content over every referenced range.

**Probe** (`x0_n0_with_two_snapshots`). The correct MicroLog run is `a300:1;a300:2;c;s;a300:3;c;s`: the same protocol as N0, with a second snapshot. Exhaustive search over 29 images reports
`D1d: rename over of /dd/log/snap.dat … touches durably referenced [0, 616)` and no R-violation. The protocol is safe, because the new snapshot extends the old one.

**Coverage gap.** N0's workload (`power_loss_micro.rs:12`) takes exactly one snapshot, so the replace path of MicroLog's own protocol is never exercised.

**Phase C impact.** ST2's `make_private` renames a byte-identical private copy over a blk file that durable index entries reference. S10/S10b will report D1(d) on the correct protocol.

**Fix:**
- For rename-over (and copy-over), flag only if some durably referenced range of `to` is not durable in the source inode, or differs from it byte for byte. This is the same byte comparison `range_durable` already does for A-PSOW.
- Add a two-snapshot N0.
- Design §5.3 lists only "copy-over"; spell out the rename-over rule there too (a BL-26 erratum).

### Minor 1: MicroLog cannot see damage done after recovery reads, and R12 is skipped silently

**Where:** `micro.rs:531-576` and `:578-580`; `harness.rs:564-566`.

**The defects:**
- MicroLog's R1 compares the records recovery read into memory. Anything recovery mutates afterwards is invisible to it.
- `continue_after_recovery` returns `None`, and `r12_continue` turns `None` into "no violations" without saying so.

**Planted bug X3 (missed).** Recovery trims the last log to one byte short of its committed end and does not sync. Exhaustive search over 29 images and 84 branches finds **nothing**.
- With X3b (the trim followed by `sync_data`), D1(a) catches it at the barrier.
- With an R12 continuation (a `Commit`, then a clean restart), X3 is caught as `R12: … R1: committed record 1 is unreadable`.
- That continuation needs MicroLog's tip encoding fixed first: `check` maps count 0 to `None`, while a flush's `new` is `tip(0)`. As things stand, an R12 commit on an empty log fails R3, as N0 does in my probe.

**Fix:**
- Re-read the committed records from the stores in `check`, which is R1's view form.
- Give MicroLog a continuation.
- Make "no continuation" an explicit `Scenario` choice (or a failure when `continue_r12` is set), so ToyChain cannot drop R12 silently.

### Minor 2: windows that capture no images pass Exhaustive mode (extends GPT Major 2)

GPT showed this for read faults. A second route: `EventSel::AfterMark` matches only while the selected occurrence is still the latest mark of its kind (`world.rs:417-420`: `seen == occurrence + 1`). So a window `from` whose mark recurs before `n` events never opens, and `explore` reports a clean result with 0 images (`harness.rs:432`, `:819`).

**Fix:** fail any forward run whose window, or whose fault, never fired. Keep N0's `images > 20` assertion, and add a generic one.

### Minor 3: §5.4's always-on model self-checks are not implemented, an undeclared deviation

**Where:** `harness.rs:653-655`.

The only `InvariantId::Model` the harness ever emits is the DFS determinism mismatch. These always-on checks are missing:
- a synced value never changes;
- sizes stay within their domain;
- bindings never name a missing inode;
- the engine recovers durable state plus the surviving pending batches.

M1, M3 and M6 cover these offline, but §5.4 makes them guards on every exploration (ToyChain's included).

**Fix:** implement the cheap ones (a size-domain assertion at resolution, binding to inode, and synced-unit immutability at barriers), or record the deviation.

### Minor 4: the M-suite budget does not reproduce at the calibration load

Three runs of the `power_loss_model` binary took **5.82, 6.26 and 6.32 user CPU-s** at load ≈ 29. §8.1 calibrated at load 30–41. That is above the 5.0 line, and well above the claimed 4.4.

The time is concentrated in two tests:
- `m10_lazy_equals_eager`: 4.0 s;
- `m6_kv_model_with_subset_power_loss`: 1.9 s.

CPU time does not gate (§8.3), but Phase B's acceptance includes re-deriving the calibration, and I found no re-derivation on the branch.

**Fix:** cut M10 to about 40 seeds or lower its eager cap (the 400-seed variant already runs nightly). Record the calibration as `Work` units or PMU instructions, which stay stable under load (±0.04% on agentbox).

### Minor 5: D1(b) to D1(f) have no tests on the branch

MicroLog declares no finalizers, so D1(f) runs on no path at checkpoint 1.

My probe `d1f_scopes` shows D1(f) behaving correctly in every case:
- an unbarriered `SinceFlushStart` is flagged;
- a barrier clears it;
- a batch written before `FlushBegin` is out of scope;
- a Buffered previous finalizer under `PreviousFinalizer` is flagged;
- a process kill keeps the old epoch's batches in scope unless a `RecoveryBegin` mark starts a new scope.

Phase C's T22/T26/T27/T29/T34 depend on these.

**Fix:** check `d1f_scopes` in, along with D1(b)/(c) resolver cases.

### Nits

- **The feature-scope test is vacuous today** (`tests/faulty_feature_scope.rs`). It is correct as written, but no runtime binary depends on `dashd-store` yet: `cargo tree -i dashd-store -e normal` lists only `dash-coreimport` (a library) and the exempt `dash-fuzz`. Consider also checking every library member's normal (non-dev) feature set.
- **`InsideWrite` covers only `write_all_at`.** Multi-page writes through ZeroFill `allocate` or `copy` cannot tear on a ProcessKill. That is harmless for ST2 (an interrupted `.unlink` tmp is removed on the next call), but worth documenting.
- **`copy` with a lazy source size** gets no intermediate sizes (`fs.rs:1315`), and a partial copy's size uses `src_len.max()` (`fs.rs:1305`).
- **`check.rs:4-7` overclaims.** It says D1 is "complete on the forward run". Major 1 shows it is not.
- **MicroLog's N0 fault plan** has two faults, both on the forward run. No fault is injected after the crash: there is no API for it, as GPT also notes.

## Scope answers

### 1. Model fidelity

**The subset WAL semantics are exact (DEC-62):**
- Every pending batch has its own survival bit, and a batch's bit is shared by all its keys, so batches stay atomic.
- Point reads and scans resolve per key, newest first, deciding only the bits of batches that wrote the key.
- A Durable write is two events, `Append` and then `Barrier`. A failed or interrupted barrier leaves everything pending.
- A barrier covers every batch appended before it. That is exact here, because store calls run on one thread.
- `m6` validates all this against `kv_model`'s independent subset reference over 200 seeds (more than 50 of them non-prefix masks), and `m16` against the native states.

**Filesystem behaviours are adversarial enough per the §2 table:**
- per-sector version choice, with no cross-sector or size coupling (a superset of `data=ordered`);
- every held size, plus sector-aligned intermediate sizes for extending writes, zero-filled gaps and copies;
- `Shrink` zeroes sector tails;
- each directory operation survives on its own, renames stay atomic, and unlinks can resurrect files;
- no fsync-of-file directory bonus;
- poisoned writes stay pending for good;
- ZeroFill matches `OsFs`'s fallback (zeros from `offset`, `flatfile/fs.rs:250-255`).

**Over-granting durability: none found in `Realistic`.** One near-miss: after a failed sync, a ProcessKill keeps the poisoned (or latched-engine) data readable. That is the fsyncgate read-side case, which the design assigns to `Extended`. As an image it equals the PowerLoss branch in which the filesystem cells survive, so it over-grants only for a nested later power loss. Acceptable until Phase D.

### 2. Checker soundness and planted bugs

| Bug | Description | Result |
|---|---|---|
| X1 | sync only the newest file of a rollover commit | **caught**: R1 and D1(a) |
| X2 | `meta.snapshot` before the rename's `sync_dir` | **caught**: R1 and D1(a); with two snapshots, also the Major 4 false positive |
| X3 | recovery trims the committed tail, unsynced | **missed**: Major 2 plus Minor 1 |
| X3b | X3, then `sync_data` | caught: D1(a) only |
| — | replace-rename without `sync_dir` | **missed by D1** (Major 1) |
| — | rename a referenced file away | **missed by D1** until a `sync_dir` (Major 1) |
| — | recovery truncate or unlink before its first barrier | **missed by D1** (Major 2) |
| — | recreated file with no `sync_dir` after a crash | **missed by D1** (Major 3) |

### 3. The four deviations

- **Lenient D1 after a crash: not acceptable as implemented.** It skips decided cells (Major 3), and separately D1 forgets the required set at boot (Major 2). Peeking at decided values needs no new decisions, and anything genuinely undecided should be re-checked later, not passed.
- **`AfterMark` with an occurrence index: acceptable**, given Minor 2's guard against empty windows.
- **`InsideWrite` decided at boot: acceptable.** It is still exhaustive and costs only branches, and it is confined to the image right after the write (`process_kill_tears_only_the_last_write`).
- **Partial `Extended`** (sync-time ENOSPC/EIO, EIO on unsynced reads, old-or-new bytes after a failed sync): **acceptable as a Phase D deferral** under §10. It does not excuse Minor 2 or GPT's recovery-R8 gap.

### 4. Self-test rigour

These test the model against independent references:
- M1: a reference world;
- M3/M4: two-way brute force, every version and size reachable and nothing else;
- M5;
- M6: `kv_model`;
- M7;
- M10: lazy equals eager on observation traces, including scans, a nested crash and all 14 `FlatFs` calls after the crash;
- M12: closed-form counts;
- M13: GPT's non-monotone case;
- M16 and M17.

These are thinner:
- M18 mostly restates `Ctx::decide`, which cannot decide twice by construction;
- M11 and M15 are smoke tests;
- M3 uses one file with a durable binding;
- M10's pre-crash state has no rename, copy or remove;
- nothing tests D1 except the A-PSOW regression (Minor 5).

Regenerating the 8 repros reproduces the checked-in files byte for byte, so they are deterministic, and all are marked "minimality verified".

### 5. Budget

Minor 4: about 6 CPU-s against the 5.0 line at the calibration load. The native regression took 0.9 CPU-s against its 0.6 budget line, and MicroLog 0.7 CPU-s (both measured with cargo overhead).

## Verdict

**CHANGES-REQUIRED.** The storage model itself is sound and exact where DEC-62 requires. The changes are all in D1:
- Majors 1–3 let ordering bugs through: on the forward run (pending rebinding) and in recovery epochs (the required set reset at boot, and decided cells skipped).
- Major 4 is a false positive that MicroLog's single-snapshot workload hides, and ST2's `make_private` will hit it in Phase C.

Fix Majors 1–4 and Minors 1–2 before Phase C relies on D1. Minors 3–5 can land with Phase C.
