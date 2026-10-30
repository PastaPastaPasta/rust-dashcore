# SP1b narrow r2: Opus review (O1, O2, O4, N5, N6, the partial-row cut, regressions)

Review of the frozen worktree `/work/scratch/sp1b-r1/wt` at **b7dadce03**, fix range `f8c61e079..b7dadce03` (4 commits; 5 files changed, +232/−37).

## Verdict: **ACCEPT-WITH-NITS**

Every r1 item in my scope is fixed, and I checked each one by running it:

- **O1:** the probe gets a private 0700 datadir, and `~/.dashcore` is left alone.
- **O2:** resuming into a complete OUT_DIR returns 0 and starts nothing.
- **N5:** the new comparison kills M4.
- **N6:** exit 2 counts as success in the unit, and `nightly.log` now records the rdashd sha256 and the checkout.
- **Partial-row cut:** it works on a copy of the acceptance data.

The unit's own setup is safe. One Minor remains: the old-root sweep in `nightly.sh` decides that TMPDIR belongs to it by checking the current directory. Run from `/work` or `/work/tmp`, it deletes other threads' `sp1b-*` dirs in the shared `/work/tmp`. The fix is a few lines, and it should go in before merge.

## Findings

### Minor 1: the sweep also fires on the shared `/work/tmp`, and it removes any `sp1b-*` dir there except the newest
- **Where:** `contrib/node/sp1b-timing/nightly.sh:45-53`.
- **Problem:** the guard is "TMPDIR resolves under `pwd -P`". That is a stand-in for "this TMPDIR is the unit's", and it fails in two ways:
  - On this box TMPDIR is `/work/tmp` by default. So a by-hand or dev run started from `/work` or `/work/tmp` passes the guard. I checked it: `cwd=/work → SWEEPS`, `cwd=/work/tmp → SWEEPS`, while the repo, `/` and `/work/conformance` don't sweep.
  - The name filter is `-name 'sp1b-*'`, so it catches far more than `timing.py`'s `mkdtemp` roots.
- **Scenario 1:** an agent runs `cd /work && …/nightly.sh /work/scratch/x/out` for a development run.
  - The sweep removes every `sp1b-*` dir in `/work/tmp` except the newest. The newest is decided by directory mtime.
  - That includes another thread's live run root, and its kept failed-session datadirs.
  - It also includes unrelated dirs. `/work/tmp/sp1b-review` exists right now (empty, from 10-27 17:07) and would be deleted.
- **Scenario 2:** a directory name with a newline breaks the `read -r old` loop. Reproduced in a fake layout:
  - `tmp/sp1b-x⏎victim` became two lines, and the script ran `rm -rf -- victim` **relative to the cwd**. That deleted `$W/victim`, outside TMPDIR.
  - Only a crafted name triggers this, and inside the unit's private `tmp/` only `mkdtemp` makes `sp1b-*` dirs. So it is a hardening point, not a live risk.
- **Fix:**
  - Make the sweep opt-in from the unit, for example `Environment=SP1B_SWEEP_TMP=1`, instead of inferring it from the cwd. Or require both the cwd check and TMPDIR ≠ `/work/tmp`.
  - Match only `mkdtemp` names: `-regextype posix-extended -regex '.*/sp1b-[a-z0-9_]{8}'`.
  - Make the pipeline NUL-safe: `-printf '%T@ %p\0' | sort -zn | head -zn -1 | cut -zd' ' -f2- | xargs -0r rm -rf --`.
- **Safe parts (checked):**
  - `-type d` without `-L` skips `sp1b-*` symlinks to dirs outside. A symlink `tmp/sp1b-lnk` was kept, and its outside target was untouched.
  - Plain files and non-`sp1b-` dirs are kept.
  - A symlinked TMPDIR makes `find` list nothing, so it fails safe, but quietly.
  - In the unit's real layout (`WorkingDirectory=/work/conformance/nightly/sp1b-timing`, `TMPDIR=…/tmp`) it touches only `tmp/sp1b-*`.

### Nit 2: a failed version check leaves its root (and `version/`) in TMPDIR
- **Where:** `timing.py:614-624`.
- **Problem:** `sys.exit` at :623 runs before `shutil.rmtree(probe)` and before any cleanup of `root`.
- **Reproduced:** with a fake dashd that reports v23, `run` exits 1 and leaves `sp1b-b3ys96h7/version/` behind.
  - The unit's sweep picks it up the next night.
  - A development run in `/work/tmp` leaks it. With a real wrong-version dashd, that means `dash.conf` and `settings.json` too.
- **Fix:** `shutil.rmtree(root)` before the `sys.exit`, or probe in a `tempfile.TemporaryDirectory(dir=TMPDIR)` before the `mkdtemp`.

### Nit 3: the netfulfilled comparison covers only one of five fixtures, though it passes on all five
- **Where:** `dashd-masternode/tests/sync_transcripts.rs:111-113` and `:264-272`.
- **What I did:** in a reflink copy, I moved the assertion `assert_eq!(requests_by_peer(&got), requests_by_peer(&core_netfulfilled(name)))` into `run_with`.
- **Result:** all 10 `sync_transcripts` tests still pass, including the four negative controls. The disabled, four-peers, blocksonly and governance replays all end with exactly dashd's `netfulfilled.dat`.
- **Suggestion:** run it in `run_with`, so every scenario's `full-sync`/`mempool-sync` end state is pinned against Core at no cost.

### Nit 4: the selftest's resume check prints a misleading line into `nightly.log`
- **Where:** `timing.py:612`, reached through `resume_selftest`.
- **Problem:** `nightly.sh` runs `selftest` first. That now prints `nothing to run: done {('rdashd', 's30'): 1, …}` before `selftest ok`.
- **Scenario:** someone reading a night's log sees "nothing to run" ahead of the real run.
- **Fix:** add a `quiet` flag, or redirect stdout in `resume_selftest`.

## Scope checks

### O1: private `-version` datadir — **fixed**
- **Code:** `timing.py:616-624` makes `root/version` with mode 0700 and passes `-datadir=<it>`. The fake-dashd log shows `-datadir=…/sp1b-*/version -version` with mode `700`.
- **Empty output:** `(version.splitlines() or [""])[0]` handles it, so the r1 `IndexError` path is gone.
- **Real rc.4 binary:** run with a private 0700 datadir, it wrote `settings.json` and `dash.conf` there. `~/.dashcore/settings.json` kept its mtime of 10-27 23:35, from my own r1 probe.

### O2: resume into a complete OUT_DIR — **fixed**
- **Code:** `short()` is checked before any root, probe or worker exists (:609-613), and `loads` is seeded at start (:714).
- **On accept1's data:** I copied `accept1/samples.tsv` (reflink) and appended a partial row, then ran `run --samples 10000 --jobs 128` with a fake dashd and `SP1B_MAX_LOAD=40`.
  - Output: `nothing to run: done {… 10000 ×4} failed {}`, exit 0.
  - No probe ran, no root was created, and no `run-summary.json` was written.
- **The selftest's `resume_selftest`** guards this path: a missing dashd/rdashd would fail it.
- **Workers:** short pairs that are past the failure cap are excluded by the same `short()`, so they can't spin.

### b7dadce03's partial-row cut — **correct**
- **Before and after:** on the copy above, `drop_partial_line` cut exactly the 26-byte partial row (5,183,860 → 5,183,834 bytes). The file is then **byte-identical to `accept1/samples.tsv`** (`cmp`), with a trailing `\n`.
- **No newline at all:** if the file has no newline, `rfind` gives −1 and it truncates to 0, which is right.
- **`errors.tsv`:** the same treatment is safe there, because error text has `\t` and `\n` replaced (:696).
- **`ks`:** it does not cut. It only skips unterminated lines (`read_samples`), so it never changes a file it judges.
- **Session ids:** `next_id` comes from complete rows only. A cut row's id may be reused, but its row is gone, so no session is counted twice.
- **When this can happen:** a session's rows go out in one buffered `write` on close. So a partial line needs a short write (for example ENOSPC), not a kill. The cut is defensive, and it is cheap.

### Pause logic and b7dadce03's "end the pause once no session is left" (mechanics only; the statistics are Sol's)
- **The lock:** `wait_load` holds `pause` while it sleeps, so the other workers queue behind it, and no dispatch happens while the load is ≥ the ceiling.
- **Ending the pause:** the pause also ends when `short()` is empty. Each queued worker then takes the lock, sees `not short()`, returns, gets `None` from `pick()` and exits. So the run cannot hang on the load once its target is met.
- **A late failure reopens a pair:** the worker whose session failed loops back through `wait_load`/`pick`, so the retry is always taken.
- **The harness's own load:** small. accept1 used 5.86 CPU-h over 3.16 h wall, about 1.9 busy CPUs, so the run won't push itself over a ceiling of 40.
- **Pauses have no limit:** `TimeoutStartSec=10h` bounds them. A night killed there is `failed` with no `ks`, as D22 now documents.
- **Invalid `SP1B_MAX_LOAD`:** `--max-load` with `SP1B_MAX_LOAD=forty` gives argparse exit 2 in `run` only, and `nightly.sh:75` turns that into exit 1. `selftest` (exit 0) and `ks` are unaffected.

### O4: TMPDIR and the old-root sweep — **fixed for the unit**
- **The unit:** TMPDIR is now `…/sp1b-timing/tmp`, under its `WorkingDirectory`. The sweep keeps the newest earlier root (one night's kept datadirs) and runs before the load wait.
- **Outside the unit:** see Minor 1 for when it runs where it shouldn't, and how it can reach outside its directory.

### N5: netfulfilled end state — **kills M4, and robust**
- **M4 in a reflink copy** (`// nf.add_fulfilled_request(&peer.addr, &allow)` at `sync_manager.rs:441`): `mnsync_allow_sync` FAILED at `sync_transcripts.rs:271`. The other 9 passed, as expected.
- **Robustness:**
  - The store is `BTreeMap<Service, BTreeMap<Vec<u8>, i64>>`, so each peer's list is ordered by name. Mapping `allow-sync-<ms>` to `allow-sync` keeps that order (`a` < `f` < `m`), and the outer list is sorted. Nothing depends on the order of the sim's addresses.
  - Expiries are compared exactly. The sim's mock clock matches dashd's mocktime: +48, +42 and +36, plus 300.
  - `core_netfulfilled` correctly strips Core's `CFlatDB` framing: compact-size magic `magicFulfilledCache`, 4-byte message start, object, 32-byte hash. Core's object is the bare map (`netfulfilledman.h:30-34`, rc.4), and SP1's object adds a version string. The test bridges that difference correctly. SP1's own file uses a different magic on purpose (`MAGIC = "rdashd-netfulfilled"`), which is not new.
  - Dropping addresses loses peer identity. But every peer's set and expiries differ, so swapping roles still changes the multiset.

### N6: the exit-2 skip and attribution — **fixed**
- **The skip path:** a fake run with `SP1B_MAX_LOAD=0 SP1B_LOAD_WAIT_S=0` logged `skipped: load: the 1-minute load never fell below 0 (now 16.94)` and exited 2.
- **Attribution lines:** `nightly.log` gains `rdashd sha256 <hex>` and `checkout v0.43.0-7306-gb7dadce038a1…` (40-hex, `--dirty`-aware).
- **systemd:** with `Type=oneshot` + `SuccessExitStatus=2`, a transient unit exiting 2 started successfully, and one exiting 1 failed. `systemd-analyze --user verify` on the service is clean.
- **No other source of exit 2:** argparse's exit 2 for `selftest`/`run` is mapped to 1 by `|| exit 1` and `nightly.sh:75`. `ks` takes the same `--samples` that `run` already accepted. A missing argument gives `${1:?}` exit 1.
- **What the commit names:** the `checkout` commit is the harness's. The rdashd binary is tied by its sha256 only, so the line names rdashd's source commit only if the installer builds rdashd from that checkout.

### Regressions
- **No non-test Rust changes:** the only Rust change is `tests/sync_transcripts.rs`.
- **`ks` reads both row widths:** 5-column rows are padded with `-`. accept1's file loads as 160,000 rows. Sol's scope covers the D values.
- **Order of steps:** `Ledger(out)`, and with it the partial cut, now runs before the binary check. That is harmless.
- **Docs:** D17 now says "five"; D21 cites :217/:220-233/:227; D22/D23 describe the fixes accurately.

## Commands run (and results)
- `git log`/`git diff f8c61e079..b7dadce03`, and a read of the whole diff: 4 commits, 5 files.
- `CARGO_TARGET_DIR=…/target-opus nice -n 10 cargo test -p dashd-masternode --test sync_transcripts --test core_flatfiles` at the head: 10 + 3 passed.
- **M4:** in a reflink copy (`opus-scratch/r2/mut`, with target `target-mut` reflinked from `target-opus`), `sync_transcripts`: `mnsync_allow_sync` FAILED (:271), 9 passed.
- **All five fixtures:** the same copy with the assertion moved into `run_with`: 10/10 passed (Nit 3).
- `python3 timing.py selftest` (from the copy): `selftest ok`, exit 0.
- **Fake dashd (prints v23) `run`:** probe args `-datadir=…/sp1b-*/version -version`, dir mode 700; exit 1; root left behind (Nit 2).
- **Real rc.4 `dashd -datadir=<private 0700> -version`:** `v24.0.0-rc.4`; `settings.json`/`dash.conf` written to the private dir; `~/.dashcore/settings.json` mtime unchanged (10-27 23:35).
- **Resume on a reflink copy of `accept1/samples.tsv` plus a partial row:** `nothing to run`, exit 0, partial row cut, `cmp` identical to accept1, no root, no summary.
- `SP1B_MAX_LOAD=forty`: `run` gave exit 2 (argparse); `selftest` gave exit 0.
- **`nightly.sh` sweep in fake layouts** (`opus-scratch/r2/sweepA`, `sweepC`):
  - old roots removed, newest kept;
  - symlink, file and other dir kept;
  - the outside target intact;
  - the newline name deleted `$W/victim` (Minor 1);
  - a symlinked TMPDIR swept nothing.
- **The case guard, evaluated for several cwds** with TMPDIR=`/work/tmp`: sweeps for `/work` and `/work/tmp`.
- **Transient units** `systemd-run --user -p Type=oneshot -p SuccessExitStatus=2` running `exit 2` and `exit 1`: success and failed, respectively. Both were `--collect`ed.
- `systemd-analyze --user verify` on a scratch copy of the unit: clean.
- **Not run:** no acceptance run and no smoke session. I wrote nothing in the frozen worktree; it is still clean at b7dadce03.
- **Cleanup:** I deleted the mutant source copy and its 9.2 GB target. Small logs and fake layouts stay in `/work/scratch/sp1b-r1/opus-scratch/r2/`.
