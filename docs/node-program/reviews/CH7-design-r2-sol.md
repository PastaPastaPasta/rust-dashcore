# CH7 design rev2 review (Sol r2)

## Verdict

**REWORK**

The six r1 findings are substantively addressed. Rev2 has one new Major: G1q can refuse a CbTx ChainLock under assumevalid even though Core skips that CbTx check. Two new Minor contract gaps remain in the newly changed H7 and delta-lifecycle text.

## R1 findings

- **M1 — resolved.** H7 is now synchronous at Core's in-attempt position: after H6 (and the connect-side BLS commit), before `finish_attempt`, the coins/Evo commits, and tip publication. The effect-target read restriction and the separate VerifyDB effect path are stated.

- **M2 — resolved.** D7 puts `meta.evo_tip`, Evo rows, and `head_blocks` in B1. `meta.tip` is in B1 only when all coins fit; otherwise it is in Bk. The B1-only marker invariant and coins-only recovery are specified and tested.

- **M3 — resolved.** D20 chooses the feasible pre-coin B1 log position, retains Core's log text, and documents the timing difference from Core. The PLAN/CH2 wording is updated in the errata.

- **M4 — resolved.** `MinedErase` carries the height, rotation index, primary key, and inverse key. H9 builds complete tombstones before the layer is committed, and the rotated disconnect/reconnect plus historical-read test is specified.

- **M5 — resolved.** The guard now decodes first and bypasses its readiness decision on any payload decode failure; the normal path uses raw transaction types and only the two necessary decoded distinctions (QC nullness and a CbTx's valid ChainLock signature), preserving the null-commitment regtest case. The mixed-block exception is sound: if Core's earlier, well-formed MNHF/AU operation needs quorum members, the node cannot reach the later malformed payload, so a readiness `SystemError` is fail-closed and is not a block verdict. D2 documents and tests that Core-order-dependent case. The D3 sentence saying `SystemError` is only possible for fully decoded blocks should be narrowed (see the nit below), but the M5 behavior is resolved.

- **m1 — resolved.** `begin_attempt` now creates a direction-specific delta; H9 appends disconnect tombstones, H6 stages `evo_tip = pprev`, and only committed outcomes push the delta. Failed, check-only, readiness/SystemError, and Fatal paths drop it, while VerifyDB stacks it in its scratch transaction.

## New findings

### Major — G1q ignores the assumevalid CbTx gate

**Location:** §3.2 D3's CbTx arm and the `-assumevalid` test row in §7.2.

The arm refuses a version-3-or-later CbTx with a valid `bestCLSignature`, but G1q is not conditioned on `check_cbtx_merkle_roots`. Core passes `fScriptChecks` as this flag and, when assumevalid disables it, skips the CbTx presence/version, roots, and `CheckCbTxBestChainlock` path (`src/evo/specialtxman.cpp:899-917, 1008-1053`; CH3 §2.8). A block with no other member-dependent operation can therefore be refused as “quorum validation (CH7b)” even though Core would skip this CbTx check and continue.

**Fix:** pass `check_cbtx_merkle_roots` into G1q and suppress the CbTx-derived readiness arm when it is false. Add an assumevalid-on test with a valid v3+ CbTx ChainLock and no other quorum-dependent operation, asserting that G1q does not refuse it.

### Minor — H7 does not state the inline callback/re-entry contract

**Location:** D15 and the changed `NotifyMnListChanged` effect/applier text in §3.8.

H7 now runs on V while the attempt holds the exclusive facade gate. D15 names AM1, DK1, QM1a, and SB19 as subscribers, but does not say which are Core-equivalent synchronous listeners and which consume the asynchronous `ChainEvent`, nor does it restate CH1 LO-5/LO-9's prohibition on dispatching to V, taking the gate, or awaiting a V-dependent reply from an inline listener. SB19/QM1a's normal `OnV` path would otherwise re-enter V (or deadlock in a release build).

**Fix:** state that only the Core-equivalent CMNAuth/governance listeners run synchronously; AM1, DK1, QM1a, and SB19 consume the post-commit async event. For every synchronous H7/VerifyDB listener, forbid `OnV` dispatch, gate acquisition, and waits for V/actor replies; pass all required facts as callback arguments.

### Minor — the attempt delta has no specified handoff to `commit_tx`

**Location:** §2.1 `NodeEvoAttempt.delta` and D1.

Rev2 says the delta lives in the attempt, but the existing connector consumes that attempt in `finish_attempt` before calling `commit_tx`, whose `EvoHooks` signature receives only `&mut EvoTarget` (`rs:connect/connector.rs:320-342`; `rs:connect/evo.rs:294`). The design does not specify where the delta (or the analogous post-BLS scheme) is held between those calls, or how a path that never opened `begin_tx` clears it. An implementation can accidentally carry a failed attempt's delta into the next attempt.

**Fix:** define an explicit pending transaction slot owned by `NodeEvo`/the target: `begin_tx` opens it, H4/H9/H6 fill it, `commit_tx` moves it into the active layer (or scratch `EvoTxn`), and `rollback_tx` clears it. Assert it is empty at `begin_attempt`, including paths where `begin_tx` did not run.

### Nit — D3's decode-status wording is contradictory

D3 says `SystemError` is only for fully decoded blocks, while D2 and the §7.2 test intentionally allow a mixed block with an earlier quorum-dependent operation and a later malformed payload to return the stub's `SystemError`. Change the sentence to say that **G1q itself** refuses only after all payloads decode; H4 may still return a readiness refusal before a later decode failure when Core's order reaches a member-dependent operation first.
