# DW-DP1-02 independent Sol review, round 9

Reviewed `a383aedc953ca843ecb27d573d80367f1a593e9f` (39 signed commits on desktop main `42bace6`) in a detached, disposable worktree. Focus: `b40c25d..a383aed`, the ownership ledger and Phase B. No new blocking DP1-02 defect was found. The verdict assumes the stated same-release P2b and DP1-02 schema-6-or-newer contract (DEC-191).

## Round 8 closure

| Finding | Closure and independent evidence |
| --- | --- |
| R8-01, lost `signed_lock` | **Closed.** P2b's surviving marked step reads `MaybeSent`; `StepClaim::Unknown` keeps the row `Awaiting` and committed. Status reports committed, discard and ordinary replacement start refuse, its own run makes no second funding call, and an unflagged control quarantines unattributed locks. The actual dispatch SQLite signed row was removed, the marker and release retained, and the session closed and reopened. Review-only ports passed. P2b's new durability gate prevents the old failed-insert trigger from sending in the first place. |
| R8-02, settled-row discard | **Closed.** The step ledger outlives rows. A legacy row holding another settled lock remains committed while its own exact signed lock is outstanding; discard refuses. Finish may put that unspent signed lock in a recovery row, after which the old row may settle. Done and failed-name variants passed, with no transfer to an unrelated waiting row. |
| R8-03, held pending count | **Closed.** `import_stranded` returns `ledger.pending()` on the funding hold before allocation; a signed lock away counts one. The ported held-path probe passed. |

Nine adapted r8 probes passed. The tenth old control used synthetic self-spend inputs that P2b now rejects; the stronger real Repair/self-spend and C4 tombstone controls both passed. Sources and results are retained under `/work/scratch/dp1-02-r9-sol/` (`review_r9.rs`, `probes2.log`, `self-spend-control.log`, `c4-control.log`).

## Ownership-model conformance

- **§3–§5:** `registration_ledger.rs` builds claims from every wallet-scoped P2b step, including missing signed bytes, then derives `standing`, `owner`, `may_take`, `reconcile`, `committed` and `pending` from one ledger. The actor's funding-call flag blocks a free-lock allocation in the pre-marker window. Every new unspent attachment in run, tap and Finish uses the ledger; the successful funding return is checked against the step's signed txid instead of trusted alone.
- **C4/C5 and Q13/Q14:** P2b itself derives flagged tombstone resolution as `NotSent`; a flagged standing marker with neither release nor signed record also reads `NotSent`. DP1-02 closes the latter row through `Cancelled(funding_cancelled)`, without retrying that marked step. Unflagged recordless markers remain unknown.
- **Q8, D15:** exact live possession wins over the draft signer; the contrary open signer closes. A cancel-coded candidate is never `row_of`, with no fallback heir, so later evidence cannot reopen that row. Consumed signed value attaches only to its open row when the identity is discovered, otherwise settles in place. Voided outpoints are cleared by one CAS.
- **DEC-197/198/am.1:** only a wedged `Unknown` step can be passed; start also requires sync, explicit naming and an hour after that step's own release. The old row remains committed. A contested lock goes to Recovery only when all contesting steps are wedged with known releases; an unfunded row blocked only by that lock reports `NeedsFinish`.
- **Conservative model difference:** §3.4/Q6's amount narrowing is available when a readable row supplies `lock_duffs`. A rowless step view carries no amount, so Rust contests a broader set for an orphan. `registration_tests/ledger.rs` explicitly tests that behavior. It gives no extra row permission; the Python model's orphan amount is an idealization.

The checked Python model completed ten scenarios, including the flagged/unflagged, restore, legacy holder and amount cases: S1–S6 and I3/I4/I6/I10–I12/D15 held; **19/19** built-in rule mutants were caught. This is bounded model evidence, not a substitute for the real J/SQLite probes above.

## Phase B decisions

| Check | Severity / result | Location | Probe or reason | Fix |
| --- | --- | --- | --- | --- |
| Release time and attribution | None found | `lease/fence.rs:1018`, `lease/mode_b.rs:1406`, `registration_ledger.rs:342` | One J read; `released` is keyed by `(wallet, step)`, and the view is wallet scoped. Per-step release test and DEC-197 override test pass; the wrong-step mutant fails. | None in DP1-02. |
| Step 2 resumable permit | None found | `registration_flow.rs:2035` | Permit is durable before the call; retry checks for the lock's identity first. The same lock is reused across a crash or height retry. Locked proof signer parks even when its signature is uncounted. Step-2 tests and the park mutant support this. | None. |
| Refusal naming another lock | None found | `registration_flow.rs:1965` | Only a refusal naming this funding step becomes its own uncertain call; another lock waits at Sync. An existing own marker is checked first. The focused test passes and the classification mutant fails. | None. |
| OD-01 broadcast paths | None found | `registration_flow.rs:2144`, `registration_flow.rs:2704`, `lease/fence.rs:3614` | The funded-lock builder uses P2b's counted funding signer and synchronous durable signed-record gate. The DP1-02 Built-lock resend checks `unrecorded_signer`; P2b's startup/deferred resume gates do likewise. Identity creation uses an already proved lock. Store/resume gate tests pass. | None. |

## Declared residuals

1. **Identity step remains `MaybeSent` after a discovered identity on a Done row:** nonblocking. It is a resumable step's stale dispatch label; the identity and asset lock are already settled, and no new Core funding is permitted by it. A P2b resolution API would improve the status.
2. **Release write after cancellation:** possible security note: see manager (`rust/crates/dw-appdb/src/dispatch.rs:1164`, `rust/crates/dw-engine/src/platform/lease/fence.rs:3366`). Nonblocking in this reviewed scope; fix belongs to P2b.
3. **Unavailable journal at step 2 writes a retryable failure:** nonblocking. The row retains its funded lock and cannot be discarded while committed; resume can retry when J returns.
4. **Real `PwLib` signer path lacks a live registration test:** coverage gap, nonblocking given the typed wiring and P2b boundary tests. The testnet attempt could not run because the faucet now requires a captcha.

## Independent mutations

Each source mutant ran alone; the runner copied the original file to a saved file, restored by copying it back and touched it before the next mutant. The unmodified focused controls passed. Exit 101 below is the expected assertion failure, not a build failure.

| Mutant | Killing test | Result |
| --- | --- | --- |
| Read release time from any step of the same wallet | `each_registration_step_shows_its_own_release_time` | Killed, exit 101 |
| Treat another lock's refusal as this row's `will_be_sent` | `another_lock_awaiting_its_resend_holds_the_funding_without_failing_the_row` | Killed, exit 101 |
| Omit the ledger's marker-backed committed contribution to `funds_committed` | `review_r9_port_unlanded_marker_refuses_discard_and_replacement` | Killed, exit 101 |
| Drop the locked-vault park at step 2 | `step2_a_proof_signer_meeting_the_lock_parks_for_the_unlock` | Killed, exit 101 |
| Allow a cancel-coded row to be `row_of` | `a_written_close_is_sticky` | Killed, exit 101 |

## Validation and boundaries

- Exact-head `cargo test --locked --offline -p dw-engine -p dw-appdb --lib --no-run` passed. `dw-appdb`: **71 passed**. `dw-engine`: **704 passed, 4 ignored**, including the long stress mutation test and E0-10b's pre-migration upgrade probe. The 9 ported probes and 2 real self-spend controls passed.
- `m4_dashpay_contract`: **9 passed**. Contract-Version **14**, item **40** after P2b item 39, and its generated surface match. Appdb's shipped migrations are unchanged; DP1-02 adds `V2026101201/02`. `a383aed` soundly removes those later columns and history rows when constructing its pre-E0-10b database.
- `cargo fmt --all -- --check` and `git diff --check 42bace6..a383aed` passed. All 39 reviewed commits verify `G`. The detached worktree was clean after removing review-only probes and restoring each mutant.
- The Python model exited 0 in **449.9 s**; its largest scenario explored **321,781** states. The real tests use fake network/Platform outcomes and local J/SQLite, not a live mined-chain or DAPI registration. High box load does not support timing or benchmark conclusions.
- Builds and tests used `nice -n 10`, `CARGO_BUILD_JOBS=12`, a scratch-owned target, and a mode-0700 scratch `TMPDIR`; long runs used `systemd-run --user`. No implementer-source changes, commits, pushes, or GitHub actions were made. The review worktree and target were removed after validation; this report is uncommitted.

**Verdict:** APPROVE-WITH-NITS
Sol gate satisfied: yes
