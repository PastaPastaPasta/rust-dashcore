# SB0 design confirmation review, round 5 — Sol (DEC-92)

Reviewed `docs/node-program/designs/SB0-shared-base.md` **rev4 at 53091a3285dcd7173a018c7b2e455fe0f24e2fcd**, using the detached checkout `/work/scratch/sb0-review-r5-sol/wt`. Read `git diff d790b55e 53091a32 -- docs/node-program/designs/SB0-shared-base.md`, §16.1, the r4 report, `briefs/REVIEW-COMMON.md`, the program context and binding DEC-104/105/107/109. Compared the alignment edits with `designs/EV-addendum-SB0.md` at fa09d8d0; that file is unchanged at the reviewed revision. SB0 line references below name 53091a32.

**Result: R4-M1 is CLOSED. The earlier minor closures stand. The 53091a32 alignment edits are consistent, with no new defect found in this scope. No blockers, majors, minors or nits. Sol gate: SATISFIED.**

## R4-M1 closure

All four requested properties are specified in the operative contracts, rather than only in the §16.1 resolution map:

- **Genesis clipping:** `SB0-shared-base.md:553` drops nonexistent negative-height elements from `required(T)`; `:599` applies that rule during coverage. Unknown bases and real below-anchor history still fail coverage. `:583` and `:829` restrict coverage and lock fetches to `[max(tip − H_keep, A_q), tip − 8]`. This removes the negative/nonexistent startup requests without weakening coverage above a non-genesis anchor.
- **Empty coverage establishes the client:** `:591` explicitly makes an empty domain pass without a request. `:601` places the `Established` transition before the MN pipeline and subscribers start. `:832` separately covers an authenticated empty active set: its history is vacuously covered and Core selection returns no quorum. Negative selection starts and starts before DIP3 produce `NoQuorum` (`:824`); the DIP3-to-quorum-root gap is unavailable without a fetch (`:826`). Thus the pre-quorum client can run wallet/header subscribers without inventing roots or usable quorums. The first commitment enters the ordinary authentication/verification pipeline, with coverage rerun (`:594`, `:614`, `:886`). Early rotated commitments still require their actual inputs and may remain unavailable (`:833`); empty coverage does not confer `Verified` status.
- **Growth and sliding:** `:829`–`:831` define the available authenticated interval. After its first singleton at `tip = A_q + 8`, its oldest end remains at A_q until `tip = A_q + H_keep`, then advances with the tip. The stated depth `min(H_keep, tip − A_q)` is measured from the tip to that oldest end, consistently with H_keep's definition. Coverage no longer waits for an unservable early-cycle `qrinfo` (`:590`); commitment verification still does.
- **Acceptance gates:** `:637` and `:880` add establishment, subscriber startup, first-commitment and domain-growth cases. SB11 (`:1411`) and SB15 (`:1415`) carry them into task acceptance: genesis and offset-8 boundaries, A_q + 7/+8, regtest tips 20 and 450, A_q + H_keep ± 1, activation overrides, and early devnet rotation. Tip 20 needs no request; tip 450 requests **432 and 442**, never 394. Coverage fallback and DEC-107's later-miss diagnostic retain their existing behavior.

## Minor closures and alignment

The r4 report explicitly listed **no new minors**. Its earlier minor closures remain intact: CH1-node's coding float is **11.5 − 11.0 = 0.5**, separately from its 1.5 merge slack (`SB0-shared-base.md:1528`); fallback still costs one forward header sync per checkpoint tried (`:608`). The older findings and manager-ratified limits remain settled.

The alignment edits agree with the pinned EV addendum §2.2 and §7:

- `LlmqConfig` is the owned per-client parameters/used-types value; LQ1's borrowed per-call `LlmqCtx` is built from it (`SB0-shared-base.md:902`, `:913`). The rename is applied to sizing, verification and SB11's row without changing their rules.
- `MnListView` remains infallible over materialized values. `LlmqChain`, `LlmqSource` and `SnapshotRows` carry the fallible reads; `MemberError<E>` retains **Missing, View and UnknownLlmqType(u8)** (`:761`). Node D4 and SPV unavailability remain product-specific mappings. EV0's existing `UnknownLlmqType` variant is preserved rather than folded into a storage failure.
- `MinedCommitmentReader` moves to shared `dash-evo::llmq`, with its associated error and complete-ancestry restriction (`:763`, `:1344`). SPV continues to construct its scan from the authenticated set and does not implement this reader. Moving the reader does not introduce a shared dependency on node DML/store state.
- `MnList::project_sml` belongs to EV4-serve in `dash-dml`, consistently in the crate map, mnxcheck description and task rows (`:137`, `:928`, `:1345`, `:1434`). EV2a retains the list-view implementation; SB19 already depends on EV4-serve. The move introduces no new scheduling edge or cycle.

§16.1 correctly records these resolutions. DEC-104/105/107/109 and the r1–r4 settled findings are not reopened.

## Verification and limits

- Checked the activation/root construction, pre-DIP3 commitment rule, absent selection start, empty scan and early-cycle availability against Dash Core **v24.0.0-rc.3**: `deploymentstatus.h:34`, `node/miner.cpp:199`, `:262`, `:294`; `chainparams.cpp:576`, `:818`; `llmq/blockprocessor.cpp:218`, `:599`; `llmq/quorumsman.cpp:711`, `:734`, `:758`; `llmq/utils.cpp:491`; `llmq/snapshot.cpp:35`, `:175`. Also checked v23.1.8's network heights and negative-start/empty-scan controls. The v24 contracts govern this design.
- Checked EV0 **3a5cb631**: `dash-evo/tests/consumers/lq1a.rs` already models infallible list views; `src/llmq/members.rs` contains the existing distinct unknown-type variant; `src/store/mod.rs` shows the complete-ancestry mined-reader contract and the shared shapes being moved. These reads substantiate the addendum alignment; they do not substitute for the later API-delta implementation review.
- Ran a disposable arithmetic probe with `TMPDIR=/dev/shm/sb0-review-r5-sol-tmp` (mode 700): **6 tests passed**. It checked both Core pins' parameter values, explicit regtest/devnet boundaries (including tips 55/56), 4,004 tip/configuration combinations against an independent enumeration of available authenticated selection starts, growth/sliding, a DIP8-after-DIP3 override, and clipping controls that preserve real or unknown missing history.
- `git diff --check d790b55e 53091a32 -- docs/node-program/designs/SB0-shared-base.md` passed. The new changes contain no source/build modification. The Candidate driver, horizon fetch path and reshaped EV interfaces remain proposed work: their production crash/restart, network-fetch and lock-verification gates are still implementation acceptance requirements.
- Followed PLAN §7 handling (CANDIDATE-SPV). No excluded/private issue material was investigated. No GitHub actions or managed dashd/oracle interactions occurred. The reviewed branch was not edited; only this requested report is written outside scratch, uncommitted. The disposable checkout, probe and private TMPDIR are removed at handoff.

## Verdict

**APPROVE. Sol gate: SATISFIED.** No blocker or major remains in this confirmation scope. Ready for the sequential Opus-high pass under DEC-92; this is not a substitute for that pass or the addendum's own approval.
