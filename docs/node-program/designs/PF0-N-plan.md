# PF0-N plan: node workloads on PR code (BL-128, ends DEC-213)

Branch `node/pf0-n` at 130bc1439 (`~/workspace/rdc-wt/pf0-n`). Probes and prototypes: `/work/scratch/pf0-n/proto/`
(`launch.c`, `startup_probe.py`, `startup7*.log`). Nothing under /work/perfgate, /work/bench or /work/chains was read or written.

## 1. Threat model delta vs DEC-76

DEC-76 holds unchanged: the gate catches accidental regressions and accidental damage to gate state by PR code. Forgery
is out of scope except where the defence is cheap. Two things change.

- **(b) is met instead of refused.** The PR's `rdashd` runs, prep runs included, but only inside a per-run sandbox.
  - **Writes:** only its own run directory (a reflinked clone in a per-gate node root) and one pipe.
  - **Not even visible:** the ledger, the anchor, reports, the cache, the rest of `$PERFGATE_ROOT`, the node-bench
    master and the dashd baselines in it, the other side's runs, `/work/bench`, `/work/chains` and `$HOME`.
  - **Unreachable:** TCP (localhost included, so not sccache), the user bus, docker, and signals to host processes.
  - **Trusted code reads what it wrote:** files are opened with `O_NOFOLLOW` as regular files (`read_regular`), and
    clones refuse symlinks that leave the tree.
  - **Hard links fail:** a link onto a read-only bind returns EXDEV, because it crosses mounts.
- **Trust per counter** (DEC-52 4 gates the first three):
  - `page_faults`: authoritative. Counted by `perf stat` outside the sandbox, and perf's output fd never reaches the
    node.
  - `alloc_bytes`: self-reportable, like the bench shim's counters. The LD_PRELOAD counter runs inside the node by
    necessity. Its only output is a pipe the trusted launcher drains; there is no file and no path. A forged line
    can only add to the sum.
  - `peak_rss_kib`: see Q3.
  - INFO only: `instructions` (external), syscalls (external strace, but `CLONE_UNTRACED` hides children) and I/O
    bytes (self-reported).
- **New finding F0 (existing bench path).** PR code can forge `instructions_process` today, the counter DEC-76 calls
  authoritative. `perf stat -o F` (and GNU `time -o`) leave F's fd open in the child. Through
  `/proc/self/fd/3`, a bench (or node) reopens the file even though the sandbox doesn't mount it. I showed this: a
  padded line written that way made `parse_perf_csv` return 7 instead of 3.6 M (`measure.py:150`). `strace -o`
  doesn't leak. The fix is cheap (the launcher below closes fds ≥3 before bwrap), so DEC-76 says to make it.
- **Residual risks, documented:**
  - CPU and time burn: node-bench timeouts bound it.
  - Disk fill inside the run dir: we add `RLIMIT_FSIZE` of 4 GiB per file in the launcher, plus node-bench's
    free-space check.
  - Memory: node-bench's `--limits` cgroup, or nothing (Q8).
  - A fake refusal to dodge a row (§5).

## 2. Sandbox mechanism (probed on agentbox, uid 1001, no root)

| mechanism | result |
|---|---|
| `bwrap` (already the bench sandbox) | **works:** `--unshare-all`, tmpfs over `/work`, `$HOME` and `/run`, binds back read-only. The probes from inside failed: localhost TCP, `kill -0` on the gate's pid, `~/.ssh`, the ledger, `systemd-run --user`, `/run/user` and the docker socket. |
| `unshare -r` / `-Urnp` | **works now**: `apparmor_restrict_unprivileged_userns=0`. The pf0-counter-gate memory note says blocked; I'll correct it. Not needed. |
| `systemd-run --user -p PrivateNetwork/ProtectHome/ProtectSystem=strict` | **rejected.** `ProtectHome` doesn't cover `/work/home`, so `$HOME` stayed visible. And the unit isn't in our process tree, so perf and strace can't wrap it. |
| perf, strace outside `bwrap` | **work.** They count and follow through the user and pid namespaces (strace sees bwrap's and the node's `execve`). |
| `wait4` max RSS through bwrap | Only with `--as-pid-1`: bwrap's own init doesn't pass it up (2 MB vs 205 MB). |
| `--as-pid-1` with `--die-with-parent` | **Broken**: a namespace init ignores its own PDEATHSIG, so the node outlived a SIGKILLed harness. The launcher handles it (below). |
| `--info-fd 3` with fd 3 closed | bwrap **hangs** in wait. The launcher always supplies it. |
| `bwrap --new-session` | Escapes node-bench's process-group signalling (`killpg`). The launcher forwards. |

**Design.** A trusted C helper, `perfgate/launch.c`, is compiled and hashed like `alloc_preload.c`. The prototype is
`proto/launch.c` (≈150 lines). Its chain:

`node-bench → launch (session leader) → perf stat -o CTL/perf.csv (own process group) → launch --inner → bwrap … --as-pid-1 → rdashd`

- **Outer `launch`:** forwards SIGTERM and SIGINT to the sandbox's pid 1 (its global pid, from bwrap's info JSON).
  It drains the alloc pipe (fd 5 inside) and a result pipe, and after perf exits writes
  `CTL/{alloc.jsonl,result.json}` with `O_EXCL|O_NOFOLLOW`.
- **`--inner`:** closes every fd but 0, 1, 2 and 5 (perf's leaked one included). Its PDEATHSIG=SIGTERM handler
  SIGKILLs the sandbox's pid 1 from the parent namespace, where that signal is delivered. It `wait4`s bwrap, which
  gives the subtree's `ru_maxrss`.
- **Probed:** only fds 0, 1, 2 and 5 inside. SIGTERM through `killpg` → the node exits 0 in 0.04 s, with counts and
  RSS written. SIGKILL of the session → no survivors. The syscall pass is the same chain with `strace -f -c -o` in
  perf's place.
- **Layout:** `--ro-bind / /`, then tmpfs over `/work`, `$HOME`, `/run`, `/tmp` and `/dev/shm`.
  - Bound back: the exe copy and the preload library (read-only), the run directory (read-write).
  - `--clearenv`, then `PATH`, `HOME=/nonexistent`, `LANG=C`, `GLIBC_TUNABLES`, `LD_PRELOAD` and
    `PERFGATE_ALLOC_FD=5`.
  - stdin `/dev/null`, stdout and stderr to files in the run dir.
  - A fixed 4-CPU affinity, in the fingerprint, because tokio sizes its pools from it.

## 3. Counters outside the process

`perf stat -e instructions:u,page-faults` and `strace -f -c` run outside bwrap, and their outputs never reach the
sandbox. Allocations go through `alloc_preload.c` in a new fd mode: one `write()` of under PIPE_BUF bytes to fd 5,
which is atomic. The path mode is removed. Peak RSS comes from the launcher's `wait4` or from the preload's VmHWM
(Q3). Every file the node wrote is read with `read_regular`, and no node-writable path is parsed for counts.

## 4. Per-gate node root and disk

- **Location:** `$PERFGATE_ROOT/node/<gate-id>/<side>/`, a `cp -a --reflink=always` of
  `$PERFGATE_ROOT/node/master`. XFS reflink works; I probed it.
- **Writable sets:** node-bench writes it, the node only its run dir. It's deleted at gate end and by `run clean`.
- **The master** is built by node-bench's own commands (sha256-pinned dashd download, fixture build), **never copied
  from /work/bench**. It's the manager's step: `perfgate run node-root init`.
- **Phase 1 needs no fixture** (the genesis workload below), so the master can start as an empty layout, and the
  fixture is built when RP0 lands (Q5).
- **Fingerprint:** the master's manifest hash (fixture tip, txs sha256, dashd sha256) joins the node fingerprint, so a
  rebuilt master makes the anchor incomparable instead of silently different.
- **Cost:**
  - phase 1: about 1 MB per gate (genesis datadir 248 KB, with per-run reflink clones deleted after each run);
  - phase 2: master ≈ 0.2–0.4 GB (estimated: pinned dashd plus a 30k-tx regtest chain), per-gate ≈ 0 until written;
  - prepared rdashd fixture datadirs: ≈ fixture size per binary per side, deleted at gate end.
  - That fits the gate-copy budget rules with a wide margin.

## 5. PENDING

- **Classifying a refusal.** node-bench runs a **preflight** of the exact node argv in the sandbox, on a throwaway
  datadir.
  - The refusal is written after rdashd has already created dash.conf, settings.json, blocks/ and debug.log, so the
    preflight can't run on a real datadir.
  - A row is pending only on exit 1 with the last stderr line matching exactly
    `^Error: (-[a-z0-9-]+) is not supported by rdashd yet \(([A-Za-z0-9-]+)\)\.$`, for an option node-bench passed.
  - rdashd names only the first refused option, so node-bench drops it and retries (≤20 times) to list **all**
    owners, e.g. `node-gbt: RP0, CH2a, IX1, P2a-net, CH1-node, MP1a, MP1b`.
  - It then exits 3, with manifest `status: pending, pending_on: [{option, owner}]`.
  - Anything else is ERROR as today. The fixture isn't needed for this.
- **Gate semantics** (needs a DEC; DEC-213 lapses):

| base | head | row | blocks? |
|---|---|---|---|
| pending | pending | `PENDING` (owners; "was …" if they changed) | no |
| ok | pending | `ERROR` ("the PR made rdashd refuse an option it accepted") | yes |
| pending | ok | `NEW`, with **no** "not in the anchor" drift ERROR (fixes a trap: the PR that implements RP0 would otherwise block on its own new row); the manager anchors right after the merge | no |
| error | anything | `ERROR`, as today | yes |

- **Verdict:** PENDING doesn't make a run INCOMPLETE. The ledger gains `"pending": {workload: [owners]}`.
- **Can a fake refusal hide a regression?** No. Faking needs base ok → head pending, which is an ERROR. With both
  sides pending, neither side measures anything, so there is nothing to hide.

## 6. Phase-1 rows, anchor and noise

- `node-startup` (fixture plus RPC) stays **PENDING** (RP0, IX1, P2a-net, CH1-node, MP1a/b, CH2a).
- **New live row `node-startup-genesis`.** node-bench workload `startup-genesis`, following ND1's recipe:
  - regtest; `-listen=0 -dnsseed=0 -connect=0 -fixedseeds=0 -printtoconsole=0 -mocktime=<fixed>`;
  - `-startupnotify="touch <run>/ready"` for readiness, then SIGTERM (exit 0 required);
  - starts on a genesis datadir prepared by one unmeasured run of the same binary, cached per sha;
  - ops 1. dashd runs it too, for the milestone ratio.
- **Probe** (7+7 runs at load ~100; trimmed range / median):

| counter | median | no mocktime | mocktime |
|---|---:|---:|---:|
| alloc_bytes | 11.60 MB | 0.0001% | 0.0001% |
| page faults | 3,056 | 0.29% | 0.23% |
| peak RSS: VmHWM, in-process | 17.8 MB | 0.6% (range) | 0.4% (range) |
| peak RSS: `wait4`, external | 17.3 MB | 3.4% | 3.8% |
| instructions (INFO) | 50.1 M | 0.026% | 0.11% |
| allocs (INFO) | 56,100 | 0.002% | 0.000% |

  Every gated signal sits under the 0.5% floor except external RSS, which falls under the existing 4 MiB step floor.
- **Before review:** a 15-run measurement on the integration tip, plus 10 no-op advisory gates (`--only 'node-*'`, my
  root) for a false-FAIL estimate.
- **After merge:** the manager runs `run anchor <tip> --only node-startup-genesis` (15 runs, ≈1.5 min). The other
  node rows aren't in the anchor while they are PENDING.

## 7. Tests

- **Unit tests** (no rdashd):
  - the launcher (fd set, forwarding, orphan kill, `O_EXCL`, RLIMIT) and the preload fd mode;
  - the refusal regex, including near-misses, other exit codes and unknown options;
  - the §5 table, the NEW-from-pending drift exemption and ledger `pending`;
  - the per-gate root lifecycle and fingerprint;
  - `PERFGATE_NODE_BENCH` outside the harness checkout → non-authoritative (Q6);
  - F0: a bench writing through `/proc/self/fd/*` leaves `instructions_process` genuine.
- **Selftest `run selftest-node`** (seconds; in the unit tests too). A malicious test binary posing as `rdashd` runs
  through the real chain in a temp `PERFGATE_ROOT`. It tries to:
  - write the ledger, anchor, reports, the master/baselines, the other side's run and a protected decoy tree;
  - rewrite perf.csv, result.json and alloc.jsonl via `/proc/self/fd`;
  - plant a debug.log symlink and hard-link the exe;
  - reach a test listener on 127.0.0.1, `kill -0` the gate;
  - daemonize a child, and flood fd 5.

  It asserts: tree hashes unchanged, perf and RSS equal a clean run's, the symlink refused, no survivors, and a
  malformed alloc line is an ERROR.
- **Acceptance** (real rdashd, my root):
  - a no-op PR → PASS (genesis OK, the rest PENDING);
  - a planted 8 MiB init allocation → FAIL on alloc_bytes, faults and RSS;
  - a PR that refuses `-fixedseeds=0` → ERROR;
  - an F0 forger bench → genuine counts.

## 8. Commits (≈1.5k lines, half of it tests; about 3–4 agent-days)

1. `fix(perfgate): keep counter output fds out of the sandbox` (F0: launcher in exec mode for benches, test, README
   trust text).
2. `feat(perfgate): trusted node launcher` (launch.c: perf/strace outside, pid-1 forwarding, orphan kill, RSS, RLIMIT).
3. `feat(perfgate): fd-only output for the preload counter`.
4. `feat(perfgate): per-gate node root and node sandbox layout` (`node-root init`, clone and clean, fingerprint).
5. `feat(node-bench): sandboxed node spawns, notify readiness, refusal preflight` (prep and measured spawns both go
   through the perfgate prefix with `{rw}`; exit 3 = pending).
6. `feat(node-bench): startup-genesis workload`.
7. `feat(perfgate): PENDING rows and node workloads on PR code` (lift `NODE_REFUSED`; gate.py table; ledger field).
8. `test(perfgate): node selftest and acceptance scenarios`.
9. `docs(perfgate): node threat model, PENDING, phase-1 rows` (README, plus the workloads.toml line).

## 9. Open questions (with recommendations)

- **Q1.** The phase-1 live row is a new `node-startup-genesis`, and `node-startup` stays PENDING. **Recommend yes.**
- **Q2.** `-mocktime` in phase 1? No measurable gain at genesis, but it pins IBD/tip-age state to a constant date.
  **Recommend yes**, with a fixed value; ND1 to confirm.
- **Q3.** Which peak RSS is gated?
  - External `wait4`: unforgeable, but it spreads 7% and falls under the 4 MiB step floor, a weak gate on 17 MB.
  - In-process VmHWM: self-reported, spreads 0.6%, at the same trust level as alloc_bytes.
  - **Recommend gating VmHWM**, with `wait4` as INFO and a flag when they disagree by more than 4 MiB.
- **Q4.** The PENDING semantics in §5 and the NEW-from-pending drift exemption need a DEC entry, which also ends
  DEC-213. **Recommend.**
- **Q5.** Phase 1 ships with an empty master. The fixture master (`node-root init --fixture`) and re-measured dashd
  baselines on it come when RP0 lands, and they won't match /work/bench's fixture. **Recommend.**
- **Q6.** An authoritative gate must use the harness checkout's node-bench: `PERFGATE_NODE_BENCH` elsewhere makes the
  run non-authoritative. **Recommend.**
- **Q7.** F0 rides this task (commit 1). It moves `instructions_process` by a small constant, to be measured; I
  expect it well under the 2% drift limit, so no re-anchor. **Recommend.**
- **Q8.** A memory cap for PR nodes: use node-bench's `--limits` cgroup if it's available unprivileged, or accept the
  risk under DEC-76. **Recommend accepting it for phase 1.**
- **Q9 (for ND1).** rdashd as pid 1 in its namespace:
  - SIG_DFL signals sent from inside the namespace are ignored;
  - notify children are never reaped;
  - abort-path behaviour.

  ND1 should also review the startup-genesis contract (args, readiness, SIGTERM). The CPU phase (deterministic bench
  mode, `d-*` work-thread list, per-thread counting) is out of scope here. Startup instructions already repeat to
  0.03–0.11%, which makes it the first CPU candidate.
