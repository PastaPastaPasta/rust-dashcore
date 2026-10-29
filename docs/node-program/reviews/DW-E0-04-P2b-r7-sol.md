# DW E0-04.P2b — Sol review, round 7 (narrow)

Reviewed detached head `f062d0910384477f3965d8b460c80f10432c5fce`, range `eb652ce..f062d09` (`fbd646e`, `f062d09`) in `/work/scratch/p2b-r7-sol/wt`. The platform and patched rust-dashcore pins remain `ebe37f8a679b1a552483dc7ff12c306291e9a32b` and `d3d50520a26f465a43851362fdcb12e0776ade14`. The implementer's worktree and branches were untouched. The review worktree is restored and clean; no commit, push, or GitHub action was made.

The release-row protocol is a sound authority for the **absence-based `NotSent` reading** under the inspected P2b producer and DP1-02 caller contract. It is deliberately conservative: a released signature without a recorded lock remains `MaybeSent`, including after reload. This meets the no-automatic-double-funding goal for the same step. It does not establish the older, literal PA-2 claim that every broadcast lock has its own durable `signed_lock` row. That distinction must be carried into DEC-191's release wording; this range updates the design's schema gate to 7, while the separate decision still says 6.

## Findings

- **R6-01 — Closed for the `NotSent` authority; preventive PA-2 remains outside this pin.** possible security note: see manager.
- **R6-02 — Partial for strict O-1; the current one-build producer remains covered.** possible security note: see manager.
- **O-2 — Partial.** possible security note: see manager.
- **S6-01 — Closed as a coverage request.** The new CoinJoin funding and mixed-sweep selection tests pass and demonstrate that the unresolved input is excluded from fresh selection, then Repair succeeds.
- **Minor documentation correction:** `docs/design/E0-04-grants-leases.md:942` still says a new flagged marker starts at schema ≥ 6; the release gate and migration now require schema ≥ 7. This is a wording error introduced by the update.

No other issue introduced by `eb652ce..f062d09` was found. **Not this round:** earlier integration and historical-send limitations still need the combined DP1-02 release review; possible security note: see manager.

## Items 1–9, touched items

Abbreviated engine paths mean `rust/crates/dw-engine/src/platform/lease/`.

| Item | Status | Evidence and limit |
|---|---|---|
| **2. PA-2 / OD-01** | **Closed for absence-based `NotSent`; literal preventive form partial** | `signers.rs:265` waits for `FundingRelease::record` before asking the vault; `fence.rs:3360` retries and refuses failed or stale writes; `dispatch.rs:1159` commits the marker-bound row. `mode_b.rs:695` requires no release to infer `NotSent`. The real-library empty-store test and my reload probe pass. The signed-lock tap still covers the normal nonempty store. possible security note: see manager. |
| **3. Schema 7 / OD-02** | **Closed** | `dispatch.rs:87`, `:880–930`, `:260–266`: 6 → 7 clears every old flag in the same migration transaction and creates the release table; new markers are flagged. `a_version_6_journal_migrates_to_7_its_markers_unflagged`, `a_release_answers_its_marker_once`, and my reopen probe pass. The separate DEC-191 text still needs its schema number revised before release. |
| **6. C4/Q13 and C5/Q14** | **Closed under the current one-build producer** | `mode_b.rs:677–703` retains a released, recordless step as `MaybeSent`, while an unreleased flagged standing marker can read `NotSent`. The real-library empty-store test, `r6_01_*` tests, and absence-guard mutation support the boundary. This status inherits item 7's producer qualification. |
| **7. O-1 one lock per call** | **Partial** | `call.rs:286` and `fence.rs:3610` require a record to cover exactly the signatures already released, with no concurrent signing. The named two-lock test and mutation pass. My empty-store probe establishes the remaining boundary limit; possible security note: see manager. |
| **8. O-2 input holds** | **Partial** | `send/psbt.rs:442–452` fences PSBT signing and includes the raw unresolved set; `:498` checks broadcast input holds. The named PSBT, CoinJoin, and sweep tests pass, and removing the PSBT broadcast check kills the test. possible security note: see manager. |
| **9. O-3 / Repair evidence** | **Closed, with stated liveness cost** | A released call with no known signed lock remains `MaybeSent`; Repair returns `NoKnownLock` rather than inventing conflict evidence. The existing exact-conflict and tombstone rules remain. `r6_01_a_released_call_with_no_recorded_lock_reads_maybe_sent` and the real-library test pass. |

## Independent probes and mutations

The two added probes and three source mutations were applied only in the review worktree and removed. Exit 101 is the expected failing test for a killed mutant; each failure was an assertion, not a compile error. `git diff --quiet f062d09` passed afterward.

| Check | Result | What it establishes |
|---|---|---|
| `review_r7_release_survives_reopen` (real SQLite) | **Pass, exit 0** | The flagged marker and matching release row survive close/reopen and reload together. |
| `review_r7_empty_store_skips_one_lock_record_boundary` (two real locks) | **Pass, exit 0** | An empty tap performs no one-lock record check; the release row keeps the step `MaybeSent`. This probe does not claim either lock was broadcast. |
| Remove the `!info.released` absence guard | **Killed, exit 101** | The `r6_01_a_released_call_with_no_recorded_lock_reads_maybe_sent` test observes the incorrect `NotSent`. |
| Neutralize `signed_only` at the record boundary | **Killed, exit 101** | The two-lock record test observes that the expected stop did not happen. |
| Remove the PSBT broadcast held-input check | **Killed, exit 101** | The PSBT test receives `NoPeers` where it requires the held-input refusal. |

## Validation and limits

- `cargo test --locked -p dw-appdb --lib dispatch::`: **29 passed, 0 failed**.
- `cargo test --locked -p dw-engine --lib platform::lease:: -- --skip platform::lease::stress_tests::`: **221 passed, 0 failed**. This includes all named new tests, the `mode_b` tests, and `real_time_bounds_hold_within_250_ms`.
- The first lease invocation also ran one stress test successfully; I stopped its longer mutation stress test and reran the focused lease suite cleanly, without stress. The implementer's broader gate was not repeated.
- `cargo fmt --all -- --check` and `git diff --check eb652ce f062d09`: **passed**. After the probes and mutations, `git status --short` was empty.
- Commands ran at nice 10 with `RUSTC_WRAPPER=`, `CARGO_BUILD_JOBS=12`, a private target, and mode-0700 disk TMPDIR. No live peer broadcast, mined ChainLock, combined DP1-02 build, or macOS test is claimed. The real-library test reaches its broadcaster but has no accepting peer.

**Verdict:** REQUIRES CHANGES — the release-row safety authority is acceptable for same-step no-double-pay, but O-1 and O-2 remain only partial under the strict requested contracts. The separate DEC-191 schema/release text must be reconciled with schema 7 before shipping.

Sol gate satisfied: no
