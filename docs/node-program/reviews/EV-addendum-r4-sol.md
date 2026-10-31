# EV addendum to SB0 — narrow confirmation review, round 4 (Sol)

| | |
|---|---|
| Under review | `docs/node-program/designs/EV-addendum-SB0.md` at **42a20ebb98d34c9f2d5d426e2a10e57bf9085698**, `node/integration`: rev3 **17c65276** plus the §5 wording fix |
| Compared with | rev2 **df34ea3e**; Sol rounds 1–3 |
| Contract | `briefs/REVIEW-COMMON.md`; PLAN §7 handling |
| Scope | R3-M1/R3-m1 closure, rev3 route/lock ordering, and retention of the earlier case-6 matrix |
| Binding decisions | DEC-111 and DEC-113; the overlay and serialization policies are not reopened |
| Core | Exact source reads at **v23.1.8** and **v24.0.0-rc.3** |
| Date | 2026-10-09 |
| Result | **APPROVE-WITH-CHANGES. Sol gate NOT satisfied.** One major route-classification gap and one minor acceptance mismatch remain. |

## Disposition

**R3-M1 — fixed for the direct members and scan entry points, but not fully closed.** Rotated runtime members and QM1 scans/`GetQuorum` dispatch before acquiring either the gate or a cache mutex (`EV-addendum-SB0.md:277-282`). V executes the whole command between attempts, acquires one shared token, writes the row inline, publishes the members, releases the token and replies. Static `use_rotation` classification conservatively sends hits and dynamically disabled rotation through the same path without inspecting mutable state before the gate. This closes the original direct-call wait cycle while preserving V's writer/ledger ownership. The new table incorrectly treats the complete qrinfo/rotationinfo builder as read-only, however; see **R4-M1**.

**R3-m1 — closed.** §2.6's liveness bullet (`:292`) and §5's CPM bullet (`:444`, changed by 42a20ebb) both allow a reader to wait across multiple attempts and leave latency/fairness to PF. Neither retains the one-attempt bound. The bounded waits in case 6-D are test completion deadlines for finite, controlled schedules, not a new general latency promise. The CPM tool is byte-identical to rev2; no schedule ruling is reopened.

## Findings

### R4-M1 — The complete qrinfo/rotationinfo builder is not a read-only route

**Severity: major. Location:** `docs/node-program/designs/EV-addendum-SB0.md:284`; also `:271`, `:317`, `:479`.

Route 4 says that the qrinfo builder and `quorum rotationinfo` read rows/mined commitments and “compute no members,” and therefore run on the caller's thread under `external()`. That is true of an isolated snapshot getter, but false of the complete builders named in the row. Both pinned Core versions contain this transitive path:

- `BuildQuorumRotationInfo` calls `BuildSimplifiedMNListDiff`, including the tip diff in both construction modes: v24 `llmq/snapshot.cpp:135-136` and `:250-252`; v23 `:90-91` and `:205-207`.
- For a post-V20 target, `BuildSimplifiedMNListDiff` calls `BuildQuorumChainlockInfo`: v24 `evo/smldiff.cpp:221-226`; v23 `:199-203`.
- That routine iterates **all `newQuorums`**, not just the non-rotated ChainLock type, and calls `qman.GetQuorum(e.llmqType, e.quorumHash)`: v24 `smldiff.cpp:76-80`; v23 `:77-81`.
- A quorum-object-cache miss builds the quorum, and `BuildQuorumFromCommitment` calls `GetAllQuorumMembers`: v24 `llmq/quorumsman.cpp:536`, `:71`; v23 `:414`, `:80`. A rotated members miss can then call `StoreSnapshotForBlock`: v24 `llmq/utils.cpp:462`; v23 `:453`.

The approved LQ1 design already records this path in §3.7.1 and §6.3.2 (`LQ1-quorum-members.md:1218`). Direct `mnlistdiff` serving reaches it too; classifying only the nested `GetQuorum` does not classify its outer caller before that caller acquires a token.

**Concrete failure scenario:** serve a post-V20 qrinfo/rotationinfo response whose diff adds a rotated quorum, with both its quorum-object and members caches cold. Following route 4, the serving thread first takes the shared token. V starts an attempt and waits for that token's exclusive release. The builder reaches the nested rotated `GetQuorum`, whose route-2 wrapper sends a command to V and waits. The serving thread cannot release its outer token until the nested call returns; V cannot service that command until the token is released. This is the original **caller → V reply → exclusive gate → caller** cycle through a newly misclassified outer route.

The blanket lock-order prohibition correctly rejects this execution, but the table provides no legal way to complete it: dispatching under the existing token violates `:277`; writing inline on the caller violates V's ownership; re-taking a shared token can deadlock behind the queued writer. A nested token alone cannot transfer the snapshot writer to the caller.

**Suggested fix:** distinguish raw snapshot getters from composite serving operations. Dispatch the whole qrinfo/rotationinfo builder, and the relevant direct `mnlistdiff` serving operation, to V **before any gate/cache acquisition** whenever it can reach materialization. Conservatively dispatching these complete builders avoids pre-gate mutable reads needed to determine which types the diff contains. Within the command, every scan/materializer/members call reuses its existing `ExternalToken` and executes inline, without self-dispatch or a second gate acquisition. Keep genuine row-only readers on the caller's thread. Update route 4 and its use in case 6-D(c), and add a post-V20, cold rotated-quorum composite-builder case with an attempt queued. This changes route wiring, not DEC-111/113.

### R4-m1 — Case 6 still requires rotated callers to wait at a gate they no longer acquire

**Severity: minor. Location:** `docs/node-program/designs/EV-addendum-SB0.md:309`; also `:308` and the case-6 change-map summary at `:490`.

The original **success/failure/proposal × base/indexed-hit** matrix, its `(M1, S1)` requirement and cases 1–5 are retained verbatim. That coverage is still required. However, the CH7b instruction says the external thread **must be parked on the gate**, observed through its waiter count. Rev3 expressly dispatches even rotated cache hits to V (`:291`). When V is paused inside the attempt, the external thread waits for the command reply; its command is queued and has not called `external()` at all. A correct implementation therefore has no shared-gate waiter for this caller, so the specified waiter assertion fails or waits indefinitely.

The corresponding real-thread negative control also needs an explicit scope: removing only the shared gate from a rotated command still leaves it serialized behind V's open attempt, so it cannot expose `(M1, S0)`. A gate-less direct-call model can still reproduce that pair, but it bypasses the new dispatch boundary as well.

**Suggested fix:** retain all six warm-hit combinations and observe the applicable wait location. For dispatched rotated calls, acknowledge the queued command and require no lookup/read/reply until release; for genuine caller-thread readers, retain the gate-waiter assertion. Specify the negative control's intentional bypass of the boundary, or exercise it through a caller-thread observer. Keep the pure LQ1a `Blocked` model, the indexed hit's delayed base insertion, and SB19's hit/miss checks. The new cold-miss case 6-D supplements these warm-hit cases; it does not replace them.

## Lock-route confirmation

- **In-attempt calls:** route 6 overrides the external dispatch split. CbTx/MNHF/asset-unlock calls, commitment checks, precompute and their nested scans/materializers/members must use the existing `AttemptHandle` throughout (`:258`, `:273`, `:286`). Case 6-D explicitly requires a nested validation-time materializer miss to enqueue nothing. It must never enter an external wrapper.
- **V calling itself:** a dispatched route-2 command passes its existing `ExternalToken` to its nested materializer/members implementation (`:267`), so those calls execute inline. Dispatching again while holding that token is expressly forbidden by `:277`. The corrected composite-builder route must obey the same rule. No separate self-dispatch exception is needed for compliant nested calls.
- **Cache-mutex holder waiting on V:** `:277` explicitly forbids it, even without a gate token. Dispatch must precede the cache lookup/lock; a miss discovered under a cache mutex cannot be repaired by dispatching while retaining that mutex. No opposite lock order is newly required by the compliant routes.
- **Dispatched reader while an attempt is queued:** if V is already blocked on the exclusive gate behind a genuine read-only caller, the dispatched operation stays in V's queue. That caller needs no V reply and can release its token; V completes/applies/releases the attempt and then services the command. If the command is already running, the attempt is only queued: V is the sole attempt opener, so there is no concurrent writer acquisition blocking the command's shared acquire. These are case 6-D(c) and (b), respectively.
- **Other routes:** the predictor does not store a row; mnxcheck's writes stay in `ShadowRows`; non-rotated members/materialization cannot emit rotation snapshots; log application remains inline on V under the exclusive gate. Their classification is sound. The composite serving path in R4-M1 is the exception to the claimed exhaustive table.

## Evidence and limits

The review compared the exact 42a20ebb design blob with df34ea3e and read all three prior Sol reviews and DEC-111/113. Integration advanced by a STATUS-only commit during the review; the reviewed design and prior-review blobs remained unchanged. No settled algorithm, crate-split, observer-isolation or schedule section was re-reviewed beyond the rev3 changes.

Review-only scratch used `/work/scratch/ev-add-r4-sol.AnLnRG/`; tests used `/dev/shm/ev-add-r4-sol-tmp`, mode 700. No Dash daemon or oracle was started or mutated.

- Rust probes built with `rustc --edition=2021 --test -D warnings`: **5 tests passed**. They cover the **18** dispatch-position/outcome/members-or-scan combinations, nested inline/token reuse, the **six** old visibility combinations with queue waits and delayed indexed-hit copying, the composite caller's queue-after-token deadlock, the prohibited cache-mutex → V wait, and V self-dispatch. Channel handshakes establish pause positions; bounded receives detect blocked replies. Negative controls have reviewer-only escapes so all threads join.
- Pinned-source/document checks with `python3 -B`: **3 tests passed**, verifying the composite builder's transitive path in both tags, byte-for-byte retention of the earlier conformance cases, removal of both latency-bound claims, and the unchanged CPM tool.

These probes check the specified integration ordering and its counterexamples. They do not execute CH7b, prove production route wiring, or establish a concrete lock's fairness; the proposed implementation and acceptance suites remain future work. All reviewer-created probes, binaries and temporary directories were removed. Only this review file was added; it is uncommitted. No push or GitHub comment was made. PLAN §7 handling was followed without accessing private material.

## Verdict

**APPROVE-WITH-CHANGES. Sol gate NOT satisfied.** R3-m1 is closed. R3-M1's direct-call fix is sound, but its overall closure requires **R4-M1**: classify and dispatch the complete builders that can reach rotated materialization before they acquire a token. Correct **R4-m1** while preserving all earlier case-6 combinations. DEC-111 and DEC-113 remain binding and unchanged.
