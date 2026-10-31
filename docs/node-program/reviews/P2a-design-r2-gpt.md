# P2a outbound design rev1 — GPT adversarial review, round 2

Reviewed `designs/P2a-outbound.md`, including “Changes in rev1” and §10, both Opus r1 reviews, DEC-45/48/51, CHARTER, PROCESS, PLAN, PRD, and the relevant A1 interfaces. Design reviewed: 1,680 lines, SHA256 `8219af4a0b720c66be90cb411a6cd1045324483d91af018ef146304b0a9052bd`.

**APPROVE-WITH-CHANGES: seven majors, no blockers.** The actor/transport/validation split is suitable. Rev1 removes the speculative job and fixes most of r0's problems, but §10 overstates completion. In particular, the lazy-hash requirement, AddrMan differential, CH1 reader contract, fast path, and phase gates still need changes. There are also two wire-visible mistakes both prior reviewers missed: a reachable null locator with different profile behavior, and send backpressure based on memory rather than wire bytes.

## Evidence and limits

I independently extracted sources from `/work/home/agent/workspace/dash` at:

- v23.1.8: `728f5055836c6d29806412fc7223ac8fe05af991`.
- v24.0.0-rc.3: `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`.

The reviewed sources include `net.cpp`, `net_processing.cpp`, `addrman.cpp`, `protocol.{h,cpp}`, `primitives/block.{h,cpp}`, `chain.cpp`, `validation.cpp`, `node/sync_manager.cpp`, and the LLMQ handler files. The central extracted blobs were compared byte-for-byte with `git show` at each tag. Both command tables contain 74 unique commands.

Scratch and reproducible checks are in `/work/scratch/p2a-r2-gpt/`. Run:

```sh
TMPDIR=/dev/shm/p2a-gpt-tmp python3 /work/scratch/p2a-r2-gpt/verify_core_contracts.py
```

The script extracts the actual function bodies from each tag and compiles them with `g++ -std=c++20 -O2 -Wall -Wextra -Werror` against small instrumented collaborators. Both tag probes passed. This tests Core control flow and accounting; the hash collaborator counts calls, rather than executing X11, and the AddrMan collaborator supplies identical valid RNG draws. It is not an end-to-end dashd transcript test or a test of an rdashd implementation. No oracle services were changed and no dashd instance was started. `TMPDIR` was created with mode 0700 and removed at completion.

Probe results, independently for both tags:

| Case | Result |
|---|---|
| 2,000 plain headers, mismatch at header 2 | Continuity check performs **one** hash, not 2,000 |
| Nine unconnecting `headers2`, compressed previous hashes after the first | `Uncompress` performs **eight** hashes before the connect check |
| Genesis-only chain, `ConsiderEviction` after 20 minutes | Null-parent locator has **one entry in v23**, **zero in v24** |
| `ConsiderEviction` before its current deadline, peer reaches old benchmark | Deadline changes from 1,300 to 1,400 seconds |
| Equal AddrMan draws, different `vRandom`, `GetAddr(max=1)` | Returned sets differ; the next permuting call does not repair them |
| Queued eight-byte `pong`, current 64-bit libstdc++ layout | Core charge is **88 bytes**, versus **32 V1 wire bytes** |

## Major findings

### G1 — Hash-once must preserve prefix short-circuiting, and `headers2` needs an explicit exception to the zero-hash claim

**Severity:** major. **Where:** `designs/P2a-outbound.md:14`, `:37`, `:861`, `:862`, `:898`, `:902`, `:903`, `:1619`. Reopens part of core M-2 / DEC-51a.

Step 7 says this is where the batch is hashed “once per header” into `HeaderBatch.hashes`; rule 2 says it “fills” that vector and uses it for continuity. Those instructions permit collecting all hashes before comparing adjacency. Core does not do that: `CheckHeadersAreContinuous` checks each header's previous hash **before hashing that header**, and returns at the first mismatch (`v23:net_processing.cpp:3183-3193`; `v24:3167-3177`).

A peer needs no PoW to send 2,000 plain headers whose first previous hash is genesis and whose second previous hash is wrong, followed by `ping`. The batch passes the connect check. Core hashes just the first header, scores +20, and proceeds. Filling the vector first makes rdashd hash 1,999 additional arbitrary headers and delay every peer. The removed side job was not the only way to recreate the original attack.

There is a second contradiction. `headers2` decoding invokes `Uncompress` **before** the connect check (`v23:net_processing.cpp:5259-5267`; `v24:5335-5345`). At both tags `primitives/block.cpp:120-123` computes the previous header's hash when `IsCompressed(PREV_BLOCK_HASH)` holds. Thus an unconnecting batch larger than eight can already have incurred up to `count-1` decoder hashes. The design's correct step 4 conflicts with “nothing hashed” in step 6, “0 on a batch Core drops” in §3.4.9, and the unconditional “never hashed” in rule 1. Small unconnecting batches also intentionally hash their last header. Rejecting either category before those Core hash points would change behavior and timing.

**Fix:** specify an incremental continuity loop: compare the current previous hash against the previous slot; return immediately on mismatch; only then compute or reuse the current header's hash. Produce a complete `HeaderBatch` only after continuity succeeds. State that the connect check adds no hashes for a large unconnecting batch, **but preserves the hashes already required by `Uncompress`**. Correct the rev1 summary and disposition wording. Add hash-call-count/order witnesses for mismatches at positions 2, middle, and last; unconnecting `headers2` with compressed and explicit previous hashes; and a truncated compressed message. Payload/outcome comparisons alone cannot verify this requirement.

### G2 — Null locator input is reachable through ordinary outbound eviction, and differs between profiles

**Severity:** major. **Where:** `designs/P2a-outbound.md:276`, `:425`, `:496`, `:873`, `:1111`, `:1300`. New finding.

The design asserts that no P2a call site can pass a null index and requires a non-null `BlockId`. It considers the “more headers” caller, but misses `ConsiderEviction`.

On a fresh genesis-only node, let an outbound block-serving peer complete the handshake, receive initial `getheaders`, and supply no useful headers. `ConsiderEviction` records the **active tip**, genesis, as `m_work_header`. After 20 minutes it calls the locator with `m_work_header->pprev`, which is null (`v23:net_processing.cpp:5822-5846`; `v24:5968-5993`). This does not require any PoW. It can occur before the headers-download deadline; a sole sync peer also does not satisfy the alternate-preferred-peer condition for a headers-timeout disconnect. The ordinary ping exchange can keep the peer alive.

Core handles this input safely, but differently: v23's `CChain::GetLocator(nullptr)` substitutes the current active tip (`chain.cpp:32-57`, especially 37-38); v24's `LocatorEntries(nullptr)` returns an empty vector (`chain.cpp:32-54`, especially 36). With tip genesis, the resulting requests carry one locator hash versus none. The design's assertion either fails on this normal path or suppresses a profile-specific wire result.

**Fix:** make the locator input nullable, or handle this caller explicitly. Under `V23_1`, null means a coherent current active-tip locator; under `V24_0`, it means an empty locator. Replace the “unreachable” classification in §3.0/PQ-23. Add a differential session with genesis as the work benchmark and advance beyond `CHAIN_SYNC_TIMEOUT`, checking locator bytes, the common-version word, and the later two-minute eviction stage under each profile.

### G3 — Send backpressure uses Core's retained-memory charge, not queued wire bytes

**Severity:** major. **Where:** `designs/P2a-outbound.md:139`, `:152`, `:1088`, `:1392`, `:1538`. New finding.

The design defines `fPauseSend` as `queued bytes > -maxsendbuffer × 1000` and gives the outbox an atomic byte counter. Core's counter is different. `PushMessage` adds `CSerializedNetMsg::GetMemoryUsage()` and tests the sum of that counter **and the transport's send memory** (`v23:net.cpp:4837-4849`; `v24:4878-4890`). `GetMemoryUsage` includes the message object's size and payload allocation capacity (`v23:135-141`; `v24:136-142`). `SocketSendData` transfers the charge to the transport when handing off a message; pause state still includes that retained memory (`v23:1712-1720,1771`; `v24:1702-1709,1765`). V1 can retain a whole payload allocation after part of it has been sent; V2 charges its encoded buffer (`v23:V1Transport::GetSendMemoryUsage` / `v24:877-882`, and `V2Transport::GetSendMemoryUsage`, `v24:1664-1670`).

A peer stops reading and floods `ping` requests, then resumes reading and counts queued `pong`s. On this host Core charges 88 bytes per queued eight-byte pong, while a V1 wire-byte budget charges 32 bytes. Ignoring fixed transport/kernel buffers, a 1 MB budget trips at approximately 11,364 versus 31,251 queued pongs. A payload-only counter diverges even further. This changes when message polling stops, the response backlog, memory growth, and cross-peer work without requiring PoW. Decrementing the budget as bytes are written also resumes polling earlier than Core when the allocation is still retained.

**Fix:** distinguish receive raw-byte accounting from send-memory accounting. Define a Core-compatible logical send-memory charge and the exact queue-to-transport ownership transition; P1 must expose the transport charge, including retained capacity. Native Rust structure size alone is not a compatibility definition. Preserve synchronization between queue publication, charging, handoff, and pause-state updates. Test stalled and partially draining V1/V2 writers against Core near the boundary, including many small pongs and one large partially sent payload.

### G4 — R3 requires a coherent read snapshot, but the API exposes two independent loads

**Severity:** major. **Where:** `designs/P2a-outbound.md:271`, `:272`, `:419`, `:425`. Part of arch M5 remains unresolved.

Publishing `TipFacts` and `ActiveChainView` “together” does not make independent `tip()` and `active_chain()` calls coherent. Even if the publisher uses one atomic pointer internally, the reader can load epoch E's tip, V can publish epoch E+1 during a reorg, and the next getter can load E+1's active chain. Only `TipFacts` exposes an epoch; the contract provides no combined snapshot, validation/retry protocol, or retained read guard.

P2b's fork search and active-chain successor walk execute under one `cs_main` scope in Core (`v23:net_processing.cpp:4731-4809`; `v24:4784-4859`). Announcement selection likewise checks active membership under the enclosing send scope (`v23:6193,6277-6282`; `v24:6338,6422-6428`). Mixing an old tip with a new chain can truncate a reply incorrectly or choose headers versus an inv from facts that never coexisted. Separate scopes between the four header-acceptance phases are allowed; torn reads **within** one such phase are not implied by that permission.

**Fix:** expose one retained `ChainReadSnapshot { tip, active_chain, epoch, ... }` for each Core critical section, or expose a joint epoch/read-retry protocol that makes the same guarantee. Require snapshot-based fork/next/contains operations and IBD evaluation against one coherent set of facts. Keep immutable index/locator walks outside it where appropriate. Add a deterministic reorg between the attempted loads and assert that the caller uses either complete epoch, never a mixture.

### G5 — The fast-path predicate can skip eviction state transitions before a deadline

**Severity:** major. **Where:** `designs/P2a-outbound.md:1371`, `:1674`. Reopens arch m16.

“ConsiderEviction's timeout not reached” is insufficient to skip section 11. Core initializes, clears, and extends the chain-work deadline **before** checking whether it has expired (`v23:net_processing.cpp:5809-5830`; `v24:5955-5976`). These are observable future-behavior changes, not optional housekeeping.

For example, take an unprotected syncing block-relay peer with the headers-sync timeout disarmed, no queued announcement or block in flight, an old work benchmark at work 1, a future chain-sync deadline at second 1,300, and a current active tip at work 2. At second 200 the peer has reached work 1. The stated predicate allows the fast path because the existing deadline has not been reached. Core resets the benchmark and deadline to second 1,400. Repeatedly skipping that state transition makes the later verification `getheaders` and disconnect occur at the wrong times. The extracted `ConsiderEviction` bodies demonstrate that reset at both tags.

Section 6 also needs attention: a previously ineligible, not-yet-syncing peer can become eligible when the best header becomes recent or another sync peer disappears, without a message queued to that peer. Checking only queues and timeout deadlines does not prove that starting sync is a no-op.

**Fix:** initially run sections 6, 10, and 11 normally, or define a predicate that excludes **every** state-changing branch and initial-sync opportunity. The proposed property test must compare all state, deadlines, benchmark pointers, and later sends, not just immediate messages. Include a peer reaching an old benchmark before the deadline, timeout clearing, and a non-syncing peer becoming eligible.

### G6 — Comparing partial `GetAddr` results as multisets cannot repair a different `vRandom`

**Severity:** major. **Where:** `designs/P2a-outbound.md:1350`, `:1482`, `:1484`, `:1551`, `:1623`. Core M-6 is only partly resolved.

Acknowledging library-dependent pruning order is correct, and exporting/alignment of `vRandom` is a valid solution. The alternative “compare `GetAddr` results as multisets until the next permuting call” is not. `GetAddr` does a **partial** shuffle and stops after the requested sample size (`v23:addrman.cpp:811-846`; `v24:815-850`). Different initial permutations subjected to equal draws can select different subsets, rather than different orderings of the same subset. Applying the next set of swaps does not reconverge the permutations.

With four surviving addresses, permutations `[1,2,3,4]` and `[3,4,1,2]`, and the same valid zero-offset draws, `GetAddr(max=1,pct=0)` returns `{1}` and `{3}`. Both extracted Core functions show this, and the next call still differs. Network/terrible filters can also make the number of draws differ, contaminating later RNG-dependent operations. This would reject a correct port or encourage a comparator broad enough to hide sampling bugs. The sentence that exact equality resumes “until the next permuting call” is not a sound differential contract.

**Fix:** choose the alignment option for exact continued operation streams: translate IDs by address key, install Core's `vRandom`, update every entry's random-position field, and align/verify the remaining RNG state. For genuinely unaligned loads, compare canonical bucket/entry invariants and use a separately defined sampling/distribution check, without claiming equal sample multisets or future draw-by-draw equality. Correct the serialization-exchange rule to permit ID renumbering and the acknowledged post-load ordering. Add a pruning-file case followed by capped and filtered `GetAddr`, then `Select`/`Add`.

### G7 — Phase C's merge gates still require behavior implemented in E and F

**Severity:** major. **Where:** `designs/P2a-outbound.md:1474`, `:1530`, `:1532`, `:1544`, `:1545`, `:1553`, `:1657`. Arch M9 is only partly resolved.

The two-task split and movement of functional/live checks to M2 exit are improvements. But phase C still requires the full `diff_p2p_multi` target for 24 CPU-hours. That target explicitly checks sync-peer selection, four-slot header protection, `RelayAddress`, outbound eviction, and +C. Header selection/protection/eviction and +C are phase E; address relay is phase F. C cannot pass that target against Core before those implementations exist. Its unqualified “transcripts equal the dashd fixtures” similarly includes the first `getheaders` of §6.1, which belongs to E.

The live state-parity prescription also needs an M2-compatible choice: §6.1 at `:1422` asks both binaries to load `regtest-40000` or use a fresh `generate`, whereas the specified M2 rdashd has tip genesis and no block-connection/mining implementation. A shared header fixture in a fake chain is useful for in-process tests, but it is not evidence that an M2 live binary has connected that chain.

The actual PLAN still has one P2a row and no O1c/XH1/P2a-core/P2a-net rows at this review. Proposed PLAN edits are therefore pending manager work, not completed dispositions. I have not edited manager-owned files.

**Fix:** define phase-specific target/script subsets. C compares handshake, gates, queues, and their multi-peer cases, ending before the E-owned sends; move the complete header/relay multi-peer differential to the joint E/F gate. Capture Core fixtures in advance without making a live rdashd comparison a C dependency. Use fresh genesis-only datadirs for M2 live handshake parity; retain larger synthetic fixtures for O1c/sim, or name and schedule the loader dependency. Apply the PLAN rows and critical-path rerun before scheduling the split, with explicit cross-half dependencies and final combined acceptance.

## Minor findings and implementation constraints

### g1 — Several post-read citation dispositions remain incomplete

**Severity:** minor. **Where:** `designs/P2a-outbound.md:1165`, `:1181`, `:1182`, `:1622`.

The new platform-ban citation `v23:net_processing.cpp:5616-5619 / v24:5762-5765` points into NOTFOUND, not the pre-read blockchain-sync guard. The guard is at `v23:3814-3824 / v24:3839-3849`. The grouped `dsq` and CoinJoin rows still name files/guards without per-pin line locations; the getdata row omits the main `ProcessGetData` ranges for its non-block outcomes. Behavior is broadly correct, but “every row cites both pins” is too strong under DEC-45. Fix the anchors and mark any remaining owner-derived references accurately. Core M-5 should be recorded as mostly, rather than fully, resolved until then.

### g2 — Mutable status and BLS context are inconsistent in the CH1 API text

**Severity:** minor. **Where:** `designs/P2a-outbound.md:274`, `:288`, `:418`, `:679`.

`entry()`'s comment labels `status` immutable, while R2 correctly says it can change. Define its atomic/seqlocked read semantics; cached status must not hide failed/ChainLock-conflicting entries. Also, `ProtoCtx` is constructed from `tip.bls`, but the declared `TipFacts` has no BLS context or equivalent derivation input. Add it to the coherent read view, or specify the exact source used at each overlay read. P2a's own current messages have no BLS field, but this is an interface for the later handlers.

### g3 — Make signal/join/finalization and the peer-map lock explicit in the liveness contract

**Severity:** minor. **Where:** `designs/P2a-outbound.md:135`, `:136`, `:140`, `:151`, `:156`, `:514`.

The unbounded handle queue and outbox break the original wait cycle. However, `initialize_node` now takes a shared peer-map mutex that is absent from the lock hierarchy. Specify that the actor obtains an owned peer reference and releases that mutex before waiting for V; otherwise synchronous initialization can wait behind the blocked actor. Core obtains a `PeerRef` under its short map lock (`v24:net_processing.cpp:1858` onward) and does not retain that lock through processing.

Also distinguish an interrupt signal from an actor join, state how a blocking reply wait is completed/cancelled, and specify terminal peer cleanup. An actor that exits at `:509` will not drain `finalize_node` commands subsequently produced by stopping ConnMan. Core finalizes nodes after joining message handling (`v23:net.cpp:4155-4195`; `v24:StopThreads`, `StopNodes:4203-4208`). Provide a cleanup owner for that terminal path and test shutdown while V is blocked and while finalizations are queued. The stated wait graph can be made safe; “interrupt before V stops” alone should not be treated as a complete join/cleanup sequence.

### g4 — Scheduler time must not replace the clock for every exponential timer

**Severity:** minor. **Where:** `designs/P2a-outbound.md:449`, `:528`, `:599`, `:964`, `:1059`.

The wording moves “the actor's timers and `exponential_rand` schedules” onto ND1's scheduler time base. The 45-second scheduled stale-tip task belongs there. Address/local-address deadlines instead use `current_time` from mockable node time in `SendMessages`/`MaybeSendAddr` (`v23:net_processing.cpp:6040-6106`; `v24:6186-6252`, especially 6209/6215), and feeler/extra-peer deadlines are checked by opencon against its node-time reads. `mockscheduler` alone does not advance those Core deadlines. Narrow the rule to scheduled callbacks; retain the distinct node-time, adjusted-time, steady-time, and scheduler-time roles. Add separate `setmocktime`-only and `mockscheduler`-only cases rather than relying exclusively on `bump_mocktime`, which advances both and can conceal a mistake.

## Audit of every r1 disposition

“Resolved” here means the revised design states the required fix; it does not claim implementation tests have already passed. Pending manager-owned PLAN/A1/O17/brief edits remain integration prerequisites.

| Core finding | Audit of rev1 |
|---|---|
| M-1 | Resolved: size limit by received type, continuation/shortness by bit 11; PQ-24 and mixed cases |
| M-2 | **Partial: G1**; job removed, but prefix short-circuit and decoder-hash exceptions need specification/tests |
| M-3 | Resolved: synchronous initialization and latency feature; observe g3's map-lock constraint |
| M-4 | Resolved: index-dependent headers/inv probes replace the active-chain probe |
| M-5 | **Mostly resolved: g1**; remaining incorrect/missing anchors |
| M-6 | **Partial: G6**; alignment option valid, multiset alternative invalid |
| m-1 | Resolved: compact-block unknown-prev request only outside IBD |
| m-2 | Resolved: stub scores and all three no-BLOOM disconnects enumerated |
| m-3 | Resolved: v23 commitment prechecks restored |
| m-4 | Resolved: inbound-masternode VERSION citation corrected |
| m-5 | Resolved: common-version word in locators stated |
| m-6 | Resolved: nonexistent functional-test references replaced/dropped |
| m-7 | Resolved: mocktime-start condition or initial mask stated |
| m-8 | Resolved: payload-before-checksum error precedence and read quirks stated |
| m-9 | Resolved: negative attempts preserved with a separate bit-exact path |
| m-10 | Resolved: health-check effects, temporary name, check ratios, error codes, invalid resolver result and retry destination corrected |
| m-11 | Resolved: feeler selection branches explicit |
| m-12 | Resolved: regtest extra-peer churn specified |
| n-1 | Resolved: half-open fixed-seed range |
| n-2 | Resolved: multiply before timeout-scale division |
| n-3 | Resolved: peers dump precedes anchors |
| n-4 | Resolved in new-6; §3.6.4's unconditional “still 0” should use the same qualification |
| n-5 | Resolved: small maxconnections exception |
| n-6 | Resolved: every non-punishing block-result exit logs |
| n-7 | Resolved: spork log difference recorded |
| n-8 | Resolved: wallet-manager presence corrected |

| Architecture finding | Audit of rev1 |
|---|---|
| M1 | Substantially resolved: queue/outbox edges removed; g3 clarifies lock and terminal shutdown mechanics |
| M2 | Resolved: no side job, pool, speculative slots, or kill switch; G1 covers remaining inline-laziness wording |
| M3 | Resolved: XH1 dependency and backend-dependent performance gates |
| M4 | Resolved at design level: headers-only scenario, both Core benches, comparable acceptance and disabled regtest checks |
| M5 | **Partial: G4 and g2**; R1–R7 added, reader coherence/API details still needed |
| M6 | Resolved: raw payload and read-at-Core-point handler interface |
| M7 | Resolved: non-vacuous +C probes |
| M8 | Resolved at design level: multi-peer oracle, seeded per-source randomness, substitutions, process isolation, size M |
| M9 | **Partial: G7**; split and exit evidence improved, C's gates still reach into E/F and PLAN edits are pending |
| M10 | Resolved: owned payload copies remove read-buffer pinning |
| m1 | Resolved: BlockSync lookup, two send positions and block-checked seam |
| m2 | Resolved: stream trait instead of concrete TCP stream |
| m3 | Resolved: speculative field removed |
| m4 | Resolved for scheduled actor callbacks; g4 narrows overbroad exponential-clock wording |
| m5 | Resolved: visible atomics and dirty refresh point |
| m6 | Resolved: OS-thread ConnMan drivers and serialization/write separation |
| m7 | Resolved: sans-IO step functions support virtual-clock tests |
| m8 | Resolved: absent test names removed; asmap cases attributed correctly |
| m9 | Resolved: bit-11 continuation rule |
| m10 | Resolved: latency/per-format ratio included as classifier features |
| m11 | Resolved in design: phase A owns clock; actual ND1 PLAN edit pending |
| m12 | Resolved: BanList leaf/no-I/O contract |
| m13 | Mostly resolved: ConnChainView named; its trait should also document where the verified-ProTx set in its prose lives |
| m14 | Explicit residual risk accepted for manager/O13b tracking; no claim of a deterministic live ladder differential |
| m15 | Resolved: skewed-time multi-peer scenario under both profiles |
| m16 | **Partial: G5**; a predicate/test was added, but the predicate is not sufficient |
| n1 | Performance ambiguity removed from gating; exact backend for the retained 383 ns/byte quotation still unnamed—remove that unused quotation or label it unknown |
| n2 | Moot: kill switch removed with job |
| n3 | Resolved: inline/single-threaded benchmark |
| Errata 1–20 | Amendments accepted in design; application to manager-owned documents pending |
| Errata 21 | Resolved: conditional latency permission and classifier feature recorded |
| Missing errata (a)–(g) | Added to design; G4/G7 refine the relevant interface/plan amendments |

## CH1 and split assessment

R1's concurrent index, R2's immutable ancestry, R4's lazy IBD latch, R5's non-null best-header initialization, R6's validation interleaving, and R7's hash-bearing batch are appropriate requirements. R3 needs the combined reader snapshot in G4. R2 needs the mutable-status clarification in g2. R4 must evaluate coherent tip/work/loading facts before irreversibly setting its latch. R6 should have a queue-service/reorg-latency test, not merely a statement that the queue is checked. R7 remains a good boundary once G1 defines when the hash vector becomes complete. None of these requires speculative hashing or validation calls for immutable locator traversal.

The split should remain P2a-core / P2a-net. Adopt the phase-specific evidence in G7 and record the joint gate after E/F. Keep the explicit interim deviations in §3.11: passing the frozen, masked M2 handshake window does not establish the charter's 24-hour indistinguishability criterion. The shortened header-response latency and per-format cost ratio must remain classifier inputs, as DEC-51j now requires.

**Verdict: APPROVE-WITH-CHANGES.** Resolve G1–G7 before treating rev1 as implementation-ready; preserve the architecture and the valid r1 fixes.
