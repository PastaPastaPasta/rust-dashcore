# PF0-N plan review — Opus high, narrow round 2

Target: `/work/scratch/pf0-n/plan-rev4.md` (D37–D63, P18), read against plan-rev3.md, my r1
(`/work/scratch/pf0-n-r1-opus/PF0-N-plan-r1-opus.md`), the manager's rev4 rulings, and perfgate at `node/pf0-n`
3b8b81886 (`~/workspace/rdc-wt/pf0-n`, read only). I ran no gate, no anchor and no `pf0-*`/`pfn-*` unit. My only
probes were tiny unprivileged ones in `/work/scratch/pf0-n-r2-opus` (0700): personality and PDEATHSIG across a bwrap
exec, and affinity widening under `taskset`. I deleted them afterwards. The new manager ruling (1c merges with 1 + 1b
under one carried re-anchor) is taken as given. Line numbers are plan-rev4.md lines.

## Facts checked for this round

- **NUMA on this VM.** `/sys/devices/system/node` has two nodes. node0 has CPUs 0-15 and about 103 GB (MemFree
  6.8 GB at review time, about 78 GB file pages). node1 has CPUs 16-31. The online count is 32 and
  `randomize_va_space` is 2. "The lowest `cpus` CPUs of node 0" is therefore `0-1` for `cpus = 2`, which is valid here.
- **Where the personality reset happens.** It happens at the exec of `/usr/bin/bwrap`, not inside the jail.
  - With `setarch -R bwrap …`, bwrap's *outer* process already has personality `0`, and so does its child, with or
    without `--unshare-user`.
  - `setarch -R unshare -Ur …` keeps `0x40000`.
  - bwrap has no file caps, and `AT_SECURE` = 0. The outer process runs under the AppArmor label `bwrap (enforce)`
    (`/etc/apparmor.d/bwrap-userns-restrict`), and its child runs under `bwrap//&unpriv_bwrap`. The likely cause is
    that AppArmor profile transition, not bwrap's code.
  - **The same exec also clears PDEATHSIG.** A `setpriv --pdeathsig KILL -- bwrap … sleep` survived its parent's
    exit, while the control `setpriv --pdeathsig KILL -- sleep` died.
- **Affinity can be widened.** Under `taskset -c 5`, `sched_setaffinity(0, {0,1})` succeeds: an outer affinity mask
  doesn't limit a child's later mask. Only a cpuset does (`AllowedCPUs=`). The invocation scope delegates only
  `+memory +pids`.
- **`counter_hash()` at 3b8b81886** (`measure.py:219-249`) hashes:
  - the bytes of `launch.c`;
  - the source of `bench_argv`, **`node.parse_perf_csv`**, `strace_region`, `split_sched`, `sandbox.wrap`,
    `sandbox.counted` and `sandbox.readonly_binds`;
  - `CC_FLAGS`, `PERF_COUNTER`, `STRACE_COUNTER`, the syscall sets and regexes, `len(readonly_binds())`;
  - the versions of perf, strace, bwrap and taskset.

  `harness` (the whole perfgate tree) is in `COMPARABLE` only, not in `ANCHOR_COMPARABLE` (`gate.py:280-283`). So a
  new `node_launch.c` leaves both anchor-comparable fields unchanged, and D44/D45 hold as written, provided
  `node.parse_perf_csv` and `sandbox.wrap` keep their exact source (D45 lists both).
- **RocksDB.** `dashd-store/src/rocks.rs:78-79` is `(available_parallelism/4).max(2)`. `cpus = 2` therefore doesn't
  starve RocksDB, and node rows are insensitive to `cpus` ≤ 11 on that path.

## r1 items

| r1 | status | where | note |
|---|---|---|---|
| M1 placement/ASLR/CPU count | **CLOSED** | D37, D38, §3 l.226-235, §11 l.744-749, §13 1c | Fixed hashed `cpus` + CPU list + node (component 13); `ncpu` = `SC_NPROCESSORS_ONLN` in both tuples, which affinity doesn't change. ASLR is off for rdashd **and its whole process tree**: personality is inherited across fork/exec, NNP stops setuid transitions, and P18 shows that the in-jail exec keeps `0x40000`. The bwrap stage itself (outer + jail-side pre-exec) runs with ASLR on, which is the same as bench rows (`sandbox.wrap(bench_argv)` puts `setarch` inside bwrap too), so the two match. Fix items: R2-3 (wrong test), R2-5 (mechanism and side effects), R2-11 (NUMA) |
| M2 strace vs census; census by exe | **CLOSED** | D39, D40, §11 l.658-661, 750-751 | Strace is dropped within D27, and identity is `(pid, starttime)` + spawn-k membership, with both negative tests. New race: R2-1 |
| M3 REFUSED algebra | **CLOSED** | D41–D43, §7 l.426-474, §11 l.752-760 | Set algebra checked below; no hole lets a regression pass. R2-6 is a consistency fix for owner-only changes |
| M4 bench launcher / global hash | **CLOSED** | D44, D45, §8 l.543-544, §11 l.761-762 | Verified against the code (facts above). R2-7 is about how the pin test is written |
| M5 datadir state | **CLOSED** | D46, §4 l.301-304, §11 l.763-765 | Fresh datadir created and verified by the broker; separate `/pf0/run/version`; exe sha recorded by the broker. R2-10: a stale §12 note |
| m1 placeholders, per-component hashes | CLOSED | D48 | |
| m2 closure roots | CLOSED | D49 | |
| m3 D26(a) pid-ns mixing | CLOSED | D50 | R2-9: a starttime tick-collision nit |
| m4 bounds, kill matrix | CLOSED | D51, D52 | R2-8: wording, plus one coverage nit tied to R2-5 |
| m5 /tmp deltas | CLOSED | D53 | |
| m6 prefix, `measure --dashd` lock | CLOSED | D54 | |
| m7 PDEATHSIG/preexec_fn | CLOSED | D55 | |
| m8 ns fds | CLOSED (design) | D56 | R2-4: open timing |
| m9 GATE size | CLOSED | D57 | |
| m10 whole-chain self-probe | CLOSED | D58 | |
| m11 prep path | CLOSED | D59–D61 | |
| m12 machine-visible NEW | CLOSED | D62 | |
| m13 testability, noise, post-merge | **OPEN (one step)** | D63, §13 l.846-861 | Testability and the noise check are fine. The post-merge list doesn't anchor node rows in the now-ruled sequence: R2-2 |
| 14 jail hardening | CLOSED | D47, P18 | CapBnd 0, CapEff 0, NNP 1, nested userns ENOSPC, rdashd reached ready |

### REFUSED/REFUSAL-REGRESSION set algebra (asked)

Notation: B = base `refused_on` options, H = head, A = anchor. ok counts as ∅. D43 runs first (B against A), then D42
(H against B).

- **D43:** B = A → ok. B ⊊ A → ERROR (obligation). B ⊄ A → ERROR (inconsistent). That partitions every case.
- **D42:** H = B, H ⊊ B, or H ⊄ B. That partitions every case too. H ⊄ B also covers "dropped one option, added
  another", which is correctly a regression.
- **Base ok, head REFUSED** is H ⊄ ∅ → REFUSAL-REGRESSION, consistent with the table.
- **Base REFUSED, head ok** is H = ∅ ⊊ B, routed to the NEW predicate with approval.
- **No pass-through.** A head regression in a gate where D43 already failed is masked as ERROR, but it still
  blocks, and the next gate after re-anchoring sees it. Retries can't launder a new refusal: each refused token must
  be an original token and gets its own retry, so H collects every refused token.
- **Innocent-PR blocking is by design and visible.** After a merged narrowing (H ⊊ B) or a NEW-FROM-REFUSED, every
  later gate is ERROR "anchor obligation" until `run anchor --only <row>`. The message points at the anchor, not the
  PR, and step 5 of §13 schedules the fix.
- **The one inconsistency** is owner-only differences (R2-6).
- **Small gaps, nit:** say that an anchor `pending` record (if any) is the same as "no record", and add the "base
  REFUSED, head error" row (the "as today" ERROR).

## New findings from the rev4 changes

### Fix in the plan text before the commit that implements each one

**R2-1. The census races the broker's registrations (D40; commits 2–3).**
- **The window.** A helper is registered when "the broker reports" it. Between the broker's fork and outer_init
  reading that report, the helper exists in `<scope>/harness`:
  - first with exe `python3` (the broker's pre-exec fork; `subprocess` vfork/fork), then node-launch;
  - unregistered and not in a spawn-k.
- **The effect.** A census sample (every 250 ms, and at each spawn's start) in that window raises
  "census: unexpected process" on a clean run. At load 50–190 the window can be tens of ms, i.e. a few percent per
  spawn and many spawns per gate. The result is false ERRORs, and given D27 they block.
- **The same applies** to any helper process that outer_init or the broker execs in the outer ns after the census
  starts: `ip`/`mount` for setup, `cp`/`rm` for D46/D60 if they aren't done in-process.
- **Fix:**
  - Accept a process whose `PPid` has a registered `(pid, starttime)` (the broker), which is in `<scope>/harness`,
    and whose exe is the broker's interpreter or node-launch, but only until the hello deadline. By then it must be a
    registered helper; otherwise ERROR.
  - State that outer_init's setup runs before the census starts, and that the D46/D60 copy, remove and mkdir are done
    in-process by the broker (no outer-ns children).
  - Test: a stress run of ≥ 200 spawns at load with zero census ERRORs, in addition to the two negative tests.

**R2-2. In the now-ruled merge order, the node rows never get anchored (§13 post-merge 2–3, D21 l.71, D45 l.95).**
- **The gap:**
  - Step 3 says "that anchor records the node rows", where "that anchor" is step 2's carried full re-anchor.
  - Under the new ruling, 1 + 1b + 1c merge on their own first. That re-anchor then runs a harness that still refuses
    node rows (DEC-76(b) is lifted only in commit 8), so it records no `ok`/`refused` node record.
  - D21's "the carried re-anchor at the merge already does" has the same problem.
- **The effect.** After the 2–9 merge, every authoritative gate is ERROR not-in-anchor for every node row, until
  someone does something §13 doesn't list.
- **Fix:** add the step "after the 2–9 merge and before the next authoritative gate: `run anchor <tip> --only node-*`
  with the new harness (records ok rows with medians and inputs, REFUSED rows with `refused_on`)". Note that `--only`
  is valid there because 2–9 change no anchor-comparable field (D45). Fix D21's wording to match.

**R2-3. The "outer affinity excluding the CPUs → ERROR" test is wrong (D37 l.87, §11 l.748).**
- **Why.** `sched_setaffinity` in the clone3 child can widen past an inherited `taskset`/`CPUAffinity=` mask (probe:
  under `taskset -c 5`, setting `{0,1}` succeeds). The test as written can't pass, and an implementer might "fix" it by
  refusing whenever the parent mask doesn't cover the CPUs, which is a policy nobody ruled on.
- **Fix:**
  - Under an outer `taskset`, the node is still on exactly `0-1`, which shows placement doesn't depend on the
    caller.
  - A test scope with a cpuset excluding them (`-p AllowedCPUs=…`, or a cpuset sub-cgroup) → EINVAL → ERROR
    "placement unavailable".
  - Fix D37's example to match.

**R2-4. The ns fds may be opened before the holder has unshared (D56 l.106).**
- **Why.** "Right after spawning it … opens `/proc/<pid>/ns/{user,mnt}`": until the child has run `unshare(2)`,
  those links are the harness's *own* namespaces. P18 hid this with `time.sleep(0.5)`. The failure is closed (nsenter
  into the init userns fails, so ERROR), but it is a load-dependent flake.
- **Fix:**
  - Open the fds only after holder.py's ready signal (GATE mounted, exes copied), on a pipe the harness holds.
  - Then assert that both ns inodes differ from `/proc/self/ns/{user,mnt}`.
  - Then confirm with `pidfd_send_signal(…, 0)`.

### Nits

**R2-5. Name the mechanism behind "bwrap resets the personality" (D37, §3 l.259, P18), and its side effects.**
- **The mechanism.** The reset happens at the exec of `/usr/bin/bwrap`; likely cause: AppArmor's `bwrap` profile
  transition. The same exec clears **PDEATHSIG** (probe above).
- **(a) §5 l.358 is wrong for unmeasured spawns.** It says "`--into` helper → its child has PDEATHSIG SIGKILL". For an
  unmeasured spawn (version, preflight, start without perf), the clone3 child execs node-launch → bwrap, which loses
  PDEATHSIG. The real backstop there is the broker's pidfd → `cgroup.kill`, and failing that, outer_init's pid-ns
  teardown. That is adequate, but the text should say so. Run the D52 helper@3 row on an unmeasured spawn, so that
  this path is the one exercised.
- **(b) The reset depends on host policy that no fingerprint records.** If an AppArmor update stops the transition,
  bwrap's counted instructions in bench and node rows shift by a small, unexplained amount. Record bwrap's AppArmor
  label (from the D58 self-probe, `/proc/<bwrap>/attr/current`) in `host_meta` as a NOTE. Alternatively add it to 1c
  now, while that commit's re-anchor is open anyway.
- **(c) State in D37 that the ASLR claim covers rdashd and its descendants,** and that the bwrap stage is randomized,
  as it is for bench rows.

**R2-6. Owner-only obligations are inconsistent (D42, D43, DEC l.504-505).**
- **The inconsistency:**
  - D43: base and anchor have equal options and differ only in owners → "ok, NOTE + obligation". It never ERRORs.
  - D42: a head owner-only change → NOTE, with no obligation.
  - The DEC says every recorded obligation makes later gates ERROR.
  - D20(4) still requires owner equality for NEW.
- **The effect.** An unenforced owner drift surfaces as ERROR "refused owners changed since the anchor" on the
  later PR that implements the option. That PR is innocent of the drift.
- **Fix:** make the D42 owner-only case record an obligation, and make D43's owner-only case ERROR "anchor obligation"
  like B ⊊ A. The owner change then reaches the anchor right after its merge, which §13 step 5 already schedules.

**R2-7. Write the `counter_hash()` pin test so a host upgrade doesn't break it (D45, §11 l.761).**
- **The problem.** The value includes the tool versions and `len(readonly_binds())`, so a literal pin fails after
  an `apt upgrade` of perf, bwrap or strace on any host.
- **Fix:**
  - Pin `_COUNTING_SOURCES` + the constant parts, and recompute the version part.
  - Also assert that the bench launcher artifact's cache key (`sandbox.compiled(LAUNCH_SRC, …)`: source, `CC_FLAGS`,
    flags) is unchanged. A change to `compiled()` for `node_launch.c` could otherwise alter the bench launcher build
    without touching `counter_hash`.

**R2-8. D52 wording.**
- The rationale lists the **holder** among the triggers of mechanism (i). §5 l.362 says, correctly, that a holder
  death does *not* tear down running invocations: it fails the gate through the holder's pidfd. Move holder@4 to its
  own mechanism ("gate fails, invocation runs out").
- Otherwise the 17-chain argument holds: (i) at all 7 points, (ii) and (iii) once each where they differ. The full
  matrix is kept as an extended test.
- The fake perf at point 2 must pass the census exe check through the same configured tool path, or the expected
  ERROR reason becomes "census".

**R2-9. D50's starttime-only check from inside the view can collide.**
- `stat` field 22 has a 10 ms tick. The broker and the view's bwrap are consecutive children of outer_init, so the
  view's pid 1 can share the broker's tick and fail the test spuriously.
- **Fix:** compare `(starttime, comm)`, or have the hooked processes set a test-token `comm`.

**R2-10. §12 l.787-789 ("the second run takes the loaded-index path") is "unchanged", but D46 may have changed its
premise.**
- Each measured run now starts on an empty datadir. If "second run" meant the measured run after the version run's
  leftovers (the M5 artifact), the expected-shift note is obsolete.
- D46 also rules out rows with two node lifetimes on one datadir per rep (restart workloads) until a new class gets
  its own D-row. Say so.

**R2-11. NUMA binding.**
- Node 0 is the right choice here.
- `MPOL_BIND` turns node-0 exhaustion into a mempolicy-constrained OOM kill, while node 1 has free memory. That kill
  is not a memcg event, so `memory.events` of spawn-k doesn't show it, and it would be classified as a bare "death by
  signal 9".
- **Fix:** keep BIND but say the ERROR text checks `/proc/vmstat` `oom_kill` and the kernel log reason. Or use
  `MPOL_PREFERRED`, which keeps placement deterministic in the normal case.
- P18's `place.c` has no `set_mempolicy`, so "the mempolicy survives every exec" is only covered by the §11 test.
  That is fine, but it isn't P18 evidence.

**R2-12. Small text fixes.**
- §15 P18 says "perf → place 2 → bwrap", but `p18.py` runs `place 2 perf … -- bwrap`.
- "`place 1 bwrap … cat` → 0" isn't in `p18.out`. The evidence is jailcheck's `personality=00000000`.
- `D` in "timeout = D+60 / RuntimeMaxSec=D+30" (§3 l.215-217) is still undefined. Define it as the sum of the class
  deadlines plus `deadline_cleanup` for the spawns the invocation may make. D51 raised those deadlines.
- §13 noise check:
  - record the loadavg per run;
  - require part of it at load ≥ 50, since D51 targets 50–190;
  - say that node rows are uncached, so the base = head no-op gates aren't vacuous. True at 3b8b81886:
    `measure_nodes` always measures.

## Summary

All five r1 majors and minors 1–12 and 14 are closed. The rev4 mechanics hold up when checked against the code and the
host:
- the `counter_hash` separation;
- ASLR for rdashd's tree;
- node-0 placement;
- the set algebra;
- the 17-chain argument.

Four plan-text defects should be fixed before the commit that implements each one:
- the census registration race (R2-1);
- the missing node-row anchor step in the ruled merge order (R2-2);
- a test that can't pass (R2-3);
- the ns-fd open timing (R2-4).

None of them lets a regression pass silently, and none needs a redesign. The rest are nits.

APPROVE-WITH-NITS
