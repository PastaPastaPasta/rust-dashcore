# EV addendum to SB0: final review, round 7 (Opus high)

| | |
|---|---|
| Under review | `docs/node-program/designs/EV-addendum-SB0.md`, **rev5 at 726ff9a1**, `node/integration` (tip 7076a419; the design blob is unchanged since 726ff9a1) |
| Sequence | DEC-92: Sol r1–r6 (r6 APPROVE, Sol gate satisfied), then this sequential Opus high pass |
| Contract | `briefs/REVIEW-COMMON.md`; PLAN §7 handling (codenames only) |
| Binding, not reopened | DEC-111 (attempt overlay), DEC-113 (serialization), DEC-117 (allow-list plus capability), DEC-119 (SB0 rev6.1 approved) |
| Inputs checked | SB0 rev6.1 (092080a5) §5.1–§5.4 and §13.4; EV2 rev4 §2.x and §6 (`resolve_list`); LQ1 rev4 §3.7–§3.8 and §7; A1 §2.10 and §3.5.5; EV0 branch `node/ev0-evo-interfaces` @3a5cb631 (`dash-evo/src/llmq/members.rs`, `src/effect.rs`, `tests/consumers/{lq1a,ev2b,ch7a}.rs`) |
| Core | v24.0.0-rc.3 and v23.1.8 in `/work/home/agent/workspace/dash` |
| Date | 2026-10-09 |
| Result | **APPROVE-WITH-CHANGES.** One major and four minors, all must-fix. All are wording and plan edits inside the approved decisions, and none needs another Sol round |

This pass weighted the four areas the brief named: Rust implementability against the real EV0 code, Core-exactness at v24, plan realism, and contradictions with SB0 rev6.1, EV2 and LQ1. Sol's r6 Rust probes covered capability minting, nested `OnV`/attempt composition, the removal of the `Send + Sync` bound and `RefCell` reentrancy. I did not repeat them; I accept them, and I build on them below.

---

## Must-fix findings

### O1 (major): the allow-listed snapshot getter races with V's inline row writes. Core's `snapshotCacheCs` critical sections must be normative

**Where:** `EV-addendum-SB0.md:301` (the `snapshot_for_block` allow-list entry: "That bookkeeping happens under the shared gate and the snapshot cache's own lock"), `:286` ("V writes rows inline and synchronously"), `:284` (log application) and `:298` (lock order).

**Core.** Both pins hold one lock across the whole read-through and across the whole write:
- `GetSnapshotForBlock` takes `LOCK(snapshotCacheCs)` and, inside it, runs the cache `get`, the `m_evoDb.Read` and the cache `insert` (v24 `llmq/snapshot.cpp:301-318`, v23 `:264-281`).
- `StoreSnapshotForBlock` takes `LOCK2(snapshotCacheCs, m_evoDb.cs)` around the raw write and the cache insert (v24 `:320-329`, v23 `:283-292`).

A read can therefore never insert a value older than a write it overlapped.

**What rev5 allows.** The gate does not serialize these two:
- route A (`RawRows::snapshot_for_block`) runs on the caller's thread under the **shared** gate;
- a route-B command on V writes rows inline, also under the **shared** gate (`:286`, `:292`).

The text puts the getter's "bookkeeping" under the snapshot-cache lock, but it does not say that the disk read is inside that lock. It also does not say that V's write and cache insert form one section. "Snapshot cache **and** V's store writer" in the lock order (`:298`) reads as two locks taken in turn. A natural Rust port takes `Mutex<LruCache>`, misses, drops the lock for the RocksDB read, and then re-locks to insert. That port is wrong.

**Failure scenario** (pruned node, cycle K's row is S0 on disk and not in the snapshot cache):
1. A caller-thread getter for (t, K) misses the cache and reads S0 from disk, without the lock.
2. On V, a dispatched `quorum members`/qrinfo/`GetQuorum` command misses the members caches for K. It recomputes and overwrites the row with S1, then inserts S1 into the cache. S1 ≠ S0 is reachable: a row reflects the inputs at its latest write, and K's work block is now pruned, so the modifier takes the hash form (LQ1 §3.7.3; v24 `evo/cbtx.cpp:181-193`).
3. The getter resumes and inserts S0. The disk now holds S1 while the cache holds S0.
4. The next rotated computation for cycle K + C reads K's previous-cycle snapshot from the cache. It gets S0, builds different quarters, and its members differ from Core's. LQ2's `Verify` then rejects a valid commitment, or accepts a different roster: a consensus split.

The same hazard exists with the log application, unless it also writes and inserts in one section. There, the exclusive gate shuts route A out, so it is benign today.

**Fix (wording plus one test, no new mechanism):**
- In §2.6, state the two critical sections as Core's:
  - `snapshot_for_block` holds the snapshot-cache lock across the cache lookup, the disk read and the insert;
  - every row write holds the snapshot-cache lock across the raw write and the cache insert. That covers V's inline writes on route B, the log application on route D, and the backing writes `AttemptRows` reaches.
- In the lock order, name this as one lock (the snapshot cache's), held across V's store write for that row. This is consistent with the existing order and adds no cycle: the getter waits on nothing else, and V never waits on the caller.
- Add to CH7b's acceptance a hook-driven interleaving test. Pause the getter after its disk read and before its insert, let a dispatched route overwrite with a different value, then resume. The cache and the next read must give S1. The negative control is a getter that drops the lock around the disk read, and it must end with S0 cached.

### O2 (minor, must-fix): EV0 commits 1 and 2 do not compile as ordered

**Where:** `:389` (commit 1: "`tests/consumers/lq1a.rs` uses them instead of its copies"), `:390` (commit 2: the consumers use `Entry = Arc<Dmn>`), `:391` (commit 3 lands `impl MnEntryView for Dmn`) and `:410` (EV2a-1 owns `impl MnListView`/`ScratchMnList for MnList`).

**Evidence.** At 3a5cb631, `dash-evo/tests/consumers/lq1a.rs:52` has `impl MnEntryView for Arc<Dmn>` and `:73` has `impl MnListView for MnList`. Both are legal today only because the traits are local copies in the test crate. Commit 1 swaps the copies for dash-evo's traits. The test crate then owns neither the traits nor `Dmn`/`MnList`/`Arc`, so both impls fail with E0117. Reproduced with rustc on a two-crate probe that mirrors those impls (two E0117 errors).

Commit 2 then makes `QuorumMemberSource { type Entry: MnEntryView }`. `FixedMembers` (lq1a.rs:224) and the ev2b/ch7a consumers need `Arc<Dmn>: MnEntryView`, which requires `Dmn: MnEntryView` inside dash-evo. The plan lands that impl only in commit 3. Commit 1's acceptance ("builds; consumers compile") and commit 2's ("every EV0 test passes") therefore cannot be met. Commit 3 would also have to delete test-crate impls, which it does not list.

**Fix.**
- Commit 1 lands `impl MnEntryView for Dmn` in `dash_evo::dml` (local before the split, so legal), and commit 3's `git mv` carries it into `dash-dml`.
- For the list, choose one option and state it in §3 and §4.1:
  - (a) Commit 1 also lands `impl MnListView for MnList` and `impl ScratchMnList for MnList` (field reads only, `Scratch = MnList`). EV2a-1's row (`:410`) then keeps only the LQ1 §2.2 contract tests and the production scratch.
  - (b) lq1a.rs wraps `MnList` in a local newtype until EV2a-1. The lean `UsedSet` is already a local type, so it stays legal.

  Option (a) is simpler and costs nothing on EV2a's zero slack.

### O3 (minor, must-fix): `VContext` must be a pure affinity token, and every attempt-opening path must run on V after it exists

**Where:** `:255-258` (`pub struct VContext { _v_only: PhantomData<*const ()>, /* … */ }`, "creates exactly one when the V thread starts"), `:284` (attempts include VerifyDB levels 3/4) and `:472` (CH7b row).

Two consequences of rev5's (correct) R5-M1 fix are left implicit. Each would push an implementer toward the escape hatch the compile-fail suite forbids.

1. **No state in `VContext`.** `begin_attempt(&'v mut VContext)` borrows the context mutably for the whole attempt, including CH7a's log application before `release_attempt`. The `/* … */` invites putting V's state in the context: chainstate, the evo store writer, the effect ledger. If any of it lives there, nothing on V can use it while the handle is alive. A rustc probe with the store inside `VContext` fails with E0499 at the log-application call. State that `VContext` is a zero-sized, `!Send`/`!Sync` proof with no fields beyond the marker, and that V's state is held beside it. Disjoint field borrows (`state.ctx` vs `state.chainstate`) then work as usual.
2. **Startup and RPC attempt paths.** A1 runs `VerifyDB` (levels 3/4, which are attempts per `:284` and A1 §2.10.1) in init step 6 (`A1-architecture.md:1494`), and the startup `activate_best_chain` in §3.5.5 step 6 (`:1903`). A1's `ChainCommand` enum (`:977-984`) has no VerifyDB or `verifychain` variant. With `VContext` minted only at V-thread start, those paths cannot open an attempt unless they run on V. An init-thread port of Core's `AppInitMain` order would need a second constructor, which defeats compile-fail cases 1 and 6.

   State, in §2.6 and as a CH1/CH7b note:
   - every attempt opener runs on V after its `VContext` exists: connect/disconnect, reorg, `TestBlockValidity`/GBT proposal, startup VerifyDB and activation, import/`-reindex`, invalidate/reconsider/precious and `EnforceChainLock`;
   - startup load + VerifyDB runs on the V thread before its command loop, or as a `ChainCommand`;
   - `verifychain` becomes a `ChainCommand`.

   Also state that tests open attempts through a real V loop harness, never through a `#[cfg(test)]` or feature-gated public constructor. Feature unification could leak a feature-gated constructor into production builds, and `cfg(test)` is invisible to the other crates' tests anyway.

### O4 (minor, must-fix): §2.3's SPV request bound contradicts SB0 §5.2 step 5 for rotated inputs

**Where:** `:191`, SPV column: "one `getmnlistd` to `at` (or the `qrinfo` for its cycle), deduplicated, within the anchor and the H_keep horizon (SB0 §5.2.1)".

A rotated commitment's member inputs reach the work blocks of cycleBase − k·C, k = 1…3 (v24 `llmq/utils.cpp:491-504`). That is cycleBase − 3C − 8 = 872 blocks below the cycle on mainnet `llmq_60_75` (C = 288), well beyond H_keep = 320 (SB0 §5.2.1 table).

SB0 bounds step 5's inputs by the **anchor** only ("Steps 1 and 5 read only accepted entries at or above the anchor", SB0 §5.2). It delivers them through "the `qrinfo` for its cycle" (§5.2.1 rule 2), a request whose *target* lies in the authenticated domain while its payload covers the deep work blocks.

Read literally, the addendum applies the H_keep bound to `at`. A `Missing(cycleBase − 3C − 8)` would then send no request, every rotated IS quorum whose inputs are not already held would stay `Unavailable`, and ISLocks on mainnet would be unverifiable. That is liveness only, but it hits SPV's main path.

**Fix:**
- bound `at` by the anchor;
- apply the H_keep/domain bound to the request **target**: a `getmnlistd` target, or the qrinfo of the **commitment's** cycle, as SB0 §5.2.1 rule 2 says;
- key the deduplication by that target, not by `at`.

SB11's caller knows which commitment it is verifying, so the mapping can name it. One SB11 test: a mainnet-shaped rotated commitment whose cycleBase − 3C − 8 work block is not held is fetched through its cycle's qrinfo and verifies.

### O5 (minor, must-fix): take the new model-only conformance work off LQ1a's zero-slack edge, or make P3 the plan number

**Where:** `:423`, `:467`, `:495`.

PLAN's LQ1a (`PLAN.md:543`, size M) already carries:
- non-rotated selection, the modifier, scans and `select_quorum_for_signing`;
- L1 `diff_quorum_members` ≥ 24 CPU-h and the first Core comparison.

rev1–rev5 add:
- the generic signatures and fallibility;
- the not-held tests and negative controls;
- `members_uncached` and its state-untouched equality;
- `reference::ModelAttempt`;
- §2.6 cases 1–6, with a 3 × 2 state-machine-gate matrix and scoped negative controls;
- §4.3's warm-cache model case;
- the node missing-list test.

§5 sizes all of this "inside M" and names P3 (31.5) as the cost if that is wrong. Most of the new volume is model-only test infrastructure that nothing on the chain before CH7b consumes. CH7b starts at week 24.0, and LQ1a ends at 13.5.

**Fix (manager's choice, recorded in §5):**
- **Either** move `ModelAttempt`, §2.6 cases 1–6 and §4.3's model case into a separate S row:
  - deps `LQ1a`;
  - feeding CH7b, with `i:` into LQ1b-E's rerun;
  - ending around 15.0 against CH7b's start of 24.0, so roughly 9 weeks of float at no REL11 cost.

  LQ1a keeps the signatures, fallibility, not-held tests, `members_uncached` and `SIGN_HEIGHT_OFFSET`, which LQ1b/SB8 consume.
- **Or** adopt P3 (REL11 31.5) as the planning number now, rather than leaving it as a sensitivity.

Rerun `cpm_ev_addendum.py` with the chosen shape.

---

## Checked and sound (no change)

- **Core anchors re-verified at both pins:**
  - `PreComputeQuorumMembers` loop and its `reset_cache` call sites: v24 `llmq/blockprocessor.cpp:34-42`, `:223`, `:523`; v23 `:30-38`, `:176`, `:387`;
  - `IsQuorumRotationEnabled`'s `cycleBase < 1` early return and its ancestor at cycleBase − 1: v24 `llmq/options.cpp:48-60`;
  - the members gate on `pprev == nullptr`: v24 `llmq/utils.cpp:644-647`;
  - the previous-cycle `nullptr` break: v24 `utils.cpp:491-494`;
  - the qrinfo "Cannot find quorum snapshot" refusal: v24 `snapshot.cpp:60`, v23 `:40`;
  - `SIGN_HEIGHT_OFFSET`: v24 `quorumsman.h:210`.

  §2.1's not-held rule, §2.2's chain contract (`Ok(None)` only above the height or at genesis, negative heights in i64 with no view call) and §2.5's fallible predicates reproduce Core's values exactly where Core sees them. They never answer a Core absence value for unheld input. The precompute loop keeps Core's predicate order and its short-circuit (the rotation predicate is evaluated before `h % interval`).
- **Split and orphan rule.** Apart from O2's commit order, the impl sites in §1.3 are legal: a forwarding `Arc<T>` impl in dash-evo, `impl MnEntryView for Dmn` in dash-dml, and `From` impls on local targets. `MnEntryView`'s methods are all `&self` getters, so the forwarding impl is mechanical.
- **Removal of the trait bounds.** No LQ1 or EV2 trait carries a `Send`/`Sync` supertrait that a `!Sync` `AttemptHandle` would have to satisfy. Only the store readers (`DmlReader`, `ListCache`, `MinedCommitmentReader`) are `Sync`, and the handle does not implement them.
- **SB0 rev6.1 consistency** (shadow engine off V, node comparison as one V command; SB8/SB11 phasing; `MemberError` variants; §7 departures): consistent. The CPM was rerun with `python3 -B`: run P gives REL11 **30.0** and X gives 31.5, matching §5. SB0 rev5's SB12a is not on this chain (SB19 depends on SB11 and EV4-serve only), so building on the rev1 overlay does not change these numbers.

## Implementation notes (not gating)

- **`OnValidation` behind trait objects.** `ChainHelper` back ends (route C) and `DmlMemberSource` are trait objects. Entry points reached through `dyn` cannot be generic over `C: OnValidation`. Make the sealed trait object-safe, so that `&dyn OnValidation` works, or pass a two-variant capability enum.
- **Precompute naming.** The loop's `facade.members(p.ty, block, reset, effects)` (`:229`) is LQ1 §3.7.4's four-argument facade entry, reached through the attempt handle so its rows land in the overlay. It is not the three-argument `QuorumMemberSource::members`. CH7b should name it that way.
- **Lock policy.** `std::sync::RwLock` does not guarantee a priority policy across platforms. "Writer-preferring" needs a lock with a documented writer preference, or a PF starvation check on each release target.

## Evidence and cleanup

- **rustc 1.98 probes** in `/work/scratch/ev-add-r7-opus/probe/`:
  - test-crate impls of dash-evo's traits for `Arc<Dmn>` and `MnList` → 2 × E0117 (O2);
  - state held inside `VContext`, with log application while the attempt handle lives → E0499 (O3).
- **Core:** exact pinned sources for every anchor cited above.
- **EV0:** the branch's real consumer files and traits.
- No workspace build or dashd run was needed for this documentation review.

Nothing was committed. No branch was modified, and no private material was touched. The scratch directory is removed after this file is written.

## Verdict

**APPROVE-WITH-CHANGES.** Must-fix:
- **O1** (major): make Core's `snapshotCacheCs` critical sections normative for the allow-listed getter and every row write, and add the interleaving test;
- **O2:** reorder the EV0 commits so commits 1–2 compile;
- **O3:** `VContext` is a stateless token, and every attempt opener runs on V;
- **O4:** bound SPV requests by the anchor and the request target, not by H_keep on `at`;
- **O5:** take the model-only suite off LQ1a's zero-slack edge, or adopt 31.5.

DEC-111, DEC-113, DEC-117 and DEC-119 stand unchanged. A manager-applied rev6 that follows these fixes as written needs no further review round. O1's wording should be checked against the cited Core lines.
