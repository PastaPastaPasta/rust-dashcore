# P3 block-download design: Opus review, round 2

- **Under review:** `node/p3-design` at **8c1cbeeea**: `docs/node-program/designs/P3-block-download.md` (cited as P3:line) and `P3-notes.md` (D1–D25).
- **Core reference:** Dash Core **v24.0.0-rc.3**. Files were extracted with `git show v24.0.0-rc.3:…` into `/work/scratch/p3-design-r2-opus/core`. Note that `~/workspace/dash` HEAD is rc.2-109, not rc.3.
- **CH1-node code:** read at the branch base (657a28c28, `dashd-chainstate/src/chain/`).
- **Abbreviations:** np, be, v and nph as in the design.
- **Method:** source comparison only; nothing was built.
- **Exclusions:** D12 was excluded as the brief asks. The ring's construction is not reviewed.

## Summary

Core fidelity is high. I re-derived each port in focus item 1 from the rc.3 source and found no ordering or condition defect. That covers:
- `FindNextBlocksToDownload`;
- in-flight bookkeeping;
- the stall CAS and the decay arithmetic;
- the float timeout;
- headers direct fetch;
- `InitData` and `FillBlock`;
- the `cmpctblock` and `blocktxn` handlers;
- HB selection;
- `NewPoWValidBlock`;
- the section-7 branch;
- the `ProcessBlock` tail against `BlockChecked`.

D22's table claim checks out against the actual release binary.

The weak point is again M2. The bound itself is fine: it is lossless, ordered, and C1 holds. But the §3.8.1 deadlock-freedom argument does not hold against CH1-node as it is built:
- During a DEC-196 (2) startup hold, V's release points do not service class M commands. A full queue plus a `ProcessHeaders` the actor sent then deadlocks the node (MJ-1).
- C5 makes V wait on a task that waits on V, which CH1-node's normative LO-9 forbids. The design does not ask for that rule to change (MJ-1).
- CH1-node's release points run every queued class-M command, including other `ProcessBlock` accepts. So §2.5's "hard maximum" is not a maximum, and the idle-point and import waits nest inside V's own release-point wait (MJ-2).

Both are fixable within C5 and A-1b. The rest of the design can proceed.

---

## Findings

### Major

#### MJ-1: §3.8.1's deadlock-freedom argument fails during the DEC-196 (2) hold, and C5 contradicts LO-9 without asking for a ruling

Design references: P3:421-434 (C5), P3:1141-1151 (§3.8.1 table), P3:1637 (A-1b), D20.

**1. The hold.** §3.8.1's `ProcessHeaders` row relies on "V's release-point wait services class R and M commands". During the startup ABC after a DEC-196 (2) tied fallback, that is false:
- `V::dispatch` holds every command except `ActivateBestChain`, `AttachCoins`, `SetLoading` and control (actor.rs:902-911, command.rs:264-272).
- `release_point()` takes nothing from `held` while `holding` is set (actor.rs:1108, 1142-1147).

CH1-node D38 states that Core runs this ABC alongside `connman->Start` (init:2755), so the net actor is live. Schedule:
1. A peer sends `headers`. The actor sends `ProcessHeaders` under the peer lock, with no drain (P2a-core `process_headers`).
2. V holds it.
3. The startup ABC connects blocks, one `BlockChecked` per `ConnectTip` (v:3432). Nothing drains them.
4. At entry 1,025, V waits in `wait_for_space` at abc.rs:730-731 (v:3760). The only thing that can drain is the actor, which waits on a held command.

The node hangs for good. An ABC of more than 1,024 connects is reachable: the tied fallback's reconnect plus stored blocks up to the 1,024 window after a crash during IBD. Core cannot hang here, because its callbacks are inline.

**2. Before the first drain.** Core registers PeerManager at init:2199, starts `loadblk` (reindex, `-loadblock`, the startup ABC) at init:2455, and starts connman at init:2755. If S6 registers the listener at Core's position but starts the actor later, V can block on a queue nobody drains. If ND1's init waits on anything V must finish first, such as startup's ABC reply under D38, that is a cycle. §3.8.1 has no row for this window, and S6 states no rule for it.

**3. LO-9.** CH1-node.md:230 (LO-9, DEC-156 (1), normative) says: "Nothing V waits on ever waits on V." It names cycle B, "a listener awaiting the net actor, which awaits `ProcessBlock`'s `Activated` reply while V waits", as excluded. C5 introduces exactly that edge: V waits on the net actor's drain, and the net actor awaits `Activated`, a reply that depends on class C work. The draining wait may make it safe, but it is an exception to a manager-ruled invariant. LO-9's runtime marker cannot be set on the actor, so the marker no longer backstops this edge. A-1b asks only to "add the case to the model".

**Fix:**
- C5: V does not wait for queue space while `holding` (D38), or before the actor's first drain. Before the first drain the queue is either unbounded, or registration happens at actor start. If the latter, show that dropping pre-start `BlockChecked` and `NewPoWValidBlock` matches Core: with no peers they touch only `m_highest_fast_announce` and `most_recent`, and `NewPoWValidBlock` is not emitted in IBD or reindex.
- Bound the overshoot during the hold by that ABC's connect count. Core's own `BlockConnected` queue holds those blocks too, so the term is no worse than Core's.
- A-1b: ask for an explicit LO-9 amendment (a DEC) that names the net actor as a V-waitable task. It may await V only through the draining `process_block` wait, or for replies that every release point serves, the hold excepted by the rule above.
- Make it a lock-and-wait rule that binds later rows. P5's ATMP waits, and any LLMQ or ChainLock handler that would await a class C reply on the actor, must use a draining wait. Today §3.8.1 is a closed-world list of P3 and P2a-core waits ("these are all its waits").
- S1 and the CH1-node model gain two cases:
  - the queue full during `commands_wait_for_startups_abc_after_a_tied_fallback`, with a `ProcessHeaders` pending;
  - the listener registered before the actor starts.

#### MJ-2: the hard maximum and C5's waits ignore CH1-node's release-point servicing of class-M `ProcessBlock` accepts

Design references: P3:492-502, P3:1291-1294, D20; P3:421-434.

**1. The bound.** `ProcessBlock` is class M (command.rs:221). Two paths run its accept, which is `AcceptBlock` and so can emit `NewPoWValidBlock` with an `Arc<Block>` (v:4832) or a failure `BlockChecked` (v:4891). The ABC tail is deferred.
- `release_point()` runs every command queued in both lanes at every ABC and invalidate iteration, whether or not anything waits (abc.rs:730, failure.rs:148, actor.rs:1114-1125).
- `VWait::wait` dispatches each command it receives with `at_release = true`, then calls `release_point()` (actor.rs:741).

So between two release points V can emit one `NewPoWValidBlock` per queued submitter. The bounded lane holds 1,024 (actor.rs:129) and the marked lane is unbounded. The queue also grows **while V waits** in `wait_for_space`, because that wait services the same commands. "1,024 entries and 8 MiB, plus one 2 MB block, plus two entries, plus one step" is therefore not a maximum. The real excess is roughly concurrent `ProcessBlock` senders × (one block + two entries).

The P2P actor sends one command at a time. RPC threads, `generate*` and import add more. Sibling blocks at tip+1 all satisfy v:4831's guard. This is local, not remote, so it is not a DoS. It does make the D20 figure and §5.1's memory term wrong.

**2. Nesting.** C5's "before each `AcceptBlock` that runs outside an ABC step" applies to an accept that V runs **inside** a `wait_for_space` wait (an idle-point wait serving the actor's `ProcessBlock`). Does that point wait again, recursively? The design does not say.

**3. Idle-point wait.** CH1-node's `VWait` calls `release_point()`, which has `debug_assert!(self.in_composite)` (actor.rs:1101). An idle-point `VWait` is therefore a new CH1-node mode, not "CH1-node's release-point wait". It needs to define whether class C is deferred while no composite runs.

**Fix:**
- State that C5 points reached inside a `wait_for_space` wait do not wait.
- Restate the maximum as: bound + (queued `ProcessBlock` accepts) × (one block + two entries) + one **outer ABC iteration's** `ConnectTip` count. Between two v:3760 calls the inner do-while (v:3768-3800) can run several `ActivateBestChainStep`s after `fInvalidFound`, so this is not one step.
- Carry that maximum into §5.1 and A-9.
- Spell out the idle-point wait mode in A-1b.
- Add an S1 case with N queued RPC `ProcessBlock`s at the wait.

### Minor

#### m-1: for non-actor origins, section 7 can overtake `NewPoWValidBlock`

Design references: P3:995-1028, Q9.

`Shared::updated_block_tip` writes each peer's `headers_relay` directly from the signals thread (peerman.rs:129-149). `NewPoWValidBlock` waits in the `SyncSignal` queue for the next drain (peerman.rs:392, 414).

For an RPC `submitblock`, V can push `NewPoWValidBlock` in `AcceptBlock`, and the signals thread can then fill `headers_relay` after ABC, both between a peer's post-`ProcessMessages` drain and its `send_messages`. Section 7 then misses `most_recent` (np:6481-6486) and encodes from disk with a fresh nonce: an extra D60 draw, and a different `cmpctblock`.

Core always hits the cache. `NewPoWValidBlock` runs inline before `UpdatedBlockTip` is even queued. The P2P path is safe, because the phase-2 drains run before section 7. Q9 covers delay, not this inversion.

**Fix:** drain the `SyncSignal`s at the start of `send_messages` (one leaf lock), or before `announce_section` reads `headers_relay`. Add the case to `diff_p2p_multi_blocks`.

#### m-2: the `NewPoWValidBlock` drain reads a published view that can predate the block's own accept scope

Design reference: P3:1011-1022.

V pushes `NewPoWValidBlock` mid-scope (v:4832), and CH1-node publishes at `scope_end` (CH1-node.md:664). CH1-node.md:173 warns that synchronous listeners must not read the published view, "which still shows the pre-hold state". The actor may drain at once, through the doorbell. If the header was new in this accept (RPC `submitblock`, an unsolicited `block`), the drain's snapshot lacks the entry.

`ProcessBlockAvailability` then cannot resolve a `hashLastUnknownBlock` equal to this block, and `PeerHasHeader(pindex)` is false where Core's is true. The result is an extra `cmpctblock` to a peer that announced the block by `inv`.

**Fix:** stamp the signal with the publication `seq` that will contain the entry, and process it against a snapshot at or after that `seq`. Or carry the facts it needs in the signal and resolve availability only after publication. Add a test with a new-header `submitblock` and an inv-announcing peer.

#### m-3: memory peak is about twice the stated bound

Design reference: P3:474-475.

`drain` returns everything and "wakes a waiting V" before the actor processes the batch. A batch of up to the full bound (8 MiB plus `Arc`s) is then still resident while V refills the queue to the bound.

**Fix:** release space only after each entry is processed, or count the batch in §5.1 and A-9.

#### m-4: the 2% budget is not measurable as written, and is ill-conditioned at the S6 heights

Design references: P3:1274-1281, D24.

- Testnet blocks at heights ≤ 6,500 are almost all coinbase-only, so V's connect cost is tiny. P3's fixed per-block cost could exceed 2% of it while mattering little. The second merkle pass, the very thing A-4b's gate is meant to catch, is near zero there.
- "Actor CPU per block ≤ Core's `ProcessMessage("block")` excluding `ProcessNewBlock`" names no Core-side method.
- "V's per-block connect time" in one rdashd process needs a per-thread method (named threads + `perf stat --per-thread -e instructions:u`, or in-process counters).

**Fix:**
- Gate the merkle pass on the sim replay with realistic transaction counts (mainnet-like blocks).
- At S6, gate on absolute actor `instructions:u` per block.
- Name the Core reference measurement: for example, the oracle session replaying the same blocks under `perf stat`, or callgrind with a function filter.

#### m-5: some READY criteria are not mechanical

- **Smoke floors** (P3:1468, D21). S3 and S4 READY need the new targets green at "their smoke floors", but no floor is defined for a new target. Give the D58/DEC-184 rule, or a number per target.
- **S5's READY condition** (P3:1584) reads "smoke as each entry point lands". Say whether S5 is READY with named PENDING rows. (S3 and S4 cannot be.)
- **§6.7 (a)**, "no misbehaviour score given to an honest peer" (P3:1548-1549), has no definition of honest. Proposal: fail on any score whose reason is one of §3.11's P3 strings unless the oracle judges that block or message invalid. Report other scores without failing.
- **Transcript 5** should pin the release `dashd` it runs against by sha256 (`~/.rust-dashcore-test/dashcore-24.0.0-rc.3/bin/dashd`).
- **The sweep** (P3:294-296) compares the emulation against "the release table's" bucket count, which the same `lower_bound` rule computes, so it is largely self-consistency. Name a real `std::unordered_map` probe as its reference: GCC 15 native, which the table check below allows, or a GCC 13.3 static probe. Transcript 5 and E4 remain the real release checks; this is for clarity only.

#### m-6: closing the queue on every exit path

Design references: P3:505-507, S6.

`close` runs "when the actor exits". Specify a drop guard, so that a panic or an early return closes it too, before any teardown step that can wait on V. `VWait` does end on V's `Shutdown`, so this is not a hang in itself. But between an actor panic and S6's shutdown, V would sit parked at its next release point.

### Nits

1. P3:3 still reads "Status: **rev0**".
2. §6.3 (P3:1411) says "four scenarios" and lists five.
3. D22 and P3:284 say `__prime_list` has "304 entries". The array is 305 `unsigned long`s: 304 primes plus a duplicated sentinel, 0x988 bytes. Extract all 305; `_M_next_bkt` relies on the sentinel (`__last_prime`).
4. P3:173-174: `PeerDownload` is "created on the first request", but the comment calls that "Core's `CNodeState` lifetime", which starts at `InitializeNode`. The behaviour is fine, because every reader handles absence; the comment is wrong.
5. §3.3.2 (P3:774): write the f64 expression in Core's operation order:
   `since_us as f64 + (spacing as f64 * (1.0 + 0.5 * others as f64)) * 1e6`.
   It is identical for Dash's 150 s spacing, but the doc should not rely on that.
6. §6.1 (P3:1339): test the decay for every t in 2..=64, not only the 17-value chain from 64. Doubling from decayed values reaches others, such as 27→54 and 5→10.
7. §3.8.1's "other origins" should list ChainLock enforcement (`ClEnforce`, class C, with InvalidateBlock and ABC release points). It is covered by the same argument, but it is the most likely reorg-storm source on Dash.
8. A-3 names two owners (CH2 / CH1-node); pick one. It should also state the property §3.8.1 relies on: `read_block` takes no lock that W holds while W waits on V's bell service.
9. A-6b should cite Core's `memusage` model (`MallocUsage` rounding for glibc LP64), because the 100,000 threshold (np:5018) depends on it exactly.
10. P3:556-561: `ActorReply::try_recv` (reply.rs) plus the doorbell would avoid hand-building a `Waker`. The poll-based form is also correct: `close` stores the value before it wakes (reply.rs:287-298).

---

## Checks that found no defect

- **In-flight bookkeeping** (np:1271-1358, 1804-1830): `seq` erase is equivalent to iterator erase. The `m_downloading_since` max-advance runs only on the front, `m_stalling_since` resets on any removal, `m_peers_downloading_from` follows Core, finalize leaves the staller and `hb_list` alone, and `in_flight_entries` is the multimap count (Q10).
- **`FindNextBlocksToDownload`** (np:1477-1559): every statement in order. That includes the fixed `nWindowEnd`, the 128-chunk rule, `last_common` advancing only on `HaveTxsDownloaded`, the `!VALID_TREE` return, staller = `waitingfor` only when `vBlocks` is empty and `waitingfor != self`, and `lower_bound(h)->second.first` as element 0.
- **Section 9 and 12** (np:6752-6839): the stall compare and CAS, the double timeout compare (`duration<double, micro>` common type, no truncation), and the getdata condition (Q1).
- **Decay** (np:2200-2207): `(t as f64 * 0.85) as i64` is the same IEEE operation as `duration_cast<seconds>(t * 0.85)`. The 64→…→2 chain is right.
- **`HeadersDirectFetchBlocks`** (np:3202-3257): the walk bound `<= 16`, the large-reorg branch, reverse fetch, the double-space log line, and the `MSG_CMPCT_BLOCK` swap conditions.
- **`cmpctblock`** (np:5052-5293): branch order, `first_in_flight`, the tracked and untracked branches, the `getblocktxn` HB clause (`already_in_flight < 2`), `fProcessBLOCKTXN` going through `ProcessCompactBlockTxns` (tail `ProcessBlock`), the reconstructed tail with `IsValid(TRANSACTIONS)` after `ProcessBlock`, and Q13.
- **`blocktxn`** (np:3734-3816): `Failed` with first-in-flight falls through without `ProcessBlock` (Q7); the null-header path.
- **`InitData`/`FillBlock`** (be:55-223): a duplicate short ID gives `FAILED` only after the loop. That is equivalent, because a duplicate adds no node and a bucket over 12 would have failed earlier. The mempool reset has no hash compare, the extra-ring compare is by txid, `have_txn` is never reset, the two `Invalid` orders hold (Q8), and `IsBlockMutated` is `FAILED`. `MaxBlockSize()/MIN_TRANSACTION_SIZE` (2,000,000/10) is above 65,535, so D22's sweep range is the accepted range.
- **HB selection** (np:1360-1416): the swap guard, `ForNode`'s `FullyConnectedOnly` (net.h:1370-1372), the stale-front pop (Q4).
- **`NewPoWValidBlock`** (np:2228-2268): the nonce drawn before the height check (Q12); `ForEachNode` is `FullyConnectedOnly` (net.h:1438-1441) plus `fDisconnect`.
- **`BlockChecked`** (np:2314-2343) **against the tail** (np:3717-3732): D1's after-delivery order reproduces Core's `mapBlockSource` and `mapBlocksInFlight` contents at signal time, and V's `ibd` is Core's in-callback `IsInitialBlockDownload()`.
- **C3/C4:** `*fNewBlock = true` at v:4835 comes before `SaveBlockToDisk` at v:4837, and `NewPoWValidBlock` is under v:4831's guard.
- **D24 / m6:** `IsBlockMutated` → `CheckMerkleRoot` sets `m_checked_merkle_root` only on success (v:4326, 4362), including Dash's asset-unlock instance-merkle check. `FillBlock`'s call marks the reconstructed block, so `merkle_checked` on all three submit paths is right.
- **D23 / m5:** `CanDirectFetch` tests only tip time (np:1428-1431), and IBD can stay true on `nMinimumChainWork` (v:1984). With direct fetch the `cmpctblock` arrives already in flight, so it takes the tracked branch, then `getblocktxn`.
- **D22 / m4, checked against the binary:**
  - The rc.3 Linux `dashd` `.comment` is `GCC: (GNU) 13.3.0`, and `ldd` lists no libstdc++.
  - The full 305-entry `__prime_list` from this box's `libstdc++.so.6.0.35` (`_ZNSt8__detail12__prime_listE`, 0x988 bytes) occurs exactly once in the stripped release binary.
  - `__fast_bkt` {2,2,2,3,5,5,7,7,11,11,11,11,13,13} occurs once.
  - The `_M_next_bkt` and `_M_need_rehash` reasoning holds: n ≤ 13 uses the fast table, n = 0 gives 1, the prime search starts at index 6, `_M_next_resize = bkt ≥ n`, so n inserts never rehash.
  - Transcript 5 is feasible: siblings at tip+1 from fresh peers, arbitrary 48-bit short IDs, `CanDirectFetch` by mocktime.
- **§3.8.1, apart from MJ-1 and MJ-2:**
  - A full bounded lane is fine: `VWait` selects the lanes, and class C goes to the unbounded `deferred` (actor.rs:917, 1121), or to `held` with backpressure off during the hold.
  - The section-7 disk read is fine, given nit 8.
  - `with_peer` cannot form a cycle: the only thread that holds a `PeerState` lock while awaiting V is the actor, on R/M `ProcessHeaders`, and `getpeerinfo` reads the chain after releasing the lock (peerman.rs `node_state_stats`).
  - Shutdown is fine: `VWait` returns on `stop`.
  - The doorbell has no lost wakeup.
- **Back-pressure on IBD throughput:** in IBD every connect comes from the actor's own `ProcessBlock`, whose wait drains each ring, and `NewPoWValidBlock` is not emitted. The bound never binds, and the cost is one wakeup per connected block, which is negligible next to a connect. The one-outstanding-`Activated` model is Core's.

## Sol r1 items

| Item | Status at rev2 | Notes |
|---|---|---|
| M1: the extra ring with an empty mempool | **Closed** | D19 restates the equivalence as "empty mempool **and** empty ring" and registers an interim ALLOWED-DIFFS entry with owners P5/MP1a, due before REL-5.6. The parity claim waits, and orphan-then-compact is a P5 acceptance row. Core has exactly the two sites np:4998 and np:5019. Asks A-6b and A-7 are well-formed (but see nit 9). |
| M2: the unbounded `SyncSignal` queue | **Partly closed** | Lossless, FIFO, and push never blocks, so order and C1 survive the cap, and C2–C4 are unaffected. But the liveness proof fails under the DEC-196 hold and before the first drain, and conflicts with LO-9 (MJ-1). The stated maximum is wrong under CH1-node's release-point servicing, and the idle and import waits are underspecified (MJ-2). m-3 also applies. |
| M3: the optional Core compact differential | **Closed** | E4 and the compact steps of `diff_p2p_multi_blocks` are required for S3/S4 READY, with named PENDING rows. The release-toolchain rule is in D21. Floors need defining (m-5). |
| m4: bucket sweep range | **Closed** | 0..=65,535 plus transitions, black-box against the release `dashd`. The table equality claim was verified against the binary. Nits 3 and m-5 (sweep reference). |
| m5: IBD direct-fetch latency claim | **Closed** | The claim is qualified, and the S2-e/S3 case was added and checked against np:1428-1431 and v:1984. |
| m6: `merkle_checked` flag | **Closed in the API** | The field and A-4b are present and the semantics match v:4326/4362. The S6 measurement is ill-conditioned (m-4). |

## Testability, PENDING rows and asks

S1, S2 and S4 have mechanical criteria. S3 and S4 READY hinge on E1–E5 from another owner; R10 records that. m-5 lists the gaps.

All asks have an owner except A-3, which names two (nit 8). A-1b needs:
- the LO-9 DEC request;
- the hold rule;
- the idle-wait mode;
- the nesting rule (MJ-1, MJ-2).

ND1 needs a new ask: the listener-registration and actor-start order against V's startup and import work (MJ-1 (2)).

## Disposition

Fix MJ-1 and MJ-2 in rev3, before S1 implements the queue. They change C5, A-1b and §3.8.1 only, plus one ND1 ask. S2's scheduler work does not depend on them. The minors can be folded into the slices that own them: m-1/m-2 into S4, m-3 into S1, m-4 into S1/S6, m-5 into S3/S5/S6, m-6 into S6.

VERDICT: ACCEPT-WITH-CHANGES
