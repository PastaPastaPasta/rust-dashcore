# P3 block-download design: Opus review, round 8 (Opus-h, independent, whole design)

- **Under review:** `node/p3-design` at **41f83cef3** (rev8.1): `docs/node-program/designs/P3-block-download.md` (cited as P3:line) and `P3-notes.md` (D1–D43). Read-only; nothing on the branch was changed.
- **Core:** Dash Core **v24.0.0-rc.3**, extracted with `git show v24.0.0-rc.3:…` into scratch (deleted after the review). np, be, v, net, neth as in the design.
- **CH1-node and P2a-core code:** read at the branch base 657a28c28 (`dashd-chainstate/src/chain/{actor,abc,command,signals}.rs`, `dashd-p2p/src/processing/{seams,sync,logic,peerman}.rs`), plus `designs/CH1-node.md`, DEC-229/230/233.
- **Method:** source comparison and schedule construction. The design isn't implemented, so no P3 test exists to run. D12 was not reviewed.

## Summary

The Core ports hold up again. I re-derived these from rc.3 and found no condition or order defect:
- `FindNextBlocksToDownload`;
- in-flight bookkeeping;
- the stall CAS and decay;
- the double-typed timeout;
- `HeadersDirectFetchBlocks`;
- the `block`, `cmpctblock` and `blocktxn` handlers;
- `InitData`/`FillBlock`;
- HB selection;
- `NewPoWValidBlock`;
- `BlockChecked` against the tail;
- section 7's cmpctblock branch.

The D41 wait-for proof under D42/D43 is sound as written, and the asks A-1…A-12 are satisfiable against CH1-node as accepted.

I found three new problems, none of them caught by five Sol rounds, because those rounds stayed inside the queue's own proof:

1. **M1:** a P2P block's HB `cmpctblock` relay waits until after our `ConnectBlock`, not before it as in Core. This is a BIP152 timing deviation, and the design claims the opposite.
2. **M2:** invariant I0 doesn't hold for handlers that await V partway through a message. The `ProcessHeaders` reply snapshot is newer than the drained one, and P3's `headers_direct_fetch` reads it. That produces outcomes Core can't reach.
3. **M3 (simplification):** `HeadersRelay` doesn't need the queue. Core's own structure, a per-peer `m_block_inv_mutex` cell that section 7 reads while it takes its chain view, gives the same Core-reachable outcomes. It also removes D38–D43, the relay slot, the S → A wait edge and DEC-233's exception.

M1 and M2 have local fixes. M3 is a ruling for the manager to make before S1 implements the queue.

---

## Findings

### Major

#### M1: the HB `cmpctblock` for a P2P block goes out after our `ConnectBlock`, not before it; no wakeup follows the accept scope's publication

**Design:** P3:524-528 (producers ring at push), P3:641-644 and P3:657-660 (`wait_for_space` rings only before it parks), P3:764-784 (the block wait blocks on `doorbell.recv()` with no timeout), P3:1386-1391 (Liveness), P3:1396-1397 ("For a P2P block the per-peer half runs in the actor's block wait, which is Core's position").

**Core:** `AcceptBlock` emits `NewPoWValidBlock` at v:4832, before `SaveBlockToDisk` and long before `ActivateBestChain` (v:4899). `PeerManagerImpl::NewPoWValidBlock` pushes the `cmpctblock` inline (np:2249-2267). That is BIP152 high-bandwidth relay before validation.

**Schedule.** A peer sends a 2 MB block B that extends the tip. HB peers H1–H3 know B's parent.
1. The actor submits B and enters the block wait.
2. V's accept scope pushes N(B) with stamp s+1, mid-scope at v:4832, and rings.
3. The actor wakes and drains with X at view_seq s. N(B) is ineligible, so the actor goes back to `recv()`. This is the normal case, because V still has to write about 2 MB (v:4837) before `scope_end` publishes s+1.
4. Publishing s+1 rings nothing:
   - no queued callback follows a class M accept;
   - `wait_for_space` at ABC's first `limit_queue` (abc.rs:731) returns at once without a ring;
   - the block wait has no 100 ms timeout.
5. The next ring is `BlockChecked(B)`'s push at v:3432, after `ConnectBlock(B)` has run every script check. Only then does the drain apply N(B) and send `cmpctblock(B)` to H1–H3.

Frame order is unchanged, so S4-x can't see this. But every HB announcement of a P2P block is now late by our own full validation time. That has two effects:
- **Fingerprint:** a peer can measure the gap between delivering a script-heavy block and the HB `cmpctblock`. Core relays in milliseconds; the port relays after validation.
- **Propagation:** a chain of rdashd hops validates before relaying at every hop, which defeats the reason HB mode exists.

RPC-origin blocks wait for the 100 ms idle wake instead, which is inside Q9 as stated. The P2P case isn't covered by Q9.

**Fix:**
- `wait_for_space` rings the doorbell whenever the queue is non-empty, not only before it parks. That is one `try_send`. CH1-node calls the C5 hook at ABC's loop head (abc.rs:730-731, v:3760), after the accept scope's `scope_end`, and C7 (b) puts a publication there.
- Equivalently: the listener gets a leaf `published()` ring after any publication that follows a push.
- State in §3.6 that N becomes eligible at the accept scope's publication and is applied before the first `ConnectTip`.
- Add an S4 test: `SimChain` pauses between accept and connect, and the HB peer must hold `cmpctblock(B)` before the pause is released.
- Correct the "Core's position" sentence.

#### M2: I0 is false across in-message V round trips; P3's `headers_direct_fetch` (and, unspecified, `cmpctblock` steps 7–13) read a snapshot newer than the drained one

**Design:** P3:1287-1291 (I0: "no message acts on a publication whose entries are still queued"; the only exception named is `getdata`), D39 (4) ("(3)'s argument needs (4)"), P3:1144-1145 ("Core holds `cs_main` across steps 7–13 but not across step 5 … the gap cannot be observed"), P3:927 (`headers_direct_fetch` from the headers path).

**Code:** P2a-core's `process_headers_message` passes the reply's `r.snap` to `update_peer_state_for_received_headers` and to `SyncCx` for `headers_direct_fetch`. The comment there reads "as Core reads the live tip: CH1's snapshot from its reply" (`sync.rs` at 657a28c28, after `process_headers`). CH1-node serves `ProcessHeaders` at any release point (CH1-node §8.3), so `r.snap` can reflect other-origin scopes whose N or `BlockChecked` entries are still queued. No drain can run there, because the actor holds the peer's lock and a drain visits peer states.

**Schedule (no unsolicited blocks needed):**
- Setup: out of IBD, tip A. Q is an outbound peer with `m_requested_hb_cmpctblocks`, and Q's best known block is A.
- RPC `submitblock B` (B extends A) races with Q's `headers(B)`.
- The actor's message-start drain runs before B's accept publication, so N(B) stays queued. Q's live lookup misses B, so `received_new_header = true`.
- V runs B's accept (N(B) pushed) and its ABC, then serves `ProcessHeaders`. `r.snap` shows tip B.
- `update_peer_state_for_received_headers` with `r.snap`:
  - `B.work > tip.work` is false, so `m_last_block_announcement` isn't set (np:3284);
  - `best_known = B`.
- At the next drain, N(B)'s per-peer half finds `PeerHasHeader(B)` true and sends no `cmpctblock(B)` to Q.
- Core reaches only two outcomes:
  - Q's headers processed first: tip A, so the announcement time is set and NewPoWValidBlock then skips Q;
  - B accepted first: NewPoWValidBlock sends `cmpctblock(B)` to Q, then Q's header is not new.
- The port produces "no `cmpctblock`, and no announcement time", which Core can't. `m_last_block_announcement` feeds `EvictExtraOutboundPeers` (np:6084-6086).

**The same hole in P3's own code:** `headers_direct_fetch` on `r.snap` showing tip B can put a block in flight before a queued `BlockChecked(B)` is applied. `BlockChecked`'s `count(hash) == size()` test (np:2334-2336) then fails, so HB selection of B's source is skipped. Core, having seen tip B, ran `BlockChecked(B)` first, with nothing in flight.

§3.5.3 doesn't say which snapshot `cmpctblock` steps 7–13 read after step 5's `ProcessHeaders`. If they read the reply's, the `bip152_highbandwidth_to` test at step 12 can disagree with Core in the same way.

**Fix (pick one, state it as an I0 clause, and test it):**
- **(a)** After a V round trip inside a message, every chain-state predicate uses the message-start snapshot X: tip, active chain, IBD, `CanDirectFetch`, work comparisons. The reply contributes only its own facts (`last_accepted`, `invalid`), and status stays last-published (D14). This is Core's schedule in which the other origin's work came after the handler's `cs_main` sections. It changes P2a-core's two `&r.snap` uses (an ask on P2a-core).
- **(b)** Split the handler at the round trip as D2 does: drop the lock, drain to `r.snap`, relock. Core releases `cs_main` at exactly these points: between `ProcessNewBlockHeaders` and the second section in `cmpctblock`, and before `UpdatePeerStateForReceivedHeaders`.

Then add S1 cases:
- headers racing an RPC block, with an HB peer;
- direct fetch racing an other-origin `BlockChecked`.

Correct P3:1144-1145.

#### M3 (simplification): route `UpdatedBlockTip` through Core's own structure, not the queue; this removes D38–D43, the relay slot, the S → A edge and DEC-233

**Design:** D34's `HeadersRelay` half, D38–D43, §2.5's `RelayList`/`RelaySlot`/`retire` (P3:484-509, P3:597-645, P3:680-702), §3.8.2 (P3:1597-1670), A-1b (6), and about eight S1 tests (P3:1916-1951).

**Core:** `UpdatedBlockTip` appends under each peer's `m_block_inv_mutex` without `cs_main` (np:2302). Section 7 reads chain state while it holds that same mutex (np:6409).

**The rule.** Keep P2a-core's existing per-peer `block_inv` leaf cell. Today the signals thread appends to it there (`peerman.rs` `updated_block_tip`), with recipients read at the callback as Core's `ForEachNode` reads them. Store the list as a `RelayList`, which D38 already proved exact under append. At the head of `send_messages`:
1. lock the peer's cell;
2. take S (one `ChainView` load, see m-1);
3. move the list out, then unlock;
4. drain V's entries to S;
5. lock the peer state, and run section 7 on S with the moved list.

V's own entries (N, `BlockChecked`) keep D37's stamped cutoff unchanged.

**Why it is exact.** If the moved list contains U(T), U's callback ran before the cell lock, and so after T's publication (C6). S, taken under the lock, then shows T. N(T) is stamped at or below T's accept publication, so the drain applies it before section 7. That is D37's (I2) argument with U's stamp replaced by the lock.

Appends after the unlock go to the next round. That is Core's scheduler blocked on `m_block_inv_mutex` behind section 7 (np:6409). The moved list holds exactly the callbacks that completed before S, so the reachable outcomes are a subset of rev8.1's eligible-`HeadersRelay` outcomes, which Sol r4–r7 accepted case by case. The named schedules carry over:
- **Sol r2's stored descendant:** N(B) is applied because S shows D.
- **Sol r3's S → N → H:** N(C) stays queued.
- **Sol r4's U(A)-behind-`BlockChecked(B)`:** no queue, so nothing is held back, and Q gets `headers(A)` at S.
- **D43's `VERACK`-after-`relay_slot`:** recipients are read at the append.
- **10,000 marks:** each peer's cell is a bounded `RelayList`.

**What goes away:**
- the coalescing rule (D38, D39 (1)) and "take from behind a later stamp" (D39 (2)), because V's stamps are monotone, so the eligible set is a prefix;
- recipient lists, `retire` and boxed slices (D40, D42);
- the relay slot, the token, its `PreStart` crossing and its barrier test (D41, D43);
- the signals thread's wait on the net actor. Today that wait blocks every other listener (wallet, ZMQ, later P5) head-of-line whenever 64 relay entries are pending.
- DEC-233's exception and A-1b (6). LO-9's marker on the signals thread could revert to "never waits".

The §2.5 maximum loses its relay term.

This is the ch2-simplify-to-core-semantics pattern: rounds r4–r7 each found one more instance of the cost of moving Core's per-peer mutex into a cross-thread queue. If the manager adopts it, a narrow Sol check of §3.6 against S4-x's five cases is enough.

### Minor

#### m-1: the drain's `view_seq` has no source, and the obvious one breaks (I2)

**Design:** P3:610-615 and P3:1280-1286 ("takes a chain snapshot X, then applies every entry stamped ≤ X's `view_seq`"). §2.4 adds no way to read it.

**Code and CH1-node:** P2a-core's `ChainSnapshot` carries `epoch`, SB4's store counter (`seams.rs:53-61`), not `view_seq`. CH1-node §6.3 maps `ChainAccess::snapshot()` to SB4's `TipCell`, which "may be one store ahead of `ChainView`". `release(hold)` stores the `TipCell` first, then the `ChainView` (CH1-node.md:176; `abc.rs` `after_release`). `view_seq` lives only in `ChainView` (`actor.rs:1270-1287`).

An implementer who reads X from `snapshot()` and the cutoff from `ChainView` gets a window in which X shows the new tip while that hold's `BlockChecked` (stamp s+1) is ineligible. A message handled on that X, for example source P's `getheaders`, answered from a tip that includes P's stored block B, then precedes the `sendcmpct(1)` that `BlockChecked(B)` would have sent inline in Core.

**Fix:**
- X and its cutoff come from **one** `ChainView` load. The drain uses `view.snap` with `view.view_seq`. Add `ChainAccess::drain_snapshot() -> (Arc<ChainSnapshot>, u64)` to §2.4 and A-1.
- Or define every stamp in SB4's `epoch` and restate C6/C7 in that counter.
- `SimChain` must model the two-cell lag, or S4-x can't catch a regression.

#### m-2: slicing: S1's acceptance needs S4's mechanisms, and S6 can't be READY before E1/E6, contrary to R10

**Design:** P3:2200-2205, P3:1796-1799, P3:2226.

- **S1 depends on S4.** S1's tests include:
  - the stamped cutoff;
  - `HeadersRelay` coalescing;
  - the relay slot's barrier test, checked against section 7 (P3:1940-1951);
  - "every peer's section 7 matches a model" (P3:1923-1924).

  But S4's content lists "`HeadersRelay` through the queue, `RelayList` per peer, every drain cut at its own snapshot, S taken at the head of `send_messages`". So S1 can't pass on its own. M3 would remove most of this; otherwise move those tests to S4, or move the mechanisms into S1.
- **S6 waits on E1/E6.** §5.1 says S6 is not READY while the Core leg of the (A − B) gate is PENDING on E1/E6. R10 says S1, S2 and S6 "land first". Either let S6 be READY with the comparison as a named PENDING row (as D21 does for S2), or record that the PLAN's headline acceptance (testnet IBD) waits on two oracle entry points.
- **No real-V ordering test.** S4-x runs only against `SimChain`, and C6/C7 enter only as CH1-node text (the S4 entry condition). Add a subset of S4-x through real V (dashd-chainstate test support) at S6. Include M1's accept-then-connect case and m-1's two-cell window.

### Nits

1. **C7 (a)'s reason (P3:468-470)**, "V publishes only at `scope_end` and `after_release`", isn't CH1-node's rule. CH1-node §6.2 also stores a `ChainView` after SB4's `accept`, mid-`ProcessHeaders`, and after `ClSigs` changes. The property P3 needs is narrower: no publication between a P3 push and the end of its scope. Scopes that push (accept, ABC holds, import accepts) satisfy it, because `accept_header` doesn't publish. State that in A-1.
2. **`Drained<'_>` (P3:615).** Say that it holds no `MutexGuard` while entries are processed. Per-entry release relocks the leaf. Otherwise V's never-blocking push (LO-5) would wait behind section 7 and per-peer work.
3. **`SignalQueue::push(&self, stamp, s: SyncSignal)` (P3:604)** accepts `SyncSignal::HeadersRelay` by type, which bypasses the relay reservation. This is Sol r7 F1's class of problem. Give V's push a V-only enum. This is moot under M3.
4. **P3:1074-1076 and §3.6.** `NodeFullyConnected` already includes `!fDisconnect` (net.cpp:4852-4855), so `NewPoWValidBlock`'s explicit `fDisconnect` return (np:2252) adds nothing. Say so, so that nobody adds a second filter.

---

## Checks that found no defect

- **Download, stall, timeout and decay** (np:1271-1358, 1477-1559, 6752-6781, 6820-6839, 2200-2207):
  - statement order;
  - the `waitingfor != peer` staller rule and the possible −1;
  - `lower_bound(h)->second.first`;
  - the redundant `RemoveBlockRequest(hash, nodeid)` in `BlockRequested`;
  - the microsecond comparison against `seconds × double`;
  - the CAS doubling capped at 64;
  - truncating decay;
  - Q1's getdata condition.
- **`HeadersDirectFetchBlocks`** (np:3202-3257): the `<= 16` walk, large-reorg logging, reverse fetch, `"from  peer"`, and the swap conditions, including the `mapBlocksInFlight.size() == 1` count.
- **Handlers** (np:5052-5293, 3717-3816, 5348-5387):
  - `cmpctblock`'s two `cs_main` sections and Q13;
  - `first_in_flight`;
  - the tracked and optimistic branches;
  - the reconstructed tail's `IsValid(TRANSACTIONS)`;
  - `blocktxn`'s Failed fall-through (Q7) and the null-header score;
  - `block`'s mutated path;
  - `emplace` (Q5);
  - the tail's `m_last_block_time`.
- **`InitData`/`FillBlock`** (be:55-223): the bucket test at each insertion, reassignment of a duplicate key, the mempool reset with no hash compare, the extra-ring compare by `GetHash()`, the two `Invalid` orders (Q8), and the post-fill `IsBlockMutated` returning FAILED.
- **HB selection and `BlockChecked`** (np:1360-1416, 2314-2343): the design's pseudo-code matches line by line. Two-argument `ForNode` is `FullyConnectedOnly` (net.h:1369-1372).
- **`NewPoWValidBlock` and `UpdatedBlockTip`** (np:2228-2307): the nonce before the height test (Q12). `m_best_height` and the service-flag cache come before the IBD return. `UpdatedBlockTip` filters on `CanRelay`; `NewPoWValidBlock` doesn't, and the design's per-peer half doesn't either.
- **Section 7** (np:6401-6496): `CanRelay` gate, `m_block_inv_mutex` held, cached versus disk encode, and `pindexBestHeaderSent` set once (D17).
- **D41/D42/D43 as written:**
  - every S → A wait ends through A's draining wait, or through V's `ServiceWait` serving A's R/M reply;
  - `close` wakes S;
  - a `PreStart` token's push is decided under the leaf lock;
  - the cap counts queued entries, held entries and reservations.

  I couldn't build a deadlock, unbounded growth or a lost announcement here. M3 is about need, not soundness.
- **Shutdown (A-12):** ABC breaks after a step (v:3841). The actor's tail runs and its drop guard closes the queue. S's later relay pushes discard, and V's pushes after close discard. Core gets the same outcome by stopping connman first.
- **Lock order:**
  - the per-peer `heights` mirror is a leaf under `PeerState`;
  - `recent` and the queue are leaves;
  - the doorbell is `bounded(1)` and never disconnected;
  - `getpeerinfo` reads the chain after releasing the state lock.
- **Asks:**
  - A-2: `BlockResult::rejected` hard-codes `new_block: false` (`command.rs`), as C3 says.
  - LO-5 and LO-8/9 are compatible with C5 under DEC-229/230/233.
  - A-10's `via_compact` is hard-coded `false` in `sync.rs`.
  - The other asks name one owner and an implementable property.
- **FT ports:** `p2p_ibd_stalling` uses `-dip3params=2000:2000`, `disable_mocktime`, and steps +1, +3, +3, +3, +2, +7, +2 s, plus `bytesrecv_per_msg`. `p2p_block_sync` is the 3-node one-block test.

## Disposition

- **M1:** a one-line ring rule plus a test, in S1/S4.
- **M2:** an I0 clause, two `r.snap` uses in P2a-core, and tests, in S1, before S2's direct fetch.
- **M3:** for the manager to rule before S1 builds the queue. If adopted, D38–D43, DEC-233 and §3.8.2 are withdrawn, and a narrow Sol check of §3.6 follows.
- **Minors:** fold into the slices that own them.

VERDICT: ACCEPT-WITH-CHANGES
