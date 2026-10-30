REWORK

Reviewed `dw/e0-04-design` at **d2a21c6136a5f28562513b41be8c46fd8c470874**. Read Changes in rev1, the entire design and model, both r1 reviews, REVIEW-COMMON, and DEC-65/DEC-67. Locations below are in `docs/design/E0-04-grants-leases.md` unless otherwise stated. `PW` and `SDK` refer to platform **bc41f1bc233dec4607d387101c1d9c2f111019b2**; relevant source blobs were extracted from that exact commit and compared with the r1 extracts.

The supplied model exits **0**, with **67 PASS, 0 FAIL**, and catches all **29 listed wrong rules**. I separately checked the closure replays and their safe rev1 counterparts, and the **exact sequential escaping mutation** from r1: First → Sent → identical Resend → Rejected. With the mutation, the actual rev0 oracle misses `NotSent` despite an immutable send; rev1 reports `reported not sent, but sent or still sendable`. This closes r1's minor 8 for that mutation. The model's narrower coverage description is appropriate.

Evidence: `/work/scratch/dw-e004-r2-gpt/model.log`, `closure_and_attacks.py`, and `closure_and_attacks.log`. The latter distinguishes replays using the supplied models from minimal extensions of the written rules. These are specification probes, not tests of an implemented E0-04 fence.

1. **R1 major 1 — CLOSED: concurrent row-less attempt settlement and the Core tombstone.**

   **Locations:** `:967–991`, L6 at `:1009`; model `rowless_rejected`, `rowless-concurrent`.

   Re-executed the original interleaving with the actual rev0 model blob: O First → R identical Resend → O definite rejection → Lock → O refused/released → R sends. It still violates safety. The rev1 `rowless-rev0` replay also produces violations with the improved oracle. Under rev1, O's rejection while R runs yields `MaybeSent`; O's later identical call is a Resend, no inputs are released, and R's send remains possible. When all attempts reject, the Core artifact becomes a tombstone before its one cleanup; later copies are refused. The corrected counterpart passes. I found no escape from this aggregate rule within its stated process-local scope. Durable resumable-step provenance is addressed separately below.

2. **R1 major 2 — PARTIAL: the original delayed-gate creation window closes, but joined Locks can reuse a stale completed gate.**

   **Locations:** `:454–466`, H8 at `:1037`, `:1329–1335`, `:1384–1386`.

   `gate_window("rev0")` admits the old pre-Lock grant after return; `gate_window("rev1")` does not. A creator starting after the freeze now waits for both the gate and drain, then cannot redeem the old-epoch grant. Insert also rechecks barrier/generation, and the detached coordinator handles a dropped caller. That reproduces and fixes the original trace. **Finding 8** is a different interleaving of the new barrier with Unlock and a second Lock; closure cannot extend to the claimed double-Lock behavior yet.

3. **R1 major 3 — CLOSED for the write-ahead requirement: a resumable step no longer depends on a later flow-phase write.**

   **Locations:** `:892–899`, `:1112–1126`, `:1276–1298`, H10 at `:1039`.

   Under `no-step-marker`, send → crash before phase persistence → new process's revoked lease → refusal still produces false Cancelled. In rev1, the `FULL` marker must return before transport. The corresponding crash retains the marker; identical bytes are Resend, while different bytes refused for that same step carry `step_possibly_dispatched` and report MaybeSent. The safe counterpart and both resumable scenarios pass. Ambiguous writes block transport until durability is established. This fixes the original prerequisite in Mode A; Mode B explicitly requires the marker before its library call. Mode B's later `Locked` outcome shortcut in **finding 10** must still respect that provenance.

4. **R1 major 4 — CLOSED in Mode A; PARTIAL by deliberate, accepted weakening in Mode B.**

   **Locations:** `:569–597`, L17 at `:1020`; model Part 4.

   The original IS proof → Platform rejection → hidden CL wait → expired signer trace produces both old-authority use and a failed funded registration under the rev0 rule. Mode A now requires `FallbackPolicy::Surface` at every unbounded fallback, including step 2's post-submission rejection, and parks/drops the hold before a continuation with a new lease. Calling step 2 only with a stored proof also addresses its initial resolution wait. Part 4 has no Mode A finding. Mode B deliberately retains the hidden wait and signing before `key_until`; its timer plus Parked mapping is a bounded key-retention residual, not the Mode A guarantee. Part 4 exposes that residual correctly, but does not check the conflicting cancellation classification in finding 10.

5. **R1 major 5 — CLOSED in Mode A; PARTIAL in Mode B, where recovery is conditional on eventual chain appearance.**

   **Locations:** payload `:842–845`, H6 at `:1035`, `:1181–1187`, `:1240–1245`.

   Under `no-payload`, `Dispatching` survives power loss while the NORMAL wallet row disappears; the improved oracle flags selectable inputs. In rev1 Mode A, the same loss restores the row from the journal payload, installs its pending-spend fence before builds open, and resumes the exact bytes. The safe counterpart passes with both reservation/fence state held and a recovery send. This preserves the accepted NORMAL wallet database and repairs the narrow dispatch-critical durability gap. Mode B explicitly lacks this repair. DP1-05 finds a finalized lock only **once it is on chain**, with no finite bound while a peer retains it. **Finding 12** addresses the resulting replacement-funding path rather than demanding Mode A's upstream PR for Mode B.

6. **R1 major 6 — PARTIAL: the immediate rebind minimum is correct, but refunds can undo it.**

   **Locations:** refund rules `:500–505`, rebind `:550–557`, settlement `:976–981`; model `rebind_cap`.

   The original zero-spent lease capped at 1,000, rebound with a fresh 1-credit grant, admits a new 100-credit transition under rev0 and refuses it under rev1. The model confirms that direct fix. It does not include a later refund of an older artifact, or charge-generation accounting. **Finding 9** shows why reducing only the current remaining balance is insufficient.

7. **R1 major 7 — CLOSED for Mode A's original history trace; PARTIAL for Mode B's definite-outcome rules.**

   **Locations:** `:1388–1415`, `:287–293`; model `lock_outcome`.

   Funding First → durable Dispatching → Sent → empty permit snapshot → Lock still gives false Cancelled under `snapshot-outcome`. Rev1 reports Sent from history, including the multi-artifact example whose identity transition remains uncommitted. Committed-but-rejected registered funding reports WillBeSent, retaining its charge and inputs. Both the wrong-rule replay and safe counterpart reproduce the intended distinction. The separate Mode B rule that calls a later signer `Locked` Cancelled is not history-safe for the pinned step-2 retry implementation (**finding 10**).

8. **MAJOR — a second Lock can return with the vault unlocked and leave grants issued before that second Lock usable.**

   **Locations:** `:1312–1315`, `:1334–1335`, `:1372–1386`; model `gate_window`/`run_lock` omit Unlock between joined Locks.

   **Interleaving:** K1 freezes while a pre-K1 permit will stall until H. K1's vault gate completes promptly, ending epoch E and locking the vault; the barrier remains for the drain. Unlock now runs, as §8.3 explicitly permits after the gate, reopening the vault in E+2. Issue grant G, but insert no lease while the barrier is set. K2 is called during that drain. Its freeze sees no new lease and joins K1's coordinator and already-completed gate. The drain then ends and both async Locks return. No vault operation occurred after K2's call: the vault is still Unlocked and G is still redeemable. A creator starting after barrier clearance reads the latest generation, redeems G, inserts an Active lease, signs, and makes a First **after K2 returned**, with authority that existed before K2. In Mode B it can likewise start a fresh permitted library call. This obeys H8; the barrier's failure to admit leases does not prevent the vault from being unlocked or issuing new grants.

   The existing vault gate correctly orders actual lock/unlock operations; it cannot supply the missing second lock operation. I verified the pinned desktop `Vault::lock`/`unlock` behavior at `dw-vault/src/vault.rs:833–862`. The original single-Lock gate-window test therefore passes while this composition fails.

   **Fix:** coalesce drain work without coalescing away a lock request's required epoch revocation. A joined request needs a vault-lock completion ordered after that request if the vault has changed epoch since the coordinator's gate. Alternatively serialize Unlock/grant issuance against the entire lock barrier with an explicit request-order contract. Add K1 gate → Unlock → grant → K2 → drain tests, including the synchronous FFI and dropped callers. Also state how an Unlock queued before the blocking lock worker reaches the gate is ordered; merely taking the same exclusive gate does not establish that the lock worker ran first.

9. **MAJOR — refunding a pre-rebind artifact can restore new signing above the replacement grant's cap.**

   **Locations:** `:500–505`, `:551–556`, `:977`; model `rebind_cap` at `e0_04_design_model.py:1227` has no refund transition.

   **Interleaving:** L originally has a 1,000-credit cap. Artifact S consumes 100 and its attempt remains unresolved, leaving 900. An epoch change moves L to NeedsGrant; rebind with a fresh 1-credit grant sets remaining to `min(900,1)=1`, while the old charge and attempt stand. S's final attempt now proves definitely unsent. The specified refund restores its 100-credit charge to the lease, making remaining 101. A newly signed 100-credit S2 passes the ordinary budget check, exceeding the fresh grant by 99. The same accounting composition exists for an abandoned funding registration or a prepared Spend charged before rebinding. This is an underspecified arithmetic rule in the design, not a claim against an existing rebind implementation.

   **Fix:** retain a persistent authority ceiling and charge provenance across rebinds. Refunding an older charge may restore old accounting, but must not replenish new signing authority beyond the fresh cap or revive a dropped purpose. Define the result with outstanding charges, refunds, multiple rebinds and mixed purposes, using checked arithmetic. Add this trace to `rebind_cap` and the lease tests; a one-time minimum followed by an ordinary refund is insufficient.

10. **MAJOR — Mode B incorrectly lists registration/top-up step 2 as a sole-artifact call for `Locked → Cancelled`.**

    **Locations:** `:289–291`, acceptance `:330–331`, `:595–596`; PW `wallet/identity/network/registration.rs:227–261`, `:469–501`; PW `wallet/asset_lock/orchestration.rs:345–405`.

    **Interleaving:** step 2 already has a ChainLock proof and signs/submits S1. A lagging Platform endpoint reports CL height too low. `submit_with_cl_height_retry` sleeps, bumps `user_fee_increase`, and invokes the signing/submission closure again. Lock runs and returns during that backoff. The new S2 signature now fails `Locked`; the enclosing call returns the signer failure even though S1 was already handed off. §2a.3's listed exception calls this Cancelled. An earlier attempt can remain queued elsewhere; the pinned helper explicitly warns that an in-flight submission can execute after cancellation. A final signing failure is not evidence that the call never handed off anything. The IS→CL fallback creates another instance of multiple signatures in this same call.

    This contradicts §4.4's Parked rule and the monotone marker/history rules. Part 4 models the fallback's Parked mapping, but not the CL-height retry or the Mode B call-outcome classifier, so its green result cannot settle the contradiction.

    **Fix:** remove these calls from the sole-artifact exception. Once a call may have produced an earlier signed/submitted artifact, retain MaybeSent/committed provenance and Parked flow state; do not refund, emit NotSent, or offer replacement funding based on the final `Locked`. If definite cancellation is required, use a host-visible monotone signing/attempt record that proves the failure preceded every successful signature. Test Locked before the first signature, between CL-height attempts, and after an IS submission, with previous step markers present.

11. **MAJOR — the funded-flow and facade copy promise stronger cancellation than Mode B provides.**

    **Locations:** `:681–690`, `:1999–2000`; DASHPAY `:455–457`; DEC-67's Mode B wording.

    **Interleaving:** funding T has already been committed and its proof obtained. Mode B step 2 signs S, then stalls before its broadcast. The UI displays **"Funds locked — finishing. Lock stops it here; you'll finish after you unlock"**. Lock ends the epoch and returns after the call permit expires. The already-signed S then goes out and completes registration while the vault remains locked, with no subsequent unlock. That is allowed by §2a.3 but disproves the displayed promise. The mode-aware exception at `:681–683` applies only before funds are committed; it does not qualify this funded-flow copy. §16's contract copy also omits the running-call qualification for its pre-funding "Lock to cancel" text.

    **Fix:** specify mode-aware copy for every running library call, including funded step 2 and the facade/DASHPAY text: Lock stops new signatures; an already-signed send may still complete. Say "Unlock to finish" only once the engine knows a further signature is required. Do not label an observed late success "Sent before the lock" without evidence of that timing. Test the signed-before-broadcast barrier through the UI in Mode B, not only its pre-funding progress label.

12. **MAJOR — Mode B's absent-artifact discard path can bypass the no-double-funding rule after power loss.**

    **Locations:** `:96–97`, `:307–310`, `:1244–1245`, `:1278–1285`, `:1986–1991`; DASHPAY restore rules `:984–996`.

    **Interleaving:** Mode B's funding call builds/sends T1; a peer retains it without revealing it to the wallet or chain. Power loss loses its NORMAL wallet row and the host's not-yet-recorded outpoint/phase. This is the expressly documented orphan case. The journal has no registered funding payload; the specified resumable marker example is the identity step, which has not started. On restart, full SPV sync and DP1-05 discovery find neither an identity nor an on-chain asset lock. The draft therefore still has **no asset-lock artifact**. §16 permits `discard_registration` when it "has none". Discard and start a replacement registration/top-up, funding T2 from a different UTXO set. The peer later releases T1. **Both independently funded transactions can confirm**; no double-spend rejection prevents this trace.

    Waiting for DP1-05 to finish is not proof T1 did not send. Reconstruction is conditional on T1 reaching the chain, and neither it nor the hidden pending transaction has a finite recovery bound. Excluding tracked rows' inputs also does nothing when the row is missing. This does not object to DEC-67's accepted weaker lock promise; it objects to the remaining claim that the UI prevents paying twice.

    **Fix:** in Mode B durably mark a funding attempt before entering its library call, keyed to the stable flow/step independently of an artifact id, and retain that unresolved history across restart. "No artifact id" permits discard/refunding only if no funding attempt could have started. Use the existing host marker mechanism to block replacement funding until positive resolution; adopt DP1-05's result if it eventually appears. State explicitly that unresolved orphan recovery can be indefinite. Add the withheld-T1/disjoint-input-T2 power-loss trace to Mode B acceptance. No change to the accepted NORMAL wallet database or upstream-PR decision is required for that UI gate.

13. **MAJOR — Repair manufactures a definitely-unsent record from negative chain/mempool observations.**

    **Locations:** `:1205–1210`, `:656`, H11 at `:1040`; PW `wallet/core/generation.rs:107–139`.

    **Interleaving:** a tracked row of unknown provenance has no journal entry, for example after restoring a wallet backup. A peer still retains its valid signed T1. Repair sees its inputs unspent and T1 absent from the local chain/mempool, satisfying the stated Discard condition. It writes Unsent with a dead origin and runs the ordinary Revoked cleanup, thereby returning DefinitelyUnsent and permitting a replacement payment/asset lock. The peer can still release T1. A replacement using the same inputs races it; a replacement funding lock using disjoint inputs can confirm alongside it. This affects Mode A's explicit unknown-row repair path despite the main journal state machine being monotone.

    Negative network observations do not prove signed bytes cannot later leave or execute. The exact pin's pending-spend rationale expressly uses this withheld-peer scenario to reject such reasoning. The design initially classifies this row Unknown and correctly keeps it; Repair has no new evidence that justifies converting it to Unsent.

    **Fix:** preserve Unknown and prohibit the definitely-unsent cleanup/replacement path unless local durable provenance proves no possible dispatch, or positive evidence proves the old transaction can no longer execute. Keep explicit "Send it" as a user-authorized dispatch of the existing bytes. If a force-forget tool is desired, define its weaker semantics separately; it must not produce DefinitelyUnsent, Cancelled, or the ordinary safe-to-retry status. Add a withheld-peer Repair test.

14. **MAJOR — Q22 still implements the recommendation DEC-67 rejected.**

    **Locations:** `:558–563`, `:740`, Q22 at `:1925`; DECISIONS.md DEC-67.

    **Interleaving:** with **"require authentication for every payment" enabled**, the user authenticates an own-key flow, then unlocks mid-flow. The epoch change puts its lease in NeedsGrant. §4.3 silently issues an internal `Credential::None` grant from the Full-scope vault and rebinds, explicitly stating that this setting does not prompt again. The next payment/Platform signature proceeds without the required second confirmation. Q22 remains "Yes" and §4.8 says the second prompt is gone. DEC-67 explicitly rejects that policy.

    **Fix:** make silent rebind conditional on the setting being **off**. With it on, use `RegistrationWait::Authorize` and require fresh authentication before restoring signing, still within the original remaining cap. Update the normative lifecycle, UX text, Q22's disposition and acceptance tests together. Q5's existing "host setting allows Credential::None" caveat is consistent with the decision but does not repair §4.3's explicit opposite rule. Q21's biometric PlatformOp choice is reflected in §3.7; the stale contrary statements at `:361` should also be aligned with that accepted section.

**Summary.** The original seven counterexamples now reproduce as violations under their rev0 rules, and their focused rev1 counterparts pass. The Core tombstone, write-ahead identity markers, surfaced Mode A fallback, durable recovery payload and history-based Mode A outcomes materially improve the design. Overall closure remains partial because joined-lock epoch changes, rebind refunds, Mode B outcomes/copy, replacement-funding gates and DEC-67's Q22 policy need correction.

Mode A **does allow sends after Lock returns** when their commit preceded the call: registered resends, a transport that becomes observable after its timeout, and pre-admitted work after the synchronous FFI return (which waits for the vault gate, not the drain). I replayed the supplied late-send trace successfully; that satisfies the precise §9.2 theorem and is not a new bypass. The new second-Lock trace preserves pre-second-Lock grants and allows new signing/First admission, which is a separate failure. Mode B's headline theorem is precise, but its UI is not consistently that weak. Its `key_until` bounds own-key retention (grant + 300 s + A, subject to timer scheduling/in-progress gated operations); it does not bound the library call or a peer-retained transaction. DP1-05 supplies eventual chain reconstruction, not a bound or proof of no earlier payment.

No branch edits, commits, pushes or posts were made. The reviewed worktree is clean at the requested SHA. Tests used `/dev/shm/dw-e004-r2-gpt-tmp` with mode 0700; it was removed after the checks. **Verdict: REWORK.**
