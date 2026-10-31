# MG-04b slice 1b, review r2 (Opus-h, narrow: fix round 1)

Branch `dw/mg-04b-1b` at 63f4c1b5 (9 signed commits on the r1 head 8dc2809a). The worktree is read-only, so every
build and run happened in the reflink copy `/work/scratch/mg04b-1b-rv2-opus`. Its target was seeded from
`/work/scratch/dw-mg-04b/target`, then every source file was touched. The copy is left in place for the manager.
Paths are relative to `rust/crates/` unless noted.

## Verdict: ACCEPT-WITH-NITS

Every r1 finding is closed. Each fix has a test, and I checked that the test fails when the fix is reverted: ten
env-gated reverts in one build, each killed by the intended test (table below). The switch is a single constant. All
five calls that hand off shared bytes refuse while it is off: the four facade calls, plus `resend_operation` of an
`update_share` row. They refuse before they write or charge anything. Nothing in dw-app (Tauri), dw-ffi or dw-console
reaches them, and no automatic rebroadcast exists. Outside a debug `test-support` build, nothing can bypass the
switch. The D76 keying, `copy_rank` and the holder parameter add no path that frees a coin that is still unspent, or
holds one coin under two rows. The three nits are about docs and test strength.

Counts: **0 M, 0 m, 3 n.**

## r1 findings: closed, each checked by revert

| r1 finding | Fix | Revert in the copy (`A6MUT=n`) | Killed by |
|---|---|---|---|
| Sol M-1 (switch) | c10912e1 | 6: the switch reads on in the shipped facade | `test_QT_126_sending_is_off_as_shipped`, `test_QT_127_update_share_sends_an_operation`. dwcli: `…_fresh_wallet_cannot_send`, `…_refusals_…` (abandon, `mn_shared_cli.rs:259`) |
| Validator NIT-1 (resend) | 7e89e531 | 7: the `update_share` resend isn't gated | `test_QT_127_update_share_sends_an_operation` |
| Opus M-1 (mutant 10) | 7b4c4d0a | 1: held coins out of `protx_context`'s collateral set | `test_QT_126_a_reserved_coin_is_no_mg04_collateral` |
| m-1 (refresh) | d9b149c5 | 2: `prepared_row` passes `None` | `mg04_a_refresh_reuses_its_own_unspent_input` |
| m-2 (malleated copy) | 9d855d0d | 3: `session_hash` keys on the envelope's txid first | `test_QT_126_a_registration_mined_under_another_txid_is_confirmed` |
| NIT-4 (`copy_rank`, sent copy) | 63f4c1b5 | 4: the sent preference dropped | `test_QT_126_a_stale_block_copy_does_not_move_the_sent_registration` |
| NIT-4 (`copy_rank`, ChainLock) | 63f4c1b5 | 5: the ChainLock rank dropped | `test_QT_126_a_chainlocked_or_mined_copy_ranks_first` (a unit test of `copy_rank`) |
| n-5 (MG-04 ops follow D70) | e1be6246 | 8: a conflicted op releases at once | `mg04_a_conflicted_update_lets_the_next_one_through` |
| n-3 (5 deep) | 660f356b | 9: `buried` one block early | the n-3 assertion, plus `…abandon_…releases_once_buried` and `mg04_a_conflicted_update_…` |
| n-2 (abandon stage) | 660f356b | 10: refuses `sent` for everyone | `test_QT_126_abandon_spends_a_reserved_coin_and_releases_once_buried` |

The remaining r1 nits were checked by reading:

- **n-1:** the doc comment now says "charged the debit, D71" (`shared.rs`).
- **n-4:** the held-coin test asserts both outcomes: `send.amount_exceeds_balance`, and then exactly the new coin.
  The `if let Ok` is gone.
- **n-6:** D72 now says the refusal is intended.
- **§12:** now says mutant 10 guards a reachable state and names its killer.

## Findings

### n-1: the contract and D71 don't list the `update_share` resend's refusal

- **Where:**
  - `docs/contracts/mn-engine.md:129`, the `resend_operation` row;
  - `:138-146`, where §2.4 says "the four calls" and names `unsupported` "right after the wallet check";
  - `docs/design/MG-04b-shared-masternodes.md:1072`, where D71 names the same four.
- **What:** 7e89e531 makes `resend_operation` of an `update_share` row refuse `unsupported{feature:
  "shared_masternodes"}` while the switch is off (`dw-engine/src/masternodes/protx.rs:668-672`). Only the code's doc
  comments say so (`shared.rs:63-71`, `protx.rs:656-658`). The refusal also sits in a different place: after
  `claim_operation` and `op_row` (so after the id's `invalid_argument`, `in_flight` and the row-not-found refusal), not
  right after the wallet check, because the row has to be read to learn its kind.
- **Scenario:** a frontend written from the contract gets `masternodes.unsupported` from `resend_operation` with no
  documented cause, and shows it as an internal error.
- **Fix:** add the refusal and its position to the `resend_operation` row and to D71. The code is right as it is.

### n-2: dwcli's `update-share` refusal doesn't test the switch

- **Where:** `rust/bin/dwcli/tests/mn_shared_cli.rs:261-275`.
- **What:** dwcli's `update-share` runs `preview_update_share` first, to size the grant
  (`bin/dwcli/src/masternodes.rs:1076`). On this test's fresh datadir v24 reads `unknown`, so the preview refuses
  `masternodes.unsupported` through `v24_context` (`shared.rs:845`) before `update_share` is ever called. The
  assertion passes whatever the switch says.
- **Checked:** with the switch forced on (`A6MUT=6`), the test fails at the `abandon` assertion (`:259`). That
  assertion does test the switch, since `abandon` has no v24 gate. The engine test `test_QT_126_sending_is_off_as_shipped`
  covers `update_share` with v24 active, so nothing is uncovered. The dwcli line only overstates what it shows.
- **Fix:** say in the comment that `update-share`'s `unsupported` here is v24's, or drop that line. Alternatively,
  rely on the engine test for `update_share` and keep `abandon` as the dwcli proof.

### n-3: nothing in CI runs the release check's `--release` leg; with two in-block copies, the list is better evidence than `copy_rank`

- **The release check:**
  - CI runs `cargo test --workspace` in debug only (`.github/workflows/ci.yml:127-128, 447, 532`). The debug legs of
    `tests/mg04b_release_check.rs` do run there, because dwcli's dev-dependency turns on `test-support` under
    workspace unification. Those legs are the probe and the source scan, which now covers `bin/` and every other crate.
  - The leg that proves the override absent from a release build (`:6`, plus the `const` assert) runs only by hand.
  - Even so, a bypass is structurally unlikely. The override is behind `cfg(any(test, all(debug_assertions, feature =
    "test-support")))`. No release or dist profile turns `debug-assertions` on (`rust/Cargo.toml:103-114`).
    `with_shared_send` is `pub(crate)`. No shipped crate names the public wrapper.
  - The brief accepts the leg not being run. The gap is only that nothing will ever run it.
  - **Fix (optional):** add the `--release` leg to `nightly.yml`, where a cold release build is affordable.
- **`copy_rank`:**
  - With two in-block copies and no ChainLock, preferring the sent copy (63f4c1b5) is a guess, and so was the lowest
    block before it. Which record is stale depends on which branch won, not on which copy was sent.
  - Funds aren't at stake. Either copy reads `confirmed`, both spend the same inputs, so releasing at once is right
    either way. Holds come from the stored columns, which D76 doesn't touch.
  - The cost of a wrong pick is `update_share` naming a hash the list doesn't hold. Core refuses that at the
    hand-off, and the lease refund applies.
  - A ChainLock settles it within seconds on Dash.
  - **Fix (optional, 1c or later):** where the synced list holds one of the copies' txids, prefer that copy in
    `shared_confirmed`/`held_shares`. The list is the ground truth for the ProTx hash.

## The switch, checked

- **One switch:** `SHARED_SENDING` (`shared.rs:72`), `false`.
  - Release builds read it directly (`shared_can_send`).
  - Test and debug `test-support` builds read `Masternodes.shared_send`. `NetworkSession::masternodes()` sets that
    from the constant (`masternodes/mod.rs`), and only `with_shared_send`, through
    `test_support::shared_masternodes_with_send_override`, sets anything else.
  - Flipping the constant fails the engine, contract and dwcli tests.
- **Every sending path is gated:**
  - The only callers of `sign_funding`, `send_registration`, `abandon_session` and `send_share_update` are the four
    facade calls. Each runs `sending?` right after `require_wallet` (`shared.rs:677, 725, 775, 886`).
  - `resend_operation` gates `update_share` rows (`protx.rs:671`).
  - Other operation kinds (MG-04's own) are correctly not gated.
  - No follower path signs or hands off anything. I found no rebroadcast of session or operation bytes.
  - dw-app, dw-ffi and dw-console call none of these. dwcli's `sign`, `send`, `abandon`, `update-share` and `resend`
    all go through `session.masternodes()`.
- **Nothing written while refused:** the shipped-facade test leaves all three wallets' rows byte-equal and sends
  nothing. `require_sending` is evaluated before `protx_op`, but its error only surfaces after the wallet check, as
  the contract says.

## Funds safety of the new code

- **D76 (`session_hash` prefers the history's registration):**
  - **No early release:** `confirmed` needs a history transaction in a block that spends exactly the template's
    inputs, so those inputs are spent whichever copy it is.
  - **Same payload in every in-block copy:** each in-block copy carries this wallet's own SIGHASH_ALL signatures,
    which cover the payload. Not comparing the payload (BL-187) therefore lets no other registration match.
  - **Excepted copy is the read copy:** `conflict_height` excepts exactly the copy `derive_stage` reads. Any other
    in-block copy would have ranked above a copy outside a block, so a sibling copy can't make the session read
    `conflicted`.
  - **Release follows the same hash:** `releasable` uses the same hash.
  - **Holds:** they are stored columns (`reserved`, `fee_inputs`), and D76 doesn't change them. No double hold is
    possible.
  - **Reorg effects:** a reorg can change which copy is chosen. That moves `pro_tx_hash`, `held_shares` and
    `update_share`'s target (n-3), not any lock.
- **`copy_rank`:** the order is a ChainLocked copy, then in a block, then sent, then the lowest height, then the
  lowest txid. It is total and deterministic, which also fixes the r1-era `find_map` over a `HashMap`, an arbitrary
  pick when a participant held two copies.
- **`protx_candidates(…, operation)`:**
  - Only `prepared_row` passes `Some(id)`. `prepare_external_registration` uses a fresh `ops::new_id()`, so nothing
    is excepted there. The refresh excepts only its own row.
  - `HELD` counts only `reserved` and `fee_inputs`, and an `External` collateral is never a wallet coin, so the row's
    collateral can't become a fee input.
  - A coin held by both this row and another one stays locked after `release_lock`, and `CoinSource::Any` still
    excludes it.
  - All of this runs under the fence (D68).
- **n-5 (`settle_operation_locks` waits for `buried`):** the conflicted op's inputs are spent by the conflicting
  transaction, so the longer hold costs nothing. `update_pending` (D23) doesn't count `Conflicted`, so a buried
  wait doesn't block the next update (`mg04_a_conflicted_update_lets_the_next_one_through`).
- **576bebfe (`conflict_height`):** behaviour is unchanged. The old `state_of` also required `block_info()`, and the
  shared copy had the same filter.

## What I ran

- **Build:** `cargo test -p dw-engine -p dwcli --features dw-engine/test-support --no-run` in the copy
  (`build.log`), with `CARGO_BUILD_JOBS=8`, the target and TMPDIR inside the copy, and `/work` at 281 G free before
  the build.
- **At the head** (logs `eng-head.log`, `it-head.log`, `dwcli-head.log`):
  - dw-engine lib `masternodes:: coins::`: **125 passed**;
  - `mg04b_release_check` (debug, `test-support`): **3 passed**;
  - `mn_masternodes_contract`: **8 passed**;
  - dwcli `mn_cli`: **3 passed**;
  - dwcli `mn_shared_cli`: **5 passed**.
- **Reverts:** `mutate.py` in the copy applies ten env-gated reverts behind `crate::mutant(n)` in one build
  (`build-mut.log`).
  - `A6MUT=0` passes 125/125.
  - Reverts 1–10 each fail exactly the tests in the table (`mut1.log` … `mut10.log`).
  - dwcli with `A6MUT=6` fails both switch tests (`dwcli-mut6.log`).
- **Not run:**
  - the `--release` leg of the release check (a cold release build, out of scope);
  - the T1 regtest (BL-501);
  - the full workspace gate.
