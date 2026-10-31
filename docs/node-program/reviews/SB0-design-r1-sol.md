# SB0 design review, round 1 — Sol (DEC-92)

Reviewed `docs/node-program/designs/SB0-shared-base.md` at **cce49b68**, in a detached scratch checkout. References to SB0 below use that revision's line numbers. Read `briefs/REVIEW-COMMON.md` first and followed its reviewer contract and the task's PLAN §7 restriction.

Inputs checked: DEC-81–103; PLAN's 2026-10-09 override and held rows; approved CH2 rev5, especially F1–F11/P1–P10; A1 and the relevant P2a/LQ1 interfaces; both SB0 legs; integration's SPV/FFI code; EV0's proposed interfaces; and PR #902's body and diff at **a0e6436441b29e522a4499356c99f93b4880704b**. Core references below are to **v24.0.0-rc.3**.

**Result: nine major findings, five minor findings, no blockers. The Sol gate is not satisfied.** The processor separation, node-only authoritative DML, and node-only RocksDB are reasonable choices, but the contracts below need revision before the sequential Opus pass.

## Major findings

### M1 — The shared header rule selects the wrong clock for v24

**Severity:** major. **Refs:** SB0 §4.2, `SB0-shared-base.md:322`; §7, `:547`; DEC-82.

The design requires the future-time check to use adjusted time. Core v24 uses **`NodeClock::now()` without the peer offset** (`validation.cpp:4509–4510`). The existing pure interface already distinguishes these: `dash-consensus/src/release.rs:54–58,96–99` selects `TimeSource::NodeClock` for v24.

**Failure scenario:** local/mock time is T, the median peer offset is +3,600 seconds, and an otherwise valid candidate is timestamped T+9,000. The proposed adjusted-time check admits it within its two-hour allowance; v24 returns `time-too-new`. A negative offset creates the opposite mismatch. This changes the node's header verdict, independently of its processor being Core-exact.

**Suggested fix:** require mockable local node time for the shared v24 contextual-header check. Keep any adjusted-time facility separate from this consensus input. Add positive/negative peer-offset and `setmocktime` vectors proving the node and shared kernel select the v24 clock.

### M2 — The shared send API changes Core's soft backpressure into queue admission

**Severity:** major. **Refs:** SB0 §3.1, `:221,238`; §3.4, `:281`; P2a §2.2.1, `P2a-outbound.md:164`, and its `NodeHandle::push_message` at `:275–276`.

SB0 describes bounded send queues and `push(...) -> Pushed`, with `Paused` when over `-maxsendbuffer`. Core's send limit is a **pause flag, not a rejection boundary**. `CConnman::PushMessage` charges retained memory, sets `fPauseSend`, and **still appends the message** (`net.cpp:4878–4890`). P2a explicitly preserves an unconditional, non-blocking push, including overlay pushes from other threads and transport memory retained through partial sends.

**Failure scenario:** a handler starts while the peer is below the limit and emits several responses. One crosses the limit; the next push encounters the proposed bounded queue. Rejecting it loses a response Core enqueues. Waiting/retrying introduces a scheduler interleaving Core does not have. Charging only queue payload bytes also resumes sends too soon while the transport retains a partially sent allocation.

**Suggested fix:** make the node's shared push unconditional and return the resulting pause state separately. Preserve P2a's retained-memory accounting, transport charge, and pause checks at Core's caller locations. If SPV needs a hard admission budget, enforce that in its processor without changing the node push contract. Test a multi-message handler crossing the limit, concurrent overlay pushes, and partial V1/V2 writes.

### M3 — The anchor pack cannot bootstrap the proposed MN trust chain near a checkpoint

**Severity:** major. **Refs:** SB0 §4.3, `:344–359`; §4.4, `:366`; §5.2, `:413–420`.

The 25-header pack supplies the contextual **header** window, but the MN pipeline also requires historical work-block headers and accepted headers for every historical diff target. No path supplies these before the anchor: `lower_anchor` is expressly reserved. A resident-window capacity of 10,000 does not create the missing history.

**Failure scenario:** a fresh store anchors at mainnet H=2,500,000 and the network tip is H+100. The pack ends below at 2,499,976. For a 576-block rotation cycle the current cycle's work block is **2,499,832**, and the work block three cycles older is **2,498,104**. These are absent. LQ1 reads such ancestors (`LQ1-quorum-members.md:246`; Core `llmq/utils.cpp:683–705`), while the new trust chain requires the corresponding diff target to be an accepted header. `qrinfo` supplies snapshots/diffs/commitments, not their block headers (`dash/src/network/message_qrinfo.rs:66–94`). The client cannot complete quorum verification without waiting for enough future chain growth or relaxing its stated new-kernel contract.

**Suggested fix:** specify bootstrap history independently of the DGW pack: choose an earlier anchor with enough verified forward history, or design authenticated historical context/backfill before starting the MN pipeline. State which below-anchor headers are context and which are accepted chain entries. Test a fresh client with the tip at the newest checkpoint, shortly above it, and across a cycle boundary.

### M4 — The cold schema cannot answer the promised branch ancestry queries

**Severity:** major. **Refs:** SB0 §4.1, `:293–311`; §4.4, `:363–368`; §9.5, `:656–657`; §11.3, `:763`.

`hdr_by_height` stores only the active header; `hdr_index` stores a hash's height/status/work, with no complete header or stable arena ID. This cannot reconstruct a stale branch's header, parent links, or an evicted `BlockId`. Looking up a stale hash's height and reading `hdr_by_height` returns the active branch's header at that height. Dropping non-active branches below the window floor also lacks a rule preserving their ancestry for later extension.

**Failure scenario:** with no ChainLock, the best chain reaches height 20,000, so the floor approaches 10,000. A competing branch forks at 8,000 and is delivered in batches until it eventually wins by work. Its below-floor headers must remain addressable while it is being extended and through a restart. The two tables cannot retain both A9,000 and B9,000's header bodies, and height is not a unique branch identity. A cold lookup or LCA walk fails or follows A. There is no guarantee a ChainLock is recent enough to make every permitted reorg resident; `cl_floor` is optional.

There is also an interface mismatch: `HeaderStore::header` is fallible, whereas the reused `ChainAncestry` is infallible and promises lifetime-valid IDs plus every ancestor down to genesis (`dash-consensus/src/ctx.rs:35–58`). Both checkpoint truncation and SQLite cold reads need an explicit adaptation of that contract. The `broken_chain_panics_in_debug` test confirms missing ancestry is a contract violation, not an ordinary partial-chain result.

**Suggested fix:** persist complete branch records keyed by hash/stable identity, with an active height index as a separate projection; define ID resolution after eviction/reopen and storage-error handling. Specify a partial-ancestry interface for anchored clients without weakening the node's existing complete-ancestry contract. Bound the resident `ActiveChain`/retained snapshots as well as the hash index. Gate windowed/unwindowed equivalence on multiple stale branches, restart, lagging readers, and a >10,000-block reorg without a ChainLock.

### M5 — SPV-to-full replay assumes the missing genesis-to-anchor prefix

**Severity:** major. **Refs:** SB0 §4.7, `:387–389`; §6.3 P10, `:529`; SB18, `:855`.

A normal anchored SPV store does not contain the complete chain. Its first stored context header is around anchor−24. Replaying its headers into a **fresh full chainstate through ordinary `accept`** cannot attach that first header: its parent is absent. Seeding the full chain with the trusted SPV anchor instead does not give V the complete index/history needed to validate blocks from genesis. This is also an unstated exception to the claims that full mode never opens the SPV store.

**Failure scenario:** an operator switches a wallet anchored at 2,500,000 to full mode. The importer starts with 2,499,976 in a genesis-only arena and gets `prev-blk-not-found`. Starting block download at the anchor skips the full node's historical validation obligation under DEC-82; starting it at genesis still needs the missing header prefix and an explicit merge with the stored suffix. A crash during an unspecified one-time import can repeat the entire replay or leave ambiguous completion state.

**Suggested fix:** define the import as a read-only, optional header hint source. Obtain/validate the missing prefix from genesis before attaching its stored suffix, using bounded batches through V and CH2's permitted persistence path. Record/resume import progress durably and skip it once completed. State the read-only import exception to “never opens.” Test a latest-anchor store, a genesis store, an interrupted import, and an invalid stored suffix; preserve B1/P10 throughout.

### M6 — `BEGIN DEFERRED` alone does not implement `KvEngine::snapshot()`

**Severity:** major. **Refs:** SB0 §9.4, `:644`; §6.3 P5, `:524`.

The engine contract requires a view **as of the snapshot call**, with later writes invisible (`dashd-store/src/engine.rs:114–115`). SQLite establishes a deferred read transaction's WAL end mark on its first actual database read, not on `BEGIN DEFERRED`.

**Failure scenario / reproduction:** on SQLite 3.46.1, write value A, open the second connection and execute `BEGIN DEFERRED`, let the writer commit B, then perform the snapshot's first SELECT. It returns **B**, although B was committed after the supposed snapshot was created. Thus an adapter advertising CH2 P5 can read state from the wrong copy point. An actual read immediately after BEGIN pins the view; a later writer commit C then remains invisible.

**Suggested fix:** establish the read view before returning the snapshot (for example, force a read of a real metadata row on the dedicated read connection), and define connection ownership and failure handling for snapshot creation. Add the exact two-connection interleaving above to the SQLite engine contract tests. No change to SQLite's prefix-recovery choice is needed.

### M7 — Keeping #902's facade verbatim cannot also bound topic queues

**Severity:** major. **Refs:** SB0 §3.2, `:255–263`; §3.3, `:274`; §3.4, `:282`; §10.3, `:719`; SB16, `:853`.

At the reviewed #902 head, `NetworkManager::subscribe` returns **`tokio::sync::mpsc::UnboundedReceiver<Inbound>`**, and the manager creates an unbounded channel for every subscription (`dash-spv/src/network/mod.rs`, `network/manager.rs` in the PR diff). Keeping that trait and its tests verbatim preserves an unbounded stage after the bounded per-peer inboxes. The existing facade also has neither `cancel` nor a defined owner-directed `RequestFailed` result, despite the proposed ledger depending on both.

**Failure scenario:** the MN/block subscriber is slow while `SpvRouter` keeps draining peer FIFOs into its subscribed topic queue. The network inboxes keep freeing space, so TCP backpressure never reaches the sender and the topic queue grows without the claimed per-peer/global bound. Avoiding that with drops violates response delivery; changing the receiver breaks the promised verbatim facade unless migration is explicitly designed.

**Suggested fix:** make a concrete, agreed facade extension: bounded subscription handles with defined fanout/backpressure, cancellation, terminal failures, and logical-request versus peer-attempt identity. Document the Rust consumer change and how the #902 compatibility adapter/tests migrate. Keep its deduplication, priority classes, controller, and mock behavior, but replace the contradictory “trait/tests verbatim” promise. Test a stalled subscriber, multi-topic fanout, retries followed by terminal failure, and late responses after cancellation.

### M8 — BLS verification does not prove an under-rejecting SML member adapter is fail-closed

**Severity:** major. **Refs:** SB0 §5.1, `:406–408`; §5.2, `:417–420`; SB8 acceptance, `:845`; reconciliation, `:932`.

The argument says any changed member set makes commitment verification fail. Core verifies `membersSig` against **only the entries whose `signers[i]` bit is set** (`llmq/commitment.cpp:76–93`). `BuildCommitmentHash` commits type, quorum hash, valid-member bits, quorum public key and vvec hash; it does not commit the full member identities (`:312–321`). `quorumSig` is verified against the committed quorum public key (`:96–107`).

**Concrete counterexample to the claimed implication:** use a three-entry roster `[A,B,C]` with signer bits `[1,1,0]`, then let the under-rejecting adapter produce `[A,B,D]` while preserving the commitment and roster length. The member-signature public-key inputs remain `[A,B]`; every commitment-hash input and quorum-signature input is unchanged. Both verification calls necessarily have the same verdict. The input-identity probe reproduces this independently of cryptographic implementation. This does not establish that a specific historical rotation produces that mutation; it disproves the universal fail-closed justification used to promote the adapter into production.

**Suggested fix:** distinguish authenticated commitment/key status from exact reconstructed membership. Either prove the SML adapter's selections are exact on the reachable domain, retain an explicit uncertainty state when unavailable DML fields can affect selection, or define a narrower justified SPV trust policy. Do not infer complete membership correctness solely from BLS success. Include non-signer-only changes and missing-property/rotation cases in SB8's mutation gate and the EV/LQ addendum.

### M9 — The plan omits required edges and understates the header split's critical path

**Severity:** major. **Refs:** SB0 §13.1, `:841,845,848,854–855`; §13.2, `:865–872`; §13.3, `:904,910–911`.

The written merge dependencies serialize **SB4 → SB5 → CH1-node**. The +1–2-week M2 estimate counts SB4 and CH1-node but omits SB5's 1.5-week implementation plus review. SB4 also changes CS2 from the old interface edge to a merge edge. A focused rerun of the committed `cpm.py`, applying exactly the new M2 split sizes/deps and keeping other rows unchanged, gives:

- CH1: **8.5 → 15.5 weeks**; ND1: **11.0 → 18.0**.
- P2b: **15.5 → 18.0 weeks**.
- REL11: **30.0 → 30.5**, with the limiting predecessor moving to the header chain rather than the claimed protected EV chain.

These are a sensitivity check, not a replacement forecast: the committed CPM still needs the intervening PLAN/DEC-103 revisions. They nevertheless refute the stated estimate under the document's own edges.

There are missing merge gates too: SB4 calls the CS3 contextual-header function (`:336`) without a CS3 dependency; SB11 requires production DIP24 construction but reaches only LQ1a through SB8, with **no LQ1b dependency**; SB18 can complete after SB16 while SB15's shared MN integration is still absent. SB17's final mobile/ABI cells depend only on SB14 although subsequent SB15/SB16 alter the product it is supposed to gate. The prose also orders SB11 → SB13, whereas SB13's table depends only on SB10.

**Failure scenario:** dispatch from the table produces a “complete” MN engine before its rotation algorithm, a product smoke gate before MN/router migration, or an M2 commitment that cannot meet its named exits. Delaying CH1 also consumes downstream slack despite promptly unholding EV0.

**Suggested fix:** reconcile the table and stage/order text, add the real kernel/product merge edges (or explicitly scoped intermediate gates), and rerun an updated CPM before recording milestone impact. If implementation against stubs overlaps SB5/CH1-node, express that as interface dependencies with merge constraints instead of counting an overlap the current graph does not permit. The early EV addendum remains sensible, subject to closing M3/M8 and the error-contract clarification below.

## Minor findings

### m1 — The layer checker has no valid rule for existing W-to-W edges

**Severity:** minor. **Refs:** SB0 §2.3, `:165–167`; SB1/SB3, `:838–840`.

L2 allows W → B,P but not W → W. Existing, intended edges such as `key-wallet-manager → key-wallet → dashcore` therefore fail before any proposed migration. B's own allowed destination set is also omitted. The feature-qualified `dash-coreimport/wallet` waiver cannot be checked from a single package-level all-features edge alone. Separately, SB1 requires every rule green, although SB3, which depends on SB1, removes the current default `clap`/file-logging violations.

**Suggested fix:** define B → B and the intended W → W,B,P closure; specify graphs with/without the wallet feature for the waiver; separate installation of the gates from closing a named, temporary baseline-violation list. Plant failures for direct and transitive normal/build N→V, V→N/Pn, unlisted wrapper crates, and forbidden hot-path W reachability. Prove the legitimate W→W chain passes.

### m2 — The no-thread claim needs a host/offload boundary, not only `std::thread` bans

**Severity:** minor. **Refs:** SB0 §2.3 L5, `:169`; §3.1, `:227–248`; §7, `:544,549`.

An injected tokio handle does not by itself imply no new OS threads: `spawn_blocking` and hostname resolution can create runtime blocking workers. “Inline commits … (`spawn_blocking`); no background threads” does not identify which layer invokes offload or bound those additional workers. SQLite itself starts no background threads; those are different claims.

**Failure scenario:** implementing SQLite offload or DNS inside S with the supplied handle passes bans on `std::thread::spawn` but induces extra blocking workers beyond the advertised two-worker mobile runtime.

**Suggested fix:** specify host-owned bounded blocking storage/DNS services and forbid runtime/pool construction and blocking-spawn entry points in S, or narrow the promise to ownership of threads and budget the induced workers. Keep the blst pooled-call ban/audit stamp and add an exercised-path thread-count test; dependency presence alone is insufficient.

### m3 — The EV split must preserve fatal store errors across all LQ1 seams

**Severity:** minor. **Refs:** SB0 §5.1, `:409`; §12.1 EV0/LQ1, `:781,784`; `LQ1-quorum-members.md:267–281,482–492`.

Dropping `MemberError::Store` and describing `Missing(BlockId)` as “request it” needs product-specific semantics. On the node, an absent list for an established connected block is a fatal invariant failure under LQ1's existing D4 contract, not recoverable network absence. `LlmqSource` and `SnapshotRows` also expose `StoreError`; changing only `MemberError` leaves references to the store being moved into node-only `dash-dml`.

**Failure scenario:** the mechanical split either pulls Pn back into shared `dash-evo`, or makes a missing authoritative node list look like an SPV fetch request without tripping the latch.

**Suggested fix:** make all shared view errors store-independent/generic and explicitly distinguish unavailable SPV input from fatal node storage/invariant failure. Preserve the latter's node-error/fail-stop path. Cover those signatures and a missing connected-block list in the EV addendum before unholding consumers.

### m4 — The mobile optimization overrides miss an active X11 implementation

**Severity:** minor. **Refs:** SB0 §8.2, `:576`; SB2, `:839`; §11.3, `:757`.

The override list includes `dash-x11`, but the reviewed `dash-consensus::hash::x11` calls **`dash_pow::hash`** (`dash-consensus/src/hash.rs:77–81`). `dash-pow` contains the eleven hash implementations, and is not on the proposed `opt-level=3` list. Optimizing the wrapper/different backend does not optimize that dependency. The 8.81 MB measurement also predates the amended set of overrides; it cannot establish that the amended profile is a free size saving with unchanged hashing speed.

**Suggested fix:** derive overrides from the actual linked hashing paths, including `dash-pow` until the backend switch and the C backend flags where used. Measure the final profile's stripped Android size and instruction counts. Keep the reported raw profile measurement and clearly label the amended profile, production-schema memory, iOS, and combined-SDK size claims as pending measurements. Arm64-only remains a product question, not an approved gate reduction.

### m5 — The feed sketch omits mechanisms needed by its claimed CH2 adapter

**Severity:** minor. **Refs:** SB0 §6.1–6.2, `:477–488,494,502–512`; CH2 `:1304–1310,1344–1345,1818–1821`.

The `ArrayQueue` sketch does not define the required `front`/`remove_round_trip` adapter, a producer wake when a consumer frees space, or how registration remains non-live until `subscribe_at`. Its sole slot waker is the subscriber's enqueue waker. “Never across a wait” also needs the explicit DEC-96 exception for `finish()`'s durable handoff commit. `HeaderAccepted`/`ChainLocked` must not appear as extra tip events in CH2's exact F1 stream.

**Failure scenario:** an SPV producer parks after a full queue; a subscriber pops the last item and then parks. Without a space-available wake the producer never calls `retry_pending`. Alternatively, registering a slot before handoff and fanout to “every target” can deliver the already-assigned pending event below the cut, the CH2 X28 case.

**Suggested fix:** make the SB4/SB5 brief specify queue capacity including any peek/compression staging, producer-space and deregistration wakes, inactive registration and captured pending targets, generational IDs if slab slots are reused, and separate tip/status event streams. Port CH2's pending-at-handoff/X28, F11 with pending fanout and slot reuse, compression, and full-queue/space-wake scenarios to the real producer. The F10 cut `next_seq` and same-lock final commit are otherwise correct as stated.

## Verification and scope conclusions

- In the detached cce49b68 checkout, with `TMPDIR=/dev/shm/sb0-review-r1-sol-tmp` and `CARGO_BUILD_JOBS=12`:
  - `cargo test --locked -p dash-consensus --lib pow::` — **7 passed**.
  - `cargo test --locked -p dash-consensus --test core_vectors pow_tests_get_next_work` — **1 passed**, including six Core next-work vectors.
  - `cargo test --locked -p dash-consensus --lib release::tests::` — **3 passed**; baseline context tests, not a proposed dual-release gate.
- A disk-backed two-connection SQLite probe reproduced M6 and verified that a real read before snapshot return pins the earlier view. A focused CPM probe produced M9's values. Arithmetic and verifier-input probes produced M3/M8's concrete witnesses. These probes do not claim device power-loss, mobile performance, or cryptographic test coverage.
- For the bundled non-genesis checkpoints at multiples of 50,000, **25 is sufficient and conservative for DGW/MTP**: Core DGW reads the parent and 23 predecessors (`pow.cpp:84–111`); MTP reads up to 11. At anchor+1, anchor through anchor−23 suffice for DGW; shipping anchor−24 adds one spare. This remains true at the newest checkpoint without a next checkpoint, and while validating the intervening forward gap. Absolute trusted chain work addresses min-work/IBD accounting. Restrict this fixed-pack claim to post-DGW anchors; pre-DGW BTC/KGW anchors require their actual longer lookback or genesis fallback. It does not solve M3's MN bootstrap.
- The node's ownership of V, authoritative DML/quorum state, and B1 block-index persistence is preserved in the stated architecture; there is no proposed authoritative shadow engine on V. F2's `tip_seq`, F10's cut, F11's same-lock removal, and P10's same-engine batch are the right contracts. The actual producer adapter still needs m5's details and tests; the tables are not an implementation proof.
- SQLite WAL/NORMAL buffered transactions, FULL durable transactions, a real FULL write for `sync_wal`, and all SPV tables in one database can provide atomicity and prefix recovery under the normal storage assumptions. `fullfsync=1` also selects full sync for checkpoints in the inspected SQLite source (`sqlite3PagerSetFlags`), so a separate checkpoint pragma is not intrinsically missing. The implementation must serialize writer commits and pin snapshot reads as in M6. Cross-task assembly/order of the advertised header/MN/locator transaction should be explicit in the SB13 brief. Native/device tests remain necessary; the host probe used a simpler schema and cannot certify the production/mobile budget.
- L2 can prevent the node depending on dash-spv and the reverse after m1's corrections. The node's independence from #902 and its bounded late-landing Q1 choice are sound staging goals; M7 concerns the actual facade contract. DEC-84 portability, honest UA, v24-only behavior, local workspace development, and Mac QA timing are retained. Define the ABI-freeze baseline around #1104 explicitly: additive callback tables need size/version negotiation; compiling current headers alone does not demonstrate compatibility with an older caller's table layout.

The scratch checkout, probe data/build artifacts, and this review's `/dev/shm` directory were removed at handoff. No commit, push, or GitHub comment was made.

## Verdict

**REWORK. Sol gate: NOT SATISFIED.** Revise M1–M9 and close the concrete interface clarifications above, then return the revised design to Sol. The DEC-92 sequential Opus-high pass should wait for satisfaction of this gate.
