# A1 / PLAN independent review — round 1

**Reviewer:** GPT design reviewer. **Date:** 2026-10-08. **Verdict:** **REWORK**.

This is a design review, not a claim that the proposed node already contains these bugs. Findings distinguish contradictions verified in Core from missing implementation contracts. I read A1, PLAN, CHARTER, PRD, DECISIONS and R1–R5, including the relevant R4 appendices, and checked the disputed behavior in source. No live invalid-block submissions or vulnerability experiments were performed.

Evidence baseline: Dash Core `develop` at `15db47875002cc09d8a21e1e35fed4a74aaf4e4f`, with targeted checks against `v23.1.8` and `v24.0.0-rc.3`; base-sdk at `33fcc59e1868c803f9608e0e78ee8bddf3abcee7`; rust-dashcore's actual `node/integration` worktree at `/work/home/agent/workspace/rdc-wt/integration` (observed at `5a96adf8`). The checkout at `/work/home/agent/workspace/rust-dashcore` is `dev`, so it was not substituted for the requested branch. `dash/src/...` below means the Dash Core repository, not rust-dashcore's `dash/` crate. Core line numbers refer to the develop baseline unless a tag is specified.

The broad decomposition is credible: base-sdk wire shapes, node-owned canonical wrappers, a single chainstate writer, explicit contexts, and separate runtime services are sensible. A1 correctly recognizes unusually important Core behavior: state-dependent validity, the broader assumevalid scope, configured-but-disabled quorum types, legacy BLS leniencies, and rotation snapshots surviving disconnect. The reference twins, private oracle branches and mixed-network tests should be retained. The policy for **EMBARGO-1** follows DEC-14; its private regression evidence must remain subject to PLAN §7 throughout implementation and triage.

The architecture cannot yet serve as an implementation contract. Recovery contains invalid-state acceptance and unrecoverable-crash paths; some claimed Core equivalences are false; and the test/task graph does not yet substantiate the charter's differential-testing bar.

## Findings

### F01 — blocker — `HAVE_DATA` is not authorization to skip validation on restart

**Location:** A1 §2.10, lines 794–799, especially startup step 3 at line 797.

**Problem:** Startup is told to replay `HAVE_DATA` blocks above the DB tip with signatures off. `HAVE_DATA` establishes that block data was accepted/stored, not that `ConnectBlock` successfully validated it. Core's `AcceptBlock` performs `CheckBlock` and contextual block checks before storing data (`dash/src/validation.cpp:4817–4825`); it does not perform the input-script and complete special-transaction validation done during connection. Its index consistency checks explicitly distinguish transaction validity from script validity (`:5671–5706`).

A received block can pass acceptance and fail its scripts during connection. A crash between those stages must not turn that block into an accepted block at startup. Nor is there a persisted active chain above `meta.tip` merely because a higher-work data-bearing candidate exists. Core's signature-free `RollforwardBlock` is used for a specific, previously validated interrupted flush identified by `head_blocks` (`:5218–5298`), not for arbitrary downloaded candidates.

**Recommended change:** Remove the blanket rule. Use ordinary activation/connection for downloaded blocks after restoring the durable tip. If fast recovery of validated-but-unflushed blocks is required, define a durable validation/replay manifest bound to the branch, reference/configuration, file data and validation mode; replay only that certified transition. Preserve the precise assumevalid scope rather than inventing a second skip mechanism. Add a differential restart case with an accepted-but-script-invalid candidate persisted before connection.

### F02 — blocker — the split-flush recovery marker can name an unpersisted block

**Location:** A1 §3.5, lines 1320–1325; §2.10, lines 794–799; PLAN CH2/CH7a/CH7b.

**Problem:** The first sub-batch publishes `[new_tip, old_tip]`, but all block-index/file-index records are deferred to the final sub-batch. If the new path was created since the previous flush, a crash after the marker or a coin sub-batch leaves no persisted index entry, ancestry, or file position for `new_tip`. The advertised `ReplayBlocks` then cannot find/read the transition. Core requires both marker heads to exist in its loaded index (`dash/src/validation.cpp:5239–5248`) and deliberately writes block/undo files and the block index before flushing coins (`:3051–3065`). Copying only Core's two-hash marker does not copy its prerequisites.

The description also reduces recovery to coins while evo remains at `old_tip`. A reorg recovery must specify how MN state, mined commitments and their height indexes, credit-pool state, MNHF, BLS context and snapshot effects move to the recovered tip. Core's replay includes EvoDB transaction/best-block handling (`:5253–5260`, `:5294–5298`). A coin-only recovery followed by clearing the marker is not a coherent node state. In addition, Core removes its best-block marker during partial writes (`dash/src/txdb.cpp:134–139`); A1 leaves `meta.tip` looking usable unless every reader knows that `head_blocks` overrides it.

**Recommended change:** Specify the entire recovery state machine before CH2: persist durable block/undo data and all replay prerequisites before publishing the transition marker; distinguish the stable old-state metadata from the partially updated coin state; define idempotent evo/LLMQ rollback and rollforward; atomically finalize every semantic table and remove the marker. Either persist sufficient deltas or reconstruct them using explicitly defined recovered parent views. Test every sub-batch boundary, interrupted recovery itself, first flush, fork change, V19/V24 boundaries and DIP24 cycle bases. Do not claim that EvoDB mismatch is impossible until this protocol is demonstrated.

### F03 — blocker — file/WAL durability ordering is contradictory and the harness does not test power loss

**Location:** A1 §2.11 line 813 versus §3.5 lines 1319–1332; PLAN M3 exit 2, line 135.

**Problem:** Section 2.11 promises a durability barrier before each chainstate flush. Section 3.5 instead commits unsynced database batches, then fsyncs block/undo files, then syncs the WAL. Under the latter ordering, the kernel/device can persist a WAL record before the referenced file bytes. Power loss before the file fsync can leave a recovered DB tip/index pointing to missing data. Waiting to release the frozen layer does not undo a WAL record already persisted. Atomicity across RocksDB column families is not atomicity across RocksDB and flat files.

Core's ordering is explicitly files first, index next, coins last (`dash/src/validation.cpp:3051–3076`); file finalization also commits the directory (`dash/src/flatfile.cpp:82–101`). A SIGKILL preserves the host's page cache and pending device writes, so the specified random process-kill harness cannot prove the advertised power-loss safety.

**Recommended change:** Choose one protocol and state its persistence assumptions. Complete the required file writes, file fsyncs and creation/rename directory barriers before a DB write that can publish their references; then durably commit the marker/state in the required order. Define fsync/WAL/write errors, disk-full behavior, and pruning ordering. Keep process-kill tests, but add VM or faulting-block-device/persistence-model tests that can lose/reorder unsynced writes, including recovery interrupted a second time. The design should distinguish atomic visibility, process-crash recovery and power-loss durability.

### F04 — blocker — success-only overlays lose Core's nontransactional rotation effects

**Location:** A1 §2.5 lines 525–554; §2.10 line 762; §3.7 line 1349; §4.7 line 1657; Appendix A lines 2025–2030; PLAN LQ1b/CH7b.

**Problem:** Rotation snapshots are returned inside output/delta structures and the block overlay is dropped on failure or proposal validation. Keeping successful snapshots outside undo handles disconnect but does not handle effects produced before a subsequent rejection.

Core calls `PreComputeQuorumMembers` before extracting/checking the block's commitments, without excluding `fJustCheck` (`dash/src/llmq/blockprocessor.cpp:223–227`; `v23.1.8:src/llmq/blockprocessor.cpp:176–180`). Non-predicting rotation writes its snapshot (`dash/src/llmq/utils.cpp:458–462`, `:512–513`) directly to the raw DB, outside the EvoDB transaction (`dash/src/llmq/snapshot.cpp:320–328`; `v23.1.8:src/llmq/snapshot.cpp:283–291`). `TestBlockValidity` rolls back its EvoDB transaction but still calls this connection path (`dash/src/validation.cpp:4947–4966`). Thus a proposal or a later-rejected connect can leave rotation state behind. A1's unqualified claim that proposal mode is side-effect free is false.

There are other effects to audit: connection can remove conflicting IS locks for a ChainLocked block before later validation returns (`dash/src/validation.cpp:2805–2815`). The pure result interface and `ReplayChainHelper` must not assume that all effects happen only on successful connections.

**Recommended change:** Separate transactional block deltas from Core-equivalent effects that must survive failure, proposal validation, disconnect or query execution. Ensure the effect channel is available even when a function returns `Err`; implement the exact persistence and ordering of each effect, rather than making all state nontransactional. Run rejection/proposal experiments against disposable oracle datadirs. Add differential sequences of failed connect/proposal → query/member selection → restart → reconnect/reorg, comparing snapshots and lock state as well as verdicts. This must precede the purity/overlay API freeze.

### F05 — major — prefetch can resurrect a spent coin after its tombstone disappears

**Location:** A1 §3.2 lines 1237–1249 and §3.4 lines 1274–1302; PLAN CH2/CH4.

**Problem:** The read order is active → frozen → prefetch → DB, but no cache-generation, invalidation or late-read fencing contract is given. A concrete permitted sequence is: S2 fetches coin X; V spends X; W writes X's deletion; the frozen tombstone is released; a later read returns the older prefetch value before consulting the DB. A MultiGet started before the spend can also finish after an eager invalidation and reinsert the stale result. Reorgs introduce the inverse stale-absence case. A bounded LRU limits memory, not stale reads.

**Recommended change:** Specify the DB snapshot/epoch associated with each prefetch result and when it is valid relative to active/frozen layers. Fence outstanding reads, invalidate all affected keys, and retain tombstones until older read results cannot be consumed. Negative results need the same treatment. Add generated schedules covering spend/create, flush completion, prefetch completion, eviction and reorg; compare against a direct-read/reference view at every lookup, not just at the end of two replay windows.

### F06 — major — `ChainView` is not an immutable cross-domain validation view

**Location:** A1 §2.10 lines 713–718 and 786–791; §3.1 lines 1228–1232; Appendix A `EvoStore`/`ChainHelper`; §1.1 rule R4.

**Problem:** An `ArcSwap<ChainView>` contains an `Arc<BlockIndex>` whose status, counts and file positions continue changing through separate atomics. Atomics prevent individual data races; they do not ensure that `HAVE_DATA`, positions, transaction counts and conflict flags form one coherent observation. An older ChainView can also be combined with newly updated mined commitments, coin state, locks or governance state. The proposed single writer does not by itself make those reads consistent. `EvoStore::mined_commitments_until(type, height, ...)` does not expose which branch/view its results belong to, an especially dangerous ambiguity during reorgs or historical serving. Core's analogous lookup resolves quorum bases through the supplied block's ancestry (`dash/src/llmq/blockprocessor.cpp:687–723`).

Explicit BLS context is the right direction, but runtime work does not always have a “block being processed.” Queued recovered signatures, DKG sessions and message decoding need a defined context source and lifetime across V19/reorgs. The policy must distinguish encoding, hashing and verification: MNAUTH signatures always use basic even though their default serialization/sign-hash components consult other context (`dash/src/evo/mnauth.cpp:40–45`, `:93–98`); DKG chooses its signing scheme from its base (`dash/src/active/dkgsession.cpp:34`); lazy wrappers retain their construction/deserialization scheme (`dash/src/bls/bls.h:409–412`, `:467–480`). One undifferentiated tip boolean is insufficient.

**Recommended change:** Define which APIs accept a pinned chain/coins/evo epoch, which may read live metadata, and how related block-index fields are published coherently. A seqlock/versioned record or release/acquire publication protocol can avoid copying the arena. Tie historical/quorum reads to branch ancestry and the appropriate store view. Map Core's actual state/lock observation order for `ChainHelper`; do not assume that either independently live reads or a blanket frozen snapshot automatically matches it. Add an operation-by-operation BLS policy table and fence/revalidate background results on context changes. Exercise delayed jobs and reads across connect, failed connect, disconnect and activation boundaries.

### F07 — major — optimistic ATMP only rechecks a subset of mutable dependencies

**Location:** A1 §2.12 lines 822–835, especially line 826; §3.7 line 1345; PLAN MP1a.

**Problem:** Commit revalidates only the tip ID and conflicts. At an unchanged tip, other accepts/removals can change ancestor/descendant limits, available mempool parents, rolling fee floors, ProTx uniqueness maps and pending asset-unlock totals. Locks can change independently of the tip. Two nonconflicting children can each pass a descendant-limit check against the same snapshot and jointly exceed the limit when committed. Per-input conflict ordering does not serialize these shared constraints.

Core holds `cs_main` and `mempool.cs` across prechecks, script checks and finalization (`dash/src/validation.cpp:1418–1445`; declared lock contracts at `:751–784`). The Rust optimization must replace that consistency guarantee, not merely release the locks. A single retry on tip change also leaves unspecified the result/order after another change.

**Recommended change:** Record every validation dependency or use chain/mempool/lock epochs with a complete commit-time recheck of inexpensive mutable checks. Reuse expensive signature results only if their bound transaction, flags and input coins are unchanged. Define retry/serialization behavior and reject precedence. Extend ordering beyond input conflicts to shared package, special-tx and lock dependencies. Differentially test concurrent admissions, parent eviction/removal, fee-floor changes, ProTx collisions and AU refresh/amount races at a fixed tip.

### F08 — major — template memoization omits ordinary transactions, order and freshness policy

**Location:** A1 §2.13 line 853; §3.7 lines 1346–1351; PLAN MN1.

**Problem:** Memoizing `BuildNewListFromBlock` by `(prev, special-tx set)` is unsound. Core's second pass removes an MN when any ordinary transaction spends its collateral (`dash/src/evo/specialtxman.cpp:732–748`). Two blocks with the same special transactions and different ordinary collateral spends have different DMLs and coinbase roots. A set also loses the order of ProTx transitions and QcTx PoSe effects that A1 elsewhere correctly treats as significant.

The template cache key omits a defined treatment of ChainLock-derived coinbase fields, IS/first-seen eligibility, governance/sporks, mining options and fee prioritization. Not every change should necessarily force a rebuild: Core itself retains a template under some such changes. But A1's background 100 ms rebuild policy is different from Core's tip-or-aged-mempool-update policy (`dash/src/rpc/mining.cpp:843–871`; `v23.1.8:src/rpc/mining.cpp:795–817`). “Same mempool” alone therefore does not guarantee matching output. Mining also reads first-seen age (`dash/src/chainlock/handler.cpp:257–268`) and fills ChainLock/governance-dependent fields (`dash/src/node/miner.cpp:299–325`).

**Recommended change:** Key DML reuse by the ordered effective transition inputs, including ordinary collateral spends and the exact parent coins/evo context. Separate immutable template data from per-call refusal/time fields. Specify which epochs/time conditions invalidate a cache and which deliberately follow Core's cached behavior. Compare cache-hit, cold-build and oracle output after each same-tip dependency change, reordered special transactions and alternate ordinary collateral-spend selection. Cut background rebuilds until their semantics and measurable advantage are established.

### F09 — major — hardlinked mutable block/undo files violate source-read-only import

**Location:** A1 §2.9 line 695; §2.10 lines 804–807; §2.21 line 1166; PLAN CH6/O7.

**Problem:** Import promises a read-only source while hardlinking `blk` and `rev`. Hardlinks share file contents. Normal append/preallocation/finalization of an imported tail, and undo generation during safe reindex, can modify the original Core datadir. Core's file finalization can truncate (`dash/src/flatfile.cpp:82–94`; `dash/src/node/blockstorage.cpp:577–604`). Restarting the source node can likewise mutate the destination's linked files. No stopped-source lock or filesystem checkpoint is required; agreement on a tip is not an atomic multi-directory snapshot.

**Recommended change:** Import from a stopped/locked source or an explicit filesystem-consistent checkpoint. Hardlink only sealed files that the importer and future node will never mutate; copy/reflink writable tails and undo files, or roll over into new destination-only files with proven position semantics. Define import into a staging datadir, restart/resume behavior and atomic finalization. Verify source content remains unchanged through import, destination mining/reorg, restart and source restart. Apply the same rule to oracle datadir construction.

### F10 — major — fast import's checks miss consensus-relevant state corruption

**Location:** A1 §2.10 line 807 and §2.21; PLAN CH6 acceptance at line 349.

**Problem:** Matching a tip, MuHash, SML root and quorum root does not validate a full state transcoder. SML omits payment heights and many other DML fields; an incorrect `nLastPaidHeight` or `nConsecutivePayments` can pass these checks and choose a different next payee (`dash/src/evo/deterministicmns.cpp:158–193`). Rotation snapshots, MNHF state, credit-pool index ranges and mined-height mappings are not certified by these tip roots. When no source MuHash is supplied, merely recomputing and logging the destination hash does not verify UTXO translation either.

**Recommended change:** Require an independently obtained source checkpoint/report, and compare complete canonical DML state, UTXOs, commitments and indexes, rotation snapshots, MNHF/deployment state and credit-pool state. Verify history required for subsequent member selection/disconnect, not just the tip. Keep safe import as the usable first delivery; mark fast import provisional until corrupt/missing-record fixtures are rejected and bounded connect/disconnect/restart differential tests pass. Do not silently weaken validation when the source evidence is unavailable.

### F11 — major — several 2× arguments count optimizations Core already has

**Location:** A1 §3.3 line 1262; §3.6 lines 1338–1341; §3.7 line 1351; metric rows 2/5/8/17 at lines 1383–1398; §3.11.

**Problem:** The concrete reference source contradicts four important baseline claims:

| Claimed advantage | Existing Core behavior |
|---|---|
| Mempool script-result hits are the main new connect lever | Full-script cache lookup/insertion already exists: `dash/src/validation.cpp:2166–2176`, `:2228–2231`. |
| An incremental ancestor-score index avoids Core's repeated sorting | Core maintains that index in `dash/src/txmempool.h:257–263`, `:367–371`, and walks it in `dash/src/node/miner.cpp:522`. |
| Core rebuilds GBT on every call | Both pins cache GBT: develop `dash/src/rpc/mining.cpp:843–865`; v23.1.8 `src/rpc/mining.cpp:795–817`. |
| Immediate ChainLock enforcement removes Core's 5-second delay | Both pins schedule enforcement immediately on a known new ChainLock: develop `dash/src/chainlock/handler.cpp:168–176`; v23.1.8 `src/chainlock/handler.cpp:138–146`. |

Core also already uses persistent `immer::map` MN lists (`dash/src/evo/deterministicmns.h:142–144`), member caches, and parallel QC signature checks (`v23.1.8:src/llmq/blockprocessor.cpp:208–225`). Those remain necessary features, but cannot be counted as newly removed Core costs.

Parallel S1/S2, fewer UTXO misses and a faster payee queue could still produce substantial gains. They do not establish the tabulated numbers. Even the stated hypothetical 35% removable connect work gives only about 1.54× with infinitely fast removal if the remaining 65% is unchanged. Pool handoffs, packed-coin decode, event backpressure, randomized BLS work and extra background GBT builds also consume CPU. The table's credible shortfalls for equal-thread ECDSA, network-bound IBD, disk and protocol-bound rounds should remain; its connect/GBT/CL explanations need correction.

**Recommended change:** Rewrite the baseline around measured incremental savings against both network pins. Move PF1 experiments before committing to the costly cache/pool optimizations. Measure full versus assumevalid replay, tip connect, actual serial fractions, cold/warm/request-after-mempool-change GBT, and total CPU at equal threads. Establish an Amdahl/resource budget and keep targets explicitly hypothetical until demonstrated. B1 currently hardcodes 23.1.8 (A1 line 1420); add the v24 reference for testnet/regtest/devnet workloads as required by PRD NFR-PERF-02(d).

### F12 — major — asynchronous flush exceeds the declared cache budget and the store gate is incomplete

**Location:** A1 §3.1 lines 1214–1226; §3.4 lines 1291–1302; §2.8 lines 610–623; §3.8; PLAN ST1/CH2/PF5.

**Problem:** Freezing an active layer at 90% of the coins allocation and allowing a new active layer to reach the same threshold holds about 180% of that allocation before waiting. The budget table accounts for only one coins cache, and not simultaneous batch serialization. CF memtables, prefetch results/in-flight blocks, native allocator overhead and persistent-state copies need aggregate accounting too. The claimed 32-byte `PackedCoin` cost does not follow from the shown enum: tag/alignment and the heap variant need a measured layout/load-factor budget.

The multiple independent net/prevalidate/script/BLS pools can simultaneously exceed available cores, with no global work budget or small-machine fallback. Equal `-par`/`-parbls` does not account for the additional compute workers.

The RocksDB table's “cannot regress by construction” claim is also false. LSM shape does not guarantee stall, amplification, CPU, RSS or disk parity. ST1 tests only coins; actual chainstate has additional CFs and shared WAL/compaction interference. A1 accepts savings in total disk, while PRD NFR-RES-02 also requires each component to be no larger. A 25% MDBX-switch threshold is not a substitute for the charter's 1% no-regression gate.

**Recommended change:** Use one measured budget across active/frozen/prefetch/batches/memtables/block cache and bound allocation before freezing. Include transient peaks and enum/node layout measurements. Give compute pools a coordinated core/priority budget and test minimum hardware as well as agentbox. Extend the bake-off with a representative multi-CF flush/reorg workload, background compaction, concurrent reads and per-component peak/final disk. Reject any configuration that fails a tracked reference metric, irrespective of which alternate engine wins by 25%.

### F13 — major — the proposed message loop has an observable non-Core cadence

**Location:** A1 §2.14 line 877; §3.1 line 1217; §4.10; PLAN P2/P4/O13.

**Problem:** Core does not put ordinary peers' `SendMessages` on a fixed 100 ms tick. It shuffles peers on each pass, calls `ProcessMessages` then `SendMessages` for each peer, and only applies the 100 ms send throttle to masternode connections. Its idle wait is interruptible by message work (`dash/src/net.cpp:3676–3720`; v23.1.8 `src/net.cpp:3682–3724`). The proposed ordinary-peer tick therefore quantizes responses/download requests/non-tx announcements and can increase latency. A round-robin receive pass followed by a separately shuffled tick is also a different cross-peer ordering.

Offloading a handler and continuing the actor can allow subsequent messages or another peer's request decisions to overtake it, even with a single actor. The design's own argument against independent per-peer machines applies to these completions too. Matching long-run Poisson trickles does not hide these request/response and contention correlations.

**Recommended change:** Implement the reference's wake-driven processing/send semantics, including shuffled passes and the MN-specific throttle. Define ordering barriers for asynchronous validation/ATMP/overlay completion and exactly when another message may proceed. Add multi-peer, backpressured, delayed-worker and flood-plus-probe transcripts and latency correlations to L6. Retain performance differences only where they preserve the reference's externally visible scheduling behavior; a UA table is insufficient.

### F14 — major — decode-domain parity is command- and state-dependent, not just a type codec

**Location:** A1 §1.4 line 143 and divergence table; §2.14 line 871; §4.4 `oracle_p2p_decode`; PLAN BS4/BS7–BS10.

**Problem:** Reader tasks eagerly decode every payload before the actor applies handshake/command gates. Core ignores a duplicate VERSION before parsing its payload (`dash/src/net_processing.cpp:3931–3934`) and ignores non-VERSION messages before VERSION (`:4174–4177`). A malformed payload in one of those states should not acquire a decode error, penalty or disconnect merely because the Rust codec ran early. A1's L6 row 6 promises ignored messages, but the proposed pipeline does not establish that behavior.

VERSION also has optional suffixes rather than the fully mandatory struct assumed by base-sdk's `codec_p2p!(Version { ... })` (`base-sdk/pkgs/p2p_core/src/msg/version.rs:185–223`). Core reads nonce/UA/height/relay/challenge/MN flag only when bytes remain (`dash/src/net_processing.cpp:3973–3998`; v23.1.8 `src/net_processing.cpp:3948–3977`). The seven BS4 items do not include that verified divergence. General P2P trailing-byte tolerance, consumed lengths, version gates and error handling must be defined per handler; `Unserialize(T)` alone is not the observable decode domain.

**Recommended change:** Keep framed raw payloads until the reference's state/command gate and negotiated context are known, then decode with command-specific policies. Add a handler table covering minimum/optional fields, trailing-byte consumption, limits, ignored states and exact error/penalty outcomes. Make BS4 include optional VERSION parsing, and fuzz message sequences and negotiated-context changes, not only independent payload objects. Treat “137 bytes” as a particular default emitted fixture, not a universal VERSION acceptance/encoding rule; UA/comments/devnet alter its size.

### F15 — major — the promised differential proof has uncovered functions and state transitions

**Location:** A1 §4.4 lines 1499–1561, §4.6/§4.7; PLAN O1/O2/O3/O10, EV1, CS5; PRD NFR-CEQ-04/06/08.

**Problem:** The declared oracle ABI cannot support the declared function-level coverage. It exposes payload decode/trivial validity and DML application, but no contextual ProTx/NetInfo checks, full commitment verification, `CheckTxInputs`, sigop/finality/sequence-lock functions, contextual header/block/transaction checks, or complete payee/value validation. EV1 nevertheless requires “special-tx checks vs oracle.” `oracle_dml_apply` is not `CheckSpecialTxInner` (`dash/src/evo/specialtxman.cpp:217–274`); matching its transition cannot prove all those checks. Several rows promise validation classes while their ABI returns only a reason string/status without a specified Core result-class encoding.

The model-based stateful targets explicitly have no Core oracle (line 1555). They can find local invariant failures but cannot prove equivalence of accept → connect → reject → proposal → disconnect → restart, including nontransactional effects and node-local decisions. Happy-chain replay, about 40 mutations and one case per `bad-*` string leave stateful sequences, multiple-failure precedence, configuration boundaries and same-tip races largely unexplored. Recording only successful-connected-block decisions likewise misses decisions/effects on failed attempts. Later functional/mixed tests are necessary but do not fulfill the charter's differential-fuzzing obligation for these paths.

**Recommended change:** Before implementation gates depend on the shim, create a function/transition → pinned Core entry point → vector source → generator/fuzzer → replay/scenario coverage matrix. Add missing contextual entry points with explicit synthetic coins/list/ancestry/deployment/state contexts and full result classes. Add automated stateful differential sequence fuzzing using disposable oracle instances/checkpoints and generated connect/disconnect/restart/lock/governance operations. Compare verdict, class, penalty, semantic state and surviving effects after every step, including rejected steps. Keep model fuzzing as an additional layer. CPU-hour budgets must be backed by meaningful input/state coverage, not stand in for it.

### F16 — major — the canonical full-state oracle assumes a helper absent from the mainnet pin

**Location:** A1 §2.9 line 686; §4.6 lines 1609–1618; Appendix B line 2098; PLAN EV2a/O7/O8.

**Problem:** `EvoSnapshot` and `CanonicalMNListHash` exist in develop/v24 rc.3 (`dash/src/evo/snapshot.cpp:150–155`) but not in `v23.1.8`: that tag has neither `src/evo/snapshot.*` nor `CanonicalMNListHash`. The roughly 150-line test patch therefore cannot simply call the advertised common helper for both pins. This is also the proposed storage snapshot codec and the evidence for non-SML state/import correctness, not an optional convenience.

StateDigest additionally mixes semantic state with cache contents and snapshot-write observations. Versionbits cache population can depend on query order, and persistent rotation effects depend on the executed operation sequence. Comparing arbitrary live/replay cache populations as if they were semantic consensus state will either create false differences or invite normalization that hides real differences.

**Recommended change:** Specify and independently review a versioned canonical full-DML/derived-state encoding for each pin; implement the necessary test-only v23 adapter without changing its validation semantics. Complete and smoke-test both pinned oracles before EV2a freezes its codec. Define which digest fields are semantic, which are history/side-effect observations, and which require an identical query schedule. Compare deployment answers rather than arbitrary cache occupancy. Include rejected/disconnected operation records with enough state to investigate F04/F15.

### F17 — major — milestone exits and merge acceptance form an inconsistent dependency graph

**Location:** PLAN §1 M2/M4/M5; §3 critical-path diagram; task rows CH3a/CH7b/LK1a/EV1/EV2a/RP3/IM1.

**Problem:** Specific graph contradictions prevent the advertised milestones from being accepted:

- M2 requires inbound and outbound handshakes (line 124), but P2 is outbound and inbound P4 is M5 (lines 376–378).
- M4 requires synced/unsynced superblock scenarios (line 151), but the real governance/trigger and sync backends GV1a/GV1b/SP1 are M5 (lines 448–450). A replay fake proves recorded decisions, not the live scenario.
- M5 requires mining and every non-wallet MUST RPC (lines 168–169), but RP3 bundles GBT/mempool RPCs with M7 SG1a/LK1c dependencies (line 396).
- M5's Core-file/functional compatibility relies on detection/import support left in IM1 at M6 (line 441).
- CH3a must pass replay/mutation acceptance before merge (line 345), while O9 depends on CH3a and O10 on O9 (lines 418–419). CH7b must pass CL-conflict scenarios, while LK1a, the CL backend, depends on CH7b (lines 351/457).
- EV1's contextual checks require list/store and cryptographic interfaces; EV2a's list/store model depends on EV1 and EV1 does not declare the BLS parity prerequisites. Interfaces or explicit staged acceptance are missing.

The critical-path diagram adds serial edges not present in task dependencies (for example CH1 → CH2 and EV2b → LQ1a → LQ1b are drawn as one chain) and omits gate dependencies such as governance. The listed dates and resource allocation cannot be assessed from two inconsistent DAGs.

**Recommended change:** Maintain one explicit graph with separate interface-ready, implementation-ready and acceptance/merge-ready dependencies. Break test-harness cycles with independently deliverable API skeletons and an integrated validation slice; do not waive gates silently. Split early inbound handshake from full serving, bring the needed governance/sync backend into M4, split RP3 by milestone and move required Core-format loaders earlier. Derive the actual resource-constrained critical path, including oracle/review/fuzz queues and calendar soaks, from that graph. Clarify that BS14–BS17 are rust-dashcore work rather than requiring all of them to merge into `node/base-sdk` (M1 exit line 108).

### F18 — major — Wave 1, task granularity and review classes under-resource the proof work

**Location:** PLAN §0 lines 49–77; §4 BS9/BS11/BS13/ST2/ND1/RP*/WL3; §8 lines 569–596; A1 §4.13.

**Problem:** With limited slots, Wave 1 starts multiple consensus implementations before O1 at priority 7 and O4 at 10, although their acceptance depends on those oracles/vectors. O3 is deferred to Wave 2 despite M0 differential gates. BS17, explicitly needed for current testnet SPV/desktop safety, is reserve work behind broad node tasks. The current ordering encourages implementation queues waiting for their proof infrastructure rather than early vertical verified slices.

The assertion that every large area is already PR-sized is not supported by the rows. BS11 is an M task spanning wrapper leniencies, both schemes, secure aggregation, threshold operations, IES and batch verification. BS9 is an M task covering DKG, sig shares, governance and CoinJoin codecs. ST2 is S despite append/preallocation/finalization, undo, scanner and corruption parity. RP1/RP3 each contain numerous independent read/write/control paths and milestones. Large slices also make 24/72-hour per-target gates and two reviewers bottlenecks that are absent from the staffing estimates.

Review classification can route high-risk changes through one OTH reviewer: BS13 changes consensus decode/hash helpers; ND1 controls validation time/init/shutdown; RP1–RP3 include block submission, chain invalidation and EvoDB repair; WL3 handles private-key-bearing import and untrusted BDB structures. Wallet signing/encryption and oracle trust code need explicit specialist/security scrutiny even if not consensus itself. A blanket task label is not enough.

**Recommended change:** Bootstrap minimal O1/O3/O4 and both pinned reference schemas alongside BS3; let independent primitive/data-model portions proceed while oracle-backed acceptance is prepared. Split deliverables by an independently reviewable API/behavior with its tests, then recalculate reviewer/fuzz resource demand. Schedule BS17 as an early consumer-safety lane. Assign the maximum applicable risk to each changed behavior/file, including hashing/decoding, state mutation, clocks, key handling and the trusted oracle—not just crate prefixes. Require two independent reviews for consensus/state-affecting optimizations regardless of their parent task's OTH label; add independent wallet/security review before dangerous import/signing code is treated as complete.

### F19 — minor — floating-point compatibility needs an admissible-domain gate before a libm port

**Location:** A1 §4.5 lines 1569–1577; PLAN CS2/PF1.

**Problem:** The exhaustive historical subsidy sweep is essential and correctly avoids the “too-high subsidy still replays” trap. It does not prove generated pre-V20 regtest/devnet chains or alternate valid histories. The rule allowing a random-input mismatch to be decided because it did not occur historically leaves the admissible consensus domain undefined. A port of one glibc implementation also does not automatically match a release binary whose compiler folded constant-exponent `pow` calls; R4 appendix C §1.4 explicitly flags that compilation behavior.

**Recommended change:** Inspect/profile the official pinned binaries, compare `x*x` and the proposed routine on the admissible compact-target/configuration domain, and define the canonical reference behavior. A mismatch on a valid supported chain/configuration must fail the gate even if absent from recorded history; exclude only inputs proven unreachable/invalid. Prefer the small arithmetic implementation if it passes those checks. I did not establish a concrete floating-point consensus divergence; this is an incomplete proof obligation and a potential unnecessary implementation cost.

### F20 — minor — cut speculative mechanisms that add review cost without established benefit

**Location:** A1 §2.3/§2.5 pure-crate requirements; §3.1/§3.3; §3.7; PLAN CS0/EV2a/ST1/CH4/MN1.

**Problem:** Sans-io and explicit context improve testability; a mandatory five-crate `no_std` feature matrix, custom lock-free/sharded caches, several independent CPU pools, multiple persistent-data-structure auxiliary caches and background template rebuilding are separate commitments. Their benefit is not yet measured, and they multiply safety obligations before a faithful reference implementation exists. The selected `imbl`/cached-cell combination should have its feature/target compatibility demonstrated at skeleton time rather than discovered during DML implementation. An in-house LevelDB-reader fallback would add another large parser/recovery surface without a measured need.

The lowest-failing `(tx,input)` requirement also adds work without matching Core's parallel block-validation interface. Core returns the generic `block-validation-failed` after a failed check queue (`dash/src/validation.cpp:2777–2779`); detailed immediate script failure strings belong to other execution paths (`:2210–2224`). Early abort and guaranteed discovery of the lowest failure require extra scheduling rules if both are retained.

**Recommended change:** First land the faithful simple reference, then add only measured hot-path optimizations under the existing twin/xcheck gates. Keep a narrow storage abstraction with memory and one real backend; use a tested import reader rather than starting a new engine/parser. Make `no_std` conditional on an actual consumer or validate the selected dependency subset immediately. Use the exact observable Core result for each script-check mode, without inventing a deterministic lowest-error requirement where Core exposes only a generic failure. Defer background GBT builds, custom cache concurrency and pool proliferation until their gains exceed their complexity and resource cost.

## Required disposition

Resolve F01–F04 before freezing chainstate/storage/evo interfaces. Resolve F05–F10 and F13–F16 in the corresponding design contracts and test coverage before treating those subsystems as implementation-ready. Correct the performance baseline and resource model before adopting the tabulated 2× expectations. Reconcile the milestone/acceptance DAG and staff the oracle, review and fuzz work as delivery dependencies.

The proposed verification layers are a strong foundation, but history equality plus happy-path differential functions and soaks is not yet proof for generated stateful behavior. F04 is a concrete example of what that combination can miss. The honest ECDSA, network, disk, DKG and CoinJoin shortfalls should remain explicit; unexplained optimism for connect/GBT/CL should not.

**Verdict: REWORK.**
