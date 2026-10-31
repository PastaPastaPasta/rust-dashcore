# DW E0-04.P2b — narrow Sol review, round 4

Exact head: `5a1e3abbddcc46c37b21ac38c65e6b230549ef01`, based on desktop main `9b70543cc830ac4a56a5186fd6a8387d81160b39`. Detached worktree: `/work/scratch/p2b-r4-sol/wt`. Compared with the r3 head `11bbb1f520d5fbbdec8679e861c1cdbbaee9c194` (`backup/p2b-prerebase-11bbb1f`). The reviewed worktree and source branch were not edited, committed or pushed. This report is uncommitted. No GitHub actions or messages.

The original R3-01 reproduction is fixed, but its money-safety class remains open: complete **chain** recovery cannot retrospectively authenticate a legacy self-spend's conflict with a lock still held by a peer. One new blocker is reproduced below. The signing port, rebase preservation, new ownership accessor and rejected-broadcast settlement passed the narrow checks.

## R3-01 closure and item 1

**Original trace closed; unconditional money-safety closure incomplete.** Schema 5 transactionally adds `provenance` and `conflicts`; every existing self-spend becomes `Legacy`, with no invented signed lock or conflict and no deleted marker. Schemas 1–4 migrate through the applicable additive steps. Loading retains that classification. Re-recording the same legacy txid does not promote it. Conflict encoding preserves the lock txid and exact input outpoint; the decoder rejects malformed lengths.

Repair now reads the inputs from the signed bytes it will broadcast and checks their txid against the prepared summary (`lease/repair.rs:186`). `record_self_spend` checks every known signed lock, including process-local records whose writes failed, and refuses `MissesSignedLock` before recording a spend that misses one. A nonempty recorded conflict set must name an actual input of every signed lock still known to the process. Under the pinned one-build-per-call contract, a record covering a signed lock whose own write was lost still establishes the conflict after restart. Positive `Sent` and proved rows continue to win.

I replayed my original r3 migration probe with only the new insertion argument and schema reconstruction adapted: **1 passed / 0 failed**. It observes `WillBeSent`, not `NotSent`, with the unrelated matching Broadcast row and incomplete recovery. The committed real-schema-3 version and the new missing, wrong-lock, wrong-input, two-lock partial-coverage, recording/refusal and restart regressions also passed independently in the 62-test Mode B suite.

The changed expectation in `an_unattributed_row_blocks_the_self_spends_not_sent_and_repair` is **safe and justified**. A self-spend with no conflict evidence cannot clear a Built/Broadcast row merely because that row appeared later. It may be the step's resumable lock, whose inputs the old spend omitted. With the present record format, retaining the pending-row veto is necessary for that fallback; explicit coverage of the relevant row's inputs could support a less restrictive rule. Applying it to all matching pending rows can cost liveness when a row belongs to another funding. It introduces no permission to pay twice. The authenticated-conflict branch still permits a covered pending lock to resolve.

That veto does not address an untracked, unconfirmed lock. The fallback at `mode_b.rs:627` remains unsafe for **legacy** evidence, as R4-01 demonstrates. My r3 fix suggestion allowed “complete recovery”; that wording needs tightening. A later scan alone cannot establish an earlier self-spend's input coverage.

## New findings, by severity

### R4-01 — blocker: complete chain recovery gives legacy negative evidence authority over a peer-held lock

**Locations:** `rust/crates/dw-engine/src/platform/lease/mode_b.rs:627` (recovery fallback accepts Legacy), `:392` (the actual completeness contract covers locks **on chain now**); `mode_b_tests.rs:2534` (legacy-to-NotSent expectation); `docs/design/E0-04-grants-leases.md:751` (corresponding fallback rule).

**Scenario:**

1. Before migration, funding A is signed and broadcast, but a peer withholds it. Its Built row is lost through the supported best-effort/partial wallet persistence path. There is no durable Sent and, in schema 3, no signed-lock record.
2. The old heuristic admits self-spend S using unrelated pending lock B. S spends B's input Y and omits A's input X. One concrete partial-wallet case is that X's receipt and A's row were not persisted; close/reopen loses them while the old process retains B's heuristic attribution. The old Repair sees no unsettled transaction for absent A. S ChainLocks. This is the historical wrong-conflict class R2/R3 established; an old S record stores no coverage proof.
3. Upgrade. Recovery now scans all chain history and rediscovers unspent X. A is still held by a peer and has never reached the chain, so the recovered tracked-lock snapshot legitimately contains no A. B has been conflicted away. The snapshot can be empty, with `recovery_complete = true`, fully satisfying the documented contract: every wallet asset lock **on chain** is present, and no row was dropped after this scan.
4. `shows_conflict` rejects legacy S, but the other disjunct accepts it because recovery is complete and no matching Built/Broadcast row stands. The standing step reads **NotSent**. X remains unspent by S, so the peer can release A and A can still confirm. A consumer allowed to discard and create another funding on NotSent can therefore pay twice.

The flaw is not that the scan missed an on-chain lock or that a recovery reset was omitted. Chain completeness excludes neither a transaction in a peer's possession nor the possibility that it later confirms. A legacy S admitted before that recovery cannot inherit the coverage of a newly constructed self-spend over the recovered coins.

**Independent evidence:** `review_r4_legacy_self_spend_and_complete_chain_scan_do_not_kill_a_withheld_lock` builds a genuinely signed type-8 A and a signed P2PKH self-spend S with disjoint inputs, creates a real schema-3 journal containing only the marker and S, and loads it through the production schema-5 migration into the real LeaseTable. With no tracked rows, S ChainLocked and recovery false, it reads MaybeSent. Turning recovery true over that same already-read empty snapshot changes it to **Some(NotSent)**. The assertion prohibiting NotSent fails, **exit 101, 0 passed / 1 failed**; an exact-name isolated rerun fails identically. Logs: `/work/scratch/p2b-r4-sol/legacy-withheld.log` and `legacy-withheld-alone.log`; retained source: `r4-probes.rs`.

This is a production-logic counterexample using real SQLite and signatures with fake tracked/Core evidence. It does not claim a live mined-peer experiment or an end-to-end run of the historical producer. Its completeness input obeys the source's contract. The production SessionTracked on this exact P2b head still inherits default-false recovery, so this fallback is dormant there; DP1-05 supplying the documented seam activates it. That is a material integration blocker, not a claim about currently shipped main: main remains at schema 2, with no legacy self-spend table.

**Fix:** Do not allow `Legacy` to use the recovery-only fallback. Keep its marker conservative regardless of later scan completeness, unless independently checked input-conflict evidence establishes that the old S kills every possible lock. A fresh Repair performed after recovery, under the current guards and using the actual transaction inputs, is a separate route to resolution. Distinguish that new evidence from an old txid-only row. Update the design and the legacy NotSent expectation, and retain the withheld-lock regression with the empty snapshot established **before** completeness is asserted. Do not redefine a chain scan to claim absence of peer-held bytes.

No additional blocker or major was found in items 2–4.

## Item 2 — secp256k1 port and rebase preservation

**Semantics unchanged in the reviewed funding/signature paths.** Commit `22c1c46` substitutes context-free `ecdsa::verify`/`ecdsa::sign`, by-value Message and `from_secret_bytes`; the legacy digest, per-input P2PKH script, DER parsing, S normalization and literal `0x01` hash-type requirement remain unchanged. The vault's main-branch port still signs the supplied digest with low-R grinding; secp256k1 0.33's compatibility methods delegate to the same context-free signing/verifying functions. Key-wallet still signs `legacy_signature_hash(..., All)` and appends the ALL byte. The pinned sighash implementation still commits the special payload.

Independent replay passed the existing low-/high-S provenance test, actual key-wallet/vault signing session, and r3's cryptographic probes: real non-ALL/ANYONECANPAY signatures are refused; payload, input order/count, sequence, locktime and version changes invalidate provenance; splitting input signatures between calls attributes neither; identical fully authorized transactions retain their exact shared inputs. The crash-before-call-end/failed-write probe also passes.

`git range-diff d50de65..11bbb1f 9b70543..a352c47` accounts for **all 29 commits**: 28 are patch-equivalent; only the generated Swift commit differs because the remove-wallet documentation/checksum was already on the new base. I verified both the base and rebased generated file contain checksum **29760** and the corrected documentation. No r3 hunk was lost. Before the three new commits, `mode_b.rs`, `call.rs`, `fence.rs`, `repair.rs` and dispatch.rs are identical to r3. The lease session delta is main's explicit-runtime background-unlock fix and its added regression. Range-diff: `/work/scratch/p2b-r4-sol/rebase-range-diff.txt`. The three reviewed new commits verify signed (`G`).

## Item 3 — signed_lock_txids and DP1-02

**The persistence gap itself is safe under the stated status rule.** The accessor returns exact recorded txids, including undurable process-local entries, and performs no status transition. A failed write followed by restart can make that list empty, but the FULL standing funding marker survives. With no row/proof/resolution, the step remains MaybeSent; a pending row yields WillBeSent, and a proof yields Sent. DP1-02's refusal to return to KeysPrepared for all three statuses blocks rebuilding. Empty txids must not independently be treated as evidence of nonpayment.

The failed-write/no-next-store/restart, exact-ownership refusal and durable index-0 restart tests passed, including the new accessor assertions. A recorded self-spend carrying authenticated coverage can safely resolve the lost signed record. R4-01 is a separate unsafe source of NotSent that would defeat DP1-02's otherwise sound consumer rule. The one-build entry point remains a substantive pin assumption; this accessor is not a general guarantee for multiple builds with partial writes under one permit. No live DP1-02 implementation or registration was claimed tested here.

## Item 4 — rejection after signing

**Verified against the actual pin `ebe37f8a679b1a552483dc7ff12c306291e9a32b`.** `asset_lock/build.rs:1143` handles Rejected by awaiting guarded untracking, queuing removal, and releasing the reservation only when the Built row was actually removed. Refused removal retains the resumable row/reservation and downgrades the error to `TransactionBroadcastUnconfirmed`. The desktop permit counts a failed call with a released signature as MaybeSent (`call.rs:736`), so it does not journal a definitely-unsent resolution merely from the rejection.

My real-session probe invokes the pinned `create_funded_asset_lock_proof` with the actual vault counting signer and dispatch tap. An unstarted SPV runtime returns `TransactionBroadcast("SPV broadcast not sent: client not started")` **after one signature**. The tracked row is absent, one signed-lock txid remains, and the ended step reads **MaybeSent**. It passes **1/0** (`pinned-rejection.log`, source `rejection-probe.rs`). A separate derivation probe with a retained Built row returns WillBeSent, then MaybeSent after its removal, **1/0** (`retained-built-rejection.log`). The guarded-removal branch was inspected, not forced through a live concurrent resume in that probe.

Qualification to the implementer's wording: a guard can also refuse because the row has advanced to a proof; that reads **Sent**, rather than WillBeSent. All of these outcomes block another payment. The historical legacy exception in R4-01 is independent of this normal rejected-call result.

## Validation and limits

- Fresh exact-head `cargo test --locked -p dw-engine -p dw-appdb --lib --no-run`: **exit 0** (`build.log`, `build.jsonl`). Dependency artifacts were copied/reflinked into a review-owned target; Cargo rebuilt and checked this head. No other worktree's target was used for builds.
- Unmodified exact-head tests: dispatch **25/0**, Mode B **62/0**, call permits **22/0**, Mode B sessions **6/0**, Repair sessions **4/0**, unrecorded self-spend release **1/0**. Total **120 passed / 0 failed** (`baseline-results.json`, suite logs).
- Original r3 migration replay **1/0**. Four adapted r3 probes plus the committed migration regression **5/0**. Rejected-call probes **2/0**. New legacy/withheld-lock invariant **0/1**, repeated alone **0/1** (`probes-results.json` and named logs).
- `cargo fmt --all -- --check` and the dispatch model: **passed** (`fmt.log`, `dispatch-model.log`).

The supplied gate identifies 5a1e3ab, **993/0**, model checks, 10-seed stress, caught CallBeforeMarker and BL-76, ending GATE EXIT 0. Those full-gate/clippy/stress/dashd results were inspected, not independently rerun in this narrow review. They do not cover R4-01. macOS/Swift execution was not performed.

Review-only tests are appended to a **copy** of the engine crate outside the clean detached worktree; production lease logic is unchanged. They use Cargo's exact feature-selected dependency artifacts and rustc arguments, retained in `exact-cargo-rustc-command.json`/`probe-compile-command.json`. Commands use nice -n 10, CARGO_BUILD_JOBS=12, umask 022 and disk TMPDIR `/work/scratch/p2b-r4-sol/tmp` at 0700. The review-owned target and temporary test directory were deleted after validation; logs and probe sources remain. No process-pattern kills, commits or pushes.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
