# ST3b power-loss harness: runbook

This directory and `dash-testkit/src/crash/` (the `st3b-crash` binary, feature `crash`) test
whether rdashd recovers from power loss and kills. The design and its decisions are in
`designs/ST3b-notes.md` of the node-program docs. This file covers running it, reading its
results and cleaning up after it.

Everything it writes lives under `/work/scratch/st3b/`: `run/`, `mnt/`, `vm/`, `fixtures/`,
`nightly/`, `device-40k/`, `weekly-vm/` and `tmp/`. It never writes to `/tmp` or `/dev/shm`.
Set `TMPDIR=/work/scratch/st3b/tmp` for anything you run by hand.

## The checks

Each point (a replayed crash image) or kill is checked with the following:

| check | what it asserts |
|---|---|
| C0 | Offline, with no node running. The image mounts, and the chainstate and block index open. Every stored block is the fixture's block (I1). The phase is classified as `pre`, `mid`, `one` or `post`. |
| C1 | rdashd starts on the image with `-connect=0`, and its log has no forbidden lines. |
| C2 | `gettxoutsetinfo` (muhash, hash_serialized_2) equals the fixture's oracle row for the recovered tip. |
| C3 | Freshness: the recovered tip is at or after the floor, that is the last acknowledgement whose flush the state holds (`Start`, `Ack(p)` or `Stopped(p)`). |
| C4 | `getblock` bytes for the tip, 10 ancestors and 32 sampled blocks, plus every acknowledged block. |
| C5 | Recovery continues: the node syncs from the fixture's dashd to the final tip, then C2. |
| C6 | Double crash: rdashd's recovery is itself recorded and replayed at its flush points, and each of those points passes C0–C4 at the first crash's floor. |

## Tools

| file | what it does |
|---|---|
| `st3b-crash` (bin) | The harness. Run it without arguments for usage. Subcommands: `selftest`, `campaign`, `c6`, `check`, `inspect`, `fault`, `vm`, `kill-smoke`, `baseline`, `sweep`, `leaks`. |
| `st3b-priv` | The only privileged entry point, run as `sudo -n st3b-priv <verb>`. It handles loop devices on files under `run/`, `st3b-*` device-mapper devices, mounts under `mnt/`, and the guest kernel copy. It guards against harness bugs; it is not a privilege boundary (ST3b-D5). |
| `st3b-priv-tests.sh` | Checks that the `kernel` verb refuses what it must, with nothing written. |
| `leak-tests.sh <st3b-crash>` | Checks that the device stack leaves nothing behind on panic, SIGKILL, a pause, or a namespace holding a mount. It checks the whole box, so run it only when no other ST3b run is up. |
| `mkfixture.py` | Builds a fixture with the pinned dashd: the chain, the oracle rows (`rows.jsonl`) and the schedule. |
| `nightly-device.sh OUT [COMPONENT...]` | The device nightly. It builds, sweeps stale runs, runs each component, and writes `summary.txt`, the ledger and the failure-dir prune. Its header documents every variable. |
| `weekly-vm.sh` | The VM weekly. `--prepare` builds the binaries, guest and image; `--component` runs one scenario; `--finish` runs the leak check and writes DONE; `--units` drives all of it as units. Its header documents every variable. |
| `vm/mkguest.sh`, `vm/init`, `vm/Dockerfile` | The guest: a busybox initramfs with stripped rdashd, and the `st3b-qemu` image (QEMU in Docker, run with `--user`). |
| `install-units.sh [--enable]` | Copies `units/` to `~/.config/systemd/user/` and reloads systemd. With `--enable` it also enables the timers. |

## Components

**Device nightly** (`nightly-device.sh`, `st3b-device-nightly.timer`, daily 04:10 UTC). Its
default list, in order:

- `kill-smoke`, labelled smoke. rdashd is SIGKILLed `$ST3B_SMOKE_KILLS` times (default 10) at
  seeded points during W-small's IBD, on plain disk, with no devices and no sudo. Each restart
  gets C1–C4 at the last acknowledgement's floor, and the end gets C2 at the tip. A process kill
  keeps the page cache, so this is a cheap check of the restart path, not of power loss.
- `selftest:ext4`, `selftest:xfs`: the log-writes stack on a known file workload, every point.
- `d-ibd-s:ext4`, `d-ibd-s:xfs`: W-small recorded once per filesystem, covering D-IBD-S,
  D-REORG-S and D-F01 (ST3b-D18). The night checks its shard `(UTC day mod 24)/24` with C0–C6
  (DEC-718), so every point index is checked once in 24 nights.
- `fl-ibd`, `fl-flush`, `fl-wal`, `df-pre`, `df-mid`, each on ext4 and xfs: the S6 fault cases.
  These are fsync errors through dm-flakey and a full disk, one `st3b-crash fault` each with up
  to 3 seeded attempts.

**Device 40k weekly** (`st3b-device-40k.timer`, Saturdays 04:10 UTC). The service waits 5 min,
then until the nightly is done, up to 6 h.
`d-ibd-40k:ext4`: W-40k recorded once. C0 runs on the week's shard, 1 point in
`$ST3B_40K_C0_EVERY` (default 4), and C1–C6 on 1 in `$ST3B_40K_SAMPLE` of those (default 10,
via `campaign --sample`). Both defaults are provisional until the owner's first measured run.
Its summary row gives `recorded=` (the recording's point count) and `c0=` (C0's CPU-s a point)
to size them from.

**VM weekly** (`weekly-vm.sh`, `st3b-vm-weekly.timer`, Sundays 05:10 UTC). Each scenario runs
in a KVM guest in Docker and is killed with QEMU SIGKILL, so the page cache is lost (a power
cut). Each kill gets C1–C4.

- `v-ibd`, `v-flush` and `v-recovery` run on W-40k; `v-reorg` runs on W-small's branch B.
- All run at `-dbcache=4`. `v-ibd` and `v-reorg` use `-dbbatchsize=20000`; `v-flush` and `v-recovery` use 2000, so a flush splits into 15–27 partial batches, a window longer than a triggered kill's latency. Their partial-batch triggers jitter by at most 5 ms, while replay and reorg triggers jitter by up to 50 ms (S7 r4, ST3b-D26).
- The routine week (owner ruling, S8) runs the W-40k components at 20 kills on ext4 and 10 on
  xfs, and v-reorg at 50 and 20. 50 a scenario on ext4 and 20 on xfs was S7's one-time
  acceptance bar.
- `v-reindex` prints SKIPPED until CH5.
- Each component is its own `st3b-vm-weekly@<component>.service`. Its drop-in
  (`units/st3b-vm-weekly@<component>.service.d/timeout.conf`) sets its timeout, and for W-40k
  its kill count (`ST3B_VM_KILLS_EXT4`/`_XFS`). `weekly-vm.sh` by hand defaults to 50 and 20; set
  those variables to run the routine counts.

## Running by hand

Setup, once:

```sh
export TMPDIR=/work/scratch/st3b/tmp
eval "$(python3 contrib/setup-dashd.py)"     # the pinned dashd under ~/.rust-dashcore-test
# Fixtures (about 10 min for W-small, longer for W-40k); see each provenance.json for its args:
contrib/node/crash/mkfixture.py /work/scratch/st3b/fixtures/w-small --blocks 1500 --seed 1 \
  --reorgs 1100:3,1350:8 --serve 1000 --f01 1210
contrib/node/crash/mkfixture.py /work/scratch/st3b/fixtures/w-40k --blocks 40000 --seed 2
nice -n 19 cargo build --release -p rdashd -p dash-testkit --features dash-testkit/crash \
  --bin rdashd --bin st3b-crash
```

First do the heavy-start check, in a command of its own. See [Budgets](#budgets-and-heavy-start-rules).

```sh
D=$HOME/.rust-dashcore-test/dashcore-24.0.0-rc.4/bin/dashd
B=target/release F=/work/scratch/st3b/fixtures
# One nightly component, or several, against already-built binaries:
ST3B_BIN_DIR=$B contrib/node/crash/nightly-device.sh /work/scratch/st3b/s<N>/out kill-smoke d-ibd-s:ext4
# The whole nightly (devices; about 2 CPU-h):
contrib/node/crash/nightly-device.sh /work/scratch/st3b/nightly/out/manual
# The runner's own test: no devices, no sudo; campaigns run --dry-run, the rest are skipped.
# Never a night's evidence.
ST3B_DRY_RUN=1 ST3B_BIN_DIR=$B contrib/node/crash/nightly-device.sh /work/scratch/st3b/s<N>/dry
# The VM weekly by hand, all in one process, or one component on a prepared week:
contrib/node/crash/weekly-vm.sh /work/scratch/st3b/weekly-vm/out/manual v-reorg-ext4
contrib/node/crash/weekly-vm.sh --component v-reorg-xfs /work/scratch/st3b/weekly-vm/out/manual
```

Test runs by hand should use their own `ST3B_LEDGER`, so they stay out of the real ledgers.

Single `st3b-crash` runs:

```sh
st3b-crash kill-smoke $F/w-small --rdashd $B/rdashd --dashd $D --kills 4 --seed 5 -- -dbcache=4 -dbbatchsize=20000
st3b-crash selftest --fs xfs --seed 7
st3b-crash campaign $F/w-small --rdashd $B/rdashd --dashd $D --fs ext4 --seed 20261101 --shard 3/24 \
  -- -dbcache=4 -dbbatchsize=20000
st3b-crash campaign $F/w-40k ... --shard 0/4 --sample 10      # the 40k set
st3b-crash campaign ... --points 400-480                      # one window again (ST3b-D16)
st3b-crash campaign ... --c0-only                             # C0 alone at every point
st3b-crash fault $F/w-small --case DF-MID --rdashd $B/rdashd --dashd $D --fs ext4 --seed 1 \
  -- -dbcache=4 -dbbatchsize=20000
st3b-crash vm $F/w-small --scenario V-REORG --fs ext4 --kills 4 --seed 5 --dashd $D \
  --guest /work/scratch/st3b/vm/guest-<id> -- -dbcache=4 -dbbatchsize=20000
st3b-crash vm $F/w-40k --scenario V-FLUSH --fs ext4 --kills 4 --seed 5 --dashd $D \
  --guest /work/scratch/st3b/vm/guest-<id> -- -dbcache=4 -dbbatchsize=2000   # as weekly-vm.sh (S7 r4)
st3b-crash baseline $F/w-40k --rdashd $B/rdashd --dashd $D -- -dbcache=4 -dbbatchsize=20000
```

A run prints `run <id>` first. Its directory is `/work/scratch/st3b/run/<id>`.

## Reading results

- `summary.txt` has the seed and shard, then one row per component: user, system and wall
  seconds, the component, `exit`, `points`, `failures` and `status`. Campaign rows also carry
  `recorded=` and `c0=`, and the file ends with `DONE failed=N`. A component that checks no
  point fails (`NO-POINTS`). Other statuses:
  - `TIMEOUT`: the unit's timeout, a stop or Ctrl-C. The row has the points the component had
    checked and its CPU-s (under a unit), or says no component had started.
  - `CPU-CAP`: the 40k unit's 4 CPU-h.
  - `DEFERRED`: the heavy-start check failed. Exit 75; the 40k unit retries every 2 h.
  - `BUSY`: the device lock stayed held. Exit 75; the nightly retries hourly and the 40k unit
    every 2 h.
  - `BUILD-FAILED`, `NO BINARY`, and `BAD-ENV` for a bad `ST3B_40K_*`.
- The ledgers are TSV, one row per component, in `nightly/ledger.tsv`,
  `device-40k/ledger.tsv` and `weekly-vm/ledger.tsv`. The VM ledger adds counted kills, wall
  seconds and the per-reorg quota lines.
- `FAIL ...` lines in a component's log name the point or kill and the check.

## Repro of a failure

A failed run keeps its directory and prints `kept <dir> for the repro`. The nightly records
it in `kept.txt` (see [Cleanup](#cleanup)).

- **A failed point** (campaign). `run/<id>/pt<i>.repro.txt` gives the log's sha256, the point,
  floor, filesystem, draw and rdashd arguments, and the failures. Beside it are `pt<i>.img`,
  `pt<i>.c0.img` and the pristine `pt<i>.c6.img`, plus the recording's log.
  - C1–C4 on a new binary: make a reflinked copy under `run/<id>/`
    (`cp --reflink=always pt<i>.img pt<i>.try.img`). Attach and mount it through the wrapper
    (`sudo -n st3b-priv losetup-attach <copy>`, then `mount <loop> <dir> ext4|xfs`, then
    `chown-mnt <dir>`), where `<dir>` is a new directory under `/work/scratch/st3b/mnt/`. Run
    `st3b-crash check $F/w-small <dir>/rdashd --rdashd <new> --floor ack:<p>` (add `--dashd $D`
    for C5), then `umount <dir>` and `losetup-detach <loop>`. `check` itself works on reflinked
    copies of the datadir.
  - C6 again: `st3b-crash c6 $F/w-small run/<id>/pt<i>.c6.img --rdashd <new> --dashd $D --point <i> --floor ack:<p>`.
  - Near the same place on a fresh recording: `campaign --seed <same> --points <i>-<i>`. This
    draws from a new log, so the point is of the same kind near the same place, not the same
    state (ST3b-D16).
  - A point number is reproduced with the head that produced it (ST3b-D24 (3)).
- **A failed kill** (kill smoke). Use `run/<id>/lap<k>/killed`, the datadir as the last kill
  left it, copied before the restart recovered it. Run `st3b-crash check $F/w-small
  run/<id>/lap<k>/killed --rdashd <bin> --floor ack:<p>`, with `<p>` from the kill's floor. That
  replays the recovery the failing check saw. `lap<k>/node` is the state after that recovery, its
  checks and the harness's final kill, so it does not replay the recovery.
  `ST3B_KEEP_RUNS=1` keeps passing laps too.
- **A failed VM kill.** The run keeps the failed lap's directory, `run/<id>/lap<k>/`: its disk
  `data.raw` and each boot's `console-<boot>.log`. Rerun with the same `--seed`. The seed fixes the sequence of kill plans, not where the kills
  land, so a VM kill is reproduced statistically: rerun the scenario with more kills
  (PLAN-ST3b-S7 §2.3).
- **A fault case** keeps the failing attempt's files in its run directory (`<case>.img`,
  `<case>.c0.img`, the trace). `ST3B_KEEP_RUNS=1` keeps a passing
  attempt too.

## Cleanup

- After every run by hand: `st3b-crash leaks <id>` must print nothing, and so must
  `docker ps --filter label=st3b.run` (VM runs).
- **Stale runs.** These are runs killed before their teardown; their lock is free.
  `st3b-crash sweep --stale` removes their devices and mounts, and `--stale-vm` removes their
  containers. Both units sweep at start. The stop paths (TIMEOUT, CPU-CAP) first stop the
  component's whole session (TERM, up to 60 s, then KILL), then sweep the run they stopped and
  add its directory to `kept.txt` for the prune. Sweep exits 3 on a run that is alive, and 4 on an orphan: devices whose
  directory is gone. A person decides what to do with an orphan (ST3b-D24 (5)).
- **Directories.** Remove them only with `/work/scratch/mgr/rm-if-idle.sh <exact path>`, one per
  call, after an `ls` of the path. Never `sudo rm`. Remove a single file with plain `rm`.
- **The failure-dir prune.** At the end of each run, the nightly keeps the newest failure dirs
  listed in `kept.txt` while there are at most 3 and they total at most 10G. It removes older
  ones through rm-if-idle. A dir that still has devices stays listed until it sweeps. The newest
  always stays, and if it alone is over 10G the summary says `KEPT OVER CAP`. The 40k unit
  shares the nightly's `kept.txt`, so the cap covers both.
- **Containers.** Kill them by name, `docker rm -f <name>` (names start `st3b-`). Never use
  `pkill -f`.
- `/work/scratch/st3b/dryrun/<id>` (dry runs) and `baseline/<id>` stay until removed by hand.

## Budgets and heavy-start rules

| unit | budget |
|---|---|
| device nightly | ≤ 2 CPU-h (PLAN-ST3b §9). d-ibd-s's 1/24 shard is most of it (ST3b-D16). |
| device 40k weekly | ≤ 4 CPU-h, enforced on the unit's cgroup (`ST3B_CPU_CAP_S=14400`, a CPU-CAP row). W-40k's IBD alone is about 40 min of rdashd CPU (S8). |
| VM weekly | ≤ 12 h wall, Sundays from 05:10 UTC (owner ruling, S8), so it ends before Monday's 04:10 nightly. The cost is wall time on one 2-vCPU guest, not CPU. Each W-40k component costs its kills plus a tail: after the last kill it syncs to the tip, 4,044–5,789 s whatever the kill count. The acceptance at S7 r4 (acc3, 48b5923c2; v-ibd-ext4 from acc1 at 52c7d2de6, whose path r4 leaves alone) measured, per counted kill and tail: v-ibd 43.8 s + 4,439 s (ext4) and 36.5 s + 4,711 s (xfs), v-flush 55.6 s + 5,789 s and 31.8 s + 5,082 s, v-recovery 53.1 s + 4,612 s and 46.5 s + 4,044 s. With r4, almost every boot's first flush is killed, so the kills sit at heights of about 100–900, where a boot's header re-sync is about 50 s rather than the 3.4 min at the tip. At the routine counts (20 ext4 / 10 xfs for the W-40k components, 50 / 20 for v-reorg), the expected components are v-ibd 5,315 + 5,076 s, v-flush 6,901 + 5,400 s, v-recovery 5,674 + 4,509 s and v-reorg 965 + 560 s: 34,400 s, or 9.6 h, plus a build and guest of 3–4 min (incremental). That is inside 12 h, so neither budget cut applies (owner ruling: v-recovery to 10/5 first, then v-flush to 15/8, never v-reorg or v-ibd). Each drop-in's timeout is (kills × wall per counted kill + tail) × 1.5, plus the 600 s guest-lock wait (`ST3B_VM_LOCK_WAIT`, which runs inside the component's unit), with the arithmetic in the drop-in. v-reorg's is its whole wall × 1.5 + 600. Size from wall per counted kill, not the p95 kill cycle: a cycle leaves out the boot, the checks and the kills outside the window. The driver's 20 h cap is the worst case: every component at its timeout plus the 3 h build bound, about 19.2 h. |
| disk | Each run ≤ 10G, kept failure dirs ≤ 10G, the VM image and kernel ≤ 1G. Each unit's checkout and release target is about 10G. |

Heavy-start rules:

- **Before any live run by hand**, check `df -BG /work` (at least 230G free) and `uptime`
  (5-minute load under 80) in a command of their own. Run one guest at a time.
- **The units check for themselves.** The VM weekly's `--prepare` and the 40k unit
  (`ST3B_MIN_FREE_G=260`, `ST3B_MAX_LOAD=80`) start only with at least 260G free and a load
  under 80. Otherwise they record DEFERRED and exit 75, and the unit retries every 2 h, at most
  6 times a day. The 40k unit checks once before its build and again once it holds the device
  lock.
- **One device run at a time.** `nightly-device.sh` holds `/work/scratch/st3b/device.lock` from
  after its build to the end. It waits up to `ST3B_LOCK_WAIT` (4 h by default, 1 h in the nightly
  unit), then records BUSY and exits 75. No component inherits the lock's fd, so nothing a run
  leaves behind holds it. One guest at a time: `weekly-vm/guest.lock`.
- **The CPU cap** counts only on a systemd service's own cgroup. Run a capped component by
  hand as `systemd-run --user --unit=<name> --same-dir --collect -E ST3B_CPU_CAP_S=…
  contrib/node/crash/nightly-device.sh …`.
- **Privileges.** Only `sudo -n st3b-priv`; Docker runs with `--user $(id -u):$(id -g)`. Builds
  run at `nice -n 19` with `CARGO_BUILD_JOBS=8`, each in its own target.

## Units and timers

`units/` holds:

- `st3b-device-nightly.{service,timer}`;
- `st3b-device-40k.{service,timer}`;
- `st3b-vm-weekly.{service,timer}`, plus `st3b-vm-weekly@.service` and its per-component
  drop-ins.

Each unit runs from its own checkout and target, which you set up once: a worktree of the
primary repository that the unit detaches at the `node/integration` tip (a local branch;
nothing is fetched).

```sh
git -C ~/workspace/rust-dashcore worktree add --detach /work/scratch/st3b/nightly/checkout node/integration
git -C ~/workspace/rust-dashcore worktree add --detach /work/scratch/st3b/device-40k/checkout node/integration
git -C ~/workspace/rust-dashcore worktree add --detach /work/scratch/st3b/weekly-vm/checkout node/integration
contrib/node/crash/install-units.sh             # copies and reloads; schedules nothing
systemctl --user start st3b-device-nightly.service    # one run by hand, as a manual unit
journalctl --user -u st3b-device-nightly.service -f
contrib/node/crash/install-units.sh --enable    # the timers (Q13: the owner, after merge)
```

READY evidence is one nightly and one weekly run, each started by hand as a unit. Per Q13 the
owner installs the timers after merge. A T3 restart kills a thread's child processes but not a
`systemd --user` unit. Use `systemd-run --user --unit=<name> --same-dir --collect <cmd>` for
any long run by hand.

## Follow-ups

- **Fixture peer pin (S8, later):** ST3b's fixture-peer defaults still name Dash Core v24.0.0-rc.4, while the program pin is rc.5. They are `ST3B_DASHD`'s default in `nightly-device.sh` and `weekly-vm.sh`, `PIN` and the default dashd in `mkfixture.py`, and `D=` above. Moving them needs a re-check of the fixtures (W-small, W-40k) against rc.5, so they stay at rc.4 until then.
- **Spread the V-FLUSH and V-RECOVERY kills over heights (S8, later):** since S7 r4, almost every boot's first flush is killed, and a boot gets about 8 blocks further, so the counted kills sit at heights of about 100–900 of W-40k's 40,000. A seeded skip over a boot's first flushes would spread them over the chain. This changes the VM path, so it waits for its own round.
