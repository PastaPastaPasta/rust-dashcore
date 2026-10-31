# DW-DP2-05 fixture handlers — Sol review round 1

## Scope

Reviewed `ed3ee5fc..1188df86` on `dw/dp2-05-fixture-handlers` from a private clone at `/work/scratch/dw-dp205fh-r1-sol/repo`. The implementer worktree was not edited.

## Findings

No Major or Minor findings.

## Engine/vector correspondence

- `rust/crates/dw-engine/src/platform/notifications_tests.rs` builds the committed vector from a real seeded `DashPay` session. It covers the 8 event kinds, 216 rows, 4 pending requests (including equal-time ordering), cursors/pages, the 1–200 limit clamp, ignored-row filtering at a page boundary, mark-read idempotence and upper bound, unignore/sweep, unknown identities, other wallets, non-Base58 ids, and Base58 ids decoding to the wrong byte lengths.
- `notification_vectors` compares the exact pretty-printed generated JSON and fails stale data. In the clone, `cargo test -p dw-engine notification_vectors -- --nocapture` passed (the test is `platform::notifications_tests::vectors::notification_vectors`).
- `apps/desktop/src/fixture/notifications.test.ts` replays every vector case and every step over the generated IPC commands. It also checks all fixture identity ids, event-kind names, event signaling, and the registered-world smoke behavior. The suite passed.
- `rust/crates/dw-app/src/api/social/events.rs` has the IPC shape/kind/error round-trip guard. `cargo test -p dw-app notification_vector_is_dw_apps -- --nocapture` passed (the test is `api::social::events::vector_tests::notification_vector_is_dw_apps`).
- The declared differences in `handlers.ts` are suitable for this UI fixture: unread count is already initialized, and fixture writes do not synthesize journal rows. The rest of the bell behavior is modeled from the vector.

## Guard/build validation

From the private clone:

- `pnpm test`: **13 files, 259 tests passed**.
- `pnpm build`: production bundle passed; `check-bundle` reported no fixture marker and no fixture sentinel.
- `pnpm build:fixture`: fixture bundle passed; all bundled module sentinels were present and test-data sentinels were absent as expected.
- `bash scripts/guard-negative-control.sh`: all wallet/module/error and six data-loading controls failed at `check-bundle` with the expected fixture sentinel, including raw/url/inline/JSON/percent-encoded paths. The new `vectors/notifications.json` is registered in `TEST_DATA`, included in the sentinel/file-list tests, and the production build computed 767 test-data sentinels as absent, so it is covered by the same dynamic rule.
- `cargo fmt --all -- --check`: passed.

## Mutation checks

In temporary copies of `handlers.ts`, the focused notification replay caught each mutant:

- ascending event-row sort (3 failed tests);
- removed ignored-sender filter (1 failed test);
- removed `readAt === null` guard (1 failed test);
- weakened `isIdentityId` to accept every string (1 failed test, including wrong-length cases).

The clone was restored to the reviewed contents after each mutation.

## DP1-06 integration note

`51178e9f` also edits `apps/desktop/src/fixture/dashpay.ts` for the newer `IdentitySummary` credit fields, so a rebase/merge will touch the same `aliceIdentity` area. A three-way `git merge-tree --write-tree 1188df86 51178e9f` completed without conflicts; the overlap is additive, but the post-merge TypeScript/build checks should still be run.

ACCEPT
