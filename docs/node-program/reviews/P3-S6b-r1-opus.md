# P3 S6b — independent Opus-h review, round 1

- **Branch:** `node/p3-s6b` at `17e082b44`.
- **Scope:** `4f263a07e..17e082b44`. The S6b commits are `f5ccd2ff3`, `d46c59ff1`, `8f187fcbc` and `17e082b44`.
- **Core reference:** v24.0.0-rc.3. `np` means `src/net_processing.cpp`.
- **Build:** a detached worktree under `/work/scratch/p3-s6b-review/wt`, with its own target seeded by reflink.

## Verdict: ACCEPT WITH NITS

The C5/C5′/C8 wiring is correct. It meets R2-1, R2-2 and R2-3, and it ends correctly on panic and on shutdown.

D120's adapter-level redo gives a Core-showable answer at six of the seven call sites. It does so for every expiry trigger taken alone. One site, `BlockRequestAllowed` in `getheaders`, can give an answer no single Core view gives. That takes a composite interleaving inside one handler's expired window. The effect is minor: one stale-fork header served.

I recommend a DEC that accepts the redo, carrying the corrected argument (m-1) and the table below. Pair it with either a cheap restart at that one pure-read site or a backlog row. Nothing blocks the merge.

## Tests run

- **Full suite.** `cargo test --locked -p dashd-p2p -p dashd-node --features test-support` passed in every binary, at load ~70. That includes `p2p::realv::*` (2/2) and `p2p::chain::*` (5/5).
- **Repeat runs.** `p2p::` ran 6 more times sequentially at load 60–90: 6/6 green.
- **Refusal count.** I instrumented the count at the head. It is `REFUSABLE=189`.

### Extra mutants

The mutants are hand patches, reverted after each run. The tests run were `-p dashd-node --lib p2p::`.

| Mutant | Result |
|---|---|
| A: the notify hook never rings `wake` | KILLED by both `realv` tests, after a 60 s timeout each, with no hang |
| B: status read at the live view (`at_view` reads `self.view.view()` first) | KILLED by `a_held_snapshot_reads_its_own_view` and the expiry test |
| C: C5′ waits for space, not quiescence, so C8 can run before the actor applies the outstanding `BlockChecked` | KILLED by `an_accept_outside_ibd_…` (`realv.rs:334`) |
| D: no queue close on the actor's panic (`CloseOnDrop` removed, `peerman.rs:806`) | KILLED by `v_parks_…` |
| F: the `nChainTx` memo ignores its `view_seq` (`chain.rs:177` `s <= seq` → `is_some()`) | **SURVIVED** the whole suite. See n-3. |

## The D120 expiry question: verdict per call site

### Setting

- **O** is the snapshot the actor drained. **N** is the newest view at the moment of an expired read. Each `at_view` call independently takes the then-newest view, so several reads in one handler can land on N1 ≤ N2 ≤ ….
- Tip facts, `in_active_chain` and `active_at` stay at O.
- Core takes `cs_main` once per handler section. An answer is Core-showable if it equals Core's answer at some single view V in [O, N]. Every such view existed while the actor held O, so Core could have taken `cs_main` there.

### What changes between O and N

- **`-loadblock` / reindex** raise `HAVE_DATA` and validity, and advance the best header and the tip.
- **Deep `invalidateblock`** (class C) sets `FAILED_*` and moves the tip down.
- **`reconsiderblock`** (class M, so V serves it even inside a C5 wait) clears `FAILED_*`.
- **CH2b pruning** *clears* `HAVE_DATA`.
- `nTx` and the validity bits are never lowered.

| # | Site | Core | Status facts read at N | Verdict | Why |
|---|---|---|---|---|---|
| 1 | `blocks.rs:267` `after_cmpct` | np:5108-5150 | `have_data`, `n_tx` | **safe** | `have_data` at N ⇒ return; Core at N returns too. `!have_data ∧ n_tx≠0` at N (pruned) ⇒ the getdata branch whatever the tip; Core at N does the same. `!have_data ∧ n_tx=0` at N ⇒ the same at O (data never came; prune keeps `nTx`), so the mix equals Core at O. `FAILED` is not read. |
| 2 | `blocks.rs:481` `block_tail` | np:5283-5290 | `IsValid(TRANSACTIONS)` | **safe** | Core reads it in a fresh `LOCK(cs_main)` after `ProcessBlock`, with no other view fact, so any later view is Core's. `FAILED` set or cleared just picks Core's answer at N. |
| 3 | `serve.rs:114` `BlockRequestAllowed` (from `getheaders` with a null locator, `serve.rs:242`) | np:2048-2055, :4805 | `valid_scripts` | **unsafe, composite only (minor)** | Safe for every single trigger. `FAILED` set ⇒ false, and e is off N's chain, so this is Core at N. `VALID_SCRIPTS` raised by connecting ⇒ e was active at that view, so Core says true there. Pruning does not touch it. The interleaving that breaks it is below. |
| 4 | `download.rs:683` direct-fetch walk | np:3212-3220 | `have_data` | **safe** | A block on N's chain but not O's was connected, so it has data at N: skipped by both. A block on O's chain but not N's has data unless pruned. A disconnect needs undo data, so it is never pruned. So `to_fetch` and the large-reorg decision equal Core at N. A pruned fork block is requested, as Core at N would. |
| 5 | `download.rs:760` `FindNextBlocksToDownload` | np:1527-1556 | `is_valid(TREE)`, `have_data`, `have_txs_downloaded` | **safe** | Same membership argument as #4. `FAILED` set ⇒ `None`, as Core at N. `last_common_block` advances only on `HaveTxsDownloaded`, which never clears, so Core at N would set it. A later entry read on a newer N2 can stop the walk after an earlier entry was queued: the result is a prefix of Core-at-N1's request list, which is benign. |
| 6 | `download.rs:871` `HeadersDirectFetchBlocks` gate | np:3209 | `IsValid(TREE)` of `last` | **safe** (one benign composite) | `FAILED` set ⇒ no fetch, as Core at N. The divergence needs `last` to be failed at O. That needs an `invalidateblock` landing between `ProcessHeaders`' reply and the post-reply drain, then a tip advance past `last.work`, then `reconsiderblock`, all in the window. The effect is up to 16 getdata for a lighter branch. Not worth a restart. |
| 7 | `download.rs:903` `pprev->IsValid(CHAIN)` | np:3252 | `IsValid(CHAIN)` of `prev` | **safe** | It only picks `MSG_CMPCT_BLOCK` or `MSG_BLOCK` for a lone request. Either is a valid protocol choice, and the read alone equals Core at N. |

No site is unreachable. Expiry can hit any handler whose `msghand` thread is descheduled while V makes 2^16 status writes.

### Site 3 interleaving

1. **Before O:** stale-fork block e (connected once, so its validity is `VALID_SCRIPTS`) is `invalidateblock`ed. At O, e is `FAILED`, off the active chain, and within a month of `best_O`.
2. **Inside one `getheaders` handler's window (O → N, ≥ 65,536 status writes):**
   - (a) `-loadblock` or a headers burst advances `m_best_header` by more than `STALE_RELAY_AGE_LIMIT` in both time and work.
   - (b) *Then* `reconsiderblock(e)` clears `FAILED`. e's fork is lighter, so it stays off the chain.
3. **The mix:**
   - `in_active_chain(O, e)` = false.
   - `valid_scripts(N, e)` = true.
   - The age tests use `best_O` and pass.
   - So the mix answers **true**, and P3 serves e's header.
4. **Core at every V in [O, N]:**
   - Before (b): `FAILED`, so false.
   - After (b): `best_V` is more than a month ahead, so false.

So the mix gives an answer Core never gives. It is exactly the stale-block fingerprinting answer `BlockRequestAllowed` exists to refuse.

Practical reach is very small: an operator `invalidateblock`/`reconsiderblock` plus a month of headers inside one microsecond-scale handler. But it is real. The handler has mutated nothing before `serve.rs:242`. So either:

- **A restart point is cheap:** take a fresh snapshot and redo `getheaders` if the adapter reports an expired read. That needs a small seam change, for example a `ChainAccess::expired(snap) -> bool` probe; or
- the DEC accepts it with a backlog row.

## Findings

### Blocker / Major

None.

### Minor

**m-1. D120's monotonicity argument is wrong for one of its own triggers.**
- **Where:** `docs/node-program/designs/P3-notes.md:127`. It says "Statuses only rise, apart from `FAILED`".
- **The problem:** CH2b pruning, which the same row lists as a trigger, clears `HAVE_DATA` (and `HAVE_UNDO`).
- **The conclusion still holds:** `nTx` and the validity bits survive a prune, and a pruned block cannot be disconnected (no undo data). That is what makes sites 1, 4 and 5 safe.
- **Ask:** the DEC should carry the corrected argument and the per-site table above, not the one-line claim.

**m-2. Site 3 composite divergence.** Described above (`serve.rs:105-121`, np:2048-2055). Decide on a restart or a backlog row in the DEC.

### Nits

**n-1. Stale D120 text about V's error.**
- `P3-notes.md:127` says "`Disconnected` is used only when none was recorded".
- Since `17e082b44`, `listener.rs:61` uses `expect("`OnV` keeps V's error")`. The code is right: `OnV` is the only source of `Interrupted`. The text is stale.

**n-2. Backlog number.** D120 and `BACKLOG.md:266` say BL-267 for the failed-then-pruned `nTx` edge. The manager is renumbering it BL-280. Update both together.

**n-3. The memo's `view_seq` guard has no test.**
- **Where:** `chain.rs:177`.
- **What happens:** mutant F (the guard ignored) survives the whole suite. Today the actor's snapshots are read in time order, so the guard protects an ordering that production does not produce.
- **Ask:** either add a unit test, or say the guard is defensive. A test would let a newer snapshot memoize a side-branch entry, then read the same entry at an older snapshot, where it had no `nTx`.

**n-4. The floor count in `8f187fcbc`'s message.**
- The message says "each side alone kept 190". Only the S6a side did.
- The arithmetic: the base had 193. S6a moved `-connect` (`only("0")`), `-dnsseed` and `-fixedseeds` (`OFF`) to `Implemented`: −3, giving 190. `-uacomment`, `-v2transport` and `-peertimeout` were `Inert::Any` and were never counted. A-1b moved `-alertnotify` from `Unsupported` to `Implemented`: −1, giving 192.
- The merge has 189, which I measured. **189 is correct, and no refusal was lost.** The comment at `support.rs:342-343` is accurate.

## Checked and fine

### C5/C5′ back-pressure

- **Wake:** `wake` is `bounded(1)` and the hook uses `try_send` (R2-2).
- **Ring order:** the hook runs in `WaitEvent::notify`, after the queue's lock is dropped (`release`, `close`). So the change is visible before the ring. A stale ring only costs one re-check of `done`.
- **`done`'s lock:** `done` takes only the queue's `inner` leaf lock. Every holder (`push`, `drain`, `release`, `close`, `held`, `ring_if_queued`) is brief and calls nothing under it (R2-3, LO-5).
- **Re-entry:** the listener holds no lock across the wait, and V's nested hooks are blocked by `in_signal_wait`.
- **The actor never needs a class-C reply V would defer:**
  - `ProcessHeaders` and `ReconsiderBlock` are class M (`command.rs:229`), and the C7 query is class R. V serves all of them in the wait.
  - The actor keeps draining while its own block waits (`blocks.rs:455`).
- **Closing:** the queue closes on panic through `CloseOnDrop` (`peerman.rs:806`), after a non-blocking `on_panic`, and in `Net::stop` before the join. Closed counts as released.
- **The newest view never expires:** the journal keeps the last build's journals through a burst (`status_journal.rs:41`, `:330-341`). So `at_view`'s retry ends and cannot spin.
- **R2-1:** the doc is at `signals.rs:237-240`, and test m-2 proves it.
- **Test lanes:** both test commands go on the bounded lane, which is FIFO. So the C5′ test's query really is served inside Y's wait.

### C8

The facts match np:2228-2268: the height test, `recent`, `PeerHasHeader(pindex)` and `(pprev)`, and `time` from the header. C5′ makes every earlier `BlockChecked` apply first, which reproduces Core's synchronous order. The call is inside the accept, before ABC's `UpdatedBlockTip`.

### `nTx` at a view

`HAVE_DATA` ∨ validity ≥ `TRANSACTIONS` is right, given that both are set together with `nTx` (`ReceivedBlockTransactions`) and that a prune keeps `nTx` and the validity bits. Within one read, the `have_txs_downloaded` walk and the status come from the same view. The memo is written only after a whole read succeeds. Apart from the tracked pruned-`FAILED` edge, I found nothing else wrong.

### Shutdown order (`d46c59ff1`, `node.rs:387-400`)

The order is: RPC stop, then `rpc_chain.detach`, then `net.stop` (unregister and queue close, then connman `Stop`), then the scheduler, then V. That matches Core init.cpp:329-354: `StopRPC`…, `UnregisterValidationInterface(peerman)`, `connman->Stop`, `scheduler->stop`. The detach after the RPC servers stop is correct.
