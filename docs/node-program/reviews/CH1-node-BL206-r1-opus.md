# CH1-node BL-206 (epoch status journal): Opus-h round 1 (independent second review)

Reviewed in a detached worktree at `cfa2fc0a1` (`/work/scratch/ch1-bl206-r1-opus/wt`, own target and TMPDIR, both since deleted). Core is `0364ed626e` (v24.0.0-rc.3); `v` = `src/validation.cpp`, `np` = `src/net_processing.cpp`.

- **Part A, BL-202 + D62a** (`8ecfadf39..9d525fecd`): narrow look, only where BL-206 depends on it.
- **Part B, BL-206** (`9d525fecd..cfa2fc0a1`): full review.

I formed this view before reading Sol r1; where I differ from it is at the end.

## Findings

| # | Sev | Part | Where | Finding |
|---|-----|------|-------|---------|
| F1 | Minor (hand-off; blocks P3 S6, not this merge) | B | `dashd-p2p/src/processing/seams.rs:143-149`, `serve.rs:104-114`; `dashd-chainstate/src/chain/view.rs:144-157` | `ChainAccess::valid_scripts(&self, id)` takes no snapshot, but D63's API is `valid_scripts(&view, id)`. |
| F2 | Minor (test gap; change requested) | B | `dash-chain/src/chain.rs:2147-2148`; tests | No committed test lowers or clears a status through the journal. Mutant M3 (journal only when a bit is set) passes the whole dash-chain suite. |
| F3 | Nit (test gap) | B | `dash-chain/src/reader.rs:171-176` | Mutant M4 (a second live read after the walk) passes the committed suite. Only a concurrent probe catches it. |
| F4 | Nit (doc) | B | `dash-chain/src/status_journal.rs:42-48` | `status_at` gives the wrong reason why a journal with no `next` ends the walk. Mutant M2 (append, then link) is equivalent. |

### F1. P3's `valid_scripts` cannot name the view its caller holds (Core `np:2048-2054`)

Core's `BlockRequestAllowed` reads `ActiveChain().Contains(pindex)`, `pindex->IsValid(BLOCK_VALID_SCRIPTS)` and `m_best_header` in one `cs_main` section (`AssertLockHeld`, `np:2050`).

P2a's `block_request_allowed` (`serve.rs:104-114`) takes `cx.snap` for the active chain and the best header. `dashd-p2p`'s `ChainSnapshot` (`seams.rs:48-61`) is described as held "for the whole Core critical section". It then calls `cx.chain.valid_scripts(e.id)`, which has no snapshot argument.

That `ChainSnapshot` is a copy of the tip facts and the epoch. It carries no `Arc<ChainReadSnapshot>` and no `ChainView`. So a production adapter over CH1-node can only implement `valid_scripts(id)` as `h.valid_scripts(&h.view(), id)`. That view is the latest one, which can be one or more publications newer than `cx.snap`. The result is not ahead of the latest published view, so BL-206's literal wording is met. It is still a mixed-epoch read that Core's single section never shows, and that is the incoherence P3 S1 D73 raised.

The trait doc makes the trap worse. It points implementers at "SB4 `EntryView::status`" (`seams.rs:145`), and `ChainViewHandle::entry` (`view.rs:144`) gives exactly that field, live and seqlocked. It can be ahead of any view, as `a_reader_sees_the_status_of_the_view_it_holds` shows at `Stage::Release`.

`ChainAccess::entry` itself returns `EntryFacts`, which has no status (`seams.rs:65-80`). So the live `entry()` is not a leak in the P3 trait as it stands. The way in is a `valid_scripts` adapter written as `entry(id).status.is_valid(..)`.

**Ask (manager / P3 S6, before production readers are wired):**
- Give `ChainAccess::valid_scripts` (and any later `status`) the caller's snapshot: `valid_scripts(&self, snap: &ChainSnapshot, id)`.
- Have P2a's `ChainSnapshot` carry an opaque handle to the CH1 view it was taken from (the `Arc<ChainView>`, or the `Arc<ChainReadSnapshot>`). The adapter then calls `ChainViewHandle::valid_scripts(&view, id)` on the matching view.
- Change the trait doc's "SB4 `EntryView::status`" to "`ChainViewHandle::status`/`valid_scripts`, as of the snapshot (D63), never `entry().status`".
- On the CH1 side, add one line to `ChainViewHandle::entry`'s doc: "not for `ChainAccess::status`/`valid_scripts`: use [`Self::status`]". Renaming it isn't needed: CH1's own readers (V, W's freeze) want the live value.

This does not block merging Part B. D63's API is right; the problem is how P3 wires it. It should go into the BL-206 closure note, or a BL row against P3 S6, so S6 does not wire `valid_scripts(id)` over `view()` or `entry()`.

### F2. Status lowering is not tested through the journal

`modify` journals on `g.status != old` (`chain.rs:2147`). That covers lowering correctly: Core's `ResetBlockFailureFlags` clears bits at `v:4180` and `v:4197`, and here that is `reconsider`, `enforce.rs:357`. Raising is covered too (`v:2900`, `v:3979`, `v:4016`). Sol showed this with two scratch probes, but neither was committed.

Every committed D63 test only raises: `raise_validity` in `a_status_write_*`, `connect_block` in V's test, and the unit tests' `VALID_*` sequence. Mutant M3 (`if g.status.bits() & !old.bits() != 0`, i.e. journal only when a bit is set) passes all 398 dash-chain lib tests.

**Change:** add one dash-chain test. Set FAILED_VALID, publish, take a snapshot, then `reset_failure_flags` (or `modify` clearing the bit). The old snapshot's `status_at` must still show FAILED_VALID, and the new snapshot must not. Sol's `sol_probe_lowered_status_keeps_prior_snapshot` has that shape.

### F3. A reader re-reading the arena after the walk is not caught

`ChainReader::status_at` reads the arena once, before the walk (`reader.rs:172-175`). Mutant M4 replaces `unwrap_or(live)` with a second arena read taken after the walk. It returns a status newer than the snapshot whenever a write lands after the walk. The owner's `Site::Reader` hook sits between the live read and the walk, so it never puts a write after the walk, and M4 passes the committed suite. My concurrent probe (below) fails it in 3 of 3 runs, in under 2.5 s each.

**Suggestion:** commit the probe, or one like it. It is about 50 lines and runs in about 0.5 s on this box. It is the only test that exercises the protocol with real threads rather than hooks. Alternatively, add a hook after the walk.

### F4. The `status_at` doc gives the wrong load-bearing reason

`status_journal.rs:44-48` says a journal without a `next` "has every write the caller's live read saw, as the writer links the next journal before it appends there". The order of link and append does not matter. What matters is that both happen in `record` before `modify`'s seqlocked arena write (`chain.rs:2148` then `:2152`), and the reader's live read comes first.

Suppose the live read saw write W. Then W's `fence(Release)` (`entry.rs:90`) synchronizes with the reader's `fence(Acquire)` (`entry.rs:116`). The `OnceLock::set` and the `Mutex` push both happen before that fence, so the reader's later `next.get()` returns `Some`.

Mutant M2 (push into the new journal, then link the full one) passes every test and the probe. It is equivalent under that argument.

**Suggestion:** reword it to "...has every write the caller's live read saw: the writer links and appends before its arena write, which the live read synchronized with". A later refactor, such as a lock-free log or writing the arena before journalling, would then keep the real invariant, not the stated one.

## Part B analysis (BL-206)

**Epoch tagging at the build boundary.** Both builds are published at once:

- `publish_locked` (`chain.rs:1665-1667`) builds the snapshot at `st.epoch + 1`, and `TipState::publish` stores it inside the same tip-lock section (`tip.rs:173-183`, debug-asserting +1).
- `set_tip_cell` builds epoch 0 (`chain.rs:545-548`).

`mark(E)` hands over the current journal and moves the writer's tag to E+1 (`status_journal.rs:122-126`). So a write tagged t ≤ E happened before build E, and t > E means after it. The first entry for `id` with `tag > E`, from `snap.journal` forward, is the pre-image of the first write after the build. That is the arena value at the build, which is the snapshot's own status. With no such entry, there was no write after the build and before the live read (that would be visible: see the ordering argument), so the live value is the build's value.

Writes before the first build get tag 0 and are hidden from epoch 0, which saw them. That is correct, and the 3261b9fba fix holds. The `debug_assert` in `mark` only fires in debug builds, but the two callers are fixed. The one test that builds a snapshot it never publishes (`spv_tests.rs:997`) ends straight after.

**The ChainView boundary.** `publish_view` (`actor.rs:1308-1327`) wraps `self.chain.snapshot()`, which is the dash-chain snapshot just stored. Nothing writes a status between `publish_status` or the release and `publish_view`: `after_release` only defers `ClEnforce` first (`abc.rs:696-706`, `locks.rs:253-260`). So the view's live-derived fields (`mtp`, `tip_chain_tx`, `large_work_invalid_chain`) agree with `status_at(view.snap)`. And because tags are taken against the dash-chain build, not the view store, even a status write in that gap would still read correctly through `status_at`.

**No reader sees a status newer than its view.** The writer journals (Mutex unlock = Release) and then does the seqlocked write (`seq` odd, `fence(Release)`, fields, `seq` even with Release). If the reader's accepted seqlock read returned W's status, the writer's release fence synchronizes with the reader's acquire fence. W's push (and, at a hand-off, W's `OnceLock::set`) then happens before the reader's `next.get()` and `log.lock()`. So the walk reaches W's journal and finds the pre-image of the first write after the build. This holds under the C++/Rust model, not only on x86-TSO. `ArcSwap::load` (acquire) against `store` (release) gives the converse.

**No reader sees a status older than its view.** Every write before build E happens before the `ArcSwap` store of E, so a reader holding E reads a live value no older than the one at build E. Entries tagged ≤ E are skipped.

**The 32-write hand-off.** A journal with `next` set is final: every push into it happens before the `set` (release), and the reader's `get` acquires it. The walk reads `next` before the log (c641b9984), so it never steps past a journal still open when it scanned it. A journal with no `next` holds every write the live read saw (F4). There is one writer (`&mut self`), so concurrent writers are excluded by type. Readers only take `log.lock()`, for a scan of at most 32 entries.

**Reorg, invalidation and lowering.** Every Node-mode status change goes through `HeaderChain::modify`. I checked the direct `group.status` writes outside it:

- `add_new`/`insert_entry` are inserts, not journalled by design: an entry inserted after a snapshot reads "as of its insertion".
- The load path (`chain.rs:1166-1176`) runs before any snapshot.
- `stage4_effects` is SPV only.
- `set_status` (Node) maps to `modify`.
- `enforce.rs:275,279` (MarkConflicting), `:343,357` (reset) and `chain.rs:1492` (accept's FAILED_CHILD) all use `modify` or `set_status`.

The journal stores the full pre-image whichever way the status moved, so lowering is handled correctly (F2 is coverage only). Disconnects write no status (Core `DisconnectTip`, `v:3249` onward, and `DisconnectBlock`, `v:2282` onward, change no `nStatus`). `BlockId`s never get reused (`chain.rs:674`, Permanent residency). A panic in `f` leaves no journal entry. A panic between `record` and `write_group` leaves an entry whose pre-image equals the live value, which is harmless.

**Retention when readers drop promptly.** The published snapshot (`ArcSwap` + `TipState.snapshot`, plus the one `ChainView` V stores) pins its journal and every later one, up to the writer's `current`. That is at most ⌈writes since that build / 32⌉ + 1 journals. A hold bounds the writes between publications (one Core ABC step, or the enforcement walk), so the total is bounded.

Journals before the published snapshot's journal drop as their snapshots drop, and the drop is iterative (`status_journal.rs:76-84`, tested at 10^5). dashd-chainstate keeps no older `ChainView` or snapshot (grep: only `ChainView.snap`). `dashd-p2p`'s `ChainSnapshot` is a copy and pins nothing.

Things that do hold snapshots, for BL-224's gauge to list:

- SB5's `FeedSnapshot` during a CH2 catch-up walk (`feed/source.rs:181`).
- `HeadersResult.snap`, `TipChange.snap` and `TipSummary.snap` returned to callers, which V drops at once.

**PF0 (DEC-261).** Nothing in the range makes the waived residual worse. The hooks are `cfg(test)` only, `record` is inlined behind a `!=`, and `mark` is one relaxed store plus an `Arc` clone ahead of the field loads.

**Verdict, Part B: ACCEPT-WITH-CHANGES.** F2 should land with the merge, F3/F4 are optional, and F1 is a hand-off to P3 S6.

## Part A (BL-202 + D62a), narrow look

I looked at Part A only where BL-206 touches it: the 377315e74 merge, and V's `modify` calls running under the actor whose stop path D62 changed.

- `Fatal(pub String, Class)` with `Fatal::assert` → `AbortCause::Panic` is used at the four D62a sites: CheckBlockIndex, C1/M1, `locks.rs:419` (Core `v:4064-4068`) and `failure.rs:270` (Core `instantsend/net_instantsend.cpp:620-624`).
- `From<ChainError>` and `Fatal::new` stay `AbortNode`.
- `FlushSite::abort(cause, ..)` trips the latch on `Panic` only. That matches Core's `AbortNode` (`shutdown.cpp:27-37`, the shutdown flushes still run, `init.cpp:379-385`, `:449-455`) against `assert`.

BL-206 does not depend on the abort class. A V stop leaves the published view, its journal chain and the readers valid, and they drop with the handle. I agree with Sol's Part A analysis and found nothing new.

**Verdict, Part A: ACCEPT.**

## Mutations (mine, beyond the owner's 14)

| Mutant | Change | Committed dash-chain lib suite | Concurrent probe (3 runs) |
|---|---|---|---|
| M1 | reader epoch off by one: `status_at(id, snap.epoch + 1)` (`reader.rs:175`) | **caught**: `a_status_write_is_read_as_of_the_snapshot` | **caught** 3/3 |
| M2 | hand-off order: push into the new journal, then link the full one (`status_journal.rs:132-143`) | survives | survives: **equivalent** (F4) |
| M3 | lowering not journalled: `g.status.bits() & !old.bits() != 0` (`chain.rs:2147`) | **survives** (F2) | survives (the probe's writes always set a bit) |
| M4 | second live read after the walk (`reader.rs:175`) | **survives** (F3) | **caught** 3/3 |
| M5 | first build boundary: `mark(0)` hands out a fresh, unlinked journal | **caught**: 3 `status_journal` unit tests | survives (the probe starts at epoch ≥ 1) |

M1 and M5 are caught. M2 is a semantic no-op. M3 and M4 expose the test gaps in F2 and F3. Sol's five mutants (raises only, new value recorded, off-by-one capacity, tag after next, no link) overlap M3 and the hand-off. Sol caught them with scratch probes that were never committed, which is why F2 matters.

**Weak-memory probe.** Loom is not usable here: `status_journal` uses `parking_lot` and `std` `OnceLock`/`Arc`, which aren't the loom shims `entry.rs` switches to, and HeaderChain pulls in non-loom code. Miri could not run the full HeaderChain test either. So I checked ordering by the argument above, and ran a stress probe on x86 for interleavings (a `chain.rs` test, not committed; the code is in the appendix):

- One writer. Each round, for each of 4 entries, it writes `junk(E+1)` then `enc(E+1)`, then `publish_status()`.
- Four reader threads loop `snapshot()` then `status_at` and require exactly `enc(snap.epoch)`.

At head, 20,000 rounds each with `rotate_at` 1, 3 and 32 gave 827,684, 664,856 and 944,092 checks with 0 bad.

## Validation

- `cargo test -p dash-chain --lib` at `cfa2fc0a1`: 398 passed, 0 failed, 1 ignored.
- Probe at head: 3 × 20,000 rounds, 0 bad. Each mutant: the full dash-chain lib suite plus 3 probe runs at 50,000 rounds, `rotate_at` 1, with sources byte-compared after restoring.
- `cargo test -p dashd-chainstate --lib` filtered to `chain::tests::view::`, `chain::tests::flush::` and `chain::tests::locks::` at `cfa2fc0a1`: 80 passed, 0 failed. That includes `a_reader_sees_the_status_of_the_view_it_holds` and the D62/D62a tests.
- Not run: GATE-FAST, PF0/perfgate (per the brief), dashd integration tests. I relied on the owner's evidence: GATE-FAST ch1-bl206d EXIT 0, mutants 14/14, and the BL-202 gate EXIT 0 and mutants 7/7.

## Where I differ from Sol r1

1. **The `entry()` trap.** I agree it is not a defect in these ranges, and the live `entry()` is the right primitive for V and W. But the trap for P3 is real, and it is wider than `entry()`. P3's `ChainAccess::valid_scripts(id)` has no snapshot argument, and its doc points at `EntryView::status`, so no correct adapter exists without a trait change (F1). That needs recording before P3 S6.
2. **Lowering.** Sol showed lowering works with scratch probes but did not ask for them to be committed. The committed suite does not catch M3 (F2).
3. **The hand-off reason.** Sol repeats "links a new journal before appending to it" as the ordering guarantee. That is not what carries the guarantee; the journal-before-arena-write order is (F4, M2 equivalent).
4. Otherwise Sol and I agree: the tagging at the build boundary, the walk order, the ordering chain, routing every status write through `modify`, and the retention for readers that drop promptly.

No Dash Core security issue was found.

## Appendix: the concurrent probe (F3)

It goes in `dash-chain/src/chain.rs` `mod tests`. It needs `StatusJournals::rotate_at` to be `pub(crate)`.

```rust
    /// Review probe (Opus r1, not committed): a writer journals and publishes while readers
    /// check that every status read is the one written before their snapshot's build.
    #[test]
    fn review_probe_status_at_under_concurrent_writes() {
        let rounds: u32 = std::env::var("PROBE_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(20_000);
        let rotate: usize = std::env::var("PROBE_ROTATE").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
        let mut chain = node();
        chain.journals.rotate_at = rotate;
        let reader = chain.reader();
        let ids = branch(&mut chain, BlockId(0), 1, 4, 1_700_000_000);
        // The value written before the build of epoch `e` for id index `k`.
        let enc = |e: u64, k: usize| Status((((e as u32) & 0xFFFF) << 8) | (k as u32) | 0x10);
        let junk = |e: u64, k: usize| Status((((e as u32) & 0xFFFF) << 8) | (k as u32) | 0x0100_0000);
        let first = chain.snapshot().epoch + 1;
        for (k, &id) in ids.iter().enumerate() {
            chain.modify(id, |g| g.status = enc(first, k));
        }
        chain.publish_status();
        assert_eq!(chain.snapshot().epoch, first);
        let stop = Arc::new(AtomicBool::new(false));
        let bad = Arc::new(AtomicU32::new(0));
        let checks = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let (reader, stop, bad, checks, ids) =
                    (reader.clone(), stop.clone(), bad.clone(), checks.clone(), ids.clone());
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let snap = reader.snapshot();
                        for (k, &id) in ids.iter().enumerate() {
                            let got = reader.status_at(&snap, id).expect("resident");
                            checks.fetch_add(1, Ordering::Relaxed);
                            if got != enc(snap.epoch, k) {
                                if bad.fetch_add(1, Ordering::Relaxed) < 5 {
                                    eprintln!("epoch {} id {k}: got {:#x} want {:#x}", snap.epoch, got.bits(), enc(snap.epoch, k).bits());
                                }
                            }
                        }
                    }
                })
            })
            .collect();
        for _ in 0..rounds {
            let next = chain.snapshot().epoch + 1;
            for (k, &id) in ids.iter().enumerate() {
                chain.modify(id, |g| g.status = junk(next, k));
                chain.modify(id, |g| g.status = enc(next, k));
            }
            chain.publish_status();
        }
        stop.store(true, Ordering::Relaxed);
        for t in threads {
            t.join().expect("reader");
        }
        eprintln!("probe: {} checks, {} bad", checks.load(Ordering::Relaxed), bad.load(Ordering::Relaxed));
        assert_eq!(bad.load(Ordering::Relaxed), 0);
    }
```

VERDICT: ACCEPT-WITH-CHANGES. Part A ACCEPT. Part B: F2 (a committed lowering test) is the one requested change. F3 and F4 are optional. F1 does not block this merge; it is a required hand-off that blocks P3 S6 wiring `ChainAccess::valid_scripts` until the trait takes the caller's snapshot.
