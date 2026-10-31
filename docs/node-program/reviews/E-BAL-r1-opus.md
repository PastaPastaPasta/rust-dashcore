# E-BAL review, round 1 (Opus): engine balance 150 DASH below Core after a reload

- **Write-up:** dashwallet-desktop `docs/research/e-bal-balance-vs-core.md` (31fb4eb, branch `dw/e-bal-balance-bug`).
- **Commits reviewed:**
  - rust-dashcore `node/fix-rescan-record-refresh` (on node/integration a7b32c83): 63943b06 (fix 1, settle loop), e1173381 (fix 2, re-emit corrected record), e6ecfd61 (dashd test). Worktree `/work/home/agent/workspace/rdc-wt/fix-rescan-record-refresh`.
  - Backport `node/fix-rescan-record-refresh-v4.2-core-v24` (on 40268cc0): 36a14719, f7de5520, a1f0a3ad.
  - platform `dw/fix-rescan-utxo-persist` (on bc41f1bc23): 67d49b4401 (store coins of `updated` records), 3f915835ea (restore `is_coinbase` on load). Worktree `/work/home/agent/workspace/dw-wt/e-bal-platform`.
  - dw `dw/e-bal-balance-bug`: e97b417 (l1-balance regtest, strict xfail), 31fb4eb (write-up). Read, not run.
- **Reference:** Dash Core `CWallet::AddToWalletIfInvolvingMe` (identical in v23.1.8 and v24.0.0-rc.3) and `MarkUnusedAddresses` / `TopUp` in `scriptpubkeyman.cpp`.
- **Branch state:** read-only. Mutation runs used `git archive` exports under `/work/scratch/ebal-review/`. TMPDIR=/dev/shm/ebal-review-tmp.

## What I ran

| Check | Result |
|---|---|
| `cargo test -p key-wallet -p key-wallet-manager`, integration branch | all pass (key-wallet lib 679) |
| same, v4.2 backport | all pass (key-wallet lib 688, 18 ignored) |
| `cargo test -p dash-spv --test dashd_sync -- --test-threads=4`, integration (eval `setup-dashd.py`, dashd 23.1.0, regtest v0.0.4) | **36/36 pass**, incl. `test_sync_wallet_paid_past_the_gap_window_in_one_transaction` |
| `cargo test -p dash-spv-ffi --test dashd_sync -- --test-threads=4`, integration | 7/7 pass (the `every block record during historical sync should arrive via inserted` assertion still holds) |
| new dashd test on v4.2 backport | pass |
| `cargo fmt --all --check`, both rust-dashcore branches | clean |
| `cargo test -p platform-wallet -p platform-wallet-storage --no-fail-fast`, platform branch at its pin (40268cc0) | 2174 pass, 0 fail; the three new tests pass |
| platform branch with rust-dashcore patched to the v4.2 backport (all 12 git crates `[patch]`ed), `-p platform-wallet -p platform-wallet-storage -p platform-wallet-ffi` | **2593 pass, 0 fail** (9 ignored): the shipping combination |
| **Mutation: fix 1 disabled** (settle loop returns after the first pool top-up, i.e. old behaviour) | `test_transaction_paying_past_the_gap_window_is_recorded_whole` FAILS ("mempool: every output is ours"); the dashd test FAILS at `tests_basic.rs:68` with "coins no emitted record owns: [f28d18a6…:3, :5, :13, …]" while the balance assertion on line 44 passes, i.e. exactly the E-BAL shape |
| **Mutation: fix 2 disabled** (`adopt_owned_outputs` not called) | both `test_redelivered_*` tests FAIL; the fix-1 test still passes |
| **Mutation: platform projection change and coinbase restore reverted** | all three new platform tests FAIL (`core_bridge.rs:7321`, `:7374`, `sqlite_core_state_reader.rs:877`) |

## 1. Fix 1: the extend-and-rematch loop (`settle_match_over_extended_windows`)

`key-wallet/src/transaction_checking/wallet_checker.rs:68-113`, called at `:388` on every path that records or confirms (after the IS early return and after `update_state` / relevance gates).

**Termination: yes.** Each pass either returns or adopts a rematch whose `involved_address_count` is strictly larger (`:104`). That count is bounded by the transaction's inputs + outputs (+ payload payout) per matched account, and the loop also returns when a pass derives nothing (`:91`) or when no output pays a newly derived script (`:98-101`). The number of passes is about (highest index the tx pays) / gap_limit, each pass costs one `check_transaction` over the tx.

**Core parity: matches, and is order-independent where Core is not.** Core marks keypool keys used output by output, topping up after each (`AddToWalletIfInvolvingMe` → `MarkUnusedAddresses` → `TopUp`), so a later output in `vout` is tested against the keypool the earlier outputs extended. Core also stores the whole transaction and evaluates `IsMine` per output when it computes credit, so outputs the top-ups reach are credited. The fixpoint here gives the same owned set for any output order. Differences that remain are window size (30 vs Core's 1000) and the late-reveal cases in §4.

**Foreign outputs: none.** Rematches run the unchanged account matcher against scripts derived from the wallet's own key sources (`NoKeySource` accounts derive nothing). An output becomes "ours" only if its script is in one of our pools.

**Cost on large wallets: neutral to better.**
- Per relevant tx the extra work is a `HashSet` of the scripts derived in that pass and one scan of `tx.output`; mark-used and gap maintenance ran before too.
- The rematch only runs when the tx pays a just-derived script, or carries any special payload.
- Before the fix, each extension needed a separate filter rescan pass to re-apply the block and extend again (a tx paying 30k consecutive addresses took k rescans). The loop collapses that to one derivation round and one rescan, which helps against bottleneck 3 (rescan volume) in the large-wallet note.
- An attacker who knows the xpub can make the wallet derive about 30 × (number of outputs) addresses with one dust-heavy transaction, which also raises the BIP158 false-positive rate. The old rescan cascade reached the same end state, only more slowly, so this is not new exposure. Core has no cap either.

Findings:

- **nit — every special-payload tx rematches** (`wallet_checker.rs:98`). `special_transaction_payload.is_none()` short-circuits only for payload-less txs, so every coinbase (CbTx payload) that derives an address runs an extra `check_transaction`. On a mining wallet that is one per block. It's cheap, but restricting this to the payloads that name scripts (ProReg / ProUpReg / ProUpServ payout, AssetLock credit outputs) would match the comment.
- **nit — the IS backfill path is not settled** (`wallet_checker.rs:320-370`). When an IS lock arrives for a known tx and an account matches without a record, `record_transaction_with_observed_spends` runs on the unsettled match, and that path does no mark-used or gap maintenance at all (that part is pre-existing). The block confirmation heals it through settle plus fix 2, so this is only worth a comment.
- **nit — dry-run checks (`update_state = false`) still report only the head of such a transaction.** That's unavoidable without mutating pools. Worth a doc line on `check_core_transaction`.

## 2. Fix 2: re-emitting a grown record as `updated`

`managed_core_funds_account.rs:774` and `adopt_owned_outputs` at `:1344`. On a confirmed re-application it adds or re-roles outputs whose addresses are now in the match, then recomputes net and direction (the #1082 formula) and returns the record. The role assignment mirrors `record_transaction`'s receive/change split for every account type, including CoinJoin (all involved), so a re-application that learns nothing returns `None`; the "third time: no event" assertion covers that. I found no spurious-churn path.

Consumers:

- **key-wallet-manager / dash-spv-ffi:** pass-through. `BlockProcessed.updated` already carried confirmations, and the docs are updated (`events.rs`, `callbacks.rs`). All 7 FFI dashd tests pass. Balance is a post-event snapshot everywhere, so nothing double-counts balance.
- **platform-wallet SQLite projection:** see §3.
- **Mobile (FFI projection):** derives `utxos_added` from every slice, including `updated` (`core_wallet_types.rs:519-605`). A grown record re-emitted long after its first sighting re-delivers all of its owned outputs, including ones spent since.
  - Swift: keeps a row spent if its spender link is at InstantSend or higher, or if a verdict arrives. The engine supplies `ObservedSpent` / `Uncredited` for those outpoints, so Swift is safe in practice.
  - Kotlin: has no verdict slot (`rs-unified-sdk-jni/src/persistence.rs:208-221`). It resets any spent row that has no spender link (`PlatformWalletPersistenceHandler.kt:1063-1086`), and its restore then loads that row. Rows without links are sweep holds and legacy flags, so this is rare, but fix 2 makes re-delivery of old records more frequent. See m4.
  - No SDK sums `net_amount` or raises "received" notifications from these events. Transaction rows are txid-keyed upserts.
- **dash-evo-tool:** accumulates records by txid. `emit_received_utxos` uses `inserted` only, so a late output gets no "received" signal. That's acceptable.

Findings:

- **minor m1 — a correction carries only the changed account's slice, and the BlockProcessed projection then overwrites the wallet-level transaction row with a partial fold.**
  - `confirm_transaction` returns `Some` only for accounts whose record changed. When account B's late output is adopted on a tx that account A funded, `updated` holds B's slice alone.
  - The BlockProcessed arm folds only the event's slices (`core_bridge.rs:1258-1271`) and upserts `record_blob` unconditionally (`core_state.rs:84-93`). The FFI does the same per txid.
  - The stored row then turns from "internal, −fee" into "incoming, +X". It's history-only, not balance.
  - The TransactionDetected arm already avoids this by rebuilding from the manager's slices (`core_bridge.rs:1131-1171`).
  - This is new on the v4.2 line through fix 2. On integration, #1082's late-input corrections already had the same shape.
  - Fix: for txids in `updated`, rebuild the folded row from `wallet_slices_and_verdicts_for_txid`, as TransactionDetected does, or make key-wallet emit every slice of a corrected txid.
- **minor m2 — the DashPay incoming-payment entry keeps the first-sighting amount.** `record_received_payment_totals` skips any txid it already has (`payments.rs:877`), so a corrected record for a DashPay receiving account never updates the payment amount. It's rare, since it needs a contact to pay past our DashPay window. Allow an amount increase for the same txid.
- **minor m3 — no dash-spv-level test drives fix 2.** e6ecfd61 collects only `TransactionDetected` and `BlockProcessed.inserted` (`tests_basic.rs:47-60`), so it pins fix 1 only. The commit message ("Before the two key-wallet fixes…") overstates this. Fix 2 is covered by key-wallet unit tests and by dw's l1-balance (dashd v24), but no rust-dashcore test runs it through the real filter-rescan path. Suggestion: a TestChain shape like dw's NEAR/PAST/MOVER, or at least a sentence in the test doc saying it covers fix 1 only.
- **minor m4 — Kotlin re-delivery reset** (see above). This is a platform follow-up: wire the verdict slot through JNI, or keep `isSpent` on re-delivery when there is no verdict and no link.

## 3. Platform: storing coins of `updated` records, and the coinbase restore

**67d49b4401** (`core_bridge.rs:1302-1315`): `new_utxos` gains `derive_new_utxos(updated)`, filtered to outpoints with no credit verdict. A verdict is absent exactly when the owning account's live `utxos` holds the coin at drain time (`utxo_credit_verdicts_from_wallet`, `:1726-1768`).

- **Spent-coin resurrection: no new path found.**
  - `UPSERT_UTXO_SQL` (`core_state.rs:764`) does set `spent = 0` on a materialised spent row, so any ungated `new_utxos` write can resurrect a coin.
  - The new write is gated on the engine holding the coin, and in-session the engine re-credits a spent outpoint only if `is_outpoint_spent`, `observed_spent` and `spent_before_funded` all miss it (`update_utxos`, `managed_core_funds_account.rs:290-345`).
  - Within one changeset `new_utxos` is applied before `spent_utxos` (`core_state.rs:116-185`), so a merged batch ends spent.
  - A spend drained before a later `updated` re-delivery can't be undone, because the engine no longer holds the coin at that later drain.
  - After a reload the engine knows no records (`rehydrate.rs:264`), so re-applied records come back as `inserted`. That's the old, ungated path (risk 2), not this change.
- **The race fix is real.** A mempool `TransactionDetected` can be drained after a chainlocked block has pruned the record. Its projection then finds `Some([])` and writes nothing (`core_bridge.rs:1151-1160`). Before this change the `updated` record was the coin's only carrier, and it was ignored. `chainlocked_confirmation_stores_the_coin_of_a_pruned_mempool_record` reproduces it, and fails with the change reverted (below). It's reachable whenever a CLSIG for height h is applied before block h is processed, so `process_block` sees `all_chainlocked`.
- **Coinbase (3f915835ea):** `load_state` collects `is_coin_base()` txids from the blob-backed rows (`core_state.rs:957-959`) and uses them for `core_utxos` rows (`:1019`). Coinbase records always travel as `inserted` with their blob, `record_blob` is never nulled for a blob-backed row, and an immature coinbase is within 100 blocks of the stored tip, so the blob exists. A height-only row (no blob) loads as non-coinbase. That can only happen for a UTXO delivered without its record, which a coinbase never is. Correct.

## 4. The author's open risks

| Risk | Assessment | Ship? |
|---|---|---|
| **R1: late output in a tx a chainlock already finalized** | Real and pre-existing. `confirm_transaction` returns before crediting (`managed_core_funds_account.rs:730`), so the output stays uncredited in memory too, and the wallet diverges from Core, which keeps the wtx and evaluates `IsMine` per output. It needs (a) a tx partly recorded on first sight, which after fix 1 means outputs with an index hole larger than gap_limit, and (b) its block re-applied after a covering chainlock was applied in-session (`apply_chain_lock` drops the record into `finalized_txids`). After a *reload* it does not happen in dw: records are not rehydrated into the engine, so the re-applied tx is new and is recorded whole. Fix later by having the finalized branch fall through to `update_utxos` + a fresh `record_transaction` for outputs not credited and not spent, emitted as `inserted`. | **follow-up** |
| **R2: SQLite ignores verdicts for `inserted`** (#992 class, phantom UTXOs) | Real and pre-existing, and not made worse; fix 1 reduces out-of-order cases. The cure is cheap and in the same function: apply the same `!cs.utxo_credit_verdicts.contains_key(..)` filter to the `inserted` loop (`core_bridge.rs:1238-1240`) and to the TransactionDetected arm (`:1193`). The verdicts are already computed over those records, and every verdict means the engine does not hold the coin. | **follow-up** (strongly recommended next, since dw ships the SQLite store) |
| **R3: rescan obligation lost mid-sweep** (#979 item 2) | Real, and broader than stated (major M1). | **follow-up** |

- **major M1 (follow-up, not a blocker) — fix 2 only fires while the earlier tx's filter batch is still uncommitted.**
  - New scripts are re-tested only against `active_batches` (`reconcile_untested_scripts`, `dash-spv/src/sync/filters/manager.rs:731-766`, before commit).
  - The filter manager's `BlockProcessed` handler ignores `new_scripts` (`sync_manager.rs:173-197`), and nothing re-tests committed batches unless a wallet's `synced_height` falls below `committed` (`sync_manager.rs:220-239`).
  - So a later payment that reveals an output of a tx whose batch has already committed leaves that output undetected, with no restart involved. Examples are any post-sync reveal, or a reveal more than about a batch later during initial sync. Core credits it.
  - The write-up (§8 bullet 3) and e1173381's message describe the gap only as "stops mid-sweep" / "rescan after a restart", which undersells it. dw's l1-balance passes because NEAR/PAST and MOVER are one block apart.
  - This comes from reading the code; I didn't reproduce it.
  - Not a blocker: the reported E-BAL loss (the split) is fully fixed by fix 1, which does not depend on rescans.
  - Ask: correct §8 and the e1173381 message, and open an issue for "re-test committed ranges for scripts derived after commit (bounded by the derived address's first-use height)". Bottleneck 3 makes an unbounded version unaffordable.

## 5. Backport

`git range-diff a7b32c83..e6ecfd61 40268cc0..a1f0a3ad`: **faithful.** The differences are all deliberate adaptations:
- The v4.2 line's `key_source_for_account_type(&to_account_type_to_check(), account_index())` is kept in `maintain_pools_for_match`, with the same placement and semantics as the line's original loop.
- Imports are trimmed because v4.2 lacks `attribute_late_inputs`.
- `recompute_net_and_direction` / `direction_for` are carried over from #1082. Their logic is identical to v4.2's inline direction code in `record_transaction` (`managed_core_funds_account.rs:901-916`), so first sighting and correction agree.
- The docs drop "input or", since late-input corrections don't exist on v4.2.
- The test commit is identical.

- **minor m5 — the v4.2 line can't correct the *spender* of a late-adopted output.** v4.2 has no `attribute_late_inputs` (#1082). A transaction that spent a late output before it was credited keeps a record without that input, so its net is too high by the output's value. Balance is still right: `observed_spent` blocks the credit, and the verdict blocks the store. Only history is wrong, and only in dw. Note it in the write-up's §8, or backport #1082's `attribute_late_inputs` with it.

## 6. Tests

- The new key-wallet tests are well aimed:
  - Fix 1 is tested in all three contexts (mempool, block, chainlocked block) and checks that the window extension is reported to the rescan.
  - Fix 2 covers the incoming case and the Sent→Received flip, plus the "nothing new, no event" idempotence.
- Each fails without its fix (mutation runs above).
- The dashd test fails without fix 1 in exactly the E-BAL way: the balance matches, but outputs are missing from the event stream.
- No regressions: 36 dash-spv dashd tests, 7 FFI dashd tests, and all key-wallet / key-wallet-manager tests, on both lines.
- Platform: see the table below.
- dw `test_l1_balance.py`: read only. It is strict-xfail, documented, and covers fix 1, fix 2 and coinbase against dashd v24 across a reload, which is the right acceptance test. Not run here (needs a patched dwcli build).

### Platform mutation and patched runs

| Run | Result |
|---|---|
| Platform at its pin (40268cc0), the branch as is | 2174 pass, 0 fail |
| Projection change (`core_bridge.rs:1302-1315`) and `is_coinbase` restore reverted | `block_processed_updated_record_stores_the_coins_the_wallet_holds`, `chainlocked_confirmation_stores_the_coin_of_a_pruned_mempool_record` and `coinbase_maturity_survives_reopen` all FAIL |
| Platform with the v4.2 backport patched in (`platform-wallet`, `-storage`, `-ffi`) | 2593 pass, 0 fail, 9 ignored. No platform test assumes `updated` means "confirmation only". |

## Verdict

**APPROVE-WITH-NITS** for shipping the fix as is: no blockers, and every test passes. M1 is a follow-up with a doc correction. Before or with the pin bump, I recommend R2's one-line verdict gate for `inserted` (follow-up) and m1 (partial-fold overwrite).
