# E0-04 narrow closure check — round 3

Design/model: **692fd20**; E0-08 contract: **369f8f0**. Read rev2/amendment first, both r2 reviews, and DEC-57/65/67. Locations below refer to `docs/design/E0-04-grants-leases.md` unless prefixed **m4**. This is a specification review, not implementation validation.

## Prior GPT r2 majors

| R2 item | Disposition | Evidence |
|---|---|---|
| 8 — joined-lock epochs | CLOSED | §8.1, §8.3, H14: each request has its own gate; unlock waits for pending gates; only drain work is shared. Model `joined_lock` covers K1 → unlock → grant → K2. |
| 9 — refunds undo replacement cap | CLOSED | §4.2 authority generations and §4.3 rebind: charges/refunds stay in their generation; new ceiling is `min(available_g, fresh_cap)`; absent purposes get zero. Model `rebind_ceiling`. |
| 10 — Mode B false Cancelled | CLOSED | §2a.3: monotone released-signature count, zero signatures AND no earlier step marker required for Cancelled; no call-name exemption; retries that already signed remain MaybeSent and park. Part 5 step-2 classifier. |
| 11 — Mode B copy overpromises | PARTIAL | §4.6 and §16.10 C2 now use DEC-67 verbatim with running-call precedence; C3 requires no running call and a further signature. But C5 still selects auto-lock copy by commitment alone; see F2. |
| 12 — power-loss duplicate funding | CLOSED within intact FULL-journal assumptions | §2a.3/§2a.5: durable flow/step funding marker precedes library entry; orphan with no row remains MaybeSent indefinitely; replacement funding is blocked. Part 5 funding trace. |
| 13 — Repair misclassifies negative observations | CLOSED | §6.5 and §2a.5 replace negative-evidence discard with a ChainLocked conflicting self-spend. Absence alone stays MaybeSent. Model `repair_resolution`. |
| 14 — Q22 | CLOSED | §4.3, §4.8, §16.3 and Q22 require fresh authentication when the setting is on; silent rebind requires it off. DEC-67 applied; Q21 admits biometrics with the combined cap (§3.7). |

The GPT r2 file contains no separately raised minor findings. Its previously closed r1 items are outside this narrow re-review; the PARTIAL annotations concerning refunds, Mode B classification and orphan recovery are resolved by items 9, 10 and 12 above.

## Amendment extensions

- **Asset-lock None exception: sound in E0-04's stated durability domain.** §16.6/H11 require no entry, no marker and no tracked row of ANY status, including Consumed, before the engine funding gate treats absence as never funded. Hosts treat every None as unknown. A live Unsent and Committing entry answer MaybeSent, so concurrent builds cannot use the exception. This must not be replaced with m4's broader rule (F1).
- **Process-lifetime NotSent tombstone: sound.** §5.5 replaces a transition tombstone atomically in the later First's J step before transport; attempts/possibly-out provenance prevent definite settlement while an earlier attempt can send. Restart loses the tombstone and answers unknown None, reducing retry availability safely. Model `rowless_tombstone`.
- **H16 restart narrowing: sound.** §2a.5/H16 retain resumable markers; other transitions lose identity/nonce records and never resolve to NotSent after restart merely from absence. In-process nonce consumption by a different transition is positive exclusion evidence. Model `restart_retry`.

I found no new unsafe NotSent path in these E0-04 rules. Journal erasure/rollback is not equivalent to an intact durable journal; the pinned contract nevertheless exposes a broader absence rule to hosts (F1).

## Remaining findings

**F1 — MAJOR: the pinned E0-08 contract still contradicts the amendment's safety boundary and funds flag.**

- E0-04 §16.6 (:2352–2389) returns None for absent asset-lock evidence, interprets it only in engine funding gates, and checks tracked rows of every status. **m4 :1205–1222, :1467–1472** instead promises `Some(NotSent)` for missing asset-lock/funding entries, exposing absence as a host retry signal without the row check. Counterexample: a tracked Consumed/unknown-provenance lock survives journal loss; m4's missing-entry rule says NotSent, enabling replacement funding although the old lock executed/can execute. E0-04's row check prevents it. Align the contract with §16.6 rather than changing the fence to match m4.
- E0-04 §16.5 (:2337–2348) explicitly makes a live Unsent funding uncommitted and Committing committed. **m4 :1222, :1298–1303** makes any registered funding whose status is not NotSent committed. A live Unsent answers MaybeSent in both documents: false in E0-04, true in m4. This changes lock copy and discard semantics. Copy the actual state predicate, not a reference accompanied by contradictory prose.
- `Option<DispatchState>` and its four variants agree; §16.4's LeaseError/code mappings and all five RevokeCause variants agree with **m4 :580–587, :1224–1234**. But E0-04 §16.1 (:2286–2291) includes Discovery, whereas **m4 :558–570, :1452** excludes it. Resolve the closed-list mismatch; this part is an API nit, not the safety concern above.

**F2 — MAJOR residual of r2 item 11: auto-lock copy still falsely promises a required unlock.**

E0-04 §4.8 (:953–954) and §16.10 C5 (:2425) say “… unlock to finish” whenever something committed, without C2's running-call or C3's further-signature guard. Mode B funded step 2 releases S1, auto-lock runs before submission, then S1 is submitted and registration completes while locked. C5 still promises an unlock is required, exactly the r2 counterexample's misleading assurance. §4.6's general C2/C4 rules do not reconcile the explicit auto-lock instruction to show C5. Use DEC-67's promise for a still-running call, then choose terminal/parked copy by observed outcome. A focused extension of `step2_findings` that preserves rev2 classification and substitutes only funded C5's copy reproduces `sign S1 → Lock → submit S1 → Platform executes S1`; the supplied model checks manual/progress copy, not C5.

## Validation

Review written first, before executing the model or optional probes, then updated with results. `TMPDIR=/dev/shm/e0-04-r3-gpt-tmp python3 docs/design/checks/e0_04_design_model.py`: **exit 0, 77 PASS, 0 FAIL**. Focused closure checks also pass; wrong rules reproduce their findings. The auto-lock model extension reproduces F2 without changing rev2 classification. Logs: `model.log` and `closure_probe.log`, with probe source `closure_probe.py`, in the scratch directory. These finite specification checks do not establish implementation correctness or cover C5 in the supplied 77 checks. Both reviewed worktrees remained clean. Scratch: `/work/scratch/e0-04-r3-gpt/`; TMPDIR `/dev/shm/e0-04-r3-gpt-tmp` (0700). No design/contract/code edits, commits, pushes or posts.

**Verdict: REWORK.**
