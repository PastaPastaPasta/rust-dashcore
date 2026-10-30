# DW DP6-03 DashPay card in Tools ▸ Information: Opus review, round 1

Branch `dw/dp6-03-dashpay-card` at `83ac8383` (base `8b0772ec`, 6 commits, 24 files, +1097/−17). The reviewer's worktree
was detached at `/work/scratch/dp603-review/wt` and has since been removed. Scripts are in `/work/scratch/dp603-review/scripts/`,
and logs are in `/work/scratch/dp603-review/*.log`.

## Verdict: **ACCEPT WITH NITS**

No Blocker and no Major. I found two Minors. Both make the card look less healthy than it is (or overpromise on copy), never
healthier. Neither needs to block. Fix them in a short r2 or record them as D-rows/follow-ups.

## What I verified (claims)

| Claim | Result |
|---|---|
| GATE EXIT 0 on 83ac8383 | `gate.log`: `head 83ac8383 dirty=0`, windows-paths ok, `advisories ok`, Rust `tests passed 2537 failed 0` (DEC-243 solo reruns, D17), vitest 74 files / 1016, Playwright 1633 passed / 559 skipped, visual 260. **Confirmed.** |
| 14/14 mutants killed | The implementer's `mutants.log` is **stale**: it shows 10/10, from before M11–M14 were added. I reran all 14 in my worktree: **14/14 killed** (`mutants-r1.log`). |
| D1: no engine change or binding | The diff touches no Rust. The three `Backend` methods wrap commands that were already generated (`identity.rs:485/491/497`). **Confirmed.** |
| Visual: only the card's 4 baselines | The diff adds 4 PNGs and modifies none. **Confirmed.** |
| D13 parity | Recounted the IOS rows: before 12/59/49/3 (the summary row said 58/50, so it was already wrong), after 12/60/48/3 = 123. IOS-114 → partial is right: the DashPay part is built, Platform-address and Shielded sync info are not. **Confirmed.** |

## Answers to the brief's questions

**1. State mapping against the engine.**
- *Healthier than it is?* No.
  - `trust.rs:307-313`: the degraded mode and the toggle both give `Trusted`, and enforcing before masternode sync gives `TrustedFallback`. All three map to Degraded/warning, and only `spv` is Running.
  - With the status unread, only `startup === "ready"` counts as an identity (`dashpayInfo.ts:214`). That is the bring-up's own finding, not a guess.
  - The stop path quiesces the loops before `SpvStateChanged{running:false}` (`bringup.rs` `stop_spv` → `stop_platform` → `stop_spv_inner`). So the D6 re-read after a stop never sees running loops: there is no Running-after-stop window.
  - Watch-only is never offered an enabled Sync now. It comes from the summary or from `noIdentity{watchOnly}`. The latter is derived from the vault's `has_wallet_secret` (`startup.rs`/`banner.rs:72`), so a wallet whose keys were detached is covered too.
- *Identity shown as Not set up?* Only when BL-153 itself says `noIdentity`. `derive_status` (`banner.rs:65-89`) never returns that while a main identity, or one on file, exists. The card trusts the designated source.
- *D5 (every identity wallet Degraded under DEC-168):* Right, and required. DASHPAY §2.2 (`DASHPAY.md:297`): "Tools ▸ Information and the release notes say that Platform data is verified against Dash's quorum service". R7 says "the gap disclosed". A warning tone (not danger) is proportionate.
- *D15:* Verified in `bringup.rs` around line 570. `identity.is_none() && no_identity && !rerun` records `NotRun` and returns, so the `noIdentity` status with a `notRun` outcome is real.

**2. Sync now (D3).**
- `dashpay_sync_soon` → `kick_dashpay_sync` (`runtime.rs:724-747`). It does nothing unless the supervisor runs, and the spawned task re-checks `loops_on`.
- The library's `sync_now` does a CAS on `is_syncing` (`dashpay_sync.rs:345-352`), and the balance kick uses `try_lock` (`balances.rs:234`). Repeated presses therefore coalesce: there is no rate-limit or re-entrancy hazard.
- The disabled state is right in every state. While a request is in flight, `asking` also holds the button.
- The 5 s re-read is safe. Each press re-arms one timer, unmount clears it, and `useEffectEvent` reads the current wallet. Concurrent invalidations coalesce in TanStack.
- What the button *promises* is weaker than what it does: see m2.

**3. `events.ts`.**
- No new subscription, so nothing can leak or subscribe twice. The change is one extra key in the existing bridge's sync case (`events.ts:42`).
- The `dashpayStatusAll` predicate is matched by object identity, so the new key gets a plain invalidation and no other query changes behaviour.
- At ≤ 4 Hz sync events, only a *mounted* card refetches: one in-memory IPC read per event. Inactive entries are only marked stale.
- `events.test.ts` is updated, and my R16 (the key removed) is killed.
- Follow-merge: MG-04, MG-07 and DP6-01 editing the same `return` line will conflict textually. It's trivial to resolve.

**4. Fixture fidelity (D11).** Every scenario is reachable except the two `lastPass` ones (see n6), which D4 documents.
- `dashpay-info-spv`'s `ready` with 2 pending is reachable, though only via a later refused drain (`bringup.rs:862-866`). The library returns `PartialAccountsPending` whenever the queue is non-empty at bring-up (`platform-wallet manager/startup.rs:438`).
- Missing scenarios: identity + incomplete outcome (the common real case for pending crypto), and identity + bring-up running (m1).

**5. Accessibility.**
- I wrote my own spec, `scripts/zz-review-dp603.spec.ts`: 21 axe cases per browser. It covers degraded, look-failed, pass-failed, watch-only, stopped, identity-unsettled, waiting-for-sync, both error states (sync read failed, wallet list failed), light and dark.
- Each case runs the page's serious/critical check, plus an axe run scoped to the card at every impact level.
- Ran together with `dashpay-info.spec.ts` and `tools.spec.ts`: **230/230 passed on Chromium and WebKit**.
- The held button has name "Sync now", the `[disabled]` state, and an accessible description equal to the visible note, in both engines. The enabled button has no `aria-describedby`. ARIA snapshots are in `e2e-review.log`.
- Reading order: the heading, then the dimmed button, then `status` (chip + headline), then the reason paragraph. That's acceptable for D3's native `disabled` choice.

**6. Mutants.**
- View model, by vitest (`scripts/mutants_review.py`, `mutants-r1.log`): 28/30 killed.
  - Survived: **R1** (Not set up beats a failed pass, the requested precedence swap) and **R11** (a failed pass beats watch-only). Both pair a failed `lastPass` with no identity or with watch-only, which the engine can't produce today (D4). See n1.
  - The other direction of the failed-vs-not-set-up swap, for the *bring-up's* failures, is an equivalent mutant: `!identity && !absent` (`dashpayInfo.ts:220`) makes the two branches mutually exclusive.
  - Ignoring `trusted_quorums_only`: in the headline it's **R2, killed**; in the row it's **R3, killed**.
- Component, by Playwright on Chromium (`scripts/e2e_mutants.py`, `e2e-mutants-r1.log`):
  - E1 (Try again always re-reads the sync status): **killed** by the wallet-list test.
  - E2 (a failed wallet list is not an error): **killed** by the wallet-list test.
  - E3 (no re-read after Sync now): **killed**.
  - E4 (no `refetchOnMount`): **killed**.
  - E5 (validator r2's old `!id` rule): **survived**.
  - So D18's "no mutant of that line fails them" is accurate only for E5: see n3.

**7. Parity.** Correct (see above).

## Findings

### Minor

**m1. A wallet with an identity shows "Stopped" while DashPay is starting, and gives the wrong reason.** `apps/desktop/src/tools/dashpayInfo.ts:233`, copy at `:82`.
- **During the bring-up.** `supervise` (`bringup.rs` around 230-250) runs every wallet's bring-up *before* SPV starts and *before* `start_loops`. So on every launch, a wallet with an identity on file reads `startup: "starting"` and loops not running. The card shows "Stopped · DashPay sync is stopped" with Sync now held, while its own Identity scan row says "Looking for identities…". D16 was written to remove exactly this kind of self-contradiction.
- **Right after SPV starts.** `SpvStateChanged{running:true}` is emitted in `start_spv_inner` (`session.rs:784`) before `mark_running` and `start_loops`. `start_loops` reaps old loop threads in `spawn_blocking`, up to 1 s each. So the D6 re-read usually lands before the loops run, and the card corrects itself only on the next `sync` or `platform` event. During sync that comes within a second. On a caught-up wallet it can take minutes.
- **While an old pass drains.** On the `LOOP_RETRY` path (30 s retries, `bringup.rs:60`) there is no event at all. In that case "They start when the wallet connects to the Dash network" is false: the wallet is connected.
- This never shows the card healthier than it is.
- *Fix:*
  - With an identity and `startup === "starting"`, show Starting (busy) rather than Stopped.
  - Soften `loopsStopped`, e.g. "…They run while the wallet is connected to the Dash network."
  - Optionally add a fixture scenario for an identity with the bring-up running.

**m2. Sync now's confirmation overpromises.** `dashpayInfo.ts:84` ("This card shows it once it reports"), `DashPayInfo.tsx:23`.
- `last_pass` is always `None` (D4), and a pass that finds nothing sends no signal (D6).
- The only visible effect is the DashPay loop's "last run". The library sets it at the pass's *end* (`dashpay_sync.rs:393`), and the card sees it only if the pass ends within the single 5 s re-read.
- A DAPI pass over every wallet routinely takes longer, so the user is told the card will show something it won't.
- *Fix:* reword, e.g. "A DashPay pass was asked for. It runs in the background; the contacts loop's last run changes when it ends." Or re-read again later. A follow-up under F1 is also fine.

### Nit

- **n1.** R1/R11 survive. D2's order between a failed pass and Not set up or Watch-only has no test. Add one vitest case per pair now; the model is pure, so it's cheap. Otherwise the order goes unpinned when F2 fills `last_pass`.
- **n2.** D14 says 24 axe tests per browser; it's 26 (13 states × 2 themes). The axe list omits `dashpay-identity-unsettled`, `dashpay-waiting-for-sync` and both error states. I ran all four: clean.
- **n3.** D18: "no mutant of that line fails them" is too broad. E1 and E2 are killed by "D7: a failed wallet list says so…". Only validator r2's pre-fix rule (E5) survives, and it's equivalent on the fixture. Reword.
- **n4.** Refresh `/work/scratch/dw-dp6-03/mutants.log`: it shows 10/10, so the 14/14 claim had no log behind it. My rerun gives 14/14.
- **n5.** The "Contacts waiting for an unlock" label (`dashpayInfo.ts:201`): the engine documents `contact_accounts_pending` as builds waiting "for an unlock or the DIP-15 rescan" (`startup_status.rs:91-92`). A neutral label such as "Contact accounts waiting to be built" would be accurate.
- **n6.** Fixtures and coverage:
  - `dashpay-info-spv` and `dashpay-info-pass-failed` (`scenarios.ts:663/673`) carry a `lastPass` the engine can't produce today. D4 says so, but a comment in `scenarios.ts` saying they are forward fixtures (F2) would help.
  - There is no e2e/fixture scenario for an identity with an incomplete outcome (`ready` status + `partialAccountsPending`/`discoveryFailed`). Vitest covers it (M14).
- **n7.** Sync now stays enabled in Failed (bring-up), Starting and Not set up. A DashPay pass doesn't re-run discovery, and the scan note itself says "It looks again the next time the wallet starts". The click is harmless but invites the wrong expectation. Consider holding it, or adding a note, when no identity is known.
- **n8.** If the `trusted_quorums_only` read fails, the card shows the degraded-mode copy for a session that has the toggle on. Both are warnings, so this is cosmetic.
- **n9.** `events.ts:42` is shared with MG-04, MG-07 and DP6-01: expect a one-line textual conflict on follow-merge.

## Reproduce

```
cd <detached worktree>/apps/desktop && pnpm install --frozen-lockfile
python3 /work/scratch/dp603-review/scripts/mutants_review.py "$PWD"
cp /work/scratch/dp603-review/scripts/zz-review-dp603.spec.ts e2e/
DW_E2E_CACHE=/work/scratch/dp603-review/e2e-cache scripts/e2e-docker.sh --reporter=line --workers=6 \
  e2e/zz-review-dp603.spec.ts e2e/dashpay-info.spec.ts e2e/tools.spec.ts
python3 /work/scratch/dp603-review/scripts/e2e_mutants.py "$PWD"
```
