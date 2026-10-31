# SB4 design confirmation, round 6 — Sol

Reviewed **45d9e6408a4aa151af850adeecd0e0a64a8e2358** against **e3d2d9ad71e5ddf80844ae6df4db6e305acf4f60**, only `docs/node-program/designs/SB4-dash-chain.md` and `docs/node-program/designs/SB0-shared-base.md`. All design line references below name those pinned blobs. Read REVIEW-COMMON, Sol r5, the relevant CH2 contract and program context. Binding: DEC-119, 123, 127, 128, 132, 136, 140, 144 and 145. DEC-145 is taken from the manager's subsequent decision record and this brief; the reviewed design blobs remain pinned. PLAN §7 handling was followed; no private inputs were read.

Scope is **only R5-M1 → DEC-144(1), R5-M2 → DEC-144(2), and DEC-145**. No settled selection, marking, locator-density or notification decision is reopened.

**Result: REWORK. Sol gate satisfied: no.** Effects/checkpoint atomicity and the per-disconnect pressure site are corrected. D43 matches Core and preserves handoff exclusion. H1–H6 do not yet establish completion when a closing mandatory subscriber remains a W target.

## Findings

### R6-M1 — closing can remove the event needed by a still-registered mandatory W target

**Severity:** major. **Refs:** `docs/node-program/designs/SB4-dash-chain.md:939` (H2's registry snapshot remains stable until mutex-taking deregistration), `:941` (H4 drops a closing target from pending and stops delivery), `:943` (H6), `:1036` (A-40).

H4 frees V's backpressure wait, but does not retire the subscriber from W's captured ack set or rewind gate. H2 expressly keeps registration stable during the hold. An atomic `closing` flag is not an ack, a rewind, or a specified cancellation of those obligations. H6's lock-free event→ack path cannot manufacture an event H4 deleted. Even an independently running ack worker cannot complete that path.

Concrete allowed schedule:

1. V enters enforcement at B110 with F0 in flight. Mandatory Synced S is one event behind: its capacity-one queue contains the connect to B110 at sequence q. Entry `pending` is empty. S's cut includes q and q+1.
2. V disconnects B110 under the hold. Its event, sequence q+1, makes the private tip B109 and becomes pending for S's full queue. D43 has handed it to pending at its step.
3. A separate deregistration thread sets S's `closing` flag and blocks on the held tip mutex. H4 makes V remove S from pending and deliver nothing further. H2's registry still includes S.
4. S's independent ack worker processes the already-queued connect q and commits/acks F0 at B110, without taking a tip or hub lock. F0 releases. At a subsequent IF_NEEDED check V freezes F1 at the current private tip B109, whose sequence is q+1. W's registry snapshot still contains S. S's durable B110 locator triggers F1's rewind gate; alternatively the missing q+1 is the sequence-number case-B obligation.
5. S cannot consume the deleted disconnect q+1. W cannot finish the gate/ack and release F1. At a later CRITICAL crossing V waits for that release without releasing the tip mutex. Deregistration therefore cannot complete and republish S's removal.

The dependencies are **V → W → S's missing disconnect**, with **deregister → V's tip mutex**. Completing deregistration would remove S, but V's wait prevents it. W and the ack worker acquire none of the prohibited locks; servicing `PruneClears` cannot supply the missing event. Weak fairness does not help an event that is no longer pending or queued.

**Suggested fix:** make closing a coordinated retirement protocol between the feed and W. Before discarding delivery, either preserve the events/acks needed by outstanding gates and captured barriers, or notify W through its independent channel and explicitly retire the corresponding obligations. Specify the linearization against both gate creation and step-5 capture, including future freezes during the hold. Preserve the durable-locator and prune-lock obligations until retirement is safe; merely deleting a prune lock is insufficient. Keep DEC-144's per-disconnect check and DEC-140's continuous hold.

**Required A-40 strengthening:** make the pending deregistering subscriber itself **mandatory and Synced**, with its required disconnect pending when closing wins. Exercise capture/gate creation both before and after closing, and an additional freeze/CRITICAL wait before the hold ends. Assert that no W target depends on an event removed by closing. The present row distinguishes a mandatory acking subscriber from a second pending deregistering subscriber, so a non-mandatory second subscriber can make it pass without testing this schedule. Keep the violating-W negative control and repeat with real W in CH2b.

### R6-m1 — the old FlushCommit sequence pointer conflicts with an intermediate freeze

**Severity:** minor. **Refs:** `docs/node-program/designs/SB4-dash-chain.md:803`; `docs/node-program/designs/SB0-shared-base.md:1142`; compare SB4 `:752–754`, `:938–939`.

SB4 §9.2 still says `FlushCommit.tip_seq` reads `TipState.snapshot` under the lock in CH2b. During the hold that snapshot retains pre-enforcement sequence q, while the private tip can already be B109 at q+1. Copying the snapshot's sequence into an intermediate freeze would misidentify the event that made `new` the tip and change CH2's case-A/case-B test. Having W take the mutex to obtain it would violate H1 and deadlock instead. The new H2 channel does not reconcile this older pointer.

**Suggested fix:** state that **V captures the matching private tip and last assigned sequence into FreezePacket under its existing guard**, and W obtains both exclusively from that packet. Update the §9.2/F2 pointers and assert this pairing in A-40/A-41 while the published snapshot still has q. No additional snapshot store or W lock is needed.

### R6-n1 — D41 omits the required sentence's final period

**Severity:** nit. **Ref:** `docs/node-program/designs/SB4-dash-chain.md:1147`.

All seven repetitions have the identical required words. Six include the full sentence; D41 instead ends `transaction (rev5, DEC-144)`. There is no semantic ambiguity or separate-commit permission. For literal word-for-word sentence compliance, insert the period before the parenthetical.

## Confirmations within scope

- **DEC-144(1): substantive closure confirmed.** SB0 `:1381`, `:1944`, and SB4 `:1048`, `:1147`, `:1161`, `:1244`, `:1257` consistently require effects, reorg truncation, synced height and locator in one wallet-DB transaction. The former “with or after” permission survives only as a description of withdrawn text. No operative separate-effects commit remains. The recording-layer row rejects an effects-only commit and covers cuts before/after the atomic reorg commit. Both the SB4 fixture (`:1047`) and SB0 pointer (`:1949`) now say **committed Buffered but not Durable**, consistent with commit-before-publish. Core's cited `CWallet::LoadToWallet` revalidation is present at rc.3 `wallet/wallet.cpp:1142–1167`.
- **DEC-144(2): sampling/order closure confirmed.** SB4 `:750–753`, D42 and A-26/A-41 restore IF_NEEDED after coins work, before each tip step, including native CRITICAL/EVO_CRITICAL wait and the following freeze under the hold. This is rc.3 `validation.cpp:3303–3316` inside the continuously held `EnforceBlock` scope (`:4053`, `:4096–4105`). No operative end-only deferral remains.
- **H1–H6: necessary progress paths, incomplete sufficiency.** H1/H2 remove W's dependency on the held shared locks and delayed control mirrors. H3 services the legitimate W→V `PruneClears` dependency instead of blocking both sides. H4's lock-free pop and wake permit pending delivery to drain. H5 makes intermediate-tip events available before the next pressure wait. H6 keeps the ack/rewind worker independent of V and the mutex. These restrictions are confined to the progress paths under the hold; they add no per-step durable commit, subscriber barrier to every disconnect, or blanket lock prohibition outside those paths. The remaining issue is H4's retirement semantics, not a reason to withdraw these paths. With an ordinary live target, CH2's case-B drain/checkpoint, rewind and fair service loops terminate at subscriber-owned I/O or V's serviced requests. With the closing target above, that proof loses its event-delivery premise.
- **A-40:** the 10-second watchdog, violating-W negative control, pruner service, blocked handoff and CH2b real-W repeat are appropriate. They need the mandatory-closing variant described in R6-M1. A watchdog is a bounded regression check, not a proof of arbitrary scheduling.
- **A-41:** the 2,000 checks, first native crossing at 102, EVO crossing, ordered events, one snapshot and recovery cuts at intermediate freezes are specified. With only a 1 MiB initial F and 72,000 bytes restored per disconnect, `1,048,576 + 102 × 72,000 = 8,392,576 > 8,388,608`; disconnect 101 stays below B. The per-boundary envelope is **B + 72,000 = 8,460,608 bytes** for this counter fixture, not an RSS invariant or a bound on arbitrary blocks. My asynchronous counter probe peaked at 8,424,000 bytes, with 34 waits/freezes and a separate EVO crossing. Intermediate-freeze crash recovery still requires the specified production CH2b test.
- **DEC-145: Core comparison confirmed.** rc.3 `DisconnectTip` changes the tip and queues `BlockDisconnected` at `validation.cpp:3316–3326`; `validationinterface.cpp:241–247` and its enqueue macro (`:180–188`) submit to `SingleThreadedSchedulerClient`. `scheduler.cpp:132–173` schedules and invokes callbacks without `cs_main`. The validation scheduler is started in `init.cpp:1573–1592`. ChainLock enforcement runs on a **different, privately owned `cl-schdlr` thread** (`chainlock/handler.cpp:50–71`, `:198–205`, `:296`). Thus callbacks can execute during EnforceBlock's hold; they need not wait for its return. The same separate-scheduler arrangement exists in v23.1.8 (`chainlock/handler.cpp:45–66`, `init.cpp:1548–1555`). This is permitted concurrency, not a promise that every callback finishes before release.
- **DEC-145: handoff exclusion confirmed.** A live pre-enforcement subscriber may process intermediate disconnect events and advance its own cursor while the global snapshot remains at the pre-enforcement cut; Core permits that too. D43 does not admit a new subscriber at that cursor. A B109 `subscribe_at` during the hold still blocks and returns `Moved` after the final tip 80, without `finish()`. A pre-hold subscription captures every event; a successful post-hold subscription cuts after them. Consequently no newly admitted subscriber receives a lineage that skipped an earlier disconnect through an intermediate handoff. If “observe a state” means an existing subscriber's event-derived cursor, intermediate cursors are expressly intended; published snapshots and admission remain pre/post only.

## Validation and limits

Exact inputs, the diff, independent probes and logs are under `/work/scratch/sb4-design-r6-sol/`.

- Ran `spec_checks.py`: real SQLite WAL/FULL transactions, a recording-layer effects-only negative control, subprocess `os._exit` cuts before/after the one wallet reorg commit and reopen/locator recovery; the 2,000-disconnect native/Evo counter probe; threaded live-target/pruner progress and violating-W watchdog controls; the mandatory-closing deadlock witness and an explicit W-retirement progress control. All expected outcomes passed. The retirement control demonstrates the missing wait escape only; it does not validate a complete durable retirement/pruning implementation.
- Ran `/work/home/agent/workspace/dash-wt/pr7841/src/test/test_dash --run_test=scheduler_tests,validationinterface_tests --log_level=test_suite`: **5 cases passed, no errors**. Its scheduler sources, validationinterface source and both test sources match rc.3 byte-for-byte. This existing newer executable is supplemental scheduling evidence, not execution of EnforceBlock plus native CH2.
- These are design/specification checks. No production SB4/SB5/CH2b implementation, actual power-loss header-store simulation, full-node RSS bound or intermediate-freeze production recovery is claimed.

The reviewed design files still match 45d9e640 in the integration worktree. Temporary test data used a mode-700 TMPDIR under `/dev/shm`, removed after testing. No reviewed source, existing review, live chain instance, private input, GitHub message, commit or push was changed. This review is left uncommitted.

## Verdict

**REWORK. Sol gate satisfied: no.** R5-M1 is closed substantively; per-disconnect pressure order and DEC-145 are confirmed. Resolve the mandatory-subscriber closing/retirement cycle and reconcile the intermediate freeze's tip/sequence capture before claiming the hold's CRITICAL wait always completes.
