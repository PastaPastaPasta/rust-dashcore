# CH1-node D68 (DEC-281): Opus-h narrow review, round 1

Scope: `909087861..c0649bf4b` on `node/ch1-node-t21` (487226b8a, 0f769cc20, 945dc4d62, 5e57c51f6,
2036f3c51, docs c0649bf4b), crate `dash-chain` plus the `dashd-chainstate` test/doc touch.
Read-only; reviewed in a detached worktree at c0649bf4b (`/work/scratch/ch1-d68-review/wt`).

## Verdict: ACCEPT WITH NITS

I found no correctness defect. The epoch→journal lookup is sound under concurrency. The
retention rule keeps the newest build's journals and expires exactly the snapshots that need a
dropped one. The single epoch counter has the same values and the same serialisation as before.
The findings below are a minor test gap, a hand-off for P3, and stale docs.

## Validation

- `cargo test -p dash-chain` at c0649bf4b, own target dir, 12 jobs: lib 413 passed, 0 failed,
  1 ignored; the other two test binaries passed (1 and 11 tests).
- `contrib/node/check-layers.py`: `layers ok` (the new `once_cell` dependency, `race`+`alloc`,
  no default features, is pure Rust and passes the S-layer gate).
- Mutation sweep: 10 hand mutants of `status_journal.rs`, run against the journal, expiry,
  `status_at`, retention and newest tests. Script: `/work/scratch/ch1-d68-review/mutants.py`.

| Mutant | Result |
|---|---|
| A `newest = false` | killed (`the_newest_snapshot_never_expires`, `the_tip_cells_snapshot_never_expires`) |
| B newest checks `journals[0]` | killed |
| C empty index → `Err(SnapshotExpired)` (`:176`) | **survived** (see N1) |
| D floor set without a pop | killed (5 tests) |
| E `partition_point(first_epoch < epoch)` | survived, **equivalent**: it only starts the walk one journal earlier, and every entry there has tag ≤ epoch |
| G floor + 1 | killed |
| I retention bound `<=` → `<` | killed |
| K count only floor expiries, not walk expiries | killed **only** by the concurrent test, which is probabilistic (see N2) |
| M floor check off | killed (4 tests) |
| N never count expiries | killed |

## 1. Epoch→journal lookup under concurrency: sound

- **Lookup** (`status_journal.rs:159-183`). The reader takes `kept.read()`, does a binary search
  for the newest journal with `first_epoch <= epoch`, clones its `Arc` and releases the lock
  before the walk. The `floor` check gives `SnapshotExpired` exactly when the snapshot's start
  journal (the last with `first_epoch <= epoch`) was popped. Popping is front-first and the front
  then has `first_epoch == floor`. So:
  - for `epoch >= floor`, the start journal is in `kept`;
  - for `epoch < floor`, it is gone.

  The walk after that is D63's walk, unchanged. Popped journals that another reader still holds
  keep their `Weak` `next` links upgradable, and the walk through them stays exact. A link that
  doesn't upgrade is `Expired`, never a wrong status.
- **Writer pops vs readers.** `move_on` (`:315-347`) changes `journals`, `floor` and `full`
  together under `kept.write()`, so no reader can see a torn index. Popped `Arc`s move into
  `gone` and drop after `drop(kept)`. A reader holding a clone keeps that journal alive, so
  nothing is used after it is freed. Journals point to each other only through `Weak`, so a
  drop never cascades through the chain.
- **Visibility.** A new journal is pushed into the index (`move_on`), then appended to, then
  the arena is written. A reader whose live seqlock read saw that arena write has the push
  happen-before its later `kept.read()`, through the lock's release/acquire. So it finds the
  journal. A reader that didn't see the write gets the same old status either way.
- **OnceBox race** (`:204-231`). Only the writer calls `get_or_init` (`record` takes
  `&mut self`, and only the `move_on` arm calls `slot.index()`, `:300`), so there is no
  competing initialiser. Readers use `get()`, an Acquire load. The OnceBox store happens-before
  the push, which happens-before the arena write, so a reader that saw the write sees the box.
  The window between `get_or_init` and the push gives a reader an empty index and `Ok(None)`.
  That is correct, because the arena hasn't been written yet (N1).
- **Lock ordering.** There are no nested locks anywhere:
  - the writer holds `kept.write()` only inside `move_on`, which takes no other lock and calls
    nothing outside it;
  - `next.set` runs before that lock, and the log push runs after it;
  - readers hold `kept.read()`, then a log `Mutex`, one at a time.

  None of the chain's other locks (tip mutex, feed locks) is taken while either is held. D66's
  rule for P3 still holds, because the chain side never holds `kept` or a log lock while it
  calls into the processor. The wording "lock-free" no longer does (N3).
- **Retention rule** (`:325-334`). It pops while `journals[1].first_epoch != epoch` and the
  kept full journals exceed `retain_at_most`. So the front left is the last journal with
  `first_epoch < epoch`, which is the newest build's start journal. That journal is never
  popped, and `floor` is at most the newest build's epoch. With no journal open at the build
  (the first journal created lazily after it), every journal has `first_epoch == epoch` and
  nothing pops. Before the first build (epoch 0) nothing pops either. That is fine, since the
  node's first build is `publish_first` → `set_tip_cell` at construction (`chain.rs:534-546`).

## 2. The behaviour change: narrow, and already within every consumer's contract

- **What changed.** Under D65 a held snapshot always kept its *own* journal (by `Arc`), and
  expired only when its walk needed a later journal that had been dropped. Under D68 any read
  of a snapshot below the floor is `SnapshotExpired`. That includes:
  - reads its own journal would have answered (`chain.rs:3394` now asserts `Err` where
    909087861 asserted `Ok(Some(before))`, "its own journal");
  - ids never written since the snapshot.

  The bound is still D65's: 2^16 entries. So only snapshots more than `RETAIN_AT_MOST` writes
  old are affected, and those could already expire under D65. Within the bound, every read is
  exact, held or not (`the_writer_keeps_the_journals_within_the_bound`).
- **Callers.**
  - In-tree production callers of `ChainReader::status_at` are only
    `ChainViewHandle::status`/`valid_scripts` (`dashd-chainstate/src/chain/view.rs:154,160`).
    Their contract already says expired → take `view()` again and redo.
  - V's own view is always the newest build. `debug_invariants` asserts it at
    `actor.rs:1468-1478`, and the newest build never expires (mutants A and B are killed).
  - Nothing in-tree reads `JournalRetention::expired`, whose meaning changed from "held journals
    dropped" to "expired reads". So no metric misreports.
  - P3's `ChainAccess` seam (`dashd-p2p/src/processing/seams.rs:126-160`) has
    `valid_scripts(id) -> bool` and `status(snap, id) -> Option<_>` with no error channel. The
    production adapter isn't wired yet (only `Tree` and `SimChain` implement it). See M1.
- **Is 2^16 enough?** At 2–3 journalled writes a block it covers about 22k–33k blocks. In
  early-chain IBD at thousands of blocks a second, that is a few seconds of wall time. That is
  plenty for message-scoped P3 snapshots and RPC replies, and V's view never expires. It isn't
  enough for a reader that holds one view across a long scan during IBD. Such a reader must
  re-take the view on `SnapshotExpired`, as the docs say. D68 makes this no worse than D65.
- **Memory.** After 2^16 writes the node keeps about 1.14 MiB of journals for good. Under D65
  that was the worst case. The D68 note says so; I accept it.

## 3. Lazy gauge and first journal: correct

- `JournalSlot::read` returns the default (0, 0, 0) before the first write. SPV and a load never
  allocate the index or a journal (`spv_tests.rs:1883,1916` assert `is_empty()`).
- `full = (len-1) * rotate_at` (`:343`) is exact, because a journal is left only when it holds
  exactly `rotate_at` entries. `read()` takes `full` and the open journal from one locked view,
  then that journal's length (`:185-195`).
- `expired` counts every `Err` from both paths, the floor and the walk (`:179-181`). The chain
  test checks it equals the readers' own count (`chain.rs:3493-3498`).
- There is no first-write race: the writer is the only initialiser.

## 4. Single epoch counter: same values, same serialisation

- `next_build` (`:274-278`) is a relaxed load+store with no RMW. It is safe because its two
  callers serialise:
  - `set_tip_cell` runs once, before any cell exists (`chain.rs:545-548`);
  - `publish_locked` runs under the tip mutex (`&mut TipState`, `chain.rs:1665-1667`), and the
    mutex orders the relaxed accesses.

  `record` reads the counter through `get_mut`, which needs `&mut HeaderChain`, so it can't race
  a build. The values match 909087861: first build 0, then +1 per publication, and
  `debug_assert_eq!(snap.epoch, self.snapshot.epoch + 1)` (`tip.rs:181`) still pins that.
- Former readers of `TipState::epoch`:
  - `publish` and `after_publish` now use `snapshot.epoch`, which is the same value by the
    invariant;
  - `vacant()` copied it, and is fine (enforce restores the real state before
    `publish_locked`, `enforce.rs:194-196`).
  - `FeedTable`'s `epoch` (`feed/table.rs:494`) is an unrelated counter.

  There is exactly one `HeaderChain` construction (`chain.rs:576`), always with a fresh
  `Shared`, so the counter and the index can't drift apart.
- `repr(align(16))` on `TipState` and `#[inline]` on `acquire_tip` don't change semantics.

## 5. Tests

The tests pin expiry (floor, walk, newest-never-expires, the gauge count), lazy creation
(`the_first_journal_comes_with_the_first_write`, the SPV `is_empty` asserts) and the D63 races
through the deterministic hooks (`Walk`/`Reader`/`Writer`). Gaps: N1 and N2. Mutant E is
equivalent.

## Findings

### Blocker
None.

### Major
None.

### Minor

**M1. Hand-off to P3 S6: the `ChainAccess` seam can't carry `SnapshotExpired`**
(`dashd-p2p/src/processing/seams.rs:153` `valid_scripts(&self, id) -> bool`; `status(snap, id)`
just below).
- When S6 wires the CH1 adapter, it must map `SnapshotExpired` to a retry on a fresh view, not
  to `false`/`None`. Mapped to `false`/`None`, `BlockRequestAllowed` would mis-report on a lagging
  snapshot.
- D68 makes this slightly more reachable than D65: a hit in the snapshot's own journal now
  expires too.
- No change is needed in this branch. Add it to the BL-206 / F1 hand-off row that already asks
  S6 to pass the caller's snapshot (Opus BL-206 r1 F1).

### Nits

**N1. The empty-index branch is untested** (`status_journal.rs:176`, `Some(None) => Ok(None)`).
- Mutant C (`Err(SnapshotExpired)`) survives.
- The branch is reachable only between `slot.index()`'s `get_or_init` and `move_on`'s push.
- Either:
  - add a `Site` hook there and assert that a reader in the window reads the live status; or
  - remove the window by having `index()` create the index with the first journal already in
    it (for example, pass the journal into `move_on` before the box is published).

**N2. Walk-path expiry counting is pinned only probabilistically**
(`status_journal.rs:600-616`).
- `a_journal_dropped_under_a_walk_expires_it` doesn't check the gauge. Mutant K (count only
  floor expiries) is killed only by the threaded `status_at_holds_under_concurrent_writes`.
- Add `assert_eq!(index.read().expired, 1)` after line 615. That makes it deterministic.

**N3. Stale "lock-free" claim** (`dashd-p2p/src/processing/seams.rs:124-125`: "CH1-node's
`ChainViewHandle` reads are lock-free (seqlocked snapshot)").
- Since D63, `status`/`valid_scripts` take a journal's log `Mutex`. Since D68 they also take
  the index's `RwLock` read side.
- D66's rule still holds, as shown in §1. The sentence should say "take only CH1-internal
  leaf locks the chain side never holds across a call into the processor".
- This is P3's file; carry it with M1.

**N4. The D68 row in `CH1-node-notes.md` (line 63) contradicts itself in three places, from
before the 0f769cc20/2036f3c51 rework.**
- "under a `parking_lot::Mutex` the reader takes once a read": it is a `RwLock`
  (`status_journal.rs:140`).
- "a `JournalSlot` (`ArcSwapOption`, one pointer …)": it is `OnceBox`. The row's later sentence
  says so, but the earlier one still stands.
- "one relaxed store of the next tag (`StatusJournals::mark`)": it is `next_build` (a relaxed
  load+store); `mark` is now `#[cfg(test)]`.

**N5.** The module doc's "a build costs one relaxed store" (`status_journal.rs:9`) is a
load+store (`:275-276`). Wording only.

## Answers in one line each

1. **Lookup under concurrency:** correct. There are no nested locks, nothing is used after it
   is freed, and no reader can see a torn index. The OnceBox has only one initialiser.
2. **Behaviour change:** yes, a snapshot more than 2^16 writes old now also expires for reads
   its own journal would have answered. The only in-tree consumer (`ChainViewHandle`) handles
   it, and V's view never expires. P3 S6 must not map it to `false`/`None` (M1). 2^16 is enough
   for message- and reply-scoped readers.
3. **Lazy gauge and journal:** correct, and there is no race. The empty-index window is
   untested (N1).
4. **Single epoch counter:** same values, serialised by the tip mutex and `&mut`. There is no
   ordering issue.
5. **Tests:** good. Mutant C survives (N1), mutant E is equivalent, and mutant K is only caught
   probabilistically (N2).
