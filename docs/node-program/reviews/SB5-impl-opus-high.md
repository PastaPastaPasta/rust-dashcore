# SB5 implementation review: Opus 5.5 (high)

- **Subject:** `node/sb5-feeds` at **b097e28a7**, on `node/integration` ad0084fc1. The range `ad0084fc1..b097e28a7` holds 62 commits.
- **Spec:** SB5 rev3 e60728067 (DEC-159) with errata D1–D83; SB4; DEC-187 with amendments 2 and 3; CH1-node rev3.1 (DEC-157, DEC-158).
- **Prior reviews:** SB5-impl-r1/r2/r3-sol (r3 APPROVE) and SB5-design-opus-high. I don't reopen their closed items here. The one place I touch a documented decision, under F-2, is marked as a follow-up and doesn't block.
- **Method:**
  - I read the feeds, hold, segmented, status, cold and open code, plus their tests and loom models.
  - I ran ten mutants against targeted test filters, with the M0 baseline at 162 passed.
  - I ran two probes in a detached worktree: SPV open cost against chain length, and a `register` re-entering `finish()`.
  - The probe source and logs are kept in `/work/scratch/sb5-opus-impl/probes/`, and the mutation logs in `/work/scratch/sb5-opus-impl/mut/`.
- **D84:** it is the implementer's in-progress follow-up, uncommitted on the branch's worktree at the time of writing. I list it separately at the end, and it is not part of this verdict.

## Summary

The feed machinery holds up under review and under mutation:

- the slot's O(1) leaf sections and the closing check under the slot lock (F11);
- the writer's no-step-while-pending rule;
- the cursor-target exclusion in fanout (F1);
- the trim on every retire;
- control coalescing that moves a kind to the back;
- release wakes fired after the guard drops (§4.8 step 4);
- `acquire_tip`'s `try_lock`-only registration (DEC-157 (2));
- CLOCK second chance;
- the issued watermark read before the resident lookup (D79/D81, L14);
- `fault::residency` marking arena and index growth (DEC-187 am. 2 (c)).

Each of these is pinned by at least one test that fails when the guard is planted wrong (M1–M9).

I found no defect that breaks CH1-node's contract or SPV-LO9 on the paths the design uses. The findings are:

- **F-1 (Minor):** a liveness hole in the public `FeedSource::subscribe_at`. A `finish()` that re-enters `register`/`subscribe_at` spins forever, where the same mistake made inside a hold returns `WouldDeadlock`.
- **F-2 (Minor, follow-up):** SPV `open` costs O(tip − floor) rows. The cost is documented (D48, D59), but D48's premise no longer holds after DEC-174.
- **F-3 (Minor):** one surviving mutant, M10. The registry's `seq_synced` can be off by one with no test noticing.
- A few nits, and two simplification candidates.

## Findings

### F-1 (Minor): `finish()` re-entry spins instead of returning `WouldDeadlock`

- **Location:**
  - `dash-chain/src/feed.rs:651-664` (`acquire_tip`, the `0 => yield_now()` arm at :660);
  - `dash-chain/src/feed/table.rs:633` (`subscribe_at` runs `finish` under the tip lock);
  - `dash-chain/src/feed/source.rs:108-115` (the public trait). Its doc says only "runs `finish()` … atomically under the tip lock", and it doesn't forbid re-entry.
- **Scenario:**
  1. A `FeedSource::subscribe_at` caller's `finish` closure calls `ChainFeeds::register` (or `subscribe_at` for a second feed).
  2. The tip lock is held by this same thread in an ordinary section, not a hold, so `held_owner` is 0.
  3. `acquire_tip`'s `try_lock` fails forever, and the loop yields forever.
  4. The result is a silent self-livelock of the subscribing thread. Every later writer step and registration then also waits on the tip lock, so the chain stalls.
- **Evidence:** the probe `zz_register_inside_finish` (probes/zz_opus_open_probe.rs:86-109) did not return within 5 s and logged "PROBE register inside finish did not return within 5 s (spinning)". The design gives `held_owner` exactly this role for holds (§4.3, DEC-157 (2): never block, refuse on the owner's thread). The ordinary handoff section is the one tip section it doesn't cover.
- **Fix:** while `finish` runs, have `subscribe_at` set the same owner token (or a separate `section_owner`), so that `acquire_tip` on that thread returns `Refused::WouldDeadlock`. Add a test that re-enters `register` and `subscribe_at` from `finish`. Doing it the other way, by documenting the prohibition on the trait, is weaker: CH2's `finish` is foreign code, and the failure mode is a hang, not an error.

### F-2 (Minor, follow-up; documented in D48 and D59, not a regression): SPV `open` reads and allocates O(tip − floor) active rows

- **Location:**
  - `dash-chain/src/chain/sb5/open.rs:251`: `active_from(self.floor)` collects every row from the floor.
  - `:257`: the density check over all of them.
  - `:299`: `set_base(base)` keeps only the window.
- **Scenario:** an SPV chain anchored far below its tip pays to reopen in proportion to its whole history, where §8 step 4 states w-scale. Examples are a desktop SPV wallet anchored at genesis, or a mobile wallet whose checkpoint anchor ages by about 210k heights a year.
- **Evidence:** the probe `zz_open_peak_vs_length` used w = 2,048, `FeedConfig::spv_mobile()`, C = 1,024 and `MemHeaderStore`:

  | Heights | Active rows | `open` peak | Retained after | Time |
  |---|---|---|---|---|
  | 5,000 | 5,001 | 1,892,156 B | 1,091,360 B | 74 ms |
  | 50,000 | 50,001 | 2,534,364 B | 1,091,920 B | 55 ms |
  | 200,000 | 200,001 | 4,717,516 B | 1,091,920 B | 118 ms |

  - The retained size is flat, so DEC-174's windowing works. The transient peak grows by about 14.6 B per height. That extrapolates to a peak of about 36 MB for a genesis-anchored mainnet chain of about 2.37M heights, plus a full `active_from` scan over SQLite. SB13 has not measured that scan.
  - The `open/mem-w10000` bench (12,000 headers at w = 10,000) can't show this, because tip − floor ≈ w there.
- **Why it's only a follow-up:**
  - D48 chose `active_from(floor)` because "C4's SPV view is unwindowed (SB4 D-C4-7)". D59 kept it after DEC-174 ("the rows below are checked, not kept"). Both were reviewed.
  - DEC-174 removed D48's premise, though. The rows below `base − 1` are now read only to check that they are dense, and D48 itself says a corrupt row there "goes undetected". The O(tip − floor) read buys a density check on data that is then dropped.
  - Simplification toward Core: Core's `LoadBlockIndex` doesn't re-verify `chainActive` rows on load.
- **Fix:** read `active_from(base − 1)` (or `base` when `base` = floor). Check the window as now and the link at `base − 1`, and let the context `walk_down` supply the heights below. Then update §8 step 4, D48 and D59 to w-scale. Add an open test with tip − floor ≫ w that bounds the peak (as the probe does) and the rows read.

### F-3 (Minor, test gap): the registry's `seq_synced` can be one seq early undetected (M10 survives)

- **Location:** `dash-chain/src/feed/table.rs:422` (`e.live = Seq::new(ctx.next_seq)`) and `:481` (`seq_synced: e.live.map(Seq::get)` in `republish`).
- **Scenario:** M10 sets `live` to `next_seq − 1`, clamped at 1, and 99 of 99 feed tests still pass.
  - `live` feeds two things: the registry's `seq_synced` (H2), and fanout's `debug_assert!(first >= live)` at :305, which a lower `live` only weakens.
  - The handoff's `seq_synced` is computed separately, so the subscriber is unaffected. A registry consumer would see a cut one seq earlier than the subscriber's.
  - `table_tests.rs:128` checks `seq_synced == 1` at a fresh table, where `next_seq` is 1 and the clamp hides the mutant.
- **Fix:** in `table_tests.rs`, after some steps (`next_seq` > 2), subscribe at the tip and assert that the registry entry's `seq_synced` equals the handoff's. Better still, make `republish` derive it from the same value the handoff returns.

### F-4 (Nit): public SPV `HeaderChain::set_active_tip` is the unbounded composite

- **Location:** `dash-chain/src/chain.rs:1820` (SPV arm) and the doc at :1697.
- **Scenario:** a public caller outside the header step moves the SPV tip in one hold, of any length. SPV-LO9's bound on hold time comes from the segmented step (`activate_segmented`), not from this API.
- **Evidence:** `tip_plan`'s doc at :1697 says "Unbounded: an SPV step … moves the tip in bounded sub-steps". The callers in tree are tests, the bench and `rules/tests.rs`.
- **Fix:** narrow it to `pub(crate)` or test-util, or state on `set_active_tip` itself that SPV-LO9 doesn't hold through it.

### F-5 (Nit): `ChainFeeds::on_release` grows without a cap

- **Location:** `dash-chain/src/feed.rs:680` and `feed/source.rs:52` (public).
- **Scenario:** a caller that loops `on_release(w)` during a hold, without waiting on `w` as the protocol asks, pushes one `Notify` per call until the release drains the list.
- **Bound:** the growth is bounded by call rate × one sub-step's hold (LO9). It is reachable only from in-process code, not from peers.
- **Fix:** cap it at `max_feeds` + a small constant (refuse or coalesce), or document the protocol as a precondition.

### F-6 (Nit): the loom hold models run a copy of the production order

- **Location:** `dash-chain/src/feed/hold_loom.rs:38-48`. The helpers `open` and `release` restate `hold_opened` → … → `hold_closing` → guard drop → `fire_release_wakes`.
- **Effect:** a reorder in `Hold::end` or `Composite::drop` isn't seen by L1–L5. It is seen by `chain/sb5/hold_tests.rs:402` (`release_wakes_after_the_guard_drops`), which killed M6.
- **Fix:** give the helpers and `Hold::end` one shared `fn release_order(guard)`, so the model runs the production sequence.

### F-7 (Nit): `FeedSlot::close` drops the reader's waker without notifying

- **Location:** `dash-chain/src/feed.rs:454`.
- **Note:** Sol already raised this. A parked reader learns about the close only on its next timed wake or `next()`. I'm only noting it remains.

### F-8 (Nit): `try_push` clones the waker on every push

- **Location:** `dash-chain/src/feed.rs:412` (`Push::Pushed(g.waker.clone())`) and `feed/table.rs:571` (`wake = w;`).
- **Effect:** each push in a run clones an `Arc`-backed waker and drops the previous one. `push_run` needs only the last one.
- **Fix:** return a `bool` and take the waker once after the run.

### F-9 (Nit): `ColdCache::insert`'s replace path leaves `by_hash` alone

- **Location:** `dash-chain/src/cold.rs:230`.
- **Effect:** it is correct today, because a replace never changes the hash for an id. That invariant isn't asserted, though.
- **Fix:** add `debug_assert_eq!` on the hash.

### F-10 (Nit): no per-feed lag diagnostic

- **Effect:** a stalled subscriber is visible only as a full slot, a pending backlog and the writer's refusals. No `(feed id, live, cursor, backlog)` counter is exposed for SB15 or the operator.

### Simplification candidates

- **S-1:** `fire_release_wakes`' buffer swap (`feed.rs:686-699`) keeps the `release_waiters` lock section allocation-free, at the cost of two lock sections per release. If F-5 caps the list, a fixed-capacity buffer reserved once would remove the swap.
- **S-2:** F-2's fix brings `open` closer to Core's load semantics (no re-verification of active rows below the window).
- **S-3:** the D39 status seed, both `StatusReader::new` seeding and `arm` re-checking, could fold into `arm` alone if `new` arms immediately. It is a candidate only, not a defect.

## Mutation table

Every mutant was an exact-string edit applied to the worktree and restored after its run (`mut.py`). The filters were the feed, status, hold, segmented, LO9, enforce-feed, A23 and F8 modules, plus cold and spv where relevant. The baseline M0 ran the union filter: 162 passed, 0 failed.

| # | Guard | Mutant | Result | Killed by |
|---|---|---|---|---|
| M1 | F11: `try_push` checks closing under the slot lock (`feed.rs`) | the check removed | **KILLED** (hang) | `feed/tests.rs:294` `dereg_wake_releases_producer` never returns; killed at 633 s |
| M2 | F3/F5: the reader's pop wakes a producer that wants space | `want_space` taken, `false` returned | **KILLED** | 7 failures, among them `producer_wake_on_space`, `f3_full_sets_want_space_and_pop_wakes`, `compress_then_wake`, `spv_hold_subscribe_on_two_workers`, `hold_q1_two_disconnects_parked` and `hold_q1_two_disconnects_crashratio`; one more hangs |
| M3 | a retire trims the pending backlog (`table.rs` `retired`) | `trim_pending()` removed | **KILLED** | `f11_step4_drops_the_backlog`, `p1_feed_model` |
| M4 | n1: a coalesced control kind moves to the back | `rotate_left` removed | **KILLED** | `f4_coalesced_moves_to_back`, `control_outstanding_per_kind` |
| M5 | F1: a target with a cursor gets no direct pushes | the `cursor.is_some()` skip removed | **KILLED** | `f1_cursor_order`, `p1_feed_model` |
| M6 | §4.8 step 4: release wakes fire after the guard drops (`hold.rs`) | the two calls swapped | **KILLED** | `chain/sb5/hold_tests.rs:402` `release_wakes_after_the_guard_drops` |
| M7 | §5.2: a CLOCK hit sets the reference bit | `referenced = true` removed | **KILLED** | `clock_second_chance`, `clock_hand_advances` |
| M8 | DEC-187 am. 2 (c): arena and index growth is residency | the `fault::residency` wrapper removed | **KILLED** | `step_budget` `peak_full_step_desktop`, `_mobile`, `_mobile_b500` ("step 545280 ≤ 205244") |
| M9 | D79/D81: `admit` refuses `id ≥ issued` | `>=` changed to `>` | **KILLED** | `refuses_a_record_not_issued` |
| M10 | F10/X28: a subscribed slot goes live at `next_seq` | `live = next_seq − 1` | **SURVIVED** | 99 passed. See F-3 |

Score: 9 of 10 killed. The survivor is a test gap in observability state, not in what the subscriber receives.

## Focus-area notes

1. **Concurrency and loom.**
   - The slot section is O(1) and holds no other lock.
   - Lock order is tip → slot → (never the table from a slot).
   - The reader does control and data in one section (F4).
   - Status publishes facts → version → wakes, and `arm` registers then re-checks.
   - L14 runs the production `resolve_by_id` and the production cache.
   - L1–L5 run a copy of the hold order (F-6). The production order is pinned by a unit test (M6).
   - I ran no loom models this round. D83's `check-loom.sh` runs all six modules (47 models, with counts checked), and Sol r3 and the gate ran them.
2. **Liveness.**
   - Registration never blocks on a hold: `try_lock` only, with `HoldActive` and `WouldDeadlock`. The exception is F-1.
   - SPV-LO9 holds on the header step's segmented path, with at most E events per sub-step and wakes after each release. F-4 is the bypass.
   - Memory under adversarial subscribers:
     - each slot is capped by `slot_bytes`;
     - pending is trimmed at every retire (M3);
     - control is at most 8 kinds per feed, coalesced (M4);
     - `on_release` (F-5) is the one uncapped list.
3. **Contract fit.**
   - CH1-node rev3.1 (DEC-157 HoldActive, DEC-158) gets the `FeedSource` and `ChainSnapshot` shape it expects. The handoff is atomic under the tip lock. Refusals are typed.
   - SPV gets a segmented stage 4 with a release-time fanout of the last segment.
   - F-1 should be fixed before CH1-node's `finish` lands.
4. **The cold path.**
   - CLOCK, the watermark, the sweep and snapshot-cold reads are sound (M7, M9).
   - The SPV open is sound but O(tip − floor), see F-2.
   - `fault::residency` costs nothing in production. Without `test-util` (and outside `cfg(test)`), `fault.rs:143-168` makes it an `#[inline(always)]` `f()`, makes `Section` a ZST and makes the intake check an empty fn. The thread-locals exist only in the test arm.
5. **Mutations:** see the table.
6. **Simplification toward Core:** S-1 to S-3, and F-2.

## D84 (separate; uncommitted at the time of writing, not reviewed as code)

At the time of writing, the branch head is still b097e28a7. The implementer's worktree has uncommitted edits to:

- `dash-chain/Cargo.toml`;
- `Cargo.lock`;
- `benches/feeds.rs`;
- `benches/support/mod.rs`;
- `src/fault.rs`;
- `src/mem_store.rs`;
- `src/mem_store/tests.rs`;
- the design's D84 entry.

What D84 touches, for whoever reviews it:

- **The self dev-dependency** `dash-chain = { features = ["test-util"] }` is removed. With it, PF0 benches measured the residency and slot-section hooks. `feed_alloc` gains `required-features = ["test-util"]`, joining `full_chain`, `spv_memory` and `step_budget`.
  - Check that `--all-features` CI still builds all four.
  - Check that `cargo test --workspace` without features skipping them is intended.
- **`fault.rs`** gets `allow(dead_code)` on `in_residency` and `in_slot_section` without `test-util`. This is a lint change only. The zero-cost property above is unaffected.
- **`benches/feeds.rs`** rebuilds its rigs on the public API, and compiles `src/mem_store.rs` with `#[path]`. The `cold/*` benches now go through `ChainReader::try_entry` over an SPV chain, so they now include the resident miss and the decode. Their numbers aren't comparable with earlier PF0 runs even though the bench IDs are unchanged. PF0 should take a new baseline.
- **`benches/support/mod.rs`** adds a `compile_error!` canary for release builds with `test-util`, which is the D84 mitigation the PF0 rule asks for.
- **`mem_store.rs` and its tests** import from the crate root so the `#[path]` include compiles. They should be checked for no behavioral change.

None of this changes the findings above. F-2's probe would need porting to the public `SpvRig` in `benches/support` if it is to become a test.

**Verdict:** APPROVE-WITH-NITS
