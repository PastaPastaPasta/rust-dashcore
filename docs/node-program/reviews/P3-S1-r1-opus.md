# P3 S1 review: Opus (high), round 1

**Branch:** `node/p3-s1`, `6dc9720c5..147f09b89` (ef2834e88, 40286563e, f1a8a890d, 147f09b89)
**Design:** `node/p3-design` `docs/node-program/designs/P3-block-download.md` §2 and §6.1's S1 rows; S1 notes D60–D73
**Core:** Dash Core v24.0.0-rc.3 (`np` = net_processing.cpp, `v` = validation.cpp, `net` = net.cpp)

## Verdict: CHANGES REQUIRED (tests only)

I found no code defect. The order is Core's under every interleaving I built: score before tail, the source-map erase positions, getdata, and announce. The queue, the region and header cell, and the two-phase loop are sound. But two of §2.6's load-bearing claims are not pinned by any test:

- the block wait drains the signal queue while it waits;
- the reply's waker wakes the wait.

A mutant that removes either one passes the whole S1 suite. Both cases can be tested with S1's own `SimChain`, and I attach probes that catch both mutants (F1). F2 and F3 are Low.

No possible Dash Core security weakness was found; nothing was written to `~/private/embargo/`.

## Findings

### F1 (Medium): the block wait's in-wait drain and its reply waker are untested; both mutants survive

**Where:**
- `dashd-p2p/src/processing/blocks.rs:117-131` (`wait_block`). Line 129 is the in-wait drain. The `DoorbellWaker` that the poll registers is at `signals.rs:144-152`.
- The design states both claims: §2.6 ("services SyncSignals while waiting"; "no wakeup is lost", design l.160, l.864), and §6.1's "An actor in its block wait with a `BlockChecked` queued and published wakes on the ring alone".
- D71 moves "the actor in its block wait waking on the ring alone" to S4/S6, because `SimChain` "has no release-point waits". But `SimChain` does have C5′'s pre-accept wait (`chain.rs:483-491`, D67), and that wait is enough to drive the case. Neither D71 nor any other row mentions the reply waker.

**Mutants (implementation only; full S1 run is `cargo test -p dashd-p2p --test sim_p3_s1 --lib`):**
- **M2:** delete `self.drain_sync_signals()` at `blocks.rs:129`. All 48 tests pass.
- **M3:** make `DoorbellWaker::wake`/`wake_by_ref` no-ops. All 48 tests pass.

**Why the suite misses them:**
- Only `getpeerinfo_does_not_block_while_the_actor_waits_on_process_block` (sim_p3_s1.rs:341) runs the actor in a real wait (`spawn_v`). It runs with the sim's default `ibd: true` (chain.rs:110), so C5′ never waits, and nothing needs draining before the reply.
- `SimChain::publish_with` rings `published()` (chain.rs:648) synchronously before every reply `slot.set` (chain.rs:469). So in every test, a ring precedes the reply, and the waker is never the only wake-up.

**Schedule A (M2 is a deadlock):**
1. Tip G, IBD latched false, V on its own thread.
2. The actor's pass drains an empty queue, then runs P's `block(B)`. Phase 1 emplaces the source, drops the region and sends `ProcessBlock(B)`.
3. Before V takes that command, V serves another origin's accept (RPC `submitblock X`): it pushes `BlockChecked(X)` with stamp s and publishes s.
4. V then serves `ProcessBlock(B)`. C5′ (`wait_quiescent`) finds one outstanding entry, rings and parks.
5. On the head, the actor wakes on that ring, drains X, the item drops, and V accepts B. Under M2, the actor goes back to sleep on the doorbell, V stays parked, and neither ever wakes.

The same dependency holds for the C5 `wait_for_space` release points in production, with a full queue in place of C5′. Core has no analogue, because its signal runs inline.

**Schedule B (M3 is a lost wakeup):**
1. V publishes B's accept and rings.
2. The actor wakes on that ring, drains and polls before V stores the reply, so the poll returns `Pending`, and the actor sleeps on the doorbell.
3. V then stores the reply. Its waker is the only wake-up left.

In production this ordering is normal. The ring that follows a publication is the queued `BlockConnected` on the signals thread (§2.5; C6 splices it after `publish_view`), and nothing orders it before CH1-node's `ActorReply` close. Under M3, the actor sleeps until some unrelated ring.

**Proof:** the probe file is kept at `/work/scratch/p3-s1-r1-opus/mut/probe_opus.rs`. I ran it as `dashd-p2p/tests/probe_opus.rs` in the review worktree and removed it afterwards. It registers a delegating `SimListener` around `Shared` that:
- (A) at `wait_quiescent`, first plays another origin's accept, `shared.block_checked(X)` then `chain.republish()`;
- (B) in `published()`, after ringing, holds V for 300 ms before the reply is stored.

The actor's `pass()` runs on a scoped thread. The test fails (exit 3) if the pass has not finished within 20 s, then asserts the new tip.

| Probe | head 147f09b89 | M2 | M3 |
|---|---|---|---|
| `another_origins_signal_at_c5_prime_is_drained_inside_the_block_wait` (A) | ok, 8/8 runs | **hangs** (exit 3) | ok in sequence, hung once when run alone (racy) |
| `the_reply_alone_wakes_the_block_wait` (B) | ok, 8/8 runs | ok | **hangs** (exit 3) |

**Fix:** add the two probes, or equivalents, to `sim_p3_s1.rs`, and update D71 to say that the in-wait ring case is covered in S1 through C5′. No code change is needed.

### F2 (Low): `on_block` drops Core's `"Block mutated: %s"` log line

**Where:**
- `dashd-p2p/src/processing/blocks.rs:92` calls `is_block_mutated`. That function discards the `Mutation::MerkleRoot(state)` reason.
- `dash-consensus/src/block.rs:83-113` added `block_mutation` so that "the caller writes Core's line", but no caller does.

**Core:** `IsBlockMutated` (v:4434-4440) logs `LogPrint(BCLog::VALIDATION, "Block mutated: %s\n", state.ToString())` whenever `CheckMerkleRoot` fails. On the P2P path this comes just before np:5367's `"Received mutated block from peer=%d"`.

**Schedule:** a peer sends a `block` whose merkle root doesn't match its transactions (sim_p3_s1's `a_mutated_block_scores_100_and_reaches_no_chain`). With `-debug=validation`, Core prints both lines; the port prints only the NET line.

This is not wire-visible. §3.11's string list also omits the line, so fix both:
- in `on_block`, use `block_mutation` and emit the VALIDATION line for `MerkleRoot(e)` with Core's `state.ToString()` form;
- add the line to §3.11 / O11's asserted-log list.

### F3 (Low): BL-206 (D73's production follow-up) names only `status`; `valid_scripts` has the same hazard

**Where:**
- D73's sim fix moved `SimChain::valid_scripts` (chain.rs:1067) to the published copy, as well as `status`.
- But D73's production paragraph and BL-206 (BACKLOG.md:205) bind only "S6's `ChainAccess::status`".
- `ChainAccess::valid_scripts` (seams.rs:152) reads the same `BLOCK_VALID_SCRIPTS` bit. P2b's `block_request_allowed` uses it (serve.rs:114, np:2048-2055) for `getheaders`/`getdata` serving.

**Schedule:** this is D73's own window, with a serving read in place of a P3 one.
1. During B's `ConnectTip`, `raise_validity` makes B `VALID_SCRIPTS` in the arena, before the `ChainView` carrying tip B is stored.
2. A `getdata(B)` region on the old view runs `BlockRequestAllowed(B)`. B is not in the old view's active chain. A live `valid_scripts(B)` returns true, but the last published value is false.

Core cannot show that state in one `cs_main` section (the same citations as D73: np:3202/3205 against v:3471/v:3761).

**Fix:** widen BL-206 to "every `ChainAccess` status read (`status`, `valid_scripts`)". This is tracking only; S1 code is unaffected.

## Notes (no finding)

**Core order and the source map.**
- Phase 1 (`blocks.rs:79-112`) matches np:5348-5387: `LoadingBlocks` before decode; mutation scored 100 with `RemoveBlockRequest(hash, from)`; `IsBlockRequested`, `RemoveBlockRequest(hash, from)`, then `emplace` (never overwrites, download.rs:227-232).
- `BlockChecked` erases unconditionally at download.rs:303 (np:2342-2343).
- The tail (`blocks.rs:135-150`) is np:3717-3732: on `new_block`, `m_last_block_time` then `RemoveBlockRequest(hash, nullopt)`; otherwise the tail's own erase.
- M1 (the tail before the C1 drain) is caught. So, for this block's own signal, the score lands before the erase and before P's `SendMessages`, which is Core's inline order. Another origin's signal can be applied in the wait, or in the post-reply drain before the tail. Both match a Core schedule where another thread takes `cs_main` between `ProcessNewBlock`'s return and the tail's `LOCK(cs_main)`.

**C2 on the reject path.**
- The tail's erase for a block that isn't new is safe only if the post-reply drain covers that block's `BlockChecked`. That needs a publication before every reply, including a `CheckBlock` failure that changes no index state (v:4884-4893).
- CH1-node already does this: `process_block_accept` calls `block_checked`, then `scope_end()` → `publish_view` (unconditional `view_seq += 1`), then the reply (dashd-chainstate accept.rs:124-137, actor.rs:1264-1288).
- C2's text names only the `Activated` reply. Consider stating the reject case in A-1. A cheap actor-side `debug_assert` that no queued entry carries `hash` after the C1 drain would turn a future host regression into a test failure instead of a silent lost punishment.

**A peer that disconnects during the wait.**
- The tail runs on the `Arc<Mutex<PeerState>>` that `process_messages` holds. `BlockDownload::finalize` runs only at the next pass's `drain_finalized`, after the tail, which is Core's order (refcounted `CNode`).
- One effect does change. If SB6 drops P from its table during V's ABC, `BlockCheckedCx::with_peer` (seams.rs:365-366) skips P's punishment. Core still finds `State(P)` there, so it runs `Misbehaving`.
- The only difference is the `"Misbehaving: peer=…"` debug line. Core's message handler skips `fDisconnect` nodes (net.cpp:3699), so `MaybeDiscourageAndDisconnect` (np:6312) never runs for P.
- The with_peer doc already records this window. S1 lengthens it from between two messages to a whole ABC. Worth a sentence in that doc; no change needed.

**Wait, shutdown and deadlock.**
- Lock order, everywhere:
  - actor and readers: state, then cell;
  - V: cell only;
  - `send_messages` head: `block_inv` (leaf), released before the drain.
- No path holds a cell across a V wait. `assert_no_header_cell` is on `wait_block`, `process_block`, `process_headers` and `wait_published`, and `process_headers_message` calls `leave_region()` first (sync.rs).
- The drain in the wait locks only source states, and the actor holds none in phase 2.
- `node_state_stats` drops the state before `wait_published`.
- Shutdown: a dropped reply becomes `Interrupted`, with no tail. `CloseOnDrop` releases a parked V. The no-timeout wait is the design's (Core has none). D72 (2) covers a V that panics.

**Signal queue.**
- One FIFO, so the order holds across peers. The cutoff takes a prefix (M5 caught).
- The stamp is the last `view_seq` + 1 (M4 caught, two-cell test).
- Each entry is released exactly once: by `Processing::drop`, by `Drained::drop` for entries not yet taken, or by `close` for queued entries. Neither path double-counts or underflows.
- Push never blocks. The bound acts only through V's waits.
- An entry is never applied twice: it leaves the deque at `drain`.
- An entry is dropped unapplied only by `close` (the actor has stopped), or by a panic mid-drain (the actor is dead).

**HeaderCell, HeaderGuard and peek (D61/D62).**
- No `unsafe`. `HeaderCell` is `Arc<{Mutex<Option<PeerHeaders>>, Condvar}>`, and `HeaderGuard` owns a `Copy` value and a cell clone, so `Send`/`Sync` are the auto-traits.
- A peek copies under the slot mutex only while the slot is `Some`, and the guard writes the whole value back in one store under that mutex. So a peek cannot see a torn state.
- The thread-local count is right only if a guard drops on the thread that locked it. That holds because the guard is stored only through `Region`, and `Region` holds a `parking_lot::MutexGuard`, which is `!Send` without `send_guard` (D61). No other caller of `PeerState::enter_region` exists.
- The token lock is not reentrant. A peek of a cell its own thread holds would self-deadlock, but no non-test caller of `peek_headers`/`with_state` runs inside a region. node-l1's harness uses `with_state` (p2p_node.rs:1026), which is itself a peek region.

## Checks run

All runs used `RUSTC_WRAPPER=`, `CARGO_BUILD_JOBS=12`, and a target dir under `/work/scratch/p3-s1-r1-opus` (0700), in a detached worktree at 147f09b89.

- `cargo test -p dashd-p2p --test sim_p3_s1 --lib`: 33 + 15 passed.
- `cargo test -p dashd-p2p` (with the probe file present): every target passed. Lib 33; inbound transcripts 7; transcripts 11; probe 2; sim_m2 44; sim_p3_s1 15; sim_serve 11; sim_sync 51.
- I touched no node-l1 or dashd-net seam, so I didn't run their tests.

## Mutants (implementation only, each reverted)

| # | Mutation | Result |
|---|---|---|
| M1 | Run `block_tail` before the post-reply `open_region` drain (logic.rs:574-578) | **Caught** by `an_invalid_block_from_two_peers_punishes_each_before_its_tail` |
| M2 | Drop the in-wait drain (blocks.rs:129) | **Survives** the suite; caught by probe A (hang) |
| M3 | `DoorbellWaker` wake/wake_by_ref no-ops (signals.rs:144-152) | **Survives** the suite; caught by probe B (hang) |
| M4 | `BlockChecked` stamp without `+ 1` (peerman.rs `block_checked`) | **Caught** by `a_drain_between_the_two_cells_neither_shows_the_new_tip_nor_applies_its_signals` |
| M5 | `drain` ignores its cutoff (signals.rs:311) | **Caught** by `signals::tests::a_drain_takes_exactly_the_prefix_at_or_below_its_cutoff_in_push_order` |

Scratch: the target and the worktree are deleted. The probe source, the mutant script and its logs remain in `/work/scratch/p3-s1-r1-opus/mut/`.
