# MG-04b slice 2b review, round 1 (Opus)

Branch `dw/mg-04b-2b`, head `8d538a65` on desktop main `372be17c`. I reviewed by reading only: no build and no runs.

## Verdict: ACCEPT-WITH-NITS

There is no M finding. Every path checks the bytes it signs, stores and sends against `BuildDissolution`, as Core builds them, for that actor, fee and penalty. The two variants can't be swapped. The full variant is never sent before its height. D23 holds both ways, because a standby send is a `dissolve` row. m-1 should still be fixed before merge: an imported standby whose fee is below the relay floor gets sent and then wedges that masternode's exits in this wallet. It is a one-line fix.

Counts: M 0, m 2, n 5.

## Findings

### m-1: `send_standby` skips the relay-fee floor that `dissolve` and `create_standby` apply, so an unrelayable imported standby wedges D23

- **Where:** `rust/crates/dw-engine/src/masternodes/shared/standby.rs:611-618` (send checks only D83's dust). The import, `:526`, checks only D89.
- **The rule elsewhere:**
  - `create_standbys` runs `check_relay_fee(&pair.full, fee)` at `:386`.
  - 2a's `plan_dissolve` runs `check_relay_fee` too (`dissolve.rs`, the `check_relay_fee(&dissolution, request.fee)?` call).
- **Sequence:**
  1. dashd builds `protx shared_dissolve <hash> 1 300 false false`: a full variant, 3 shares, fee 300 duffs.
     - Consensus takes any fee in (0, `MAX_FEE`] (`specialtxman.cpp:1762-1763`; `BuildDissolution` only needs `fee > 0`, `providertx_service.cpp:648-650`).
     - The relay floor at 1,000 duffs/kB for its ~300 bytes is about 300 duffs, so a fee slightly under that, or any fee near 100, isn't relayed.
     - The 1,000-duff floor applies only to dash-qt's fee field (`sharedmndialogs.cpp:71`), not to RPC.
  2. `import_shared_message` passes D89 and stores the standby.
  3. Past the period, `send_standby` passes `full_valid`, D89 again and the dust check. It saves a `dissolve` row and hands it off.
  4. SPV peers drop it without a definite refusal (`deliver` keeps `sent`, `ops.rs:470-480`).
- **Wrong state:**
  - The row reads `sent` forever.
    - A row from your own share has an actor output, so it isn't an unseen dissolution and the list rule never settles it.
    - `discard_operation` refuses a `sent` row.
    - A dissolve row has no abandon, because the collateral isn't the wallet's.
  - `check_no_pending_maintenance` treats `sent` as pending (`update.rs:350-358`). So this wallet's 2a `dissolve`, every other `send_standby` (even a correctly paid standby of its own at another fee) and `update_share` for that masternode all refuse `update_pending` indefinitely.
  - A resend repeats the same unrelayable bytes, and is charged penalty + fee again for an own share.
  - The share's principal (≥ `MIN_SHARE_AMOUNT`) can't be exited from this wallet until another participant dissolves.
- **Why it matters:** D83 already refuses the dust actor output for the same reason ("would not be relayed"). The relay floor is the other half of that guard, and it's missing here.
- **Fix:**
  - At `send_standby`, after `check`, refuse below `fee_for(MIN_FEE_PER_KB, tx.to_bytes().len())` with the existing "below the relay minimum" `invalid_argument`. The bytes are exact, so use the stored transaction's size, not a dummy-signature size.
  - Optionally also add a `SharedWarning`, or a refusal, at import so the user learns it before the period ends.
  - Test: import a dashd-shaped standby at fee 100 and assert the send is refused with nothing saved.

### m-2: the header-tip half of "list height or header tip, whichever is higher" has no test, so the mutant survives on the send path

- **Where:** `standby.rs:278` (`let tip = list_height.max(self.hub.tracker().tip_height().unwrap_or(0))`).
- **Why it survives:** every standby test moves validity with `list_at` (`tests/standby.rs:119-121`, used at `:421`, `:425`), which raises the list height. None sets the header tip above the list.
  - `f.tip` appears only at `:579`, in the settlement part after the sends.
  - Implementer mutants M1–M23 don't touch this line.
  - Mutating it to `let tip = list_height;` should therefore survive the engine suite. I didn't build or run this; it's from reading.
- **What the line decides:** it is one of the validator-round changes listed in the brief. It decides both D85's `shared_too_early` and m-20's `full_valid` guard when the list lags the headers.
  - With the mutant, at list 1,047 and headers 1,049, the wallet refuses the full variant that is already valid.
  - Worse, it lets `immediate` go out, paying the penalty for nothing, while the full one is stored.
- **Fix:** add a test in `send_standby_refuses_the_immediate_once_the_full_is_valid`: `list_at(1_047); f.tip(1_049);` and assert `shared_stage{full_valid}` for immediate and a send for full.

### n-1: a row's `full_valid_from` is frozen at create/import time; the file and description go stale if a reorg moves the registration

- **Where:** `standby.rs:430` and `:538` store `penalty_free_from(&mn)`. `export_standby` prints `row.full_valid_from` (`:503`), and `standby_of` returns it.
- **Failure:** the registration is reorged from block 1,000 into block 1,003. `send_standby` judges the current height correctly, since it reads `reg` fresh. The exported file, however, still says "valid from block 1,050", or "valid now" at tip 1,049, while dashd refuses it until 1,053 (`bad-prodis-penalty-floor`).
- **Fix:** print and return `penalty_free_from(&reg.mn())`. The row's column can stay as a record, but don't display it.

### n-2: `send_standby` doesn't check that the variant read back is the one asked for

- **Where:** `standby.rs:597-611`. The column chosen by `variant` is decoded and checked, but `checked.variant` isn't compared with `variant`. Only the penalty recorded follows `checked.variant`.
- **Reachability:** today's writers never put a variant in the wrong column (create compares at `:410`; import stores by `checked.variant`; M19 kills a swap at import).
- **Why bother:** the m-20 and D85 guards are reasoned on the requested variant, so a one-line `checked.variant == want` makes the send path independent of the store.
- **Fix:** compare the two and return `Internal` on a mismatch, as create does.

### n-3: create's refund comment overstates what the code does

- **Where:** `standby.rs:444`.
- **The comment:** "Nothing new stored, nothing kept: the charge goes back." The code refunds only when storing errors before the first write. A repeated `create_standby` that stores nothing (`Ok(false)` for every row) keeps its charge.
- **No funds effect:** the lease ends at `:460`, and `begin_lease` always redeems a fresh grant, so neither path has an observable effect.
- **Fix:** reword the comment, e.g. "a failed store before any write refunds", or drop the refunds as dead. The brief lists this refund as a validator-round change, but it is unobservable.

### n-4: the oracle test pins the standby count but not the refused-mutation count

- **Where:** `rust/crates/dw-protx/tests/oracle_shared.rs:813` (`assert!(mutations > 0)`).
- **Problem:** the claim is that all 24 mutations dashd refuses are refused. If a fixture regeneration drops most of them, the test still passes.
- **Fix:** `assert_eq!(mutations, 24)`, as `checked == 8` is pinned.

### n-5: the CLI's fee default is labelled "dash-qt's default", but it is dash-qt's minimum

- **Where:** `rust/bin/dwcli/src/masternodes.rs:358`. The same text appears at `:339` and `:350`, carried over from 2a.
- **Detail:** dash-qt's fee field defaults to `DEFAULT_DISSOLVE_FEE{100000}` (`sharedmndialogs.cpp:67`, `:96`), and 1,000 is `MIN_DISSOLVE_FEE` (`:71`).
- **Fix:** keep 1,000 and say "dash-qt's minimum", or switch to 100,000.

## M10 (export without the zero-charge gate)

I agree it's equivalent, with one race left out:

- `FlowKind::Dissolution` takes only `GrantKind::MasternodeOp` (`platform/lease/session.rs:33-36`).
- `issue` checks each grant's kind and wallet (`:78-91`, `:114`). For every `MasternodeOp` token it sets `signers.masternode` and a budget at the grant's cap (`:155-162`).
- `charge_tx_signer(Masternode, 0)` can then fail only through `with_entry`'s `state.signs()` check (`table.rs:871-873`): a lock or epoch change revoking the lease between `begin_lease` returning and the gate. There's no await between them, so only `lock_vault_sync` from another thread can do it, and no deterministic test can.
- Wrong-purpose, other-wallet, unknown and revoked grants are all refused inside `begin_lease`.

There's no path where a lease begins with a grant that should fail export. Don't replace the gate with `check_tx_signer`: that requires budget > 0, and the grant is 0 by design.

## What I checked

- **Funds trace, both variants:**
  - **Create.** `pair` is `BuildDissolution(…, 0, fee)` and `(…, nEarlyPenalty, fee)`, refusing the pair if either refuses (matches `createStandby` storing nothing). The vault signs the exact bytes. Each signed variant goes through `standby::check` with the variant compared, and the checked bytes are stored.
  - **Export.** It prints the stored bytes.
  - **Import.** D89, then storing the re-serialized decoded transaction under `checked.variant`, keyed by `checked.actor` and the fee derived from the outputs.
  - **Send.** It reads the requested column, re-checks it, then `HandOff` saves `signed.to_bytes()` and delivers `typed(&signed)`, the same transaction.
  - Fee derivation can't collide across variants: with P > 0 the non-actor values differ.
  - Output sums use checked arithmetic.
- **Core parity (rc.3):**
  - `MakeSignHash` (`providertx.cpp:506-533`) commits only to the transaction and payload, so a standby never goes stale under ProUpShare or ProUpSharedReg, matching `specialtxman.cpp:1802-1806` (everything is immutable after registration).
  - `CheckProDisTxForList` (`specialtxman.cpp:1676-1787`) matches `rules::check_dissolution` in order and reasons. That covers early at `:1707`, the floor at `:1734`, sum, fee and bonus, and the one signature over `MakeSignHash(tx, 1)`.
  - Validity is monotone (`:1705-1706`), so D89's check at max(tip + 1, registered + period) is sound and accepts nothing that is never valid.
  - `BuildDissolution` (`providertx_service.cpp:642-680`): version 3, final sequence, lock time 0, outputs in order, the actor's last and omitted at 0.
  - `DissolveShared`'s `next_height = tip + 1` (`:1249`) matches `spend_height`.
  - The relay floor is m-1. The dust check matches 2a's D83.
  - The deliberate divergence: Core's `broadcastStandbyDissolution` and `SharedMnReadDissolution` (`masternodelist.cpp:300-373`, `sharedmnwidgets.cpp:272-350`) broadcast any decodable ProDisTx, whereas D89 refuses consensus-valid but non-builder ones (overpaid penalty, unanimous). That is documented in D89 and m-19, so I don't count it as a finding.
- **D23:** standby sends are `dissolve` rows under `update_lock` and `check_no_pending_maintenance`. 2a's `dissolve`, `update_share` and resend see them, as the engine test at `tests/standby.rs:399-403` and regtest step S show.
- **State:**
  - **`foreign` rows.** In a wallet with no share they settle by D81's list rule, the registration's height read from the history (residual D87). In a wallet holding a non-actor share, the standby pays that share, so it is seen in the history, and any other dissolution conflicts it there; that is 2a's argument, unchanged.
  - **Reorgs and restarts.** `list_settled` is computed on every read, so a reorg relisting the masternode returns the row to `sent`, and nothing persists across a restart.
  - **Debits.** A foreign row is charged 0 on send and resend (`row_debit`); an own row is charged penalty + fee (M15–M17 kill these).
- **Prompt:** `prompt_standbys` reads the shipped `SHARED_SENDING`, needs a synced list and a listed entry, and counts any fee's row per (hash, share).
- **Store:** the appdb `ON CONFLICT … COALESCE` writes only the missing variants (D84), and the migration's CHECKs match.
- **Tests and mutants:** I read `apply.py`, mutants M1–M23 and V1–V2. The cheap survivor is m-2. The create refund (n-3) is unobservable, so there's nothing to kill.
- **Regtest step S** (`regtest/functional/dwd_shared.py`): I read it, and it asserts the brief's claims (`testmempoolaccept` floor reason, byte-equal mining, delisting, `update_pending`, then `shared_too_early`). I didn't run it.
- **Not built, not run:** I made no reflink copy.
