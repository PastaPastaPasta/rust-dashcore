# CI on the personal fork: the pre-push check

DEC-650 (§3a and §3d of `research/GITHUB-CI-STUDY.md`) and DEC-670 put rust-dashcore CI on
PastaPastaPasta/rust-dashcore. A push there is public and permanent. This section covers Stage 0,
the local prerequisites. Nothing here pushes, and no workflow is changed.

## What is pushed

DEC-670 picks history form (c). Each CI run pushes one parentless commit of a tip tree, with the
message `ci snapshot of node/integration <full sha>`, as the branch `ci/<12-hex short sha>`, and
deletes the branch after the run. No history is published.

The snapshot's `.github/workflows/` holds one file, `node-gate.yml`, so none of the inherited
upstream workflows (`rust.yml`, `fuzz.yml`, `update-masternode-seeds.yml` and the rest) runs on the
fork (DEC-700). `snapshot <rev> --workflow <rev>:<path>` takes it from a branch, for example
`node/ci-stage2:.github/workflows/node-gate.yml`, and `--workflow <file>` takes it from a file.
Without the flag it is `contrib/node/node-gate-minimal.yml`: `cargo fmt --check` and
`cargo check --workspace --locked`, for the first pushes before the Stage 2 workflow lands.

## `contrib/node/check-fork-push.sh`

```
contrib/node/check-fork-push.sh snapshot <rev> [--workflow <rev>:<path> | <file>] # the form in use
contrib/node/check-fork-push.sh range <remote-ref>..<rev> --ref <branch> # real history (not used now)
contrib/node/check-fork-push.sh --selftest
contrib/node/check-fork-push.sh --install-gitleaks <dir>
contrib/node/push-fork-snapshot.sh [--delete] <12-hex short sha>
```

`snapshot` builds the snapshot commit of `<rev>`'s tree. Its author and committer dates are
`<rev>`'s committer date, so the same tip always gives the same commit. Before that, the root
`Cargo.toml` and `Cargo.lock` have their local `file://` sources replaced by the public URLs in
`contrib/node/fork-push-sources.txt`, in a private index; the worktree is not touched. The check
covers exactly that commit: its tree, its raw commit text (headers and message) and its ref name.
On a pass, and only then, it points the local ref `refs/fork-push/ci/<short>` at the commit and
writes a receipt (commit, tree, tip, denylist SHA-256, time) to the private
`fork-push/receipts/<short>.json`. On a failure it removes both and prints "not pushable".
`snapshot` never pushes.

`range` checks every object `git rev-list --objects` lists for the range (a superset of
`git log -p`), every commit message in it and the branch name. The tree-level checks (3, 5, 8, 9)
run on the range's tip. The commits' authors and tags are not checked yet; that comes with the
first real-history push.

Exit status: 0 pass, 1 a check failed, 2 setup error (for example a missing denylist). Output
names a check and a pushed path, or `commit message` or `ref name`. It never prints a denylist
line or private content.

The `CHECK_FORK_PUSH_*` variables that replace an input (the fingerprint binary, the public,
pointer, allowlist, sources and secrets files, the fork fixture, the size limit and a git URL map)
exist for the selftest. `snapshot` and `range` refuse to run with any of them set unless
`--allow-overrides` comes first.

### Pushing

`contrib/node/push-fork-snapshot.sh <short>` is the only push. It checks the ref against the
receipt again: the same commit, no parent, the same tree, the denylist unchanged since the check,
and the receipt at most `MAX_AGE_HOURS` (default 12) old. It then pushes that commit by SHA as
`ci/<short>`, never forced. When `ci/<short>` already exists on the fork it refuses. `--delete`
removes the remote branch and the local ref after the CI run, and the receipt stays as the record.

### The checks

The checks are the study's nine items.

1. **Denylist.** Matches a private list of distinctive terms against:
   - every pushed blob;
   - the decoded content of compressed blobs (gzip, zstd, xz, bzip2 and zip, nested up to three
     levels);
   - the commit messages;
   - the ref name.

   Each text is matched in its raw form (ASCII case and whitespace runs folded) and in text forms
   that undo common ways of splitting a term:
   - NFKC normalisation and case folding;
   - Unicode format characters (zero-width spaces, joiners) removed in one form and read as spaces
     in another, with no-break spaces read as spaces;
   - comment and quote leaders (`//`, `///`, `//!`, `#`, `--`, `;`, `>`, `%`, `*`) dropped after a
     line break, so a term wrapped across comment lines still matches;
   - adjacent string literals joined (`"a" "b"`, `"a" + "b"`, `"a", "b"` and a backslash line
     continuation), so a term split across `concat!` arguments still matches.

   The denylist does not see encoded forms such as base64 or hex. Check 2 covers copied private
   bytes, but a term written out by hand in such an encoding passes.

   A compressed blob that cannot be decoded completely fails this check, naming the reason:
   - truncated or corrupt data;
   - bytes after the last member (trailing zero padding is accepted);
   - more than three levels of nesting;
   - a decoded size over 2 GiB for one blob or 3 GiB in total;
   - a nested archive inside a decoded layer over 256 MiB.

   The rest of such a blob cannot be seen, so it does not pass. zstd needs Python 3.14's
   `compression.zstd`, and the check refuses to run without it.

   The list lives outside the repository: `$DASH_EMBARGO_REGISTRY/fork-push/denylist.txt`, where
   the registry defaults to `~/private/embargo`. It is read at check time. The check fails closed
   when the list is missing, empty or readable by others (it must be 0600 in a 0700 directory).
   Lines are stripped, and a line that then starts with `#` is a comment. A private README next to
   the list records how each line was chosen. The script uses its own variable rather than
   `DASH_EMBARGO_DIR`, because the gate sets that one to the private-test tree below the registry.
2. **Content fingerprint.** `contrib/node/fork-push-fp` is std-only Rust, built into
   `target/fork-push-fp`. It reports any pushed blob, or decoded layer, that shares a run of 64
   bytes or more with a file under the registry.

   It spools the pushed bytes through a file and holds them in memory (under 4 GiB, since check 1
   caps the decoded total at 3 GiB). It indexes the 32-byte chunk at every offset that is a
   multiple of 16. Each private file streams through a rolling hash at every byte offset, and each
   candidate is confirmed byte by byte. Every shared 64-byte run holds two such chunks. So a run is
   found when one of them has at least 4 distinct byte values and occurs at most 4,096 times among
   the pushed chunks; more copies than that are a repeated pattern. The count of capped lookups is
   printed. Runs are tracked per diagonal (pushed offset minus private offset), so one run never
   hides another in the same blob.

   A run is not reported when:
   - it has fewer than 4 distinct byte values;
   - it also occurs whole in a public tree listed in `contrib/node/fork-push-public.txt` (the tip
     trees of remote-tracking refs of public GitHub repositories);
   - every 32-byte private chunk in it is in the private `fork-push/fingerprint-pins.txt`, which
     holds chunks a reviewer has looked at.

   Pins are content hashes, so a pin exempts that chunk in every pushed blob, not only where it was
   reviewed.

   Two kinds of private file are never read:
   - a private file whose git blob id is a blob in the history of one of those public repositories;
   - a private file under a subtree of the private `fork-push/fingerprint-scope.txt`. That file
     lists machine-generated subtrees, and records the reason for each privately.

   The check prints how many files each scope line drops, how many history copies it skipped and
   how many symlinks it passed over (links are not followed). Private files shorter than 64 bytes
   count when a pushed blob equals them. The detailed report, which names private files, goes to
   `fork-push/reports/` in the registry. The console shows pushed paths only.
3. **Pointer report.** Lists files that name a codename, the private-test switch or the registry
   path and are not pinned in `contrib/node/fork-push-pointers.txt` (paths only). Names without
   content are allowed on the personal fork (DEC-670), so this check warns and does not fail. A new
   pointer file still gets a manager look before it is pinned.
4. **Messages and ref names.** Folded into check 1.
5. **Local paths and machine config.** Fails on:
   - a committed `.cargo/config(.toml)` with a source replacement, a `[patch]` or `paths`;
   - a `Cargo.toml` dependency, `[patch]` or `[replace]` with a git source other than
     `https://github.com/<owner>/<repo>`;
   - a path dependency outside the repository;
   - a `Cargo.lock` git source that is not on GitHub;
   - a non-doc file naming `/work/chains`, `/work/bench`, `/work/cache` or a private home path.
     Doc files are `*.md`, `*.rst`, `*.adoc` and anything under `docs/`.

   For a root pin rewritten to a public fork (`fork-push-sources.txt`), the check also fails when:
   - the pinned rev is not exactly a published tag of the fork's tag pattern;
   - an upstream rev the `[patch]` stands in for is not an ancestor of the upstream default branch;
   - the rewritten `Cargo.lock` does not resolve unchanged. This is tested by
     `cargo metadata --locked --offline` on a `git archive` export, with the fork URL replaced by the
     local clone in an untracked config.
6. **Size and data.** Fails on:
   - a blob of 50,000,000 bytes or more;
   - datadir and database file names (`blocks/blk*.dat`, `chainstate/`, `*.ldb`, `*.sst`,
     `MANIFEST-*`, `*.sqlite`, `*.db`, `wallet.dat` and similar);
   - SQLite, LevelDB and RocksDB magic;
   - a raw block file (network magic, length, block, then the next record's magic).
7. **Secret scan.** Runs gitleaks 8.30.1 with archives scanned four levels deep. It scans:
   - the pushed blobs at their paths;
   - every blob again under a neutral name, since the default config skips some paths, such as
     `*.bin`;
   - the messages;
   - the ref name.

   It runs with the check's own config (the default rules), an empty ignore file and
   `--ignore-gitleaks-allow`, and with no `GITLEAKS_*` variable in its environment. So a pushed
   `.gitleaks.toml`, a pushed `.gitleaksignore`, an inline `gitleaks:allow` comment or the caller's
   environment cannot suppress a finding.

   The binary is pinned by version and SHA-256. Set `GITLEAKS` to its path, or install it with
   `--install-gitleaks <dir>`, which checks the release tarball's SHA-256. The check fails closed
   when gitleaks is absent or is not the pinned build. A finding passes only when its rule and the
   SHA-256 of its value are in `contrib/node/fork-push-secrets.txt`, the reviewed public test values
   (test vectors, Core's regtest spork key). gitleaks runs locally only.
8. **Private tests skip cleanly.** Fails when:
   - anything under `.github/` sets `DASH_EMBARGO_DIR`;
   - a test listed in `contrib/node/private-tests.txt` is missing from the tree;
   - a listed test does not return through `node_sink::private_tree`'s skip guard before it reads
     anything;
   - a file outside `node-sink/src/` calls `private_tree` or `embargo_dir` and is not listed.

   `check-private-tests.sh --expect-skipped` reads the same list, and the CI workflow runs it.
9. **Workflow triggers.** No workflow in the pushed tree fires on a `push` or `create` of the pushed
   branch, apart from `node-gate.yml`. GitHub's branch filter globs (`*`, `**`, `?`, `+`, `[...]`)
   and negations are honoured.

   On the fork, read-only through `gh api`, no active workflow on the default branch may have a
   `schedule`, `workflow_run` or `delete` trigger. GitHub runs those from the default branch
   whatever is pushed. An active workflow whose file is absent, and a workflow file the API lists
   without content, fail too. Any API error fails the check. The check prints the default branch and
   each workflow's triggers and state.

### The allowlist

`contrib/node/fork-push-allow.txt` records the reviewed exceptions to checks 5 and 6, one per line:
`<path or glob> <check> <reason>[:<arg>] [key=value ...]`. The reasons are a closed list, with no
free text, so no line can carry a private term. An entry is checked again on every run.

| Check | Reason | Covers | Guard |
|---|---|---|---|
| 5 | `local-tool` (glob allowed) | a script or tool run by hand | no file under `.github/` names it |
| 5 | `nested-workspace` (glob allowed) | the Cargo sources of a manifest or lock outside the root workspace | not a root member (or excluded), a `[workspace]` of its own (or excluded), not named under `.github/` |
| 5 | `inert-literal` | a local path as a string that is compared or quoted, never opened | the per-class count |
| 5 | `absent-skip` | a test that returns when the local path is absent | the per-class count |
| 5 | `env-gated:<VAR>`, `feature-gated:<feature>` | code reached only with that variable or feature | the name still occurs in the file; the per-class count |
| 6 | `public-data`, `synthetic-data` | a fixture of public chain or network data, or of synthetic regtest data | `blob=<git blob id>`: a changed file fails |

The per-class count is the number of local paths the file names per class (`chains`, `bench`,
`cache`, `private`). A changed count fails until someone reviews the file again.

The CI workflow will need to handle `dash-oracle-sys`. It is `feature-gated:oracle` because its
`build.rs` builds against a local Core tree. So a hosted-runner job must not build with
`--all-features`: it needs an explicit feature set that leaves out `oracle`, or a skip of the
oracle crates.

### Selftest and run time

`--selftest` runs `fork-push-fp`'s own tests. It then plants each failure class in a throwaway
repository against a synthetic registry, so no real private data is used. The plants include:
- split terms (comment-wrapped, NBSP, ZWSP, `concat!`);
- nested and zstd/xz archives, and broken ones;
- suppression files;
- moved private tests;
- default-branch triggers;
- allowlist drift;
- the source rewrite against a temporary tag.

Run time on agentbox is about 10 minutes, most of it check 2 reading the registry.

## `contrib/node/check-private-tests.sh --expect-skipped`

This is the CI mode. `DASH_EMBARGO_DIR` must be unset; the mode refuses to run when it is set. It
runs `quirk_01_embargo_1` and `private_seed_replay`, and passes only when each prints its one
`SKIPPED` line and libtest reports exactly that one test passing: none ran against a private tree,
failed or panicked. The classifier and EMBARGO-5 steps live in the private tree, so they do not run.
The gate mode, which requires the private tree, is unchanged. `--selftest expect-skipped` checks
the mode against a stand-in cargo.

## `contrib/node/check-self-devdep.py`

The gate's self-dev-dependency check, moved here unchanged from `/work/scratch/gate-self-devdep.py`.
It is run from the repository root: `python3 -I contrib/node/check-self-devdep.py`.

## GitHub tier 1: `.github/workflows/node-gate.yml` (Stage 2)

DEC-700 and DEC-705 move local tier 1 (gate-fast) to GitHub, and DEC-713 makes a green push run the
required check for READY and train candidates. The workflow runs on a `ci/<12-hex>` push and on
`workflow_dispatch` (inputs `snapshot`, `source`, `deps_only`). The fork's `master` carries the same
file (DEC-702), so dispatch works and its cache scope is shared; every change to the workflow goes
to `master` too, through `check-fork-push.sh range … --only 1,7`.

Jobs:

- **Deps** builds the shard matrix from `.github/ci-groups.yml`, putting every workspace member in
  exactly one test shard. It owns the one dependency-closure cache (DEC-699): third-party units
  only, keyed on the toolchain, `Cargo.lock`, the manifests and `ci-groups.yml`.
  - Only runs on `master` build and save it (DEC-713): cold, one selection at a time, and with the
    workspace units stripped after each selection.
  - A push run whose key is missing restores the newest closure with the same toolchain into its
    shards. They build what changed.
  - Save rule (v10; DEC-699, manager 10-29): a `deps_only` save on master costs 3.4 GB of the free
    10 GB.
    - It is worth it only for an integration-tip snapshot whose shards rebuilt third-party units
      that a save would hold. Summary prints the dispatch then.
    - Task branches restore master's closure and save nothing. Each branch has its own key, because
      the key covers every manifest; their shards rebuilt 0 third-party units on 10-29.
    - `ALWAYS_REBUILT` (cbindgen) lists units the closure in use lacks, which the save rule leaves
      out (BL-520). The rdashd shard's `--bin`/`--test` selection makes clap build-only, so cbindgen
      links debuginfo-0 clap units and gets its own hash. v8's deps job, which saved the v2 closure,
      never built that selection. Empty the list once a v9+ deps job has saved the closure for a
      new key.
  - The key doesn't cover node-gate.yml, so `CACHE_GEN` names the deps job's build list: how it
    plans, builds, strips and saves (BL-520).
    - `CACHE_GEN_BUILDS` pins `<CACHE_GEN>-<hash>` of that list, read from the copy of the file
      that runs, with comments and blank lines left out.
    - The deps job's "CACHE_GEN covers the build list" step fails every run when the list changes
      under the same `CACHE_GEN`. The fix is to bump `CACHE_GEN` and re-pin to the hash it prints.
    - The v2 pin is v10's list, not v8's, so the cbindgen gap above is accepted rather than flagged.
  - Summary's cache metric on every run: exact hits out of shards, partial restores, MB restored,
    and the rebuilt third-party units by name.
  - The first closure took 41 minutes to build. It is 3.2 GiB compressed and 15.9 GB unpacked.
- **Static** runs the static lane.
- **The test shards** are `conformance`, `rdashd`, `runtime`, `app-pure-shared` and
  `core-spv-wallet-ffi-rpc`; the last also takes the excluded and unassigned members. There are also
  `clippy` and `extras`. Each restores the closure.
  - Since v9, `rdashd` runs only rdashd's bin and test targets (its dashd v24 differential suites),
    and `runtime` runs the rest of node-runtime.
  - `rdashd` builds node-runtime's whole selection with its features. The dashd-* crates that
    rdashd links are only transitive dependencies, so `--features` can't name them.
  - Checked with `cargo --unit-graph`: every unit either shard builds, including all 37 workspace
    units of `rdashd`, is identical to one in the v8 runtime build.
- **Dispatch with `source`** and `source_ref` (required with it) names the run like the
  snapshot's push run. It is for redo and side-by-side runs only; push runs are the gating runs.
- **Summary** records:
  - the cache result (hit, partial or miss), restore seconds and bytes;
  - third-party units rebuilt after the restore;
  - per-job wall, CPU and peak memory;
  - test counts.

**The gate:** `contrib/node/rdc-ci-verdict.sh <label> <full sha> [--run <id>] [--override DEC-<n>]`.
It checks the newest node-gate run of that commit's snapshot that wasn't cancelled. Only a push of
`ci/<short>` or a dispatch on `master` with `source` counts; both carry the same title. A dispatch
from another branch runs that branch's copy of the workflow, so it is a lookup error.

The verdict is GREEN (exit 0) only when all of these hold:

- the run completed with every job of its final attempt successful, and the Deps, Static, Summary
  and conformance jobs are present;
- the run is on at most its second attempt;
- every other run of the source has completed, and none ended red.

The rerun rule (manager) has two parts:

- One `gh run rerun --failed` per source commit is allowed, and only for a BL-20 test or a runner or
  infrastructure failure. The verdict line names what attempt 1 failed, so this can be checked.
  - The BL-20 list is `BL20_SUITES` in gate-fast's `test-run.py`, mirrored here. A failed binary
    qualifies only when every one of its failures is on the list:
    - dash-spv and dash-spv-ffi `tests/dashd_*` suites, tests `tests_*::`;
    - dash-p2p-transport `tests/dashd_interop.rs` (BL-263).
  - Interim waiver, not a rerun (BL-519, DEC-744): dashd-chainstate lib's
    `chain::tests::node_stores::an_abort_node_with_w_held_at_a_prune_clear_completes_the_prune` fails
    in GitHub runs of every tree since train 47, a regression (3/3), so a rerun only spends runner time. A snapshot whose
    only failure is that test at `node_stores.rs:1485` reads "BL-519 known-red → GREEN (DEC-744)",
    with no rerun, for a branch that touches neither dashd-chainstate's `chain/` nor V's or W's exit
    path. The waiver ends when ND1's fix merges.
- No re-dispatch or re-push of a red source without a DEC. `--override DEC-<n>` (the manager's
  only) lets the green through and prints the DEC in the line.

A cancelled run counts as red if a job in any of its attempts had failed or timed out (DEC-715).
Summary isn't counted, since copies of the workflow before v9 run it after a cancel and it fails then.
Other cancelled runs are ignored.
The selftest is `contrib/node/rdc-ci-verdict-selftest.sh`: 25 cases, the run-backed ones through a
stub `gh`.

The verdict matches runs on the full source SHA, whatever the branch the snapshot came from, and
prints that branch (`<sha> of node/train-47`).

### Step inventory: gate-fast against node-gate

`=` the same command, `~` the same check run differently, `local` deliberately not on GitHub,
`missing` not run on GitHub.

| Lane | gate-fast step | node-gate | | Notes |
|---|---|---|---|---|
| static | fmt | Static / fmt | = | |
| static | pins | Static / pins | ~ | the export points the root's base SDK `[patch]` at the public fork; the job clones that fork to the local URL the other manifests name and points every manifest back at it |
| static | deny | Static / deny | = | cargo-deny 0.20.2 |
| static | r2 | Static / r2 | = | runs when `check-r2.sh` exists (not today, on either side) |
| static | layers | Static / layers | = | graph rules; L7 belongs to check-mobile.sh, not tier 1 |
| static | groups | Static / groups | ~ | the snapshot has no `build-and-test.yml`; a stand-in from `ci-groups.yml` makes its matrix sub-check vacuous |
| static | self-devdep | Static / self-devdep | = | the tree's copy; warns NOT RUN on a tree without it |
| static | sb12a-captures | Static / sb12a-captures | = | |
| static | release-refs | Static / release-refs | = | TMPDIR is the runner's temp dir |
| static | protocol-tables | Static / protocol-tables | ~ | shallow fetch of the pinned tag from github.com/dashpay/dash |
| clippy | workspace, all features and targets, `-D warnings` | clippy | = | |
| test | rdashd-build | runtime / rdashd-build | ~ | same target dir; the binary is copied out before the shard's test build |
| test | test-build, `cargo test --workspace --no-run` | each test shard / test-build | ~ | `-p <shard crates>` plus those crates' workspace-resolved features. Members a shard builds only as dependencies, and third-party crates, still unify per shard: 26 member nodes across the four shards, e.g. dashd-store without `rocksdb` under conformance. The plan step logs each one. Every test target's own features match (see below). |
| test | test (test-run.py: every binary and doc test) | each test shard / test | ~ | plain `cargo test --no-fail-fast`, binaries one at a time |
| test | BL-20 rerun of the dashd suites | - | missing | stricter on GitHub: a flake fails the shard; rerun the failed job |
| test | per-binary limits (900 s, 2400 s SLOW) | - | missing | the job's 120-minute timeout bounds a hang |
| test | dashd binaries at 4 threads, each in its own network namespace | test shards | ~ | the runner's default is 4 threads; nothing else shares the runner's ports |
| test | eatmydata dashd wrapper | - | missing | speed only |
| test | dashd 23.1.0, the regtest data, ST2_DASHD 24.0.0-rc.4 | test shards | = | setup-dashd.py from public releases; ST2_DASHD fails closed |
| aux | no-libm | extras / no-libm | = | |
| aux | public-api | extras / public-api | = | nightly-2026-07-15, cargo-public-api 0.52.0 |
| aux | oracle-diff (trigger-gated) | - | local | needs the Core oracle archives (DEC-650) |
| pure | check-pure (trigger-gated) | extras / pure | ~ | always runs, since a snapshot has no merge base; the base SDK pin is pointed back as for pins |
| planted | crash_planted (trigger-gated) | extras / planted | ~ | always runs |
| tier 2 | private tests | extras / private tests skip cleanly | ~ | `--expect-skipped` only; the private runs stay local |

Also local: PF0, the farms, nightly chain-data suites, tier 2 (`nightly.sh`) and the gate
selftest. Tests that read local data (`/work/chains`, the conformance regtest-v24 tree,
`/work/scratch/copy`) skip themselves on GitHub as they do on any machine without it.

### Side-by-side (DEC-690 (4), DEC-705: one run)

The side-by-side compares T47's local tier 1 run with GitHub run
[40840039160](https://github.com/PastaPastaPasta/rust-dashcore/actions/runs/40840039160).

- **Local:** `gate-6b88c125e`, 10-29, gate-fast run `1029-060624-gate-6b88c125e-Ny7`.
- **GitHub:** a master dispatch with `source` = `34dc9456fdb7…`, which is 6b88c125e with two notes
  trimmed by CI-S0's export. It ran node-gate v7.
- **Verdict:** all 9 jobs passed (`rdc-ci-verdict.sh` GREEN).

The comparison keys every test binary (`Running <src> (<binary>)`) and doc-test crate, and
compares their passed/failed/ignored counts.

| | local T47 | GitHub 40840039160 |
|---|---|---|
| binaries and doc-test crates | 386 | 386, the same keys |
| `test result` lines | 403 | 403 |
| passed / failed / ignored | 7,689 / 1 / 71 | 7,690 / 0 / 71 |
| keys with different counts | | 1: dash-spv `dashd_sync`, local 37 + 1 failed in the first pass then passed on the BL-20 rerun, GitHub 38 |

Per shard (passed / ignored): conformance 301 / 10, runtime 1,926 / 15, app-pure-shared
2,481 / 8, core-spv-wallet-ffi-rpc 2,982 / 38.

Feature-gated test targets: 114 targets carry `required-features` or a top-level
`#![cfg(feature = …)]`.

- The workspace-wide resolve enables 15 of them, and both sides ran exactly those 15:
  - dash-coreimport `full`: evodb_serve, flatfiles, flatfiles_memory, netstate;
  - dash-evo consumers;
  - dash-network serde_names;
  - dash-p2p eviction;
  - dashcore serde_opcodes;
  - dashcore_hashes x11_chain;
  - dashd-masternode `net`: spork_transcripts, sync_transcripts;
  - dashd-store `rocksdb`: engine_contract, fail_stop, rocks_wal_nonprefix;
  - key-wallet-ffi wallet_layout_v0.
- Neither side runs the other 99:
  - oracle-gated: dash-oracle-sys 27 and node-l1 45, which stay local (oracle-diff);
  - dashd-store `faulty`: the 12 power_loss suites;
  - dashd-chainstate `crash`: 3, of which crash_planted runs in both planted lanes;
  - dash-chain `test-util`: 4;
  - node-l1 `sut`: 4;
  - dashd-node `test-support`: 2;
  - dash-spv-ffi `blst-no-threads`: 1;
  - rdashd `test-hooks`: 1.
- The check is `/work/scratch/ci-stage2/tools/gated-targets.py`, run against the local log.

Timing:

- **Warm run:** 22 min wall (09:03:38–09:25:48). That covers Static 1 min, clippy 5,
  app-pure-shared 10, extras 10, conformance 11, core-spv-wallet-ffi-rpc 13 and runtime 21.
  - The dashd suites take 16 of the runtime shard's 21 minutes.
  - Each shard restores the closure in 53–93 s and rebuilds 0 third-party units.
  - The shards' test builds take 40–129 s.
- **Cold closure:** the `deps_only` Deps job took 55 min: 51 min building every selection, then a
  54 s save.
- **Local tier 1, the same commit:** about 56 min (06:19–07:15). Its test step alone is 44 min.

The cache holds one 3.4 GB entry, master's v2 closure. The three v1 entries were deleted on 10-29
because the total was 13.6 GB, over the free 10 GB.
