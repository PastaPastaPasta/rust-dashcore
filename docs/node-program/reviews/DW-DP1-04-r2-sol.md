# DW-DP1-04 round-2 narrow review — Sol

Reviewed `dw/dp1-04-contest-watch` at `528bef81..7b541404` (DP1-04 fix `a73f6fab` plus the U-07 and DP1-06 merges), the round-1 findings, pinned Platform `ebe37f8`, and M4 Contract-Version 19. The implementer worktree was read only and remained clean. Scratch work was isolated under `/work/scratch/dw-dp104-r2-sol` with private `TMPDIR` and Cargo targets.

## Round-1 findings

| Finding | Decision | Evidence |
|---|---|---|
| Major 1: old win becomes unread on restore | **CLOSED** | The name tap emits only a read `UsernameRegistered` for a watched label (`rust/crates/dw-engine/src/platform/journal.rs:978-1025`); the watch alone journals `ContestWon` at the resolving block time (`contest_watch.rs:677-715`). The pass checks for a catch-up boundary before adoption or a network read (`contest_watch.rs:498-505`). The name-first restore probe at `contest_watch_tests.rs:738-769` yields both rows read. |
| Major 2: full contender page proves participation | **CLOSED** | `Poll::truncated` applies only to an open, full page (`contest_watch.rs:145-153`); `verdict` leaves an absent identity on that page `Open`, and treats an absent identity in a resolved result as `Unplaced` (`contest_watch.rs:179-194`). The pinned Drive verifier supplies the stored complete contender list after resolution. The full-page fake at `contest_watch_tests.rs:698-735` keeps the open row and distinguishes resolved listed from resolved absent. |
| Major 3: unbounded end-date pagination | **CLOSED** | `ends_page` requests limit 100 once (`contest_watch.rs:287-294`), and `SdkContests::end_page` uses one direct `FetchMany` query with `limit: Some(limit)` (`contest_watch.rs:324-349`). The old SDK helper's internal pagination is no longer called. `watch_contests` invokes `ends_page` at most once per pass (`contest_watch.rs:533-543`). The 150-poll fake at `contest_watch_tests.rs:675-695` observes `(2 vote pages, 1 end page)` and one end limit of 100. |
| Minor 4: `Contests` signal bypasses one-second floor | **CLOSED** | The supervisor sets the floor after a pass and applies it to `Contests`, `WalletAdded`, and `Readmit` signals (`bringup.rs:289-330`). |
| Minor 5: cap test follows mutable constant | **CLOSED** | The test asserts literal `(16, 1)` and the next-pass remainder `(20, 2)` (`contest_watch_tests.rs:651-669`). The 16→17 mutant now fails. |

## New findings

None in the DP1-04 changes. The on-demand status path returns `Open` for an open full page missing the identity (`contest_watch.rs:768-775`); this is consistent with the contract's requirement to reject an unwatched label only when a **complete** contender list proves absence (`docs/contracts/m4-dashpay-engine.md:445,2474-2479`).

Restore sequencing is also consistent: startup bring-up and the recovery names pass finish before the supervisor's first contest timer (`bringup.rs:222-249`); recovery advances the catch-up boundary before subsystem work (`bringup.rs:535-540`), and the watch refuses a wallet without a boundary (`contest_watch.rs:498-505`). A won row is persisted before the journal attempt, retried if journaling fails, and removed only after listing or its one-day wait (`contest_watch.rs:677-715`). Loss and lock journal before row removal; unplaced rows age out without an event (`contest_watch.rs:661-675,718-745`).

The pass remains bounded by 16 due rows, one vote-state page per row, and one end-date page (`contest_watch.rs:513-567`), under the supervisor's 20-second timeout (`bringup.rs:419-430`). The bound counts logical requests; the SDK transport can still retry a request. No new request loop is present.

M4 is consistent: header and generated-list marker say version 19 with SHA-256 `6246593e1b229970510db240ef8a7f8ed613ffa33d9252610f7ef206ddbbb939` (`docs/contracts/m4-dashpay-engine.md:3,546`); item 45 and §2.3 describe the watch and `contest_status` (`:445,2458-2497`). `IMPLEMENTED` contains `DashPay.contest_status` and the merged DP1-06 bodies (`rust/crates/dw-engine/tests/m4_dashpay_contract.rs:80-96`); `NOT_FACADE` lists the watch files (`:130-137`); the stub assertion is replaced with an `invalid_argument` check (`:1223-1227`). This follows the three edits and bless sequence in `dw-m4-contract-bless-steps`. The full contract suite passed, and a surface-only `DW_BLESS=1` run found the listing already equal and made no write.

## Probes and checks

| Check | Result |
|---|---|
| Adapted round-1 catch-up proof, name listed before watch query; current head's `an_old_win_listed_before_the_watch_asks_is_stored_read` | **Pass**: username and old win read, no unread news. The original scratch patch's event-order expectation was adapted to the new sole-writer design. |
| Full first page fake, including repeated open query and resolved listed/absent results | **Pass** (`a_full_open_page_without_us_keeps_the_watch_open`). |
| More than one end-date page fake | **Pass** (`a_pass_reads_one_end_date_page`): one page limit 100, later row's end remains unknown. |
| `PASS_CAP` 16→17 mutant | **Killed**: `a_pass_asks_about_sixteen_rows_at_most` failed with 17 versus 16; `mut-cap.log`. |
| `Locked`→`Lost` mutant | **Killed**: `every_vote_state_has_one_verdict` failed; `mut-mapping.log`. |
| Remove wallet-object equality from write fence | **Killed**: `a_pass_that_outlives_its_wallet_writes_nothing` failed; `mut-fence.log`. |
| Contest-watch suite | 21 passed, 0 failed, 1 ignored public-testnet case. |
| Journal suite | 33 passed, 0 failed. |
| M4 contract suite | 9 passed, 0 failed; surface-only `DW_BLESS=1` check 1 passed, no write. |
| Formatting | `cargo fmt --all -- --check` passed. |

The three rerun probe logs are `probe-catchup.log`, `probe-full-page.log`, and `probe-end-page.log` (each 1 passed, 0 failed). The mutation status is recorded in `mutations-r2.status` (`101` means the targeted mutant test failed, as intended). `git diff --check 528bef81..7b541404` reports trailing whitespace in merged DP1-06 generated TypeScript files; that is outside DP1-04 and is not a review finding. The implementer's reported wider gate was 1,624 passed and 0 failed; this review did not rerun it.

ACCEPT
