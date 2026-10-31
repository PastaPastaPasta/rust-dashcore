# DW E0-04.P2a — Sol review, round 2

**Verdict: REQUIRES CHANGES.** One remaining High finding in the new `keep_sent` recovery path prevents full closure of F1/F2 and the authority gate. F3's decision (c), adapting the witness to distinguish a dispatch attempt from subsequent bookkeeping, is sound. The two named latent gaps may wait for their consumer rows, with the conditions below.

## Scope and authority

- Date: 2026-10-09. Reviewer: Codex, gpt-6.1-sol.
- Desktop worktree: `/work/home/agent/workspace/dw-wt/e0-04-p2a-leases`, branch `dw/e0-04-p2a-leases`.
- Requested fix range: `d86e6a065530a1b485ec936ff90798952ff51c25` → `d99102912a1c64a5aa4bcb2cc6e0277c1db1211f`. This includes DEC-134 at `270665a`, which r1 had already reviewed, and the r1 fix commit `d991029`.
- Binding references: r1 `DW-E0-04-P2a-r1-sol.md`; `docs/design/E0-04-grants-leases.md`, approved at `723d9d3` (DEC-73), particularly §5.5, §7.6, L5, H10/H14/H16 and §13; DEC-134; the two requested memory notes. I inspected the actual fix diff and final implementation, including the amended design text.
- Source references below are relative to the desktop worktree and use unchanged HEAD line numbers. No production fix was made. Temporary review-only test includes were restored byte-for-byte. Desktop HEAD is unchanged and its worktree is clean. This report is deliberately uncommitted.

## Findings, by severity

### R2-F1 — High: `keep_sent` restores transport authority before durable evidence, and can permanently lose the marker

**Locations:** `rust/crates/dw-engine/src/platform/lease/fence.rs:660`, `:1814`, `:1822`, `:1848`; copy admission at `:1537`. Journal deletion: `rust/crates/dw-appdb/src/dispatch.rs:286`. Existing positive-race regression: `lease/tests.rs:2122`.

When `note_seen` reports `Sent` during a `Resolving` delete, `keep_sent` changes the entry back to `Admitted`. Its markers remain `Mark::Durable` in memory, although the SQLite delete may already have removed them. Restoration runs afterward, outside J, without installing a `Committing` or `Ambiguous` obligation. Consequently:

1. A copy can receive `Resend` while restoration is still blocked and SQLite has zero markers. This is its own step's pre-existing marker, not a marker begun by a later copy.
2. A failed restoration is only logged. The memory marker stays `Durable`; subsequent copies do not repair it.
3. If the delete commits and then returns an error, `written == false` suppresses restoration altogether. An error is not evidence that the delete had no effect. A panic after a committed delete has the same ambiguity; the restore call itself also lacks the delete's panic/retry protection.

I independently reproduced the first three behaviors with a gated backend wrapping an actual file-backed `DispatchJournal` under the requested TMPDIR. The interleaving is the same supported race as the implementation's own `r1_f2_an_artifact_seen_sent_during_its_resolution_keeps_its_marker`:

- Admit a step-scoped First and verify its marker exists.
- Start `finish(NotSent)` and block its journal delete.
- Call `note_seen` for that artifact while it is `Resolving`.
- Release the delete; either block the restore, fail it before writing, or report an error after the delete committed.
- Admit the same artifact and step again; in the failure cases, reopen SQLite and load a new table.

Observed results:

| Probe | Result |
| --- | --- |
| Restore blocked after successful delete | SQLite markers **0**; copy verdict **Resend** before restoration completes. |
| Restore fails before writing | Reopened markers **0**; next copy **Resend**; repair writes **0**. |
| Delete commits, then reports error | Reopened markers **0**; next copy **Resend**; repair writes **0**. |
| Successful restoration control | Reopened markers **1**; recovery correctly returns **Resend**. |

In both failure cases this process reports `Sent`, retains the 10-credit charge, and emits only `DispatchResolved(Sent)`. Thus I did **not** demonstrate a double refund. Nevertheless, after reopening, a dead lease's request is `Refused { cleanup: false, step_possibly_dispatched: false }`: the durable step evidence of the observed execution has disappeared. A resumable flow can then treat the step as canceled and construct another artifact. That is the lost-recovery-evidence/double-payment risk behind r1 F1, now introduced through restoration rather than initial admission. The current checker accepts both logged failure traces as `Ok(())`, because it never invalidates the earlier `Durable` event during the delete/restore sequence.

**Required correction:** preserve durable recovery evidence when `Sent` overtakes a resolution. Restoration must be an artifact-wide durability obligation under J; copies must defer until it lands. Treat a failed or panicking delete as potentially committed, and make failed/panicking restoration retryable rather than retaining a false `Durable` marker. Also cover interruption between deletion and restoration: successful best-effort rewriting alone does not close that recovery window. Add the three probes above to the regression gate and represent marker invalidation/restoration in its oracle.

This belongs to the newly added P2a settlement path and the specific F1/F2 guarantees requested for this round. It is separate from the two explicitly deferred consumer gaps.

## F1–F8 disposition

| r1 item | r2 decision and evidence |
| --- | --- |
| **F1 — copy before marker** | **Not fully closed.** The original differently scoped copy and ambiguous-marker cases are fixed by artifact-wide `obligation` (`fence.rs:504`), and the post-write Resend recheck works. Both installed F1 regressions pass; `CopyBeforeMarker` makes the original regression fail. R2-F1 reopens the same durability requirement during restoration. |
| **F2 — refunded NotSent revived by marker** | **Not fully closed.** Durable deletion before refund, the `NotSent` precedence, failed-delete deferral/retry, settle-on-return, nonce-slot settlement and pre-delete `Sent` protection pass their regressions. A marker writer returning on an already sent artifact does not refund it. R2-F1 leaves the concurrent `Sent` restoration branch unsafe. |
| **F3 — late First dispatch** | **Closed; decision (c) accepted.** Past-deadline entry is refused; a library future already entered is cut as `MaybeSent`, without a poll at or after its deadline. See the independent literal/adapted witness assessment below. |
| **F4 — duplicate register survives removal** | **Closed.** Single-flight registration (`fence.rs:1049`) re-decides after the owner writer returns; the removal barrier prevents another writer from starting. The same-origin/competing-origin/removal regression passes, and `DuplicateRegister` makes it fail. Register panics are caught before writer bookkeeping is removed and waiters notified. |
| **F5 — exact Spend cap** | **Closed.** `charge_spend_signer` (`table.rs:847`) obtains the signer and charges inclusively in one J step. The installed one-duff-over/exact-cap/refund regression passes. My original r1 exact-cap witness now passes independently: debit/cap `1,000,226` duffs. |
| **F6 — lease ownership** | **Closed.** Owning clones share `LeaseOwner`; its last drop ends authority (`table.rs:727`). Lookup handles borrow. The session retains the facade's owner (`platform/flows.rs:176`), and `end_flow` drops it outside the owner-map lock. Both last-owner and facade lookup/drop regressions pass; a temporary facade lookup leaves the lease Active. |
| **F7 — weak stress checker** | **Original blind spots corrected; full gate not yet satisfied.** The checker now rejects early Lock return, transport after Finish, missing scoped markers, incorrect refunds and Resend after definite resolution. It exercises scoped copies, duplicate registration/removal, epoch changes and deadline stalls. Its bad-trace tests and all eight stress mutations pass. R2-F1 supplies a remaining accepted invalid durability trace, requiring one recovery-oracle extension, not a second independent production finding. |
| **F8 — zero cost without purpose** | **Closed.** Credits and Funding presence are checked separately from the amount (`fence.rs:1079`, `:1604`); the zero-cost refusal and authorized-zero regression passes. |

The blanket claim that every fix's regression fails when reverted is stronger than the independently rerun negative controls here. I replayed the CopyBeforeMarker, MarkerOutlives, LateEntry and DuplicateRegister controls; each failed its targeted assertion with exit 101. F5/F6/F8's positive regressions and fix logic were reviewed and tested, but I did not rebuild separate reversions of those three fixes.

## F3 witness adaptation and settlement safety

I reran the **original** r1 literal witness unchanged. It still fails `!task.is_finished()`: Lock returned after approximately **10.102 s**, the task still waited on the held manager guard, and releasing it produced `BroadcastUnknown("the lease's deadline passed during the hand-off")`. The old witness's print label about dispatch running after release is inherited text; that is no longer what its result establishes.

The adaptation is sound because `hand_off` (`fence.rs:2053`) owns the library future and checks expiry **before every poll**. On the cut it returns and drops that future. `fenced` then finishes the attempt as `MaybeSent`; the subsequent task continuation performs metadata writes and wallet-state reads (`send/mod.rs:1193`, `:1735`), with no route back to dispatch. The installed real-SPV adaptation passed: no attempt remains in flight after the cut, and release of the held guard produces the unknown result rather than SPV's no-peers rejection. The late-entry, hanging-call and exact-deadline-poll tests also passed. The pinned SPV pre-enqueue waits run in the canceled future; they do not spawn a later dispatch in this stalled-guard path. Previously enqueued bytes may still be sent, as the design permits.

The new settlement paths preserve the aggregate-attempt rule and retain charges/reservations when the journal refuses a resolution. `catch_unwind` converts a panicking delete into retryable `Resolving { failed: true }`; no refund or NotSent event is published. A returning marker write settles an idle definitely-unsent or nonce-slot-losing artifact only after all writers finish, and the sent-set check prevents refund of an already observed sent artifact. These cases passed. **I cannot confirm the overall “no double-pay or lost-resolution path” claim because R2-F1 loses durable sent evidence.**

## The two named latent gaps

Both may wait **for their first consuming rows**, and neither should be represented as fixed:

- **`bind_spend` versus `Resolving` (`fence.rs:1165`).** A new same-txid Spend binding can be overwritten/removed by the older resolution, without accounting correctly for the new charge. `Resolving` requires step markers; the current engine-owned leased TxDraft path passes a scope without a step. The problematic step-scoped Core preparation is therefore absent from current P2a production wiring. Fix and test it before a consumer introduces that path, including P5/DP3-01 or relevant P2b/P4 wiring.
- **A copy's marker lands after removal's erase (`fence.rs:1990`).** Erase joins registration writers, not every row-less step writer. The held removal barrier also does not forbid Resend marker work. DEC-134's registered-row race is closed, but this broader race remains. Current production TxDraft hand-offs have no step markers; library resumable-step/call-permit wiring is deferred to P2b/P4. Those rows must join or cancel all marker/resolution/restore writers before erasure and cover concurrent copy/removal. A barrier alone is insufficient.

These deferrals are conditional scope judgments against §13, not a waiver for shipping either interleaving once the corresponding consumer is installed.

## Independent validation

Cargo commands ran in the desktop `rust/` workspace with **umask 022**, **TMPDIR `/dev/shm/e0-04-p2a-r2-sol` mode 0700**, **nice 10**, `CARGO_BUILD_JOBS=12` and `RUSTC_WRAPPER=`. The existing target belongs to this desktop worktree; no target directory was shared with another worktree.

| Check | Result |
| --- | --- |
| `cargo test -p dw-engine --lib platform::lease --locked -- --nocapture`, with `DW_LEASE_STRESS_SEEDS=10` | **66 passed, 0 failed**. Includes all lease/session regressions, checker self-tests, 10 stress seeds and eight mutation checks. |
| `cargo test --workspace --locked` on unchanged HEAD | **680 passed, 0 failed, 3 ignored**, exit 0. Includes dw-appdb's targeted marker deletion and journal tests. |
| `cargo fmt --all --check` | Pass. |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Pass. |
| `python3 -I docs/design/checks/e0_04_design_model.py` | **80 PASS**, final `ok`. |
| `git diff --check d86e6a0 d991029` | Pass. |
| Stress mutations | CheckThenAct→I2; RevokeInDrain→I2; TransportFirst→I1; EarlyReturn→I5; CopyBeforeMarker→I11; MarkerOutlives→I16; LateEntry→L5; DuplicateRegister→DEC-134. All caught at their intended invariant. |
| Independent SQLite restoration probes | **Three required-behavior assertions fail**, one successful-restoration control passes. Exit 101, with no compilation failure. |
| Original r1 F5 witness | Pass. |
| Original r1 F3 literal witness | Fails the task-liveness assertion as expected; the adapted transport-deadline property passes. |

Host-local evidence is in the TMPDIR: `lease.log`, `workspace.log`, `model.log`, `fmt.log`, `clippy.log`, `negative-*.log`, `restore-witness.rs`, `restore-witness.log`, `original-exact-cap.log`, `original-f3-literal.log`, and the probe SQLite files. Reproduce the new probes by temporarily including `restore-witness.rs` at the end of `lease/tests.rs`, then running `cargo test -p dw-engine --lib review_r2 --locked -- --nocapture --test-threads=1`. The source and interleavings above distinguish the failure from the passing restoration control.

File-backed SQLite on tmpfs and reopen checks establish the missing persisted rows and fault behavior; they do not simulate physical power loss. The model and stress checker passing are useful gates, not a proof that the implementation refines every recovery rule.

**Approval status: REQUIRES CHANGES.** Close R2-F1's durable-restoration and oracle paths before declaring F1/F2 and the Sol authority gate satisfied. F3 requires no stronger task-liveness bound for its unrelated post-dispatch bookkeeping.

**Sol gate satisfied: no**
