# DW-DP2-05 round 4 — Sol (narrow)

Reviewed `ef4e2b6b..d57f7a2e` (commits `550bdea2` and `d57f7a2e`) in the read-only worktree `dw/dp2-05-notifications`. No source changes were made in that worktree.

## Round-3 item rulings

| Item | Ruling | Evidence |
|---|---|---|
| 1. Partial removal: appdb delete failure could admit an old writer | **CLOSED** | `remove_wallet` now calls `live.tap.forget(&id)` after the blocking persister/appdb job has returned and before propagating its error (`rust/crates/dw-engine/src/wallets.rs:255-274`). `AppDb::delete_wallet` commits the `removed_wallets` record before the row-delete transaction and remembers failures (`rust/crates/dw-appdb/src/lib.rs:521-550`). `readmit_wallet` only removes the record together with all wallet rows, and maps failed cleanup to `RemovalUnfinished` while the transaction rollback keeps the record (`rust/crates/dw-appdb/src/lib.rs:576-602`). The three new notification tests cover successful readmission, refusal while cleanup still fails, and a failed record insert (`rust/crates/dw-engine/src/platform/notifications_tests.rs:516-582`). The m1 mutant that moved `tap.forget` after `deleted??` failed all three targeted failed-removal tests. |
| 2. Single-mark regression test was nondeterministic | **CLOSED** | `journal` extends the recount `Events` changes into the same set before the sole mark (`rust/crates/dw-engine/src/platform/journal.rs:913-920`). The test hook records each `mark_platform` call (`rust/crates/dw-engine/src/pump.rs:99-156`), and the assertion requires one mark containing `Events`, `Identities`, `Payments`, and `Names` (`rust/crates/dw-engine/src/platform/journal_tests.rs:1229-1257`). The two-mark mutant failed the exact test with a separate `Events` mark. |

## Narrow audits

### Lock order

The new paths do not introduce a lock cycle. `record_events_if` takes the sole appdb connection and invokes the epoch admission check while that connection is held (`rust/crates/dw-appdb/src/dashpay_journal.rs:67-81`; `rust/crates/dw-engine/src/platform/journal.rs:897-902`). Removal awaits the blocking delete job, releases that connection, and only then takes the tap epoch/caches/pump path in `tap.forget` (`rust/crates/dw-engine/src/wallets.rs:258-274`; `rust/crates/dw-engine/src/platform/journal.rs:598-606`). Imports/restore/watch-only registration hold the wallet lifecycle lock before their `readmit_wallet` appdb operation (`rust/crates/dw-engine/src/keys.rs:464-472,529-539`; `rust/crates/dw-engine/src/multiwallet.rs:437-452`). No path takes the lifecycle lock while holding the appdb connection or tap epoch guard.

### Crash and transaction windows

The standalone record commit is safe for the stated crash cases. The record insert is committed before the row-delete transaction (`rust/crates/dw-appdb/src/lib.rs:533-543`). A crash after that commit leaves a durable tombstone; a lone next open deletes the rows first and removes the record only if the cleanup transaction commits (`rust/crates/dw-appdb/src/lib.rs:553-573`). If cleanup fails, the record remains. Readmission performs record removal and row cleanup in one transaction, so a cleanup failure rolls back the record deletion and keeps the wallet refused (`rust/crates/dw-appdb/src/lib.rs:584-602`). A failed record insert is remembered only in the live `AppDb` instance (`rust/crates/dw-appdb/src/lib.rs:544-549`); after reopening, rows can remain until a later import. This is the documented residual (`docs/contracts/m4-dashpay-engine.md:2171-2178`), and the same-session path is covered by `a_removal_that_could_not_record_the_wallet_is_finished_by_the_reimport`.

### False refusal check

`readmit_wallet` cleans up only when a durable removed record exists or this instance recorded a failed removal (`rust/crates/dw-appdb/src/lib.rs:587-593`). A wallet that was never removed returns `Ok(false)` without deleting rows. The migration test explicitly checks that an unrelated wallet is not refused (`rust/crates/dw-appdb/src/migration_tests.rs:1003-1023`). In engine paths, `delete_wallet` is reached only after the wallet has left the manager (`rust/crates/dw-engine/src/wallets.rs:247-266`), while imports and restores serialize with the lifecycle lock.

## Declared residuals

| Residual | Ruling |
|---|---|
| Failed `removed_wallets` record insert, followed by reopen, leaves rows until a later-session import | **Acceptable and documented.** The in-memory failure marker cannot survive the reopen; the contract explicitly calls this residual out (`docs/contracts/m4-dashpay-engine.md:2174-2178`). |
| Dispatch-journal rows survive a failed delete | **Acceptable for this narrow review.** This is pre-existing and outside the tap/appdb notification-generation fix; the notification journal rows and epoch fence are handled independently. |
| Lone-open warning does not name wallet ids | **Acceptable diagnostic nit.** `forget_removals` emits one generic warning when cleanup fails (`rust/crates/dw-appdb/src/lib.rs:557-573`); it does not affect cleanup, refusal, or admission behavior. |

## Checks run

- `git diff --check ef4e2b6b..d57f7a2e`: clean.
- Clean scratch-clone baseline (`CARGO_TARGET_DIR=/work/scratch/dw-dp205-r4-sol/target`, `TMPDIR=/work/scratch/dw-dp205-r4-sol/mut-tmp`, `nice -n 10`, `CARGO_BUILD_JOBS=8`):
  - `dw-appdb` three new removal/open tests: **3 passed**.
  - `dw-engine` failed-removal/reimport tests: **3 passed** (two filtered together plus the failed-record test exact).
  - `dw-engine` exact `platform::journal_tests::the_session_store_taps_what_the_persister_accepts`: **1 passed**.
- Scratch-clone mutants, each restored clean afterward:
  - m1 (move `tap.forget` after `deleted??`): targeted failed-removal tests **failed** with old rows admitted.
  - m3 (omit `delete_wallet_rows` in `readmit_wallet`): AppDb cleanup test **failed** its `RemovalUnfinished` assertion; engine tests **failed** (reimport was admitted / old row landed).
  - m5 (restore two separate marks): exact single-mark test **failed** with an extra standalone `Events` mark.
- Scratch clone was clean at the end; no GitHub, push, or testnet writes were performed.

ACCEPT
