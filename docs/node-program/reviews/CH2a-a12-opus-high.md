# CH2a a1+a2 — Opus high (sequential pass after Sol r1)

**Reviewed:** `node/ch2a-coins` `4cba2def → 6890badf` (committed history only), from a detached worktree at `6890badf`. In scope: the `dashd-chainstate` coins layers, tip, block overlay, ledger, `StableBytes`, the twin and model, their node-fuzz adapters, the DEC-131 docs commit `287d51f2`, and S1 `6890badf`. The uncommitted a3 work in the branch worktree was not read.

**Basis:** CH2 rev5 (`640d37f9`, plus the branch's cite updates and DEC-131's §6.7 amendment), DEC-95, DEC-99, DEC-102, DEC-103, DEC-108 and DEC-131, REVIEW-COMMON, and Core `v24.0.0-rc.3`: `coins.cpp`/`coins.h`, `validation.cpp` (`:2945-2966`, `:3036-3046`, `:5866-5892`, and ConnectBlock/DisconnectBlock's best-block sites `:2365`, `:2523`, `:2538`, `:2912`). The rule applied is to adopt Core semantics and not chase stricter-than-Core invariants. Codenames only (PLAN §7). No private material was read.

**Summary.** No blocker or major. I re-derived every transition against Core and found the tip, F and child transitions correct (table below), the S1 fix right, and `StableBytes` sound. The findings are about what the evidence shows:
- The model's membership oracle is loose enough that two realistic regressions pass the whole seeded model.
- The committed planted-bug test catches X13 through the allocator meter, not through a lookup.
- One PF1 witness row records `boundary_state` at a point that is not Core's call site.
- Two API edges let b1 or CH4 reproduce S1-class mistakes.

## Findings

### O1 — minor: the model's membership oracle passes two regressions (`have_inputs` not caching, fetch caching as DIRTY)

**Where:** `dashd-chainstate/src/coins/model.rs:440` (`Members::tip`'s `twin_only`), `:731` (the overlay add's `twin_only`), and `:660-677` (`tip_have`).

`tip_have` compares `have_in_cache` with a predicate recomputed from active and F (`:668`), which restates the implementation; it never consults the twin. The twin enters membership only through `Members::explained()` on an add or a commit, and `twin_only` accepts *any* key with no active entry and a clean twin entry that the cache's view holds unspent. The documented cause (D12) is narrower: a clean twin entry fetched from **F**, which the cache never copies and loses at the release. The predicate does not check that the key was in F.

**Counterexamples** (scratch, env-gated mutations in `/work/scratch/ch2a-a12-opus-high/mutations.diff`; 1,080 seeded runs per row, 9 configs × 60 seeds × both drivers, `review_mut.rs`):

| Mutation | Model | Unit tests |
|---|---|---|
| MB: `have_inputs` resolves without caching in active (`overlay.rs:65` → `cached`, else `peek_below`) | **0 caught** | `a_failed_long_input_connect_leaves_the_parent_whole` fails ("cached in active"); `a_committed_long_input_connect_disconnects_to_identical_coins` **hangs** in `grow_shard` (O7) |
| MI: `fetch_bytes` caches a DB or prefetch hit as DIRTY (`cache.rs:277`) | **0 caught** | 7 fail (L0 (a), uncache, membership, sims) |
| MF: `merge` over F's unspent coin marks FRESH (`cache.rs:422-429`) | 20 caught ("the view") | 4 fail |

MB breaks "Things CH2a must get right" item 2 (O-R6-m3) and DEC-102 membership. MI makes clean entries un-uncacheable (mempool `coins_to_uncache`) and inflates `dirty_bytes`. Neither changes the database, which is why the twin cannot see them. The unit suite catches both today. But the model is what the O3 farm runs nightly as a3, b1 and CH2b change these paths, and it is blind to this class.

**Fix:**
- Keep the key set of the most recently frozen layer in the model. The twin flushes at every freeze, so each clean twin entry was fetched since then. Require `twin_only ⇒ k ∈ keys(last F)`.
- Have `tip_have`, and the block loop after `have_inputs`, assert `Members::tip(..).explained()` against the twin, not only the structural predicate.
- Keep MB and MI as negative controls beside X29.

### O2 — minor: the committed planted-bug test catches X13 through the meter and X10 through xcheck, and asserts no reason

**Where:** `dashd-chainstate/src/coins/tests/model.rs:134-147`, which only `println!`s the reason; `cache.rs:133-135` says X13 is caught "by the release guard, and without it by a lookup mismatch".

Run as committed (`--nocapture`), the reasons are:

```
x10: caught as "an unexpected read failure"   (xcheck's corruption abort; the model always sets xcheck on)
x13: caught as "tagged != sigma - R"           (the allocator meter: F leaves the ledger while W's Arc keeps it alive)
x29: caught as "the best block after a commit"
```

X13 never reaches a lookup in the committed evidence. The meter fires at the release op itself, before any read. With the meter off (scratch `REVIEW_NOMETER`), X13 is caught semantically in 517 of 1,080 runs ("the view" 456, "disconnect spend" 32, "overlay spend" 15, …). With xcheck and the meter both off, X10 is caught as "the view" or "overlay spend" (40 of 1,080). So both bugs *can* be caught for the right reason. The committed test just does not show it, and it would keep passing if the semantic path regressed. Also, xcheck is always on in the model (`model.rs` `Model::new`), so the production path (xcheck off) of `read_below`/`peek_below` is never the one the model exercises. That path is small, which is why this stays minor.

**Fix:** assert the reason class per bug: X13 with the meter off must be a lookup or view mismatch, X10 with xcheck off must be a view mismatch, and the as-committed reasons can stay as a second leg. Run a fraction of model inputs with xcheck off; one config bit would do it.

### O3 — minor: PF1 witness (iii) records CRITICAL at a point that is not Core's call site

**Where:** `docs/node-program/research/PF1-CH2a-layout-gate.md:428` and `:433` ("The long-input block is CRITICAL at its boundary: F with the block's undo … is past B"); `dashd-chainstate/src/coins/tests/witness.rs:176` (`m.report` after the commit while `undo` is alive).

Core decides size state in `FlushStateToDisk` after `ConnectBlock` has written and dropped the block's undo (`validation.cpp:2945-2966`, called from ConnectTip's IF_NEEDED flush). The design and the model call `size_state`/`boundary_state` only at boundaries with `held_out` 0; the model asserts it (`model.rs` `boundary`, "held_out at a boundary"). The witness reports while the 48,700 undo coins are still held, because it keeps them for the disconnect.

At the real boundary, row (iii) is **OK**, not CRITICAL: Σ − held_out = 714,096,644 − 48,700 × 9,583 = **247,404,544** ≤ large = max(9B/10, B − 10 MiB) = **254,541,824** (B = 265,027,584). Row (ii) remains CRITICAL either way (6,920,192 > 1,835,008), so only (iii)'s state and narrative are wrong. Its "Σ at the boundary" column is mislabeled the same way. The test records this; it does not assert it, so nothing fails.

**Fix:** report `boundary_state` after the undo drops (or both values, labeled), and correct the (iii) row and the narrative at `:433`.

### O4 — minor: `BlockOverlay::best_block` does not cache the parent's like Core's child, so `commit` without `set_best_block` writes `None` into the tip

**Where:** `dashd-chainstate/src/coins/overlay.rs:222-224` and `:159-161`; `cache.rs:432-434`.

Core's `GetBestBlock` caches the base's hash in `hashBlock` (`coins.cpp:172-176`). `ConnectBlock` calls it on the child at every connect (`validation.cpp:2523`, `assert(hashPrevBlock == view.GetBestBlock())`). So a child flushed without `SetBestBlock` hands the *parent's own* best back (`coins.cpp:250`, `hashBlock = hashBlockIn`). Ours hands `None`, and the tip then reads through to F's tip or the durable block.

**Probe** (`review_mut.rs::review_overlay_best_is_not_memoized`, passes): the tip is durable at block 1 and committed to block 2. Open an overlay; `best_block()` returns block 2; `commit()` without `set_best_block`. The tip then reports **block 1**, where Core's would report block 2.

Core's own call pattern always sets the best before the flush (`:2365`, `:2538`, `:2912`, and the `fJustCheck` view is never flushed), so this is unreachable today. It is exactly the S1 observable, though, and the model cannot see it: `overlay_best` memoizes in the twin, and the model always sets the best before committing.

**Fix (API, for b1):** `commit(self, best: BlockRef)`, so the child's best is mandatory, as every Core flush site makes it. Or cache the parent's best on the overlay's first `best_block()`, as Core does.

### O5 — minor: the prefetch fence does not fix the order of "take the generation" and "take the snapshot", and the model cannot detect the wrong order

**Where:** `dashd-chainstate/src/coins/prefetch.rs:4` ("A batch carries the `released_gen` current when its snapshot was taken"); CH2 §2.4.3 item 1 (A1 §3.4 verbatim); `model.rs` `produce` (reads the database and stamps in one synchronous step).

The race needs a producer on another thread (CH4's S2) that **takes its snapshot, then reads** the generation:
1. Snapshot at generation g, before W's write of F lands.
2. W lands and V releases (g → g+1, map cleared).
3. The producer reads g+1 and stamps the pre-F batch with it.
4. V admits the batch.

F no longer shadows the keys it changed, so a key F spent comes back from the batch: a resurrected coin, consensus-relevant. The safe orders are "read the generation, then snapshot" or "V stamps the generation when it dispatches the request". The current wording admits both readings. The model's atomic `produce` exercises neither, which puts this in the class no a2 test can see.

**Fix:** state the order in `prefetch.rs` and §2.4.3 for CH4: `fetch_gen` is read before the snapshot is taken, or V assigns it at dispatch. CH4 should get a deterministic interleaving test for it.

### O6 — nit: `freeze(tip, …)` takes the block separately from the tip's best block

**Where:** `cache.rs:493`. Nothing ties `tip` to `self.best_block()`. F's tip becomes `durable` at the release (`:519`) and, in a3, the database's best-block record. A b1 bug that passes another block would leave the coins and the recorded best inconsistent. In the model the two are always equal (`block(self.height)`), so a mismatch cannot show there either. **Fix:** derive F's tip from `self.best` (or `debug_assert_eq!` it).

### O7 — nit: `grow_shard` loops forever if `have_inputs` stops caching

**Where:** `dashd-chainstate/src/coins/tests/overlay.rs:76`. Under MB (O1), `a_committed_long_input_connect_disconnects_to_identical_coins` hangs instead of failing, because it has no "cached in active" assert before `grow_shard`. **Fix:** bound the loop and assert the shard grew.

### O8 — nit: the model compares only the best block's hash

`tip_best` and `overlay_best` compare `BlockRef::hash`. `BlockRef` also carries `id` and `height`, which a3 and CH2b consume, so a mix-up in them is invisible. The model's `block(h)` derives all three from `h`, so comparing the whole `BlockRef` costs nothing.

### O9 — nit: `resize` reports only the coins share

`cache.rs:556` returns `Unchanged` when only the coins database's share changes. Core flushes ALWAYS in that case (`validation.cpp:5869-5891`): it returns early only when both sizes are unchanged. The doc comment says so, but b1 must remember it. The model's `resize` also runs `boundary()` for `Shrank`, where Core flushes ALWAYS regardless of state. Record both in b1's list.

### O10 — nit: make `StableBytes`' `!Sync` a compile-time fact

The `push` SAFETY argument (`stable.rs:29-31`) rests on the type being `!Sync`. Today that is implied by `UnsafeCell` and `NonNull`. A static negative assertion would stop a future `unsafe impl Sync` (for a `Send` overlay, say) from silently voiding the argument.

### O11 — nit: PF1's 1.216× is not a same-workload ratio

`PF1-CH2a-layout-gate.md:12`, `:39`, `:92`: `N_core` uses Core's per-coin cost for a **P2PKH** coin (115.67 B), while `N_res` is measured on the mainnet coin-length histogram. P2PKH is Core's cheapest coin: every script of 28 bytes or less fits the prevector inline, and longer ones heap-allocate. So the true same-workload ratio is ≥ 1.216×, and the floor claim (≥ 1.043×) is conservative, which is fine. The text should still say "against Core's minimum per-coin cost", not "Core's count".

## Transition audit (re-derived; no finding)

| Path | Ours | Core (`coins.cpp`, v24) | Verdict |
|---|---|---|---|
| tip `fetch` miss | prefetch `take` or DB read → active, flags ∅ | `FetchCoin` emplaces clean (`:44-59`) | equal |
| tip `spend`, active FRESH / other / none-but-F-unspent / none-below | remove / DIRTY tombstone / DIRTY tombstone / DIRTY tombstone | `:129-149` | equal |
| tip `spend` over an F tombstone | `false`, nothing written | Core after the flush: no entry → `false` | equal (f1586ceb) |
| tip `add` | overwrite check active then F (D2); FRESH iff active's entry is not DIRTY | `:70-108` | stricter only in a crash that is unreachable on a valid chain; DEC-131 class |
| `merge`: absent parent / F tombstone | child's flags moved up; FRESH and spent dropped | `:191-214` | equal (57e2239d) |
| `merge`: own FRESH and child spent | erase | `:225-229` | equal |
| `merge`: own other | own ∪ DIRTY, SPENT follows the child | `:230-247` | equal |
| `merge`: F unspent, no own | DIRTY (± SPENT) shadow | Core before the flush: `0 ∣ DIRTY` | equal outcome; FRESH is never promoted |
| `merge` end | `best = child's` | `hashBlock = hashBlockIn` (`:250`) | equal, except O4 (child's `None`) |
| child `add`/`spend` | Core's child rules; parent coin copied, not moved | `:70-149` on the child | equal; DEC-131 deviation declared |
| `uncache` | active flags ∅ only | `:284-297` | equal on active; F immutable (D12) |
| `have_in_cache` | active then F, unspent, no reads below | `:167-170` | equal up to D12 |
| freeze / release | F = old active; `durable = F.tip`; gen+1; map cleared | `Flush` = `BatchWrite` + `clear` | equal; O6 |
| prefetch admit | `fetch_gen == released_gen`, ≤ B/16 | — | sound given O5's order |
| `core_size_arith`, B(t) | i64, saturating | `validation.cpp:2945-2966` | equal (edge tests rechecked) |

The cites I spot-checked (`coins.cpp:44/61/70/129/153/162/167/172/178/182/254/284/303/325`; `validation.cpp:2945-2966`, `:3036-3046`, `:5866-5892`) match v24.0.0-rc.3.

## `unsafe` and `StableBytes` beyond Miri

- **`StableBytes`** (`stable.rs`):
  - Every returned borrow derives from a `NonNull` taken from `Box::leak`, and is tied to `&self`.
  - The `UnsafeCell<Vec<NonNull<[u8]>>>` is mutated only in `push`. The two shared readers (`len`, `allocated_bytes`) create no reference that outlives their expression, and none of the three re-enters. The global allocator, the only code that runs inside `Vec::push`, never touches a `StableBytes`.
  - Growth moves pointers, never pointees.
  - `Drop` needs `&mut self`, so every borrow has ended.
  - Zero-length boxes are dangling and non-null, and `Box::from_raw` on them deallocates nothing.
  - The type is `!Send` and `!Sync` (`NonNull`, `UnsafeCell`); see O10.
  - In the overlay, `resolve` ties `CoinRef<'_>` to `&self`. No `&self` method frees `scratch`, and `commit(self)` consumes the overlay, so no borrow can outlive the scratch.

  The argument holds without Miri.
- **Miri:** I re-ran it myself on `nightly-2026-07-15`, `cargo miri test -p dashd-chainstate --lib -- coins::stable`. It passed 2/2 under Stacked Borrows and 2/2 with `-Zmiri-tree-borrows` (`miri.log`). Miri covers only the two unit tests, not the overlay's use; the argument above covers that.
- **`alloc_tag`** (test, bench and fuzz binaries only): the header layout, the `align > 16` path and `realloc`'s re-tagging are right. The header read in front of a `Box`'s retagged pointer is what Stacked Borrows rejects. `lib.rs` documents it, Sol noted it, and it does not affect runtime binaries.

## Accounting

- The meter is a genuine two-sided check: allocator bytes under the coins tag against `Usage::total() − R`, per op, in its own domain per thread. A missed tag or a missed ledger term fails it, as X13's catch shows.
- `presize`, the m6 rule, `prefetch_cap`, `threshold` and `layers` match §2.3 and §2.5.
- The shard trigger, `needs_rebuild` and `needs_boundary_rebuild` match DEC-108's §2.3 amendment (`capacity − len` is hashbrown's `growth_left`).
- The boundary rebuild runs only without F, which matches DEC-108.
- `size_state` is Core's arithmetic on native Σ, which D11 declares as not falling at Core's blocks. `boundary_state` against `expected_boundary` is a restatement, not independent evidence; the independent evidence is the meter plus `core_size_arith`'s edge test.
- Core-relative numbers: witness (ii)'s 0.998× of Core's end-of-ConnectBlock memory is computed as the PF1 text says (tip + the child's spent entries + undo scripts). For (iii), see O3; for the N_core basis, see O11.

## Independent validation

All runs used `TMPDIR=/dev/shm/ch2a-a12-opus-tmp` (mode 700), `RUSTC_WRAPPER=` and separate target directories, from a detached worktree at `6890badf`. Scratch: `/work/scratch/ch2a-a12-opus-high/`.

| Check | Result |
|---|---|
| `cargo test -p dashd-chainstate --features model` | **81 passed**, 0 failed (`test.log`) |
| `cargo fmt -p dashd-chainstate -- --check`; `cargo clippy -p dashd-chainstate --all-features --all-targets -- -D warnings` | pass; pass |
| Miri on `coins::stable`, Stacked and Tree Borrows | 2/2 and 2/2 |
| Soak: a temporary integration test (`review_soak.rs`), 16 threads × 600 s, both drivers (2:1), random config byte, inputs of 1–6,000 bytes, tagging allocator installed, release build with debug assertions and overflow checks | **621,950 runs, 395,878,703 ops, 0 failures** (`soak.log`) |
| Planted bugs and mutations, 1,080 seeded runs each | see O1 and O2 (`mutation-results.txt`, `mutations.diff`, `review_mut.rs`) |

## Which bug classes remain invisible to the model

1. **Membership-only and flag over-approximation** that leaves the database unchanged (O1: MB, MI). The unit tests catch these today; the model does not.
2. **Cross-thread ordering:** the producer's generation-versus-snapshot order (O5), and W's `Arc` drop versus its report (guarded at runtime by `strong_count`, X13). The model is single-threaded and atomic per op.
3. **Best-block plumbing outside the hash:** a commit without a best (O4), F's tip versus the best (O6), and `id`/`height` (O8).
4. **Boundary decisions at the wrong call site** (O3): the model asserts `held_out == 0` at its own boundaries, but nothing checks that the evidence harness reports there.

## Verdict

**APPROVE-WITH-NITS.** No blocker or major. O1–O5 are minor: O1–O3 are evidence and coverage gaps, O4–O5 are API contracts for b1 and CH4. None is a defect in the reviewed transitions. I recommend fixing O3's PF1 row and O4's signature before b1 consumes `commit`, and putting O5's ordering into §2.4.3 before CH4.
