# DW MG-09 Governance tab: independent Opus-h review, round 2 (narrow)

- **Branch:** `dw/mg-09-governance-ui` at `03f15967`, which is 5 commits on `ecfa0fbd`. All 5 are signed (`%G?` = G). `git diff ecfa0fbd 03f15967` covers 12 files, +485/−59, and has no Rust changes.
- **Scope:** the round's diff and the round-1 findings (`DW-MG-09-r1-opus.md`).
- **Reviewer:** Opus-h, read-only.
  - Everything ran in a detached worktree, `/work/scratch/mg09-review/r2/wt`, with its own e2e cache.
  - Playwright ran in the pinned container, capped at `--cpus 8 --workers=6`.
  - No cargo build was needed, because the round touches no Rust. The worktree and cache are deleted.

## Verdict: ACCEPT WITH NITS

All seven round-1 findings are closed. I traced every path brief item 1 lists to `governance_create_proposal`. None of them can:
- pay a fee the review's "You pay" row didn't show;
- ask a spend grant above `feeGrantCap(confirmed)`;
- pay a second 1 DASH fee for a proposal the engine already holds.

The snapshot logic holds under every ordering I could produce, including the engine's own ordering (refusing the passed superblock), which I ran by changing the fixture. There is no Blocker, Major or Minor. The four nits below are about test coverage and wording, and none of them touches money.

## What I verified

| Claim | Result |
|---|---|
| vitest 996 | **996/996** (73 files), and typecheck is clean. |
| Playwright | `e2e/governance.spec.ts` on Chromium **and** WebKit, light and dark, with axe: **46/46**. `routes` + `coinjoin` + `settings*` on both browsers: **278/278**. Visual run (`DW_VISUAL=1`, Chromium, `-g governance`): **6/6**, which includes the new create-review baselines with "You pay". I didn't rerun the full 1519. |
| Rust 2537/0 | Not rerun. The round changes no Rust file, so the r1 basis stands. |
| GL regtest at 09:25Z on 03f15967 | `result.json` has `ok: true` and `source: {head: 03f15967cc4b…, dirty: false}`. The new `source_head()` (`dwd_governance_ui.py:63-66`) records it from `git -C HERE`. `6-create-review.png` shows Fee "1.00000000 + network fee 0.00000235" and "You pay 1.00000235". |
| r1 mutants M2, M4, M5 killed | **All killed**, each run alone (Chromium e2e). M2 (Fee row drops the burn) fails `create light/dark` on the new `create-review-fee` text. M4 (grant ×100) fails 6 create tests, because the fixture now refuses a cap above fee + nf + max(nf, 10k). M5 (no re-read) fails the D33 "fee changed at Pay" test. |
| D28 corrected text | Accurate. It names the parser (`launch.rs`) and the engine's refusal at open (`session.rs`) as the guard for `open_network` over IPC. |
| D36 engine claim | Confirmed. `require_synced` keys on `last_synced_at` (`dw-engine/src/governance/reads.rs:91-101`), and votes go through `require_relaying` (enabled + `require_synced`, `voting.rs:184`). The fixture now matches. |

## 1. The round-1 findings

**m1, closed.** In `CreateProposalSheet.tsx:193-231`, `confirmed` is the snapshot the review renders when Pay is pressed (`figures`, `:130`).
- **Unchanged fee:** Pay re-reads, then compares with `sameFee` (`:204`). Only on a match does it authorize `feeGrantCap(confirmed)` (`:211`). The cap is never sized from a figure the user didn't see.
- **Changed fee:** the re-read writes the new figures into the snapshot and sets `feeChanged`, then returns before `authorize` (`:204-209`).
- **The confirm press after a change:** the new figures, the warning and `busy=false` (from `finally`) land in one batched render. So the button is enabled only in a frame that already shows the new "You pay".
- **Re-read error:** it returns before the grant (`:202`).
- **Residual, accepted:** the margin. The engine may pay up to `max(nf, 10,000)` duffs above "You pay" if create-time coin selection differs from the re-read milliseconds earlier. D29 documents this, and r1 asked to keep it.

**m2, closed.**
- `feeGrantCap`, `feeTotal`, `sameFee` and `createdFrom` are pure and unit-tested (`model.test.ts`).
- The fixture refuses an over-cap grant (`fixture/governance.ts`, create handler), and its network fee can be moved.
- The e2e asserts the Fee and "You pay" texts.
- New tests cover the slow Submit (D17), `governance-banned`, `governance-empty`, and a whole-call vote refusal (`governancePhase`).

**m3, closed.** On any error from `create_proposal` other than a field refusal:
- `setBusy(false)` and `checkMine`'s `setAfter({checking})` happen in the same microtask, so they batch, and no frame shows Pay enabled.
- While `after !== null`, Pay and Back stay off (`:466`, `:479`), and `create` returns early (`:196`).
- If the engine row is there, "was made", and Pay stays off. Then "Show it in Mine" → Mine's Submit re-broadcasts the saved bytes (same txid).
- If Mine is unreadable, it shows "unknown" with Check again, which reuses the **original** `known` set (`:425`).
- If there is no row, the engine says nothing was paid: `pay_fee` keeps a row on every maybe-sent path and deletes it only on `Outcome::NotSent` (`proposals.rs:445-500`). Pay may come back.
- No IPC timeout exists on the renderer or in dw-app (`api/tauri.ts`, `api/governance.rs:165-176`). So the engine has always finished when `checkMine` reads.

**D34's match can't miss a real row.**
- The engine's `MyProposal` fields round-trip the draft exactly. `proposal_json` copies name, url and address. `format_money` → `parse_amount` is exact. `payments_requested` is `ceil((end − start)/cycle)` = `ceil(count·cycle/cycle)` = `count` (`dw-governance/src/proposal.rs:136-147, 266-273`).
- A just-created row is `feeUnsent` or `feePaid{0}`, both `inProgress`.
- `feeFailed` means another confirmed transaction spent the fee's coins, so excluding it can't cause a double payment of a live proposal.
- The one theoretical miss is a row `my_proposals` can't decode (`create_proposal`'s own read failing on decode, then Mine skipping the row). The engine has just written that row with its own encoder, so I treat it as unreachable and raise no finding.

**n1, closed.** `OutcomePicker` (`VoteSheet.tsx:202-235`):
- `role="radiogroup"` with `role="radio"` and `aria-checked`;
- one tab stop (the chosen outcome, else Yes);
- arrow keys move and choose, and Space/Enter choose via the button.

The themed vote-sheet tests run axe with an outcome checked, in light and dark, on Chromium and WebKit: 0 serious. D37 records it.

**n2, closed** (D28 text). **n3, closed** (`model.ts:569`, D35, test updated). **n4, closed:**
- `listed` (`MasternodesPage.tsx:63`) keeps the list under the notice once `lastSyncedAt` is set.
- D32 still holds for a first sync, since the `governance-syncing` and `governance-failed` scenarios have `lastSyncedAt: null`, and the test checks Mine below.

## 2. The snapshot (755517ff, 144cf4a0, 03f15967)

**What it's keyed to.** The key is `JSON.stringify(draft)`, which covers name, url, address, amount (duffs), count, first height and coins. The fee and network fee are the snapshot itself. A background re-read (engine event) never replaces it (`:126`). The D33 e2e checks this, and my mutant A below confirms it.

**A stale snapshot is never paid.** A press pays `confirmed` only after a re-read **for the same draft** matches it (`:203-204`). The draft paid is the `draft` captured with `confirmed` in the same render.

**"Pay off until the new preview loads" holds:**
- Once `draftKey` moves, `figures` is undefined until the effect stores the new key's first preview. Pay's `disabled` and `create`'s guard both read `figures`.
- `preview.data` belongs to the current key, because there is no `placeholderData`, and `gcTime: 0` drops the old entry.
- If the new preview never loads or is refused, Pay stays off.

**The engine's ordering (D33), run by changing the fixture.** I made `draftProblems` refuse passed heights the way the engine does (`upcoming(12, passed)`), then ran two orderings:
- the list held, so the old draft's preview re-read lands first;
- both re-read together.

Both ended on **Payments with `first_payment` flagged and 0 `create_proposal` calls**. The background refusal goes through the effect at `:143-147`. If it lands while a press is in its passphrase step, the `shownKey` check after `authorize` (`:212`) stops it. If it lands later, the engine refuses the create's field, so no fee is paid either way. With the list re-read first, the field had already moved to the next superblock (Dec 10), and was still flagged (Nit 3).

## 3. Mutants (this round, each run alone, restored after)

| # | Mutant | vitest | e2e (governance.spec, Chromium) |
|---|---|---|---|
| A | Pay confirms the live fee, not the snapshot (`const confirmed = figures` → `preview.data`) | survives | **caught**: "a background re-read never changes the figures…; Pay still stops" |
| B | The draft match drops the "held before Pay" exclusion (`!known.has(p.hash)` → `known.size >= 0`) | **caught**: `createdFrom … new Set([made.hash])` | survives (no e2e has an older identical proposal) |
| C | Drop the "draft moved during the re-read" stop (`:203`) | survives | survives |

The brief suggested "Pay uses the live fee after a confirm" as `feeGrantCap(r.data)`. That form is equivalent at that point, because `sameFee` has just held, so I used the meaningful form, A. The second `shownKey` check (`:212`, the draft moving during the passphrase step) is also untested: no governance test holds the credential call. See Nit 2.

## Findings

### Nit

**N1. D34 can match an older proposal when Mine hadn't loaded at the press.** `CreateProposalSheet.tsx:197`, `model.ts:537-548`.
- **Cause:** `known` is built from `mine.data ?? []`. If Mine was still loading or failing when Pay was pressed, `known` is empty. An older in-progress proposal with the same name, link, address, amount and count would then be taken as "this call's".
- **Effect:** the notice says it was made, `credential.setError(null)` hides the real error, and Pay stays off. That's the safe direction: no second fee, just a hidden failure.
- **When it can happen:** only with "Another address…" (a new address is fresh per sheet) and a duplicate draft.
- **Fix (optional):** gate Pay on `mine.isSuccess` as well, or treat `mine.data === undefined` as "unknown" rather than an empty `known`.

**N2. The draft-moved stops (`:203`, `:212`) have no test.** D33 claims "a press whose draft moved during its re-read or passphrase stops there". Mutant C survives, and the `:212` check has no test either.
- **Production impact:** none on money. Without these checks the engine refuses the passed height at create, so no fee is paid.
- **The fixture hides it:** it validates passed heights, so there the stale draft would be created.
- **Suggested test:** `hold("governance_preview_proposal")` (or the unlock command), move `superblocksPassed`, emit, release, then expect 0 creates.

**N3. The engine's ordering of D33 has no test, and its Payments step gives no reason.** `fixture/governance.ts:82-85, 272`.
- **The deviation is honest:** it is documented, and the path it hides is safe (I ran it, §2).
- **What the user sees:** when the old draft's re-read wins, they land on Payments. The first payment is already moved to the next superblock but still flagged "Choose one of the next superblocks.", with no sentence saying the chosen one passed. The `create-first-moved` warning exists only on the other ordering.
- **Suggested fixes:**
  - a fixture switch that refuses passed heights like the engine, plus one e2e test of that ordering;
  - clearing the `first_payment` error when the superblock effect moves `first`.

**N4. A resync after `synced` (D36) is tested only as `failed`.** `governance-peers-lost` covers synced → failed. Synced → syncing (with `lastSyncedAt` set) uses the same `listed` branch but has no scenario. A one-line scenario would pin it.

## 4. Regressions outside the round

- **Vote sheet (D31):** the only change is `OutcomePicker`, and the themed vote-sheet, "keeps its result after the row leaves", whole-call refusal and banned tests pass on both browsers.
- **Mine while syncing or failed (D32):** still shown below the notice for a first sync (the states test).
- **Sidebar, Show Masternodes and routes:** `routes` / `coinjoin` / `settings*` pass 278/278.
- **CSS:** `app.css` adds one rule (`.mg-outcomes .segment[aria-checked="true"]`), scoped to the picker.
