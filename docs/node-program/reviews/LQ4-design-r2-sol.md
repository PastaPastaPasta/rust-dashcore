# LQ4 design review, round 2 (Sol)

**Target:** `designs/LQ4-dkg.md`, rev1 (835 lines), against Dash Core `v24.0.0-rc.4`.

**Verdict: APPROVE-WITH-NITS.** The three round-1 majors are closed. The DKG state-machine, M2 batch coverage, M3 acceptance split, and the X1/X2 claims agree with rc.4. I found three planning follow-ups to align with DEC-619 and one wording nit; none changes the design's technical shape.

## Findings

### m1. DEC-619 Q11 is still left open in the document

**Design:** §5 P-4 row, §6 Q11, and §7 (lines 761, 797, 833). **Core:** `CFinalCommitment::Verify` reads members at `src/llmq/commitment.cpp:177`, then calls `VerifySignatureAsync` at `:200-203`, whose second read is at `:53` (and its parentless-base guard is `:48-51`). DEC-619 already rules that P-4 is BL-475, implemented by the LQ4 implementer as a pre-slice before LQ4b-1 (with the SB8/LQ2 owners finished). The design still says the owner is undecided and asks Q11. Replace that open question and the owner placeholder with the DEC-619 assignment and pre-slice ordering.

### m2. The Q12 merge gate is not stated in the acceptance text

**Design:** §4.3 T3-C/LQ4b-1 (lines 672-677), §6 Q12 (line 798). **Core:** the own pcommit is enqueued at `src/llmq/net_dkg.cpp:357-365` and processed by the batch pipeline at `:392-445`; valid pcommits are the only ones served by `src/llmq/dkgsession.cpp:678-685`. The document correctly permits the rc.3 commit-omit/commit-lie ambiguity, but DEC-619 says LQ4b-1 may start on that equivalence and may merge only after the labelled rc.4 rerun agrees. State that explicit merge gate in the slice acceptance and close Q12 rather than leaving it as an open question.

### m3. Q13's optional capture spike remains presented as an undecided choice

**Design:** §4.1 capture list, §4.3 T6 family B, §6 Q13 (lines 609-610, 710-714, 799). **Core:** the stock functional framework caps tests at 20 nodes (`test/functional/test_framework/util.py:324`, enforced by `test_framework.py:951` and `util.py:362`). DEC-619 rules “no 33+ MN capture spike”; family B is the required carrier for M2. The required 33/40/65 injected-member family and batch positions 1–8 are sufficient, but the document should state the spike is skipped and remove the unresolved funding question/optional-capture wording.

### n1. Clarify the database-error sentence

**Design:** §3.2 `DkgStore` comment (lines 449-459). Core writes are direct dbwrapper calls (`src/llmq/dkgsessionmgr.cpp:125-143`), while `GetVerifiedContributions` explicitly returns false when a vvec row is absent (`:163-169`) and ignores the boolean result of the sk read (`:180-183`). “Fail-stops on any database error, as Core's dbwrapper throws” should distinguish backend exceptions (fail-stop policy) from an expected missing-row result, which remains `None`/invalid-key behavior in the trait.

## Round-1 finding disposition

- **M1 — closed.** `verified_contributions` no longer accepts a caller-supplied members slice; §3.4.1 rows 1–6 require the facade access at each Core point. The Core source has the per-call access at `src/llmq/dkgsessionmgr.cpp:145-148`, and the relay-member access at `src/llmq/utils.cpp:778-781`.
- **M2 — closed.** Required T6 family B uses injected 33, 40, and 65 members, puts the 32nd pending contribution at batch positions 1–8, and tests the mid-batch write/remaining-remainder behavior. Core triggers at `src/active/dkgsession.cpp:136-147,160-182` and from `src/llmq/dkgsession.cpp:257-263`.
- **M3 — closed.** T3-C adopts deterministic qcontrib inputs, replays justify-lie keys through typed RNG, and treats commit-lie as equivalence because the liar's own pcommit is rejected before relay. Core's rejection/relay boundary is `src/llmq/dkgsession.cpp:579-615` and `src/llmq/dkgsession.cpp:678-685`; the own-message path is `src/llmq/net_dkg.cpp:357-365,392-445`.
- **m1 — closed.** §2.1 now distinguishes rotated and non-rotated mining windows; rc.4 parameters show the rotated offset at `src/llmq/params.h:231-234,388-394`.
- **m2 — closed.** `active_pro_tx`, member-derived initialization, member-only relay topology, and the non-member replay are specified; Core sets membership at `src/llmq/dkgsession.cpp:106-116` and initializes relay members only at `:135-146`.
- **m3 — closed.** §4.4 separates receive-time key/hash failures, batch-signature failures, PreVerify bans, and the valid-point forced variants; Core checks these at `src/llmq/dkgsession.cpp:512-552,579-611` and scores batch failures at `src/llmq/net_dkg.cpp:425-437`.
- **m4 — closed.** §7 supplies revised LQ4a/LQ4b/DK1 cells and adds BS11c and O2a-DKG, with P-1/P-2 dependencies.
- **m5 — closed.** §5 and §7 make DK1 depend on LQ4b-1, allowing LQ4b-2 to run beside it.
- **m6 — closed.** §3.3 pins per-connection sender identity, score multiplicity, whole-batch PreVerify, and session-hash set semantics, with T1 rows; these match `src/llmq/net_dkg.cpp:400-445` and `src/llmq/dkgsession.cpp:720-755`.
- **m7 — closed.** `DkgRng` is typed and §2.9 gives the draw order. The simulated-error gate is `src/llmq/dkgsession.cpp:56-63` and `GetRandBool` is `src/random.cpp:653-661`; commit-lie draws are `src/active/dkgsession.cpp:531-540`.
- **m8 — closed.** The 12/7 capture and effective override values are explicit; the override assignment is in Core's test chainparams at `src/chainparams.cpp:1019-1027`.
- **m9 — closed.** `ConnSnapshot` filters fully connected nodes and the capture recipe logs fold inputs; Core's fold consumes `ForEachNode` facts at `src/active/dkgsession.cpp:245-257`.
- **m10 — closed.** §3.3 gives the phase windows and §3.4 specifies nulling the current session before Init; the windows are `src/llmq/dkgsessionmgr.cpp:57-123`, and the handler reset is `src/active/dkgsessionhandler.cpp:94-105`.
- **m11 — closed.** The flag-straddle T4/T6 cases and per-step `env.bls` are present; Core separates the session flag at `src/active/dkgsession.cpp:34` from the process-wide decode/hash scheme.
- **n1 — closed.** Start functions are single-shot and phase-ordered (§3.3), with the unreachable “clear earlier bad flag” case removed.
- **n2 — closed.** The 70240, 70241, and 70242 rows are present; the v24 threshold is `src/version.h:23`.
- **n3 — closed.** T4 says `llmq_devnet` with five dropouts rather than a nonexistent shipped seven-member type.
- **n4 — closed.** `DkgEvent` excludes cxxtimer fields.
- **n5 — closed.** `use_legacy_bls` is derived from `v19_after_base`.
- **n6 — closed.** §3.2 explains why one call's rate snapshot equals Core's per-decision reads.
- **n7 — closed.** `phase_for_tip` accepts both the unvalidated-snapshot and rotation-at-tip gates; the gates are `src/active/dkgsessionhandler.cpp:45-51`.
- **n8 — closed.** §7 updates DK1's v24 scope and removes the stale “both pins” wording.
- **n9 — closed.** The store contract states DK1's fail-stop policy.
- **n10 — closed.** The invalid-key signing aside was removed; Core checks `skShare.IsValid()` at `src/active/dkgsession.cpp:518-522` before signing.

## X1/X2 and slicing checks

X1 is correct: Core's first members read is at `src/llmq/commitment.cpp:177`; `Verify(checkSigs=true)` calls the signature verifier at `:200-203`, and that verifier reads members again at `:53` after its own parentless-base check (`:48-51`). The §3.4.1 access table and P-4 placement preserve both observable accesses.

X2 is correct for a commit-lie start: `SendCommitment` has just built the quorum vvec before constructing the own pcommit (`src/active/dkgsession.cpp:501-510,531-552`), so the liar's loopback reaches the mismatch/signature checks in `src/llmq/dkgsession.cpp:579-611`; the own sender is `-1` (`src/llmq/net_dkg.cpp:357-365`), and only `validCommitments` are served (`src/llmq/dkgsession.cpp:678-685`).

The slicing holds: P-1 and P-4 gate LQ4b-1, P-2 gates LQ4b-2, LQ4a-1 precedes LQ4a-2, and §7's PLAN replacement text reflects the acceptance split and DK1's LQ4b-1 dependency. Apply the three DEC-619 planning edits above before approval is recorded.
