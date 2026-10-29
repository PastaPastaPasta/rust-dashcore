# Continual upstream sync: proposal (item 2, 2026-10-28)

Status: **proposal (DEC-649, PROPOSED). It amends PLAN §6 only once pasta approves.** It changes
nothing today and pushes nothing. The SW-UPSTREAM catch-up in flight (step A on node/train-46) finishes
under the current §6 first.

## Why

- The weekly cadence in §6.1/§6.3 lapsed from 10-07 to 10-28 (no STATUS row owned it).
- Upstream moves faster than a weekly batch absorbs cheaply:
  - rust-dashcore `dev`: 88 commits in the last 30 days; `node/integration` is 62 behind today.
  - base-sdk `develop`: 381 commits in the last 30 days.
- Large batches mean large conflicts. A daily merge keeps each one to a few upstream commits, and a
  conflict names its upstream author while the change is fresh.

## Cadence

| Repo | Daily (automatic) | Lands on the integration branch |
|---|---|---|
| rust-dashcore `dev` → `node/integration` | fetch; `git merge --no-ff upstream/dev` onto a **standing `node/sync` branch** (rebuilt each day from the integration tip); run the cheap checks; write the report | the next train cut that day, if the sync is green and moves no bench input (below). Otherwise batched, at most twice a week, on a re-anchor train |
| base-sdk `develop` → `node/base-sdk` | tag the head (§6.3 step 1); rebase a **candidate `node/base-sdk-next`** onto `develop`; `just test lint` | the pin move (`bump-base-sdk.sh`) is batched with the rust-dashcore re-anchor train, at most twice a week, or sooner when a needed fix lands upstream |

Each fetch is a no-op on days upstream did not move. Keeping §6.1's "never rebase node/integration"
and §6.3's tag-before-rebase rule makes the daily cadence safe. `node/sync` is disposable: rebuilt
from the tip each day, never a task base.

## What lands when, without a daily PF0 re-anchor

- DEC-604 already skips PF0 when no bench input changes. The daily report computes the bench-input
  closure (perfgate's `bench_inputs`) for the sync merge:
  - **closure unchanged** (most days: upstream touches dash-spv, FFI, key-wallet, CI): the sync rides
    the day's train as one more `--no-ff` merge. It runs the train's normal tier-1 gate and no PF0.
  - **closure changed** (e.g. `dash/`, `hashes/`, a `Cargo.lock` move under a bench crate): the sync
    waits on `node/sync` and is merged at most twice a week (Mon/Thu) on a train that re-anchors PF0
    once for everything it carries (incl. any base-sdk pin move). Every row of a changed package is
    re-anchored (the PF0 re-anchor rule).
- A security fix upstream lands off-cycle as today (§6.1).
- **Lag bound:** `node/integration` is never more than 4 days behind upstream `dev`, and the train merge
  of a green sync is never skipped for bench reasons alone.

## Who owns it

- **A standing SYNC workstream** with a STATUS row that never closes. One delegate thread (Sonnet for
  the mechanical merge and the checks, escalating to Opus-h on a non-trivial conflict). It is woken by
  a daily systemd timer (`node-sync-daily.timer`, 05:30Z, before the first train) that runs the
  script and sends a `t3_thread_send` only when the script reports a conflict or a red check.
- **The manager** owns the train merge (single writer of node/integration) and the heartbeat line
  `sync: rdc lag N / bsdk lag M / last green <date>` (from
  `git rev-list --count node/integration..upstream/dev`). A lag above 2 days is a heartbeat flag.

## Cheap conflict triage

1. **rerere** (already on): a resolution recorded once is reused every day, and the daily report lists
   every rerere replay with `git diff --cached` so a stale replay is seen.
2. **Pre-classification by path**, from the §6.2 table, in the report:
   - upstream-only paths → take upstream (automatic);
   - additive-carry paths (bridge feature, BS15/V24a/BS18, WL1/WL5, `ci-groups.yml`, root
     `Cargo.toml`) → the delegate re-applies the carry;
   - "upstream fixed it differently" → flagged to the manager; upstream wins unless BS2 or the
     conformance tests say otherwise.
3. **Overlap report:** `git merge-tree --write-tree` against the integration tip plus every open task
   branch head. It names which in-flight tasks a sync will conflict with, so their owners follow-merge
   the same day instead of at their merge.
4. **Escalation:** a conflict not resolved within one working day aborts that day's sync (§6.1's rule:
   integration never goes red), opens an `adapt to upstream <sha>` task, and the next day's sync
   retries on top.
5. **Cargo.lock:** regenerate with `cargo update -p` for the conflicting packages only (§6.2), then pins
   and deny.

## Checks per daily sync (cheap; the full gate runs on the train)

- `cargo metadata`, `fmt --check`, `deny`, pins, layers, groups;
- `cargo check --workspace --all-targets`;
- the dash-spv dashd suites, only when `dash-spv*` changed;
- BS2's regtest set, only when `dash/`, `hashes/` or `key-wallet*` changed.

About 15–40 CPU-min, against 3.5–7.9 CPU-h for a tier-1 gate. Once the GitHub CI stages are approved
(DEC-650 stage 3), the daily sync becomes a PR inside the personal fork and these checks run there.

## PLAN §6 amendment (text to apply on approval)

- §6.1 Cadence: "Daily, automatic, onto `node/sync`. Lands on the next train when green and
  bench-input-neutral (DEC-604), else at most twice a week on a re-anchor train. Lag bound 4 days."
- §6.1 Procedure step 1: merge onto `node/sync` (rebuilt daily from the integration tip); the train
  merges `node/sync` with `--no-ff`.
- §6.3 Cadence: "Daily rebase of `node/base-sdk-next` + `just test lint`; the pin move is batched
  with the rust-dashcore re-anchor train." Tag-before-rebase and the DEC-239 push rule unchanged.
- §6.4 is unchanged.
- New §6.5: SYNC workstream ownership and the heartbeat lag line, as above.
