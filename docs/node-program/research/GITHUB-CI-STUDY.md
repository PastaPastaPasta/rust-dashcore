# GitHub CI study: personal-fork CI for the node program (item 3, 2026-10-28)

Status: **study + proposal (DEC-650, PROPOSED).** Nothing here changes the process, and nothing is pushed
and no workflow is changed until pasta approves DEC-650. Scope set by pasta (10-28 rulings): pushes and
GitHub Actions are acceptable on pasta's personal public forks (PastaPastaPasta/*); dashpay/* keeps the
existing rules (full-history scrub, the §7 rules, DEC-91 conditions, pasta's OK per upstream PR).
Kept on personal forks too: the EMBARGO-1 codename-only rule, the embargo grep before every push
(DEC-239 style), nothing from the private registry or `DASH_EMBARGO_DIR` in a tracked file or a CI
secret, and the private embargo tests stay local.

Sources: GitHub docs as of 2026-10-28 (runner specs, limits, caching, billing pages), runner-images#4974,
the gate's own `steps.tsv` from 10-26..10-28 (122 tier-1 gates), and the fork repos' settings via
`gh api` (read-only).

## 3a. What must be checked before the first push to the personal rust-dashcore fork

Current state of the fork (PastaPastaPasta/rust-dashcore): public, Actions **enabled** with all actions
allowed, 10 refs (master and 8 old topic branches). No `node/*` ref exists there yet.

**What a push publishes.** Pushing `node/integration` publishes its whole history: 10,870 commits, of
which 3,468 are shared with upstream `dev` and 7,395 are newer than the R4 commit. Public repos are
mirrored and archived (GH Archive, Software Heritage), so a push cannot be taken back, and GitHub keeps
objects after a force-push.

**History finding (EMBARGO-1).** 18 commits between the R4 commit and the scrub commit still carry the
pre-scrub text (details only in the private registry). The tip tree is clean of it. pasta's ruling says
the embargo is not a blocker for personal forks, but the kept codename-only rule exists to protect
exactly that text, so the push form is pasta's choice:

| Option | What is public | Cost |
|---|---|---|
| (a) push `node/integration` history as is | everything, incl. the 18 pre-scrub commits | none; residual exposure permanent |
| (b) rewrite history to drop the pre-scrub blobs | full history, scrubbed | every SHA after R4 changes (7,395 commits): task branches, PF0 anchors and ledger, DEC/STATUS citations. Not recommended |
| **(c) CI-only snapshot commits (recommended)** | the tip tree only: `ci/<short-sha>` = one parentless commit of the tree at `<sha>`, message `ci snapshot of node/integration <sha>` | CI has no history (the gate needs none); branches are throwaway and deleted after the run |

Option (c) also lets each task READY candidate be CI'd the same way, without publishing task history.

**Pre-push checklist** (a script, `contrib/node/check-fork-push.sh`, runs it; any hit stops the push):

1. **Embargo grep, DEC-239 style, on exactly what is pushed** (tree for option c; `git log -p` of the
   pushed range for a or b): the private per-entry denylist of distinctive terms (every EMBARGO-n and
   open CANDIDATE file, not only EMBARGO-1), kept outside the repo and read at check time.
2. **Content fingerprint:** no blob in the pushed objects equals, or contains a 64-byte window of, any
   file in the private registry (repro scripts, test data, notes).
3. **Pointers stay mechanism-free:** the tip tree already names codenames (EMBARGO- in 59 files,
   CANDIDATE- in 20) and the private-test switch (`DASH_EMBARGO_DIR` or the registry path in 64 files) by
   design; those are names only. The check pins the current set and fails on new files outside it, so a
   new pointer gets a manager look before it goes public. **Question for pasta:** confirm that names
   without content are fine on the personal fork (the ruling reads that way).
4. **Commit messages, branch and tag names** of the pushed refs: same denylist; branch names never carry
   a candidate's descriptive name.
5. **Local paths and machine config:** no `.cargo/config.toml` source replacement or `file://` [patch]
   committed; `Cargo.toml` git deps resolve to public URLs (the base-sdk fork tag must be pushed first);
   no `/work/chains`, `/work/bench`, `/work/cache`, `~/private` path in a non-doc file that CI would read.
6. **Size and data:** no oracle datadirs, chain snapshots or captured mainnet databases; largest blob
   under 50 MB (GitHub warns at 50, rejects at 100).
7. **Secrets:** a secret scan (gitleaks or `trufflehog git --only-verified`) of the pushed objects; CI
   workflows get **no repository secrets** at all in stages 1–3 (they need none).
8. **Private tests in CI:** they must skip cleanly when `DASH_EMBARGO_DIR` is unset. Today the agentbox
   gate asserts it is set (the private-selftest step); the CI workflow sets nothing and runs
   `check-private-tests.sh --expect-skipped` (a new mode, stage 0) so a private test cannot run, or fail loudly, in public logs.
9. **Workflow triggers:** the inherited upstream workflows trigger only on `main`/`dev` pushes,
   PRs and cron. A `ci/*` or `node/*` push triggers nothing until our workflow exists. Scheduled
   upstream workflows (fuzz nightly, weekly masternode-seed update) run on the fork's default branch:
   check before the first push that they are disabled on the fork, so they don't open PRs or burn the
   shared 40-job pool.

## 3b. Which gate steps can run on GitHub-hosted runners

Measured tier-1 step wall times on agentbox (10-28 gates, warm sccache, 12 build jobs): `test` 1,400 s
avg (36 runs), `pure` 810–1,060 s, `oracle-diff` 590–1,160 s, `planted` 100–1,070 s, `clippy`
210–920 s, `test-build` 210–440 s, `rdashd-build` 20–250 s, the 11 script checks 1–55 s each.

| Step | GitHub? | Why |
|---|---|---|
| fmt, deny, pins, r2, layers, groups, self-devdep, no-libm, public-api, sb12a-captures, release-refs | **yes** | pure scripts and cargo metadata; self-devdep's script moves from scratch into `contrib/node/` first |
| protocol-tables | **yes** | needs a public dashpay/dash checkout at the pinned tag |
| clippy, test-build, rdashd-build | **yes** | plain cargo |
| test (incl. dashd integration suites) | **yes, sharded** | `setup-dashd.py` downloads public dashd releases; shard by `test-durations.json` (test-run.py already schedules from it); private tests skip (3a item 8) |
| pure, planted | **yes** | plain cargo with features |
| oracle-diff | **local (stage 3+: maybe)** | links the Core oracle shim built from `node/oracle-shim-*` Core branches, which are never pushed (§6.4, DEC-09), and pinned by archive sha256 for gcc 15.2 / binutils 2.46, which hosted ubuntu-24.04 doesn't have. Moving it needs pasta's OK to push the shim branch to the personal dash fork plus an ubuntu 26.04 container |
| PF0 / perfgate, bench-test | **local** | `perf stat -e instructions:u` does not work on GitHub-hosted runners: perf_event_open fails with EPERM on Azure VMs (runner-images#4974, closed as a platform limitation; still reported 10-2026). Larger runners are org-only. The PF0 ledger is anchored to agentbox counters anyway |
| private-tests, private-selftest | **local** | the embargo rule |
| nightly test-full, oracle datadir suites, fuzz farm / fuzz-smoke beyond a short smoke, CodeQL combs of rust-dashcore | **local** | `/work/chains` data (pasta's, too large), DEC-184 long fuzz legs; a 60 s fuzz-smoke could move later |

So about **90% of tier-1 CPU can move** (everything except oracle-diff), and with it all the
disk-heavy `target/` churn in the gate slots.

## 3c. Runner and cache limits against the gate

**Runners (standard GitHub-hosted, personal account on Pro):**

| | Public repo | Private repo |
|---|---|---|
| Linux x64 / arm64 | 4 vCPU, 16 GB RAM, 14 GB SSD | 2 vCPU, 8 GB, 14 GB |
| Windows x64 / arm64 | 4 vCPU, 16 GB, 14 GB | 2 vCPU, 8 GB, 14 GB |
| macOS (M1 arm64) | 3 vCPU, 7 GB, 14 GB (Intel images: 4 / 14 / 14) | same |
| Minutes | **free, unlimited** for standard runners | included $18/month (≈ 3,000 Linux 2-core minutes); then Linux 2-core $0.006/min, Windows $0.010, macOS $0.062 |

- **Concurrency:** 40 concurrent jobs across all of pasta's repos (pasta's figure; the docs list 40 for
  Pro). The docs give one per-OS sub-cap: **macOS 5** concurrent; Linux and Windows share the 40 with
  no separate cap. The pool is shared with dashwallet-desktop CI (≈10 jobs per run, 1–2 macOS).
- **Job time:** 6 h per job; 35 days per workflow run; a job queued 24 h is cancelled.
- **Cache:** 10 GB per repo by default on every plan, evicted after 7 days unused, and LRU beyond the
  cap. The owner can raise it, up to 10 TB for user repos, billed only above 10 GB (≈ $2.80/month at
  50 GB, $13.30 at 200 GB); this needs a payment method on the account.
- **Larger runners:** only for Team or Enterprise organisations, so not available on a personal
  account.

**Fit with the measured gate cost (3.5–7.9 CPU-h per tier 1):**

- **Disk is the real constraint.** A gate slot's targets are 60–113 GB. 14 GB per runner means each shard
  builds only its crates (`-p` sets), with `debug = "line-tables-only"` in a CI profile, and the runner
  image's unused toolchains cleared first. Stage 2 measures the largest shard before anything depends
  on it.
- **Compile duplication:** cold shards each rebuild their dependency closure. Expect 20–30 CPU-min
  compile per shard on 4 vCPU without cache, so a 16-shard tier 1 is about 8–10 runner-hours and
  25–40 min wall. A 10 GB cache can't hold 16 shards' targets; sccache with the GitHub Actions cache
  backend under a raised cap (≈ 200 GB, ≈ $13/month) cuts that by half or more. Raising the cap is
  pasta's call.
- **Shard count vs the 40-job pool:** agentbox ran 30–50 tier-1 gates/day this week (122 over
  10-26..10-28). 40 shards per gate would need 40 × 0.5 h × 40 gates ≈ 800 job-hours/day of a
  960/day pool. So **16 shards per gate** (≈ 2 gates in flight plus desktop CI), and GitHub tier 1
  runs only on READY candidates, train cuts and integration merges. Intermediate fix-loop gates stay
  local (or skip tier 1).
- **Private repo:** not viable. A 16-shard tier 1 on 2-vCPU runners is ≈ 700–1,000 runner-min ≈
  $4–6 per gate, so the $18 included lasts about 4 gates; at 30–50 gates/day that is $4,000–9,000/month.
  The ruling makes the public personal fork the path, and this study assumes it.

## 3d. Staged rollout (proposal; each stage needs the previous one green for a week)

**Stage 0 — local prerequisites (no push).** Write `check-fork-push.sh` (3a) and its selftest. Move
`gate-self-devdep.py` into the repo. Make private tests skip under `--expect-skipped`. Disable the
fork's inherited scheduled workflows. pasta picks the history option (3a; recommendation c).
CPU saved: none.

**Stage 1 — base-sdk CI on PastaPastaPasta/base-sdk.** The fork already runs upstream's per-package
workflows on pushes, and the DEC-239 post-merge push of `node-sdk-integration` already happens. Add
the fork branches (`node-sdk-integration`, `bsdk/*` READY-PUSH branches) to the workflows' triggers on
the fork only, never in a branch bound for dashpay/base-sdk. Include upstream's CodeQL job, so a
READY-PUSH branch gets its comb on GitHub. Run 1 week in shadow (local comb still required), compare
verdicts. Then retire the **local CodeQL comb** for fork CI (kept locally only right before a dashpay
PR push, per DEC-91, until CI parity has 2 weeks of evidence). Saves the heaviest local job (~55 GB
RAM per comb, one at a time) and frees the comb queue.

**Stage 2 — rust-dashcore on PastaPastaPasta/rust-dashcore, GitHub-hosted tier 1.** A fork-only
workflow `node-gate.yml` (in a `ci/` path that upstream never sees) runs on `ci/*` snapshot pushes:
16 shards by `test-durations.json`, plus one lint job (fmt, deny, pins, layers, groups, self-devdep,
public-api, no-libm, protocol-tables, release-refs, sb12a-captures, clippy) and one
planted/pure/rdashd-build job. It reports one summary check. Locally, the gate keeps oracle-diff, PF0,
private tests and nightly. Two weeks of shadow (both run; verdicts compared, flakes logged as BLs),
then GitHub tier 1 replaces the local tier-1 lanes for READY and train gates. **CPU saved: ≈ 90% of
tier-1 CPU**, about 3–7 CPU-h per gate. At 30–50 gates/day that is ≈ 100–300 CPU-h/day, roughly
10–25% of the box, plus 60–110 GB per gate slot of `target/` (two of the three slots can go).

**Stage 3 — the daily upstream sync as a PR** (see `UPSTREAM-SYNC-DAILY.md`, DEC-649). The daily
sync branch is pushed as a snapshot and its PR opened **inside the fork** (base: a fork mirror branch
of the integration tip snapshot; never against dashpay/*). Fork CI is the sync's gate, and its PR
thread is the conflict log. Retires the local sync gate run.

**Later, each needing its own pasta OK:** oracle-diff on GitHub (the shim branch on the personal dash
fork plus an ubuntu 26.04 container), and branch protection with required checks on the forks.

## Desktop (item 1 scope): dashwallet-desktop on real PRs

PastaPastaPasta/dashwallet-desktop is public, and `ci.yml` already runs on `push` to `main` and
`dw/**` and on `pull_request` (Linux, macOS, Windows jobs). `main` has no branch protection. The
switch needs no workflow change:

- the implementer pushes `dw/<task>`; the manager opens a PR to `main` (`file-pr` conventions, body file
  per repo and branch);
- the PR merges with `gh pr merge --merge --match-head-commit <sha>` after CI is green and the review
  is ACCEPT;
- the manager owns the PR watch (babysit).

Optional and pasta's call: protect `main` with the CI checks required. Windows Swift is a known
non-blocking gap, so it would be excluded from the required set. This keeps DEC-13's
manager-merges rule; only the merge vehicle changes. Proposed as part of DEC-650 stage 1 because it
touches no node history.
