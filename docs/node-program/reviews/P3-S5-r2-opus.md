# P3 S5 review, round 2 (narrow): `block_download_model` (node/p3-s5 at e8b3a709c)

Reviewer: Opus-h (standing in for Sol). Fix range 80c72b3b6..e8b3a709c: 728b0fa86 (code) and
e8b3a709c (D108 and the floor). Core reference: `v24.0.0-rc.3:src/net_processing.cpp` (np), as in r1.

## Verdict: ACCEPT

Both findings are closed, and the nits are applied or reasonably declined. The floor is right by
D58's method. Nothing in the range goes beyond the stated intent. One optional wording nit (n1)
needs no re-review.

## 1. F1: closed

- `compare` (`dashd-p2p/src/sim/block_download_model.rs:241-261`) now builds each block's peers
  from `model.map` in insertion order (:243). For every block and every one of the 8 peers it
  checks `bd.in_flight_facts(&hash, p)` against `{already: peers.len(), first: peers.first()
  is none or p, from_this_peer: peers.contains(p)}` (:251-260).
- That is exactly Core's `cmpctblock` read at np:5125-5137:
  - `already_in_flight` = `distance(equal_range)`;
  - `first_in_flight` = `already_in_flight == 0 || range.first->second.first == pfrom`;
  - `requested_block_from_this_peer` = the peer is anywhere in the range.

  The implementation side, `download.rs:416-423`, reads `entries.first()`, the same element as
  the staller read at `download.rs:783` (`self.in_flight[&e.hash][0].peer`, i.e. np:1554's
  `lower_bound(hash)->second.first`).

**Does `in_flight_facts` expose what both of Core's consumers read? Yes.**

- `cmpctblock` reads exactly these three facts.
- The staller is the unique `p` with `first == true` on a non-empty block. Since `compare` asks
  all 8 peers and the model only has peers 0..8, the first peer's identity is pinned after every
  step.

**Is any order-dependent state still uncompared? None that can stay hidden.**

- Order below the first slot is not read directly, and Core never reads it directly either.
  Core's only order-sensitive read is the first element.
- A permutation in slot k becomes the first-slot fact as soon as the entries ahead of it are
  removed or finalized, and `compare` runs after every step. So any order bug surfaces on some
  op extension the target can reach. The killed mutants M1 and M7 below are exactly that class,
  in removal and in finalize.
- The other order-carrying state is already compared:
  - each peer's `vBlocksInFlight` order (`queued`);
  - its stats mirror (`heights`);
  - list-entry identity for the front test, through `times`.
- Core never iterates `mapBlocksInFlight` across hashes in this code, so the Rust `HashMap`'s key
  order is unobservable.
- `partialBlock` stays S3's, as r1 noted.

**The staller read at `download.rs:783` is outside the model's ops** (no
`FindNextBlocksToDownload` step). A mutant of that read itself (M6: `[0]` → `.last()`) survives
the model's tests, as it should, and the lib test
`the_window_edge_names_the_first_in_flight_peer_as_the_staller` kills it. So the state is pinned
by the model and the consumer by the lib test. This is not a finding.

## 2. F2: closed

- The step is `step.cast_signed()` (:275), the proptest uses `-16..16i8` (:303) and the clock
  starts at 10⁹ µs (:175).
- Mutants M2 (`max` → `now`) and M8 (`max` → `min`) are both killed on `times`, at the r1 sweep
  input `b6278f0650fc…`.

**Is there still a lower bound on the clock that matters? No.**

- The only value where the clock's sign or magnitude matters in this code is 0. That is the
  `m_stalling_since == 0us` "not stalling" sentinel: np:6833, `download.rs:661`, model :163,
  reset at np:1318.
- All three sides share Core's sentinel semantics, so even a clock at 0 would not diverge.
  Back-step magnitude doesn't matter either: `max` is magnitude-free, and section 9's timeout
  arithmetic is not one of the model's ops.

**Does the 10⁹ start mask anything? No.**

- Core's `GetTime` is never ≤ 0 in practice. `setmocktime` rejects negative values, mocktime 0
  means "off" (the wall clock), and the wall clock is about 1.7×10¹⁵ µs.
- Starting at 10⁹ keeps the model inside Core's domain rather than probing a negative clock that
  neither side was written for.
- The old 1,000 µs start with signed steps would have gone negative within 8 steps, so moving the
  start was needed.

**The "under 31 MB" claim is right.**

- The clock reaches exactly 0 after 10⁹ / 128 = 7,812,500 steps of −128 µs, which is
  1 + 4 × 7,812,500 = 31,250,001 bytes. Any shorter input keeps it ≥ 128 µs.
- So "under 31 MB" holds in decimal MB, but not in MiB (31 MiB = 32,505,856 bytes). See n1.
- libFuzzer inputs here are kilobytes and proptest runs at most 120 steps, so the bound has no
  practical effect.

## 3. N5 declined: reasonable

Sharing `entry()` with `download.rs`'s tests would make the non-`sim` unit tests depend on the
`sim` module through the self dev-dependency. The program avoids growing that dependency (the
self-dev-dep rule in PROCESS.md's Gates section). It would also couple the model's fixtures to
the implementation's tests, which works against the model's independence. The duplication is
11 lines of test fixture. Declining is the better call.

## 4. N7: arithmetic right, 380,000 safe

**Arithmetic by D58's method** (P3-block-download.md §6.5: half the execs of a 3,600 CPU-s window,
at the rate measured when the row lands, with the rate recorded):

- 68,974 / 320 = 215.5 execs/CPU-s;
- × 1,800 = 387,961, rounded down to 380,000.

The row and its comment record the rate, the run and the head. Smoke 3 ran at 728b0fa86, and
e8b3a709c changes only `P3-notes.md` and `targets.toml` (`git diff --stat 728b0fa86 e8b3a709c`),
so the measured code is what ships.

**The causal claim checks out.** I measured `check` over the same 40 fixed 4,097-byte inputs
(1,024 steps each), with release builds of both heads and `perf stat -e instructions:u`, minus a
0-input baseline. I used instruction counts rather than wall clock, per the box's guidance.

| Head | Instructions per input | errs |
|---|---|---|
| 80c72b3b6 | 31.7 M | 0/40 |
| e8b3a709c | 81.5 M | 0/40 |

- That is 2.57× more work per step from the 128 extra `in_flight_facts` reads.
- libFuzzer's fixed per-exec overhead (mutation, coverage) dilutes the ratio, so the observed
  464 → 215 /CPU-s (2.16×) matches. Corpus growth is not needed to explain it.

**Neither unreachable nor too lax.**

- 215 × 3,600 ≈ 776,000 per window, so the floor has the 2× margin D58 intends. It stays
  reachable unless the per-exec cost roughly doubles again.
- It isn't lax by the program's rule. Half a window is D58's chosen bar, and the rate was taken on
  a grown corpus, the slower and representative case.
- Lowering a floor is legitimate here. The target's per-exec work grew, so the old 830,000 would
  have been unreachable in one window (215 × 3,600 < 830,000). D58's "a floor only rises" is about
  re-measuring rather than carrying a stale value. D108 records the reason.

## 5. Nothing beyond the stated intent

The full diff is four files.

**`block_download_model.rs`:**

- the doc rewrite (N1, N2, F1, F2);
- the private `PEERS`/`BLOCKS`/`Op`/`Model`/`run` (N4); `check` stays `pub`, and it is node-fuzz's
  only use (`node-fuzz/src/models/block_download_model.rs:8`);
- the citation now ends at np:1322 (N3);
- the `i8` step and the 10⁹ start (F2);
- the `in_flight_facts` compare (F1);
- the sweep over `0..600` (N6).

**`download.rs`:** the citation only (N3).

**`P3-notes.md`:** D106 amended (the equivalence claim struck, 15/15, the new floor) and D108
added.

**`targets.toml`:** the floor and its comment.

The u8 → i8 reinterpretation changes what the existing grown corpus's step bytes mean. That is
harmless for a model target, since nothing stores expected outputs, and smoke 3 ran clean on that
corpus.

D108 cites `reviews/P3-S5-r1-opus.md`. It is in `~/workspace/node-program/reviews` but not yet in
the rust-dashcore tree, the same as S3/S4's reviews. That is the manager's filing step, not a
finding.

## Nit (optional, no re-review)

- **n1** `block_download_model.rs:11-12` and D108 (2) say "stays positive for any input under
  31 MB". The exact bound is 31,250,001 bytes, where 7,812,500 steps of −128 µs reach 0. "Under
  31 MB" is true in decimal MB but reads as false in MiB. "Under 31.25 MB (7.8 million steps)"
  would be exact. It has no practical effect.

## What I ran

All long jobs ran as `systemd-run --user … -p MemoryMax=20G` with `CARGO_BUILD_JOBS=8`. Logs and
scripts are in /work/scratch/p3-s5-r1-sol: `r2-wt.sh`, `r2-mut.sh` and `r2-probe.sh`, with output
in `r2-mut-*.log` and `journalctl --user -u p3s5-r2-{wt,mut,probe}`.

1. **Worktree at e8b3a709c, detached** (unit `p3s5-r2-wt`, in `target/` after
   `cargo clean -p dashd-p2p`):
   - `cargo test -p dashd-p2p --lib -- sim::block_download_model`: 2/2 pass (6.5 s).
   - The same at `PROPTEST_CASES=20000`: 2/2 pass (256 s, against r1's 122 s, consistent with the
     added reads).
   - All 87 `dashd-p2p` lib tests pass.
   - `cargo clippy -p dashd-p2p --all-targets --features sim -- -D warnings`: clean.
   - `git status`: clean.
2. **Mutants** (unit `p3s5-r2-mut`) on a `git archive e8b3a709c` copy, with its own
   `target-mut` (reflink-seeded) rather than the worktree's target, which avoids r1's
   stale-artifact trap. Each mutant was applied to a fresh copy of the pristine file and checked
   applied. All ran at `PROPTEST_CASES=2000`, filter `sim::block_download_model`; the base copy
   passes.

   | Mutant | Line and change | Result |
   |---|---|---|
   | M1 | `download.rs:544`, `remove(i)` → `swap_remove(i)` | killed, `in_flight_facts` (sweep and proptest) |
   | M3 | `download.rs:596`, `push` → `insert(0, …)` | killed, `in_flight_facts` |
   | M2 | `download.rs:536`, `.max(now_us)` → `now_us` | killed, `times` |
   | M5 (new) | `download.rs:420`, `first` from `entries.last()` | killed, `in_flight_facts` |
   | M7 (new) | `download.rs:1004`, `FinalizeNode` uses `swap_remove` of the peer's entry instead of `retain` | killed, `in_flight_facts` |
   | M8 (new) | `download.rs:536`, `max` → `min` | killed, `times` |
   | M6 (new, outside the model's scope) | `download.rs:783`, staller `[0]` → `.last()` | survives the model's tests (expected); killed by the lib test `the_window_edge_names_the_first_in_flight_peer_as_the_staller` |

3. **N7 probe** (unit `p3s5-r2-probe`): a scratch-only `rate_probe` test appended to archive
   copies of 80c72b3b6 and e8b3a709c, release builds in separate targets, then
   `perf stat -e instructions:u` at `PROBE_N=0` and `PROBE_N=40`. Results are in §4.
4. **Core lines read:** np:1305-1311, 1548-1556, 5124-5138, 6754 and 6833-6834.

No libFuzzer run, and nothing written to any corpus. Nothing in ~/workspace was edited, committed
or pushed.

**Cleanup:**

- removed the worktree (`git worktree remove --force`);
- removed `target/`, `target-mut/`, `target-old/` and `target-new/`;
- removed the `mut/`, `old/` and `new/` copies.

Scripts, logs and the reviews are kept.
