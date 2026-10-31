# CH1-node BL-224: Opus-h review, round 1 (after Sol r1)

Target ac0219686 (node/ch1-node-bl224), range d94c015c2..ac0219686. The focus is the BL-224 commits (966439810, f9d06202e, 96830ff72, ac0219686). The merged BL-206/D63a commits were only read where they interact with expiry (the `last` skip).

## Verdict

**ACCEPT WITH NITS**

I found no correctness defect. Expiry can't produce a use-after-drop, a torn read, or a wrong-epoch status. The newest-snapshot guard holds in every reachable state of `HeaderChain`. No production caller unwraps the new `Result`. The three nits below are about documentation and the handoff contract. I don't re-report Sol's open-journal `entries` nit.

## What I checked, and why it holds

1. **Weak links: upgrade before use, no use-after-drop, no ABA.**
   - `StatusJournal::status_at` (status_journal.rs:70-91) follows `next` only through `Weak::upgrade`. The `hop` it upgrades stays alive for as long as the borrow `j = &hop` is used. The next upgrade is evaluated before the old `hop` drops.
   - The `Weak` keeps the `ArcInner` allocation alive, so an upgrade can't land on a reused allocation. There is no ABA.
   - Log reads happen under the journal's mutex. `OnceLock::get` (Acquire) pairs with `set` (Release) in `move_on`. Arc's own counters handle the strong-count CAS against the writer's `pop_front`.
   - The diff contains no `unsafe`.
   - The iterative `Drop` could safely go. Once a journal's contents drop, its `next` `Weak` drops too, so a dead successor costs one `ArcInner` (about 72 B) and nothing further down the chain is kept.

2. **An expired reader never reads another status.** `older` only ever loses its front (status_journal.rs:241-253), and a held journal is only reached through live links. So:
   - A walk that returns `Ok` has followed live links from the snapshot's journal all the way to the open one. That is the complete D63 history, so the answer is exact.
   - A walk that meets a dropped link returns `Err(SnapshotExpired)`.
   - A snapshot whose journal was popped can still answer exactly when either (a) its own log has the entry, or (b) every later journal is still alive, for example because a newer held snapshot keeps them. Both are correct values, not wrong ones.
   - Front-only popping also means a snapshot that hasn't expired never meets a dead link. A held journal in `older` can't be popped unless it is the front, and it can only become the front once every journal before it is gone.
   - D63a's `last` skip only skips a write when no build has happened since the entry it relies on. That entry therefore sits in the newest snapshot's region, which is never dropped.

3. **The newest-snapshot guard is defined at the right moment.**
   - `build_snapshot` has two callers, `set_tip_cell` and `publish_locked`. Both store the build in the tip cell (`TipState::publish`, tip.rs:173-184) on the writer thread before any further write, so "the last build" and "the tip cell's snapshot" are the same thing.
   - Inside a hold nothing is built (`refuse_in_hold`). `Hold::release` builds once.
   - `first_epoch == epoch` can only mean "moved to after the last `mark`", because `mark` raises the epoch strictly. So the guard's test identifies the last build's journal exactly.

4. **B4 is equivalent, and for a stronger reason than D65 gives.**
   - The tip cell always holds the last build's journal J\*. `move_on` pushes `current` into `older` before its loop runs. So at every move_on after `mark(0)`, J\* is in `older` and the front comes at or before it, which gives `front.first_epoch ≤ J*.first_epoch < epoch`.
   - The mutant's distinguishing state (a front with `first_epoch == epoch`) therefore can't be reached in `HeaderChain`. Before `mark(0)` nothing holds a journal. Only a bare `StatusJournals` whose newest snapshot was dropped, plus a walker's hop, could reach it.
   - Empirically, B4 also passed my probe (below) at 20,000 rounds.

5. **Callers.**
   - The only users of `ChainReader::status_at` and `ChainViewHandle::{status, valid_scripts}` are tests, and they `expect`/match on purpose.
   - `dashd-p2p`'s `cx.chain.valid_scripts(e.id)` (serve.rs:114) goes through the `ChainAccess` seam, which BL-236 still has to wire up.
   - No production path unwraps an expiry that can actually happen.

6. **Is 65,536 a sane bound?**
   - Each `Entry` is 16 B (u32 id, u32 status, u64 tag). Each journal is about 72 B of `ArcInner` plus a 512 B log. At the bound that comes to roughly 1.2 MiB, which is negligible next to a mainnet block index of about 2.3M entries.
   - Header sync journals almost nothing, since insertion isn't journalled. The bound only matters for block download and connect, where 22k to 33k blocks is a reasonable expiry horizon for a "read and drop" reader.
   - A read of a snapshot that is about to expire walks up to 2,048 journals: one lock, one CAS and 32 comparisons each, on the order of 100 µs. That's acceptable.
   - One unbounded case remains: a burst of writes with no build in between (a large prune, the final loop of `invalidateblock`, load-time `ResetBlockFailureFlags`). It retains, and makes the newest snapshot's readers walk, the whole burst until the next build. This is documented, and it matches Core, where `cs_main` blocks readers for the same span. Sol noted it too.

7. **Many short-lived holders.** An unheld front is popped at the next move_on. A walker's transient hop only delays that by one rotation. There's no other growth path.

## Findings

### N1 (nit): the gauge's "every holder" claim leaves out expired holders

**Where:** dash-chain/src/status_journal.rs:122-124 (`JournalRetention` doc), and the D65 note ("a lower bound on the oldest held snapshot's epoch", "the gauge counts them all").

**Scenario:** A feed subscriber, or a reply, holds a snapshot until its journal expires, and keeps holding it. Its journal has been popped from `older`, but the `Arc` still pins it: up to `rotate_at` entries plus about 600 B per distinct expired snapshot.
- `entries` doesn't include that journal.
- `oldest_epoch` is now *newer* than this holder's epoch, so the D65 sentence calling it a lower bound on the oldest held snapshot's epoch is false for it.
- The field doc ("No held snapshot *the writer keeps journals for*") is scoped correctly; the struct doc and D65 are not.

The memory involved is bounded per holder, so this doesn't break the bound. But a monitor reading the gauge can't see these holders except through the cumulative `expired` count.

**Fix:** In both places, say that expired holders show only in `expired`. That's cheaper and clearer than counting them.

### N2 (nit): "a newer view never expires" depends on an unstated V invariant

**Where:** dashd-chainstate/src/chain/actor.rs:233 (`view: Arc<ArcSwap<ChainView>>`) and 1430-1440 (`scope_end` / `publish_view`), abc.rs:820-831 (`after_release`), view.rs:150-160.

`ChainViewHandle::view()` returns V's own `ChainView` copy, not the tip cell's snapshot. D65's guarantee names only the tip cell. Its recovery advice, "read a newer view", is what BL-236's adapter will do on `SnapshotExpired`. That only works if V's stored view is always the last build.

In this tree it is, by inspection:
- Every dashd-chainstate step runs inside a hold.
- Every publication is followed at once by `publish_view`, with no status write in between (`scope_end`; `release` → `after_release`).

But nothing asserts or documents this. A future path that publishes without `publish_view`, then writes past the bound inside the same command, would leave `h.view()` lagging the tip cell. The view could then expire, and the adapter's retry would get the same expired view back until the command ends.

**Fix:** Add a sentence to D65 and to the `ChainViewHandle::status` doc. Better still, add a `debug_assert_eq!(view.snap.epoch, <tip cell epoch>)` in `publish_view`.

### N3 (nit): B15 survives, and BL-236's row doesn't carry the expiry contract

**Where:** dashd-chainstate/src/chain/view.rs:152-160; ~/workspace/node-program/BACKLOG.md BL-236 (and BL-226).

The implementer's reason for B15 surviving holds: lowering the bound from dashd-chainstate needs dash-chain's `test-util`. Since the pass-through is a single line and its type carries the error, I accept it.

The remaining gap is that the only record of "the adapter must handle `SnapshotExpired`" is D65. The BL-236 row (and BL-226) still describes calling `ChainViewHandle::valid_scripts` on the exact view, with no mention of expiry. P3 reads those rows.

**Fix:** The manager should amend BL-236 (and BL-226). The adapter maps `SnapshotExpired` to "re-take `h.view()` and redo the whole decision on it", never to `false`, because `false` would silently refuse to serve. BL-236's own test should exercise that path; it covers B15 once a bound-lowering hook is reachable from the adapter's crate.

## Disagreements with Sol r1

None on substance. Two additions:
- Sol's "holder coverage is structural" is true for live holders but not for expired ones (N1).
- The never-expires guarantee for `h.view()` rests on V's `publish_view` invariant, not only the tip cell (N2).

## Validation

All runs were in a detached worktree at ac0219686 under /work/scratch/ch1-bl224-r1-opus/, with its own (reflink-seeded) target dir.

- `cargo test -p dash-chain --lib`: **409 passed, 0 failed, 1 ignored**.
- `cargo test -p dashd-chainstate --lib chain::tests::view`: **10 passed**.
- `cargo clippy -p dash-chain -p dashd-chainstate --all-targets -- -D warnings`: **clean**.
- `status_at_holds_under_concurrent_writes --exact`: **20/20 passed**. All 20 ran in parallel alongside 4 probe processes, at load 55→98. The test includes the (1, 2) low-bound case.
- **Scratch probe** `probe_bl224_held_rings_under_bursts`, added to my worktree only and reverted afterwards:
  - Six reader threads each keep a ring of 1 to 512 old snapshots and re-read both a random ring member and the newest one.
  - The writer writes in permuted order with decoy writes. Every 7th round is a 3-pass burst with no build, during which the writer checks that the tip cell's snapshot reads exactly and never expires.
  - (rotate_at, retain_at_most) ∈ {(1,1), (1,3), (2,4), (3,7), (32,64), (1,64)}.
  - Run 4× at 40,000 rounds in parallel: **0 wrong statuses, 0 tip-snapshot failures**, about 1.5 to 3.2M exact reads and up to 760k expired reads per configuration, with gauge `expired` up to about 35k.
- **B4 mutant plus the probe** at 20,000 rounds, alongside all journal and `chain::tests::the_*` tests: passed. That fits the equivalence argument in point 4.
- I read the mutant script and log (/work/scratch/ch1-node/mut-bl224.py, .log): 14/16 caught, B4 equivalent, B15 survives (N3).

The worktree and target were removed afterwards; only this file and probe.rs are left in the scratch dir.
