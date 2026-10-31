# DW MG-09 Governance tab: independent Opus-h review, round 1

- **Branch:** `dw/mg-09-governance-ui` at `ecfa0fbd`, against desktop main `e8d88078` (`git diff e8d88078 ecfa0fbd`, 59 files). Merge `da7dea96`: only its two conflicts.
- **Reviewer:** Opus-h, read-only. Builds and tests ran in a detached worktree under `/work/scratch/mg09-review/` with its own target; both are deleted.

## Verdict: CHANGES REQUIRED (Minor-only)

There is no Blocker and no Major. The engine boundary, D28 and the vote path are sound. Most of the UX matches MG-00, and the claimed evidence holds up.

Three Minors sit on the money path, so they should land before merge:

- **m1:** Pay authorizes and pays a re-read fee that the user never saw.
- **m2:** No test pins the fee grant's cap, the fee shown on the review, or the re-read. Three of my five mutants survive every functional test.
- **m3:** An error from `create_proposal` after the fee went out re-enables Pay.

Each fix is small. A focused round 2 on that diff is enough.

## What I verified

| Claim / area | Result |
|---|---|
| GL regtest at 07:24Z on ecfa0fbd | `result.json` `ok: true`. The judge (`regtest/functional/dwd_governance_ui.py:170-189`) checks **every** node: `getcurrentvotes` holds exactly one `yes:funding` vote from the sheet's masternode, and `gobject get` shows the created proposal with `CollateralHash == fee_txid` and the name, url, 2.5 amount and a wallet address that were entered. The implementer's report says it re-ran on ecfa0fbd after the review fixes. The logs don't name the commit, so I take that on the report. |
| Rust (dw-app) | My worktree ran `launch::`, `api::network::tests` and `settings::tests`: 12/12 pass, including `takes_a_collateral_rpc_on_regtest_only`. I didn't rerun the full 2537. |
| vitest | `src/governance`, `src/fixture/governance.test.ts`, `fixture.test.ts` and `src/settings`: 114/114. Typecheck clean. |
| Playwright | `e2e/governance.spec.ts` on Chromium **and** WebKit, light and dark, with axe: 30/30. I ran it on a private port (14923), because the repo's config reuses any server on 1420. Visual baselines skipped, as the brief says. |
| Engine boundary (§5) | Every call is a `governance_*` command that BL-232 already bound (`api/tauri.ts:214-228`), and no new command was added. The only new dw-app surface is `Route::Masternodes`, `UiSettings::show_masternodes` (D20, `#[serde(default)]` at struct level, so older files still read) and the launch flag (D28). No second path into the governance engine. |
| D28 `--collateral-rpc` | The flag is refused at parse unless the launch network is regtest, including the mainnet default (`launch.rs:228`). It isn't in `SessionSettings`, so neither the renderer nor IPC can set it (`network.rs:92-110`). `LaunchInfo` doesn't expose it, and nothing persists it. The engine refuses it again for any non-regtest network at open (`dw-engine/src/session.rs:264-275`). Mainnet and testnet keep `SdkSource` (DAPI) (`session.rs:474-481`). See n2 for the one IPC edge, which fails closed. |
| Double payment | The UI side holds: `busy` guards `create()`, the Pay button is disabled while busy, and the sheet can't be closed (`CreateProposalSheet.tsx:150,340,383`). React flushes discrete-event state before the next click. The engine side holds too: per-(wallet, hash) `in_flight`, a fee that is definitely unsent deletes the row and returns `Err`, and a fee that may have gone out returns `Ok` with the row kept as `fee_unsent` (`proposals.rs:445-500`). [Submit] re-broadcasts the saved bytes (same txid), so it can't pay a second fee. One residual path is m3. |
| [Submit] per MG-06 D24 | Shown for `feeUnsent`, `feePaid{0}` and `readyToSubmit{noPeerAccepted}` only (`model.ts:548-583`), which matches mg-engine §2.5. Mutant M1 is caught. |
| Vote sheet | Rows come only from `voting_masternodes`, the masternodes whose key the keychain derives. The grant is MG-05's: `credential.authorize({kind:"governance"})`, then `governance_vote(…, grant.id)` (`VoteSheet.tsx:238-258`). Nothing signs outside that. Banned rows are disabled. That matches the engine, which refuses an own vote from a banned key as `VoteRejection::Banned` (`dw-governance/src/store.rs:2175-2182`), even though MG-02 D16 counts peers' banned votes. "k of n votes sent" counts the engine's one-per-masternode casts, and the weight comes from the listed rows (`model.ts:463-480`). Refusals map to sentences (D13), with a fallback for unknown reasons. |
| Merge `da7dea96` | `App.tsx` keeps both imports. `app.css` keeps main's file byte-identical (0 removed lines against e8d88078), with the MG-09 block appended after DP4-03's. |
| Shared UI | The edits are minimal: the sidebar item and badge map (`shell.tsx`), a `governance` slot in `StatusBar`, the route, the Show Masternodes row (`General.tsx`) and `DEFAULT_PREFS`. The new global `.chip-*` and `.chip-row` classes don't collide with anything on main. |

## Mutants (mine, one at a time, restored after each)

| # | Mutant | vitest | e2e (governance.spec, Chromium) |
|---|---|---|---|
| M1 | `mineRow`: `feePaid` shows [Submit] at any n (`model.ts` `submit: s.confirmations === 0` → `true`) | **caught** | **caught** |
| M2 | The review's Fee row drops the 1 DASH burn and shows only the network fee (`CreateProposalSheet.tsx:319`) | survives | survives |
| M3 | Vote sheet: a banned masternode is eligible (`voteRows` `if (!n.valid)` → `if (false)`) | **caught** | **caught** |
| M4 | The grant cap is 100× (`maxDuffs: (fee + networkFee + margin) * 100`) | survives | survives |
| M5 | Pay doesn't re-read the preview (`await preview.refetch()` → `preview`) | survives | survives |

M2 changes pixels in the `governance-create-review` baseline, so the Docker-only visual run (`DW_VISUAL=1`) would probably flag it. No assertion checks it. The brief's third mutant (listing a masternode without a voting key) can't be made in the UI, because the engine supplies the rows. M3 is the nearest UI equivalent.

## Findings

### Minor

**m1. Pay pays a re-read fee the user never confirmed, under a cap above what was shown.** `apps/desktop/src/screens/masternodes/CreateProposalSheet.tsx:149-160`.
- **What happens:** [Pay] re-reads `preview_proposal`. In the same click it asks a spend grant of `fee + nf′ + max(nf′, 10,000)` and calls `create_proposal`, where `nf′` is the re-read network fee. The review the user confirmed shows the earlier `nf`. The new figure appears only after the payment is under way.
- **Effect:** the grant covers up to `nf′ + max(nf′, 0.0001 DASH)` more than the confirmed "1 DASH + network fee nf". The fee actually paid can differ from the confirmed one by `nf′ − nf`, and nothing bounds that difference: if the coins moved since the review (MG-06 D25), the new selection may need many inputs.
- **Amounts:** small in practice, since the network fee is in duffs. The cap is still bounded, and the lease is a `Proposal` flow. It is still a payment above what the user saw.
- **D29 says** "the re-read shows the fee that is actually paid", but it shows it after the payment.
- **Fix:**
  - If `r.data.fee !== preview.fee` or `r.data.networkFee` differs from the reviewed one, stop before `authorize`.
  - Say "The network fee changed to X" and let the next press pay. The review then shows the new figures.
  - Keep the margin; it covers create-time coin selection.
  - If the re-read fails, the code already returns before the grant (`:155`). That is correct.

**m2. No test covers the money parts of the wizard, and three mutants survive (M2, M4, M5).**
- **Missing assertions:**
  - The fixture checks only a lower bound on the grant (`fixture/governance.ts:389-390`); nothing asserts the cap's upper bound.
  - The fixture's preview always returns the same `FEE_NETWORK` (`:378`), so the D29 re-read never sees a change.
  - No e2e checks `create-review-fee`. `governance.spec.ts:162-165` only checks that the warning is visible.
- **Fix:**
  - Pull the cap into a pure function (`feeGrantCap(preview)`) with a unit test.
  - Have the fixture's `create_proposal` refuse a cap above `fee + nf + max(nf, 10_000)`, so e2e catches an over-grant.
  - Assert the review row's text: "1.00000000 … + network fee …".
  - Add a fixture switch whose second preview returns a higher network fee, and test m1's stop.
- **Brief items not covered:**
  - A **slow submit** (D17: "Sending…" on the button, row kept). No scenario or test.
  - The `governance-banned` and `governance-empty` scenarios are defined (`fixture/governance.ts:528,538`) but no test uses them.
  - There is no whole-call vote refusal (for example `governance.not_synced` or `list_unavailable` on the sheet). Only the wrong passphrase is tested.

**m3. An error from `create_proposal` after the fee went out re-enables Pay, so one more press pays a second 1 DASH fee.** `CreateProposalSheet.tsx:163-166`.
- **When it happens:** after `pay_fee` returns `Ok(hash)`, the engine still runs `self.my_proposal(wallet, &hash)` (`proposals.rs:403-404`). It can fail on `session.live()`, an appdb read or a decode, and an IPC failure after the engine returned looks the same to the renderer. The proposal and its fee exist, but the wizard shows a generic error with Pay enabled.
- **Second fee:** a new press re-previews and takes a new grant. The `in_flight` guard covers only the same draft in the same second, so a later press makes a new object with a new hash, and a second fee is paid.
- **Likelihood:** low. The passphrase field is cleared after each attempt, which slows a retry when one is asked.
- **Fix:** on any error that isn't a field refusal and isn't a definite `send.*` refusal, re-read `my_proposals` before Pay is enabled again. If a proposal with this draft's name, amount and address appeared, close the wizard and go to Mine. Otherwise keep the error. A fixture failure mode ("paid, then the read failed") would test it.

### Nit

**n1. The vote outcome is an `aria-pressed` segmented group, but UX-SPEC §4.25.4 says "segmented Yes · No · Abstain (radiogroup)".** `VoteSheet.tsx:272-286`. Commit `4dac1545` made this change on purpose ("uses the aria-pressed segmented pattern"), but there is no D-row for it. Add one, or use `role="radiogroup"`/`radio`.

**n2. D28: `SessionSettings::over` forwards `collateral_rpc` to whatever network `open_network` opens.** `rust/crates/dw-app/src/api/network.rs:107-109`.
- A regtest launch that receives `open_network(Some("testnet"))` over IPC passes the flag through, and the engine then refuses to open that network (`session.rs:264`). So this fails closed: no non-regtest network can reach the RPC source, and the renderer only ever opens the launch network (`api/tauri.ts:30`).
- This is the implementer's declined nit #7. The decline is fine, but its reason ("launch already refuses the flag off regtest") is incomplete. The guard that holds here is the engine's check at open, which D28 does cite.
- A `network == Regtest` filter in `over` would make the intent local to that function. Optional.

**n3. `lastErrorText` claims "The fee wasn't sent: …" for errors the engine records only when the fee may have gone out.** `apps/desktop/src/governance/model.ts:532-537`.
- The engine keeps `last_error` only for a maybe-sent create (`proposals.rs:478-482`) or for any resend failure ("whatever it ran into, the bytes may be out already", `:750-756`). Any text other than the `broadcast outcome unknown` prefix (`no peers`, `broadcast rejected` on a resend, an internal error) reads as a definite "wasn't sent".
- That could push a user to create a new proposal instead of pressing Submit.
- **Fix:** say "may not have reached the network" for every case. The prefix match on the engine's Display string is brittle too. It's related to declined nit #3, but this is about meaning, not wording.

**n4. Once synced, a later `synced → failed` hides the whole list and pane.** `MasternodesPage.tsx:99-121`.
- This happens when every peer is lost (`resync` goes to `waiting` the same way), even though the engine still answers reads once a run has reached `synced` (mg-engine §1).
- A sheet that is already open stays (D31), and a vote from it then gets the "Proposals are still loading" sentence.
- Consider keeping the last list under the warning notice. Not required by MG-00.

## The seven declined nits

None needed reversing:

1. **Outcome preset over valid rather than eligible rows:** fine. §4.25.4 "preselects the last vote seen".
2. **Dates in the system locale while numbers are en-US:** acceptable as is. The app formats numbers the same way elsewhere.
3. **Wording of the engine's error text:** fine as a wording decline. n3 is a separate issue of meaning.
4. **The D6 question repeating:** fine. It asks only while sync is off.
5. **A stale label on the new address after a rename:** fine.
6. **The fixture's `in_flight` refusal never clearing:** fixture-only, fine.
7. **No regtest check in `network.rs`:** fine, since it fails closed in the engine. See n2 for the reasoning.

## UX conformance with MG-00

- **The list:**
  - Rows, chips, Too close to call and margins: D10.
  - Filters with counts: All, Needs my vote, Mine (D22). Needs my vote: D8.
  - Sort and search.
- **States:**
  - The phases: disabled with Turn on (D6), syncing with counts, failed with Try again, and ready. Mine shows below the notice in the first three (D32).
  - No masternodes: Browsing only, Vote disabled. Watch-only: the notice; Create and Vote disabled (D18).
  - Masternodes without voting keys: covered by the engine's list (D4), with "No masternode in this wallet can vote on this proposal".
- **The detail pane:** Link as text with Copy (D9), and Asks for from the epochs (D23).
- **The numbers sheet** (MG-00 D20).
- **The frame:** the sidebar item "Masternodes", last, with its D3 visibility and D5 badge. Show Masternodes defaults to off and asks once to turn governance on. With no tab bar until MG-10, the title stays "Masternodes" (D1).
- **D-rows:** every deviation I found has one, except n1.
