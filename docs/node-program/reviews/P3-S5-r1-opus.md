# P3 S5 review, round 1: `block_download_model` (node/p3-s5 at 80c72b3b6)

Reviewer: Opus (stand-in for Sol). Range 66aec49a9..80c72b3b6 (6e9e059b5, 7175b2ea5, 80c72b3b6).
Core reference: `~/workspace/dash` `v24.0.0-rc.3:src/net_processing.cpp` (the notes' pin; HEAD is
off by one line in this region).

## Verdict: CHANGES

Two small, concrete changes. Neither is a bug in `BlockDownload`. Both are blind spots that the
model and the implementation share, so a real bug there would pass both the proptest and the fuzz
target:

- **F1:** per-hash entry order in `mapBlocksInFlight` is never compared. Core reads that order
  (the staller at np:1554, `first_in_flight` at np:5130). A mutant that breaks it survives every
  test in the crate.
- **F2:** the clock can't step back, so the `std::max` in `RemoveBlockRequest` (np:1310) is never
  exercised. D106 calls the `max(now)` → `now` mutant "equivalent, as in Core", but Core's
  `GetTime<µs>()` can go backwards, and so can ours.

Everything else checks out against Core line by line (see below). The rest are nits.

## Findings

### F1 (CHANGES): the model never compares per-hash order in `mapBlocksInFlight`; a mutant that breaks it survives everything

`dashd-p2p/src/sim/block_download_model.rs:230-240` (`compare`): for each block it checks only
`IsBlockRequested` and `IsBlockRequestedFromOutbound`. `in_flight_entries` checks only the total
count. The order of peers within one hash is never checked, yet:

- Core's multimap keeps equal keys in insertion order, and Core reads the first one:
  - `FindNextBlocksToDownload`'s staller is `mapBlocksInFlight.lower_bound(hash)->second.first`
    (np:1554). The Rust equivalent is `self.in_flight[&e.hash][0].peer`
    (`download.rs:783`).
  - `cmpctblock`'s `first_in_flight` (np:5130) is `in_flight_facts().first`
    (`download.rs:416-423`). It picks between `getdata(MSG_BLOCK)` and `RemoveBlockRequest` on
    a failed `InitData`, and decides whether `getblocktxn` is sent.
- The design states it as the invariant of `in_flight` (P3-block-download.md:186-188: "a C++
  multimap keeps equal keys in insertion order, so `lower_bound(h)->second.first` is element 0").
- `Model.map` already holds that order, so this is one missing comparison, not missing state.

The brief's question 2 lists `vBlocksInFlight` order. That one *is* covered: `queued` compares
each peer's list in order. The per-hash order is the piece that is missing.

**Concrete sequence** (mask `0x00`, step 1 each):
`Request(0,b0) Request(1,b0) Request(2,b0) Remove(b0, Some(0))`. As `check` bytes:
`00 | 00 00 00 01 | 00 01 00 01 | 00 02 00 01 | 03 00 00 01`.

- Core afterwards: b0 → [1, 2]. Peer 1 is the staller and the `first_in_flight` peer.
- With `entries.remove(i)` → `entries.swap_remove(i)` at `download.rs:544`: b0 → [2, 1]. Every
  compared field still matches.

**Measured (mutants on a separate copy of the tree, not the worktree):**

| Mutant | Model tests (proptest, 2,000 cases, + sweep) | Other tests |
|---|---|---|
| M1: `download.rs:544` `remove(i)` → `swap_remove(i)` | **survives** | **survives** all 87 `dashd-p2p` lib tests, `sim_compact` (5), `sim_blocks` (11), `sim_sync` (39), `dashd_p3_transcripts` (51) |
| M3: `download.rs:596` `push(..)` → `insert(0, ..)` | **survives** | killed only by the lib test `the_window_edge_names_the_first_in_flight_peer_as_the_staller` |

So nothing in the tree catches an order bug in removal, and the target that exists to fuzz this
bookkeeping cannot see one.

**Fix (verified):** in `compare`, for each block and each peer, compare
`bd.in_flight_facts(&hash, p)` against the model:

- `first` = the model's first entry for b is p, or there are none;
- `already` = the number of model entries for b.

I added exactly that to a copy. It passes on the unmutated code (2,000 cases plus the sweep). It
kills M1 (`in_flight_order` at the sweep's first input, and at proptest) and M3. Comparing the
full per-hash peer list would be stronger, but `in_flight_facts` is the existing public read and
covers both of Core's consumers. Add M1/M3 to D106's mutant list.

### F2 (CHANGES, small): a clock that never goes back hides Core's `std::max`, and the doc and D106 claim it is equivalent

- `block_download_model.rs:8-9`: "a fourth [byte] that advances the clock by that many µs
  (Core's `GetTime<µs>()` never goes back)".
- `block_download_model.rs:166`: `now += i64::from(step)`.
- D106 (`P3-notes.md`, item 4): "`max(now)` → `now` under a clock that never goes back … equivalent
  here, as they are in Core".

That is not true for Core:

- `GetTime<T>()` is `NodeClock::now()` (`util/time.h:135`, `util/time.cpp:27-36`), which is
  mocktime when it is set, and otherwise `std::chrono::system_clock`. Both step back: regtest
  `setmocktime` takes any value, and the wall clock moves with NTP or manual changes.
- That is exactly why np:1310 takes `std::max(state.m_downloading_since, GetTime())`.
- Ours is the same: `node_now_micros` reads `node_time()`, which honours `set_mocktime`
  (`dash-clock/src/lib.rs:87-90`, with a test at 244-256).

The `max` is live code on both sides, and the target cannot exercise it.

**Concrete sequence** (mask `0x00`):
`Request(0,b0)` +10 µs, `Request(0,b1)` +10, `Remove(b0, Some(0))` −100.

- Core: `m_downloading_since` = max(1010, 920) = 1010.
- The `max`-less mutant: 920.
- As bytes, with a signed step: `00 | 00 00 00 0a | 00 00 01 0a | 03 00 00 9c`.

**Measured on the copy:**

| Run | Change | Result |
|---|---|---|
| E1 | step read as `step as i8` (clock may step back), no mutant | passes: no false positive; both sides take the max |
| E2 | E1 plus M2 (`download.rs:536` `.max(now_us)` → `now_us`) | **killed** by the sweep: `times`, input `b6278f0650fc…` |
| M2 alone | the current module, M2 applied | survives |

**Fix:** make the clock step signed. `i64::from(step as i8)` is enough, since Core never asserts
monotonicity here. Alternatively, keep `u8` and give back-steps their own op. Then:

- correct the module doc (lines 8-9) and D106's equivalence claim: only the self-removal mutant is
  truly equivalent;
- move `max(now)` → `now` into the killed list;
- give the proptest strategy a signed range too (`-16..16i8`, `block_download_model.rs:282`).

### Nits

- **N1** `block_download_model.rs:7-8`: "recorded at its first request as `m_is_inbound` is".
  Core records `m_is_inbound` in the `CNodeState` constructor at `InitializeNode` (np:559, np:1745),
  not at the first request. Recording it at the first request is the Rust representation (design
  §2, P3-block-download.md:189-191: "created on the first request … every reader treats an absent
  entry as an empty one"), and the model copies that choice. With a fixed per-input mask it can't
  be observed. Say "recorded at connect (`InitializeNode`); both sides create state at the first
  request" so the doc doesn't misstate Core.
- **N2** The model's lazy state follows Rust, not Core. `Model.since`/`inbound` exist only after
  the first request, `finalize` returns early without them, and `compare` checks
  `times(p) == None` for a peer that never requested; Core would have `(0, 0)` from
  `InitializeNode`. This is observationally equivalent, because Core never reads a never-requesting
  peer's fields here, and the staller always has state. It is the one place the "transliteration"
  copies the implementation's shape. Either accept it explicitly in the module doc, or compare
  `times(p).unwrap_or((0,0))` against a model that seeds `(0,0)` for all 8 peers.
- **N3** `block_download_model.rs:85` and `download.rs:518` cite np:1287-1321, but
  `RemoveBlockRequest`'s closing brace is np:1322 (1321 closes the `while`). Trivial.
- **N4** Visibility: `pub struct Model` (`block_download_model.rs:74`) has private fields and no
  public methods. `pub enum Op` and `pub fn run` are only used by the module's own tests; node-fuzz
  uses only `check`. They can be private (`Op` and `run` too, since the tests are a child module).
- **N5** `entry()` is duplicated verbatim: `block_download_model.rs:58-68` and the
  `download.rs` tests at 1036-1046. Make the sim one `pub(crate)` and have the `download.rs` tests
  use it, or leave it. It's cosmetic.
- **N6** `check_holds_on_a_sweep` (`block_download_model.rs:298`):
  `(0..600).map(|i| i % 400)` gives lengths 0..399 and then 0..199 again, 600 inputs in all. If
  that is intended, a comment would help; as written it reads like a typo for `0..400`.
- **N7** Floor follow-up: if F1 adds 8 × 16 `in_flight_facts` calls per step, the exec rate drops,
  and D106's 464/s, and with it 830,000, should be re-measured on the new head (D58: a floor only
  rises, so re-measure rather than keep it). Also, smoke 1's 3.1 GB peak RSS (D106) looks high for
  a model of 8 × 16 entries. If that is the unit's build plus libFuzzer rather than the target,
  say so in the note.

## Answers to the brief's questions

**1. Faithful and independent?** Yes, apart from F1 and F2. I checked each cited line against
rc.3:

- **`BlockRequested` (np:1324-1358):** the short-circuit when the same peer already has an entry
  (1333-1340); the self-`RemoveBlockRequest` (1343), a no-op after the short-circuit and kept in
  the model at :113; insert at the list end (1345); on a 0→1 transition, `m_downloading_since =
  now` and `++` (1348-1352); the multimap insert (1353). The model at :109-126 matches.
  `pit`/`partialBlock` is not modelled (see 2).
- **`RemoveBlockRequest` (np:1287-1322):**
  - per matching entry: front → `max` (1309-1311), erase from the list, empty → `--`
    (1313-1317), `m_stalling_since = 0` regardless (1318), erase from the multimap;
  - `from_peer` filters (1301-1304);
  - the `MAX_CMPCTBLOCKS_INFLIGHT_PER_BLOCK` `Assume` is not modelled. The model reaches 8
    entries per hash, which Core's callers never do (≤3 via `cmpctblock`, 1 elsewhere). That is
    harmless for bookkeeping equivalence, and the Rust has no matching assert to trip.

  The model at :86-106 matches, and the order of effects across peers commutes.
- **`FinalizeNode` (np:1804-1830):** erase this peer's multimap entries for each of its list's
  hashes, then `-= !empty`, then erase the state, then run the consistency asserts when the last
  peer goes. The model at :129-139 matches. The Rust `peers.is_empty()` debug-asserts fire when
  the last peer *that ever requested* goes, which is earlier than Core's, but the invariant they
  check still holds then.
- **Stall start (np:6832-6837):** if `pto`'s list is empty and a staller is named, set its
  `m_stalling_since` if it is 0, at `current_time`. The model at :147-155 matches.
- **Skipped steps:** skipping a step whose staller has nothing in flight is justified:
  `waitingfor` comes from `mapBlocksInFlight.lower_bound(..)` (np:1552-1554), so a named staller
  always has an entry. Core also never names `pto` itself (np:1542, `waitingfor != peer.m_id`).
  `Stall(p, Some(p))` reaches nothing on either side, which is fine.
- **Independence:** the model keeps Core's shape, not the Rust one:
  - one global insertion-ordered multimap list against Rust's `HashMap<hash, Vec<InFlight>>`;
  - list-entry identity by iterator id with a `begin() == it` test;
  - entry count from `map.len()` against Rust's separate counter;
  - heights derived from the list against Rust's mirror cell.

  Shared assumptions remain: lazy state (N1/N2, harmless), the order blind spot (F1) and the
  monotone clock (F2).

**2. Coverage of Core's state.** Each item below is Core state, with whether the model covers it:

| Core state | Covered? |
|---|---|
| Each peer's `vBlocksInFlight` order | ✓ `queued`, in order |
| `m_downloading_since`, which is the first-in-flight timeout's start | ✓ (but see F2) |
| `m_stalling_since` | ✓ |
| `m_peers_downloading_from` | ✓ |
| `mapBlocksInFlight.size()` | ✓ (also read by `BlockChecked`, np:2327, `download.rs:983`) |
| `m_is_inbound` | ✓ |
| `IsBlockRequested` / `…FromOutbound` | ✓ |
| Stats mirror (`vHeightInFlight`, np:1888) | ✓ |
| **Per-hash multimap order** | **✗, F1** |
| `QueuedBlock::partialBlock`, i.e. `BlockRequested`'s `pit` path, read by `init_tracked`'s "already syncing" and `fill_partial` | ✗; S3's domain. `init_tracked` delegates to `block_requested`, so the counters are covered, but whether an entry keeps its partial across a re-request is not |
| `m_block_stalling_timeout` (raise and decay) | ✗; S2's units |
| Section 9's disconnect decisions (`section9`), which read these fields | ✗; a natural extension, not required by §6.5 |

**3. Reachability of `check`'s ops.**

- Every op and every state the brief lists is reachable:
  - several peers per block;
  - inbound and outbound (any mask);
  - finalize while stalling: `Request(s,b)`, `Stall(p,s)`, `Finalize(s)`;
  - re-requesting a block in flight from the same peer (returns false) and from another peer;
  - a peer whose state survives its list emptying.
- `% 8` and `% 16` of a byte are unbiased. The `Some`/`None` split at `0x80` is 50/50.
- Two things are unreachable: a clock stepping back (F2), and "ids reused after finalize". Core
  never reuses `NodeId`; the model's re-creation acts like a fresh peer with the same mask bit,
  so this is harmless.

**4. `min_certify_execs`.**

- The arithmetic is right: 151,357 / 326 = 464.3/CPU-s; × 1,800 (half a 3,600 CPU-s window,
  D58/D53) = 835,700, rounded down to 830,000.
- A floor is needed because 464 × 1,800 is below the 1,000,000 default. `cmpct_reconstruct`, at
  925/s, clears the default and so has no key.
- The row is consistent with the other P3 model row: owner `dashd-p2p`, `kind = "model"`,
  `pins = ["v24.0"]`, `smoke = 300` (D31), `nightly = 1`.
- `route = "public"` is right, since nothing decodes a transaction or block.
- The comment names the rate and its source run, as D53 asks.
- See N7 for a re-measure after F1.

**5. Did the move lose any assertion?** No. The old proptest (`download.rs`, removed) asserted:

- the `block_requested` return;
- `peers_downloading_from` and `in_flight_entries`;
- `queued`, `times` and heights for 4 peers;
- `is_block_requested` for 6 blocks.

All of these remain, for 8 peers and 16 blocks, and `is_inbound` and
`is_block_requested_from_outbound` are new. The old `Stall` wrote `stalling_since` on any peer
with state, including one with an empty list. Core can't reach that state: a stall is set only
on a staller with blocks in flight, and every removal resets it. The new `Stall` goes through the
real `start_stall` instead, which is strictly better. The density is lower (8 × 16 with up to
120 ops, against 4 × 6 with 80), but the proptest still produces multi-peer blocks routinely (the
M3 regression shrank to two requests).

**6. Dead, duplicated or simpler?** N4 (visibility), N5 (`entry` duplicated), N6 (the sweep
range). The model's `finalize` could be a single `retain(|e| e.1 != peer)`, but the per-entry loop
mirrors np:1804-1813 and is fine. `run`'s `staller.is_none()` arm only exercises a no-op return;
it is harmless. Nothing is dead.

## What I ran

All builds used `CARGO_TARGET_DIR=/work/scratch/p3-s5-r1-sol/target`, `-j8` and `nice`. Mutants
were applied to a `git archive 80c72b3b6` copy at `/work/scratch/p3-s5-r1-sol/mut`, never to the
worktree. The scripts are `mutants.sh` and `fixcheck.sh`, and their logs are `mut-*.log` and
`fix-*.log`, all in the scratch directory.

1. `cargo test -p dashd-p2p --lib -- sim::block_download_model` on the worktree: 2/2 pass
   (`test1.log`).
2. `cargo clippy -p dashd-p2p --all-targets --features sim -- -D warnings` on the worktree: clean.
3. `PROPTEST_CASES=20000` on the same filter, on the worktree after `cargo clean -p dashd-p2p`:
   2/2 pass (122 s).
4. Mutants at `PROPTEST_CASES=2000`: M1, M2 and M3 as in F1/F2. M1 and M3 were also run against
   all 87 lib tests, and M1 against `sim_compact`, `sim_blocks`, `sim_sync` and
   `dashd_p3_transcripts`. E1 and E2 were the signed-clock checks.
5. The F1 fix prototype on the copy: base passes, M1 and M3 are killed.
6. Core lines were checked against `git show v24.0.0-rc.3:src/net_processing.cpp`, plus
   `util/time.{h,cpp}` for `GetTime`.

I did no libFuzzer run. The target needs the node-fuzz/oracle build, and `run.sh` writes into the
corpus sinks. The owner's two smokes stand. The worktree is unmodified, and `git status` is clean.

**Incident, fixed:** my first 20,000-case run on the worktree hit the shared-target mtime trap. The
copy's mutant build had the same artifact names and newer mtimes, so cargo ran the M3+fix binary
against the worktree. As a result, proptest wrote an untracked
`dashd-p2p/proptest-regressions/sim/block_download_model.txt` (the M3 shrink:
`Request(0,11), Request(1,11)`) into the worktree. I removed it and checked that `git status` was
clean, then ran `cargo clean -p dashd-p2p` and reran (item 3 above).

Cleanup: the `mut/` copy is deleted. To recreate it: `git -C wt archive 80c72b3b6 | tar -x -C mut`. The scripts, the logs and `target/` (1.1 GB) are kept for round 2.
