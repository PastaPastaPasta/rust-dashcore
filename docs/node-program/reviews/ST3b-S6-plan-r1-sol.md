# ST3b S6 plan review r1 (Sol)

Reviewed `/work/scratch/st3b/PLAN-ST3b-S6.md` against the approved parent §§3.5–3.6 and §7 S6 (DEC-698), the specified harness files, rdashd's abort/flush/shutdown code, and Dash Core **v24.0.0-rc.3**, using `git show` for the Core pin. Correctness only. Read `REVIEW-COMMON.md` first. No privileged devices, node processes, commits, pushes, or external posts. The implementation worktree was read-only to this reviewer; it was actively changing (head observed finally at `294f84ea48a37050b70cc50387590ae0eec3cab8`, with an existing dirty `rust_node.rs`). Findings below concern the addendum, not the author's in-progress implementation.

**Verdict: CHANGES-REQUIRED.** No blockers; five majors and two minors.

## Findings

### M1 — major: last-FLUSH replay can discard completed FUA writes, producing an impossible exit-time crash state

**Location:** `PLAN-ST3b-S6.md:29`.

**Scenario:** the log contains `FLUSH F; FUA journal-commit J; a_k; exit`, with no later successful FLUSH because the fault rejects subsequent writes. The proposed prefix through F excludes J, although J completed durably before the acknowledgement and exit. In a PREFLUSH/FUA sequence, the flush persists earlier writes and the subsequent FUA persists the commit itself. Losing the latter is not “nothing unflushed survives”: FUA is independently durable. On XFS this distinction particularly matters. The check can manufacture a rollback or an uncommitted filesystem transaction that a power loss at exit cannot produce.

**Evidence:** `dash-testkit/src/crash/logwrites.rs:11` explicitly distinguishes flush coverage from FUA durability; its `PointKind::Torn` contract and `points` survivor selection retain every FUA in the selected window. `image.rs:108` applies exactly the selected entries, with no implicit inclusion of later FUA writes. This is different from the existing campaign's valid historical FLUSH point: here the claimed crash time is **exit**, after those FUA completions.

**Fix:** preserve the exit cutoff before repair. Either replay the logged prefix through `exit` as one allowed survivor state (and describe it accordingly), or retain the prefix through the last FLUSH **plus all completed FUA entries before exit**, in log order, for the proposed minimal survivor state. Include a synthetic trailing-FUA test and assert what is included; do not let restore/unlink/unmount writes move the power-loss cutoff.

### M2 — major: the floor at the last-FLUSH entry omits the latest acknowledged state

**Location:** `PLAN-ST3b-S6.md:29`.

**Scenario:** `... a_(k-1); writes; FLUSH F; a_k; fault; exit`. Passing F's prefix to `campaign::floor` chooses `a_(k-1)`, because `a_k`'s mark follows F. If `a_k` is the only acknowledgement, the chosen floor is `Start`. A broken rdashd can lose an acknowledged flush, recover an internally consistent older chain, and pass C0–C3. Even after M1 is fixed, keeping this floor leaves the false green.

**Evidence:** `campaign.rs:139–152` selects only marks at or before the supplied prefix; `baseline.rs:144–152` marks an acknowledgement **after** `c2` returns. The parent §3.5 requires the last acknowledgement before the window as the minimum. The existing `floor` is correct for the existing campaign's historical point, but this new caller supplies the wrong time boundary.

Also, “acks during the window fail” is an assumption, not an oracle. Dirty data may already have become durable before the swap, a clean fsync need not submit any bio, and a broken implementation can falsely report success. `checks::c2` itself makes two durability-bearing `gettxoutsetinfo` calls (`checks.rs:87–98`); the first can succeed and the second fail, leaving `baseline::Drive::ack` with no mark for the successful first response.

**Fix:** record actual successful durability responses and their tips, including successes during arming/the fault and partial success of the two-call C2 operation. Pin the exit-time recovery floor to the latest such acknowledged trajectory position before exit, at least the last pre-window acknowledgement. Do not weaken it to fit the replayed bytes. Use the same acknowledgement obligation for repaired-device recovery; never treat an abort as a clean `stop` floor. Add `FLUSH; ack; exit` and `success; second RPC fails` regression cases.

### M3 — major: “exactly one abort line” rejects intentional Core-compatible shutdown behavior

**Location:** `PLAN-ST3b-S6.md:27`.

**Scenario:** rdashd correctly aborts on an I/O error. Shutdown's forced flush then observes the latched store failure and emits another `*** System error while flushing: ...`. Similarly, persistent low space can be reported again by shutdown. The process exits 1 without connecting more blocks, yet the proposed checker fails it solely for multiple abort lines.

**Evidence:** `dashd-node/src/abort.rs:69–83` logs and requests shutdown without deduplication. `dashd-node/src/steps.rs:635–653` expressly documents repeat aborts as DEC-241/DEC-253/BL-482 behavior; `:696–716` implements repeats for flush-owned and store failures. `dashd-node/src/sut.rs:1781–1799` expects repeated write-failure reports, and `:1802–1830` requires **two** low-space abort/error/flush-failure triples for the two shutdown flushes. Core rc.3 `shutdown.cpp:26–37` similarly has no one-shot guard, and `init.cpp:378–385,449–455` invokes forced flush twice during shutdown.

**Fix:** require a sanctioned **first causal abort after effective injection**, exit 1 within the bound, no panic, and no `UpdateTip` after that first abort. Permit the documented shutdown repeats and validate their messages/context. Do not require a product behavior change to make this new harness pass.

### M4 — major: a permanent whole-device fault can mask dropped fsync errors and retries

**Location:** `PLAN-ST3b-S6.md:11,27–31,52–53`.

**Scenario:** the flat-file barrier receives EIO, but a mutant ignores that result or retries the fsync. The next RocksDB write/sync on the permanently failing device returns an error, which takes the normal abort path before the next `UpdateTip`. All proposed checks can pass: a sanctioned abort, status 1, no tip after abort, and recovery to the last good state. The first persistence error was nevertheless mishandled. A bounded retry that ultimately propagates an error is likewise observationally identical to correct immediate propagation. Filesystem journal abort/shutdown can make this masking even stronger.

**Evidence:** `flush/machine.rs:544–561` performs the files barrier before the subsequent database operations; `:692–703` has buffered writes followed by a WAL barrier. There are several later operations available to cause an acceptable abort. The log check begins at **abort**, not at the first failed persistence operation. Core rc.3 `util/fs_helpers.cpp:122–152` reports failed `fflush`/`fdatasync`/`fsync` without retrying that operation, and `flatfile.cpp:96–105` propagates its failure. Holding the window until exit is a good liveness test, but cannot by itself prove this error-propagation contract. In §7 the proposed “continues after the window closes” observation is unreachable in a passing run, since the window closes only after exit.

**Fix:** keep the persistent-fault scenarios, but add an independent observation/test for the failed operation: syscall tracing or a narrowly scoped test seam that identifies the affected fd/path, return error, and subsequent action. Demonstrate that dropped-error and retry-to-success mutants fail, with the other operations healthy enough not to force a substitute abort first. A bounded/one-shot failure with that observation can complement the persistent window. Distinguish a forbidden retry that converts failed persistence into success from Core's permitted shutdown flush attempts/reports. Do not claim the permanent-window/log checks alone detect these mutants.

### M5 — major: DF-MID (and the named FL phases) can pass without hitting the claimed operation

**Location:** `PLAN-ST3b-S6.md:14–15,22,31`.

**Scenario:** the watcher sees `Writing partial batch`, but the small flush completes before the watcher runs and fallocate finishes. The filler reaches its target; the **next** forced flush fails the initial space check and logs `Disk space is too low!`. DF-MID passes the proposed checks without any mid-flush ENOSPC. Thus a broken mid-flush ENOSPC path can remain completely untested. Accepting a low-space precheck as DF-MID explicitly permits this false coverage. The analogous FL-WAL table swap can encounter a blk/rev barrier or a filesystem metadata write before the intended WAL sync; any later sanctioned abort is accepted as WAL coverage.

**Evidence:** `flush/machine.rs:692–703` emits the partial-batch line immediately before a buffered write and proceeds without waiting for an external watcher. The space checks are a separate earlier step (`:521–534`, `:1234–1263`). `baseline::node_args` enables coindb, and the existing driver's acknowledgements force additional flushes. Neither a tailed line, injected jitter nor `suspend --nolockfs` establishes that the intended operation is still in progress when injection becomes effective. Fallocate reaching ENOSPC proves the **filler** hit an allocation limit, not rdashd.

**Fix:** make fault/phase coverage a checked result. Require rdashd's ENOSPC in the targeted in-flight flush for DF-MID; a later `CheckDiskSpace` refusal is DF-PRE coverage only. Synchronize at a test seam or obtain operation/phase evidence and retry missed windows without counting them as passes. Similarly require a failed WAL persistence operation for FL-WAL and an active flush operation for FL-FLUSH. Record trigger time, effective resume/fill time, failed operation and errno. A case that never achieves its named condition must fail coverage or be retried, not pass as another case.

### m1 — minor: free the filler before C0's writable RocksDB open too

**Location:** `PLAN-ST3b-S6.md:23,29`.

**Scenario:** the DF-MID power-loss copy retains a durable filler and has no user-available space. C0 runs before C1 and fails creating/recovering RocksDB files, even though removing the filler would permit correct recovery. This is a harness-induced false red, not a recovery defect.

**Evidence:** `inspect.rs:1–4` states C0's database open writes; `:141–160` opens RocksDB and runs the recovery open. `campaign::check_image` gives C0 and C1 separate image copies, so freeing only C1's copy is insufficient.

**Fix:** perform the operator's filler removal, allowing an already-absent filler, on **each writable checking copy before either C0 or C1**. Verify sufficient available bytes afterward and preserve the pristine replay image for evidence. Do not truncate/delete node-owned files to make space.

### m2 — minor: the matcher is not a complete or exact rc.3 message specification

**Location:** `PLAN-ST3b-S6.md:21–23,27`.

The listed flat-file flush prefixes, `Failed to write block`, `Failed to write undo data`, and `Disk space is too low!` are correctly spelled. The full Core flush messages are:

- `Flushing block file to disk failed. This is likely the result of an I/O error.`
- `Flushing undo file to disk failed. This is likely the result of an I/O error.`

Core rc.3 `validation.cpp:3066,3093,3098` also contains `Failed to write to block index database`, `Failed to write to coin database`, and `Failed to commit EvoDB`. These are false-return branches; ordinary LevelDB I/O failures normally throw via `dbwrapper.cpp:200–214,261–268` and become `System error while flushing: ...`, so do not claim all these branches are independently reachable in this workload. In addition, `validation.cpp:4843–4844` catches block-storage exceptions as `System error: ...` outside a flush; a stream write failure need not use `Failed to write block`. Startup low space is the separate `Error: Disk space is low for %s` template (`init.cpp:2390–2396`), not an AbortNode message.

**Failure scenario:** a checker advertised as accepting all rc.3 I/O-abort forms rejects a valid reference path; conversely unrestricted `starts_with("Failed to write block")` accepts arbitrary missuffixed text rather than the documented message/path.

**Fix:** specify a path-aware message grammar. Match fixed Core messages exactly, document rdashd's flat-file path/OS-error suffixes as its own grammar, and bind variable system-error prefixes to the observed storage failure. State which Core branches are outside the non-evo S6 scope rather than claiming an exhaustive list. Do not simply allow every generic system error.

## Direct answers to the five questions

1. **Flakey semantics: yes, with high confidence at the target-map level; not locally kernel-verified.** In the standard dm-flakey implementation the down interval is selected by `elapsed % (up + down) >= up`; with up=0/down=1 every mapping is down. `error_writes` kills write bios with an I/O error before forwarding them. Empty PREFLUSH bios submitted by dm/block flush handling are write-direction bios (`REQ_OP_WRITE | REQ_PREFLUSH`), so the lack of a payload does not exempt them. Flakey advertises flush handling (`num_flush_bios`), so these are mapped. Already submitted bios drained by suspend are outside the newly active fault. Reads remain enabled. This does **not** mean every fsync must issue a bio or fail: a clean file/fully completed earlier persistence operation may need no device I/O.

   `suspend --nolockfs` skips the filesystem freeze/sync, so it avoids the dirty-page writeback forced by `freeze_bdev`/filesystem freeze. It still drains in-flight I/O; normal background writeback and activity racing the suspension can occur. It is not an atomic userspace phase boundary, and is not the same as `--noflush`. With a linear/flakey top target, there is no top-target cache requiring a new persistence flush. I did not inspect this machine's exact Linux 7.0.0-38 source or exercise its devices; the confidence is in the longstanding dm-flakey/dm-suspend semantics, not a measured certification of this kernel. Add a narrowly scoped preflight that proves rejection of one data write and one empty flush if exact-kernel evidence is required.

2. **Power-loss image/floor: not as specified.** M1 is the FUA error; M2 is the freshness error. Keeping the exit boundary before repair is the right approach. The existing whole-log/data-image comparison remains useful for reader/replayer validation, but does not repair either error.

3. **Window to exit: sound for bounded persistent-fault fail-stop, insufficient for the stronger claims.** A correct case cannot pass merely by staying idle because the timeout fails. An arbitrary abort does not independently prove a target bio failed, and does not prove the named WAL/flush phase was hit. DF-PRE intentionally exercises a low-space refusal rather than a failed write. DF-MID can presently pass without rdashd seeing ENOSPC; FL can mask earlier dropped sync errors (M4/M5). Observe natural child exit separately from RPC failure: existing `RustNode::stop` sends SIGTERM and `Drop` sends SIGKILL; neither may be the event credited as fault-induced exit. The deadline needs an independent child waiter, not a blocking RPC's completion.

4. **Disk full lever: yes.** Core rc.3 `util/fs_helpers.cpp:107–113` checks `fs::space(dir).available >= 52428800 + additional_bytes`. On Linux that available count is `f_bavail * f_frsize`, matching rdashd's `dashd-store/src/flatfile/fs.rs:301–315`. Fallocate consumes real allocation space, unlike truncate; the filler must be on the same filesystem as the checked datadir/blocks path and its post-operation availability must be measured. Use `f_bavail`, not `f_bfree` (ext4's reserved blocks differ). Forty MiB reliably violates the base check. The additional-space checks also matter: Core flat-file allocation checks the next allocation increment (`flatfile.cpp:58–76`); the coins check uses `48 * 2 * 2 * CoinsTip().GetCacheSize()` (`validation.cpp:3088`). A filler hitting ENOSPC need not make the numerical availability exactly zero. The expected strings are correctly spelled prefixes but incomplete as an exhaustive Core specification (m2).

5. **Missing discrimination:** M4/M5 need observed fault/operation coverage and negative controls for swallowed fsync failure, retry-to-success, continued block processing, and DF-MID falling through to DF-PRE. Preserve every successful acknowledgement rather than assuming none can occur under the fault. Use the first abort for the no-progress check and permit legitimate shutdown reports (M3).

## Validation performed

Read all requested harness modules, wrapper and notes; traced the flush machine, disk-space implementation, C0 database open, node abort and shutdown, and the specified rc.3 reference functions. No full workspace build was warranted for a plan-only review, and no privileged acceptance runs were attempted.

Compiled and ran three unprivileged Rust checks in `reviews/s6-r1-sol-checks/`: they use the exact `campaign::floor` function extracted from the source and include the actual `logwrites.rs` module. **3/3 passed**, demonstrating (a) last-FLUSH floor omits the last successful ack, (b) the selected prefix excludes a trailing FUA that the existing torn model retains, and (c) omitting the sole ack reduces the floor to `Start`. These are counterexample checks, not proof of real-kernel behavior. Results: `/work/scratch/st3b/reviews/s6-r1-sol-checks/result.txt`. Test TMPDIR was an owner-only mode-0700 `/dev/shm` directory, removed afterward.

**Verdict: CHANGES-REQUIRED.**
