# P3 S6b — independent Opus-h confirm, round 2 (DEC-292 restart)

- **Branch:** `node/p3-s6b` at `b5d5bed05`. That is a docs-only follow-merge of `dcf57416d` on top of the fix commit `0e817f5f2`.
- **Scope:** `git show 0e817f5f2`. Round 1 is `reviews/P3-S6b-r1-opus.md` (m-2, site 3). The ruling is DEC-292 (2).
- **Core reference:** v24.0.0-rc.3. `np` means `src/net_processing.cpp`.
- **Build:** a detached worktree at `/work/scratch/p3-s6b-r2-review/wt`, with its own target (reflink-seeded) and 12 jobs.

## Verdict: ACCEPT WITH NITS

The restart does what DEC-292 (2) asks:

- `serve_headers` takes every view fact from the one `snap` it was given.
- Nothing is sent, stored or logged before the only read that can expire.
- The loop makes progress, because each retry takes the newest published view.
- The new test pins the restart on its reachable (serve) branch. Two extra mutants survive on branches that no test sets up. Both are test gaps, not code defects (n-1).

## Tests run

| Command | Result |
|---|---|
| `cargo test --locked -p dashd-p2p --all-features` | all green, at load ~30–40 |
| `cargo test --locked -p dashd-node --features test-support --lib p2p::chain` | 6/6 green, including `an_expired_snapshot_reads_again_on_the_newest_view` and `the_txs_memo_never_answers_for_an_older_view` |

### Mutants

The mutants were hand patches, reverted after each run. The tree was checked clean after each.

| Mutant | Patch | Result |
|---|---|---|
| **A** (asked for): min-work on the new view; `BlockRequestAllowed` takes `in_active_chain` and the best header from O, `valid_scripts` from N | `serve.rs:114`, `:117` → `&cx.snap` | **KILLED**, `sim_serve.rs:275` (the interleaving) |
| **B**: whole decision on N except the best-header facts, which stay at O | `serve.rs:117` → `&cx.snap.tip` | **KILLED**, `sim_serve.rs:275` |
| **R**: refuse on expiry (implementer's) | `valid_scripts(..).unwrap_or(false)` | **KILLED**, `sim_serve.rs:266` (the control) |
| **X**: the sim answers an expired view from the newest statuses (implementer's "mix", at the adapter) | `sim/chain.rs:1288` | **KILLED**, `sim_serve.rs:275` |
| **C**: `in_active_chain` at O, rest at N | `serve.rs:114` → `&cx.snap` | SURVIVED the whole suite (n-1) |
| **E**: the restart skips the min-work check (it stays at O) | `serve.rs:254` → `cx.snap.tip.tip_work` | SURVIVED the whole suite (n-1) |

## The five questions

### 1. One view per attempt in `serve_headers` (`serve.rs:240-296`)

Every view-dependent read takes the parameter `snap`:

- the min-work test (`:254`);
- `in_active_chain`, the best header and `valid_scripts` in `block_request_allowed` (`:114-130`);
- `find_fork_in_global_index` and `entry_at` (locator branch);
- `collect` and `active_run`;
- `best_header_sent`'s fallback tip (`:293`).

The other reads cannot drift with the view:

- `entry_by_hash`, `entry` and `ancestor` read the immutable block tree, as Core's `LookupBlockIndex` does.
- `min_chain_work` is a constant.
- `in_active_chain`/`active_at` read the view's own active chain, which never expires (`chain.rs:345-351`).

Before the `?` at `:270`, the side effects are:

- **The min-work branch:** `cx.push(empty)`, then a return of `Ok(())`. It returns before the read, so it never restarts after sending.
- **Two silent returns:** `entry_by_hash` → `None` and `entry(best_header)` → `None`.

The `debug!` "ignoring request" log, the `"{cmd} {} to …"` log, the `best_header_sent` store (`:293`) and the push (`:294`) all come after the read. There is no partial send, no state write and no log line before the expiring read. **Clean.**

### 2. Other `BlockRequestAllowed` callers

Core has exactly three: np:2781 (`ProcessGetBlockData`), np:3451 (`PrepareBlockFilterRequest`) and np:4805 (getheaders). `getblocks` (np:4636-4703) does not call it, and neither does `getblocktxn` (np:4704-4759, which defers to getdata).

In the port:

- **getdata blocks:** `process_getdata_with` (`logic.rs:915-958`) serves no block at M2. `ProcessGetBlockData` is P4's job.
- **getcfilters/getcfheaders/getcfcheckpt** (`stubs.rs:93`): stop at the service check. `NODE_COMPACT_FILTERS` is off, and filter serving is P6b's job.
- **getblocktxn and getblocks** (`stubs.rs:50`, `:85`): stubs.

So getheaders is the only live caller, and `valid_scripts` has no other caller. **Confirmed.** `block_request_allowed` is private and returns `Result<bool, ViewExpired>`, so P4 and P6b cannot call it without handling the restart. See n-3.

### 3. Livelock

`NodeChain::snapshot()` is `drain_snapshot().0`, the newest *published* view (`chain.rs:250`, `:369-373`). It is not a cached copy that could already be expired, and taking it has no signal side effect. Each retry can fail only if V lands a further 2^16 status writes in the µs window between that `snapshot()` and the one `valid_scripts` read. CH1 keeps the newest view readable (`status_journal.rs:41`, `:330-341`).

Progress is therefore guaranteed in any realistic schedule. Fairness is not needed: each iteration starts on the newest view, so a run of expiries cannot starve it. The only cause would be a writer that keeps sustaining more than 2^16 writes per µs window. On shutdown, V stops writing.

Core avoids this by holding `cs_main`, and the port's equivalent is cheap. See n-2 for a consistency nit, which is not a requirement.

### 4. Do the test and the mutants pin the restart?

**Yes, on the branch where the mix was unsafe.**

- **The control** (`sim_serve.rs:265-266`) can pass only through the restart. The message's view O still has e `FAILED`, so a read at O refuses. This also proves that `inside_the_next_getheaders`' `held` really is the message's view.
- **The interleaving** (`:268-275`) kills every variant that pairs N's `VALID_SCRIPTS` with O's best header: A, B and X.
- **The asked-for mutant A** is killed.
- **C and E survive** (n-1). Both touch branches that the r1 interleaving does not exercise. The code is right on both.

### 5. D120/D121 against the code

D121 matches the code:

- **Seam:** `seams.rs:194-200`.
- **The adapter:** `chain.rs:254-257`, with no `at_view` redo.
- **The restart:** `serve.rs:229-235`. The claim "nothing sent or stored before `valid_scripts`" is true.
- **Sim:** `expire`/`set_failed` at `sim/chain.rs:801-818`. `valid_scripts` is `is_valid(Scripts)`, which excludes failed entries.
- **Tests and mutants:** reproduced as listed.
- **`expired_reads`:** going from 2 to 1 is right, because `valid_scripts` no longer counts.

D120 carries the corrected argument (r1 m-1), the per-site table, n-1, n-2 (BL-280) and n-4, all accurate. Two places still describe the old `valid_scripts` redo (n-4).

## Findings

### Blocker / Major / Minor

None.

### Nits

**n-1. Two restart-adjacent mutants survive (test gap only).**

- **C:** `in_active_chain` read at O (`serve.rs:114`). It survives, but the mix is wrong in the refuse direction. Take an entry e that is off O's chain, `VALID_SCRIPTS` and fresh at O. By N it is back on the active chain, and N's best header is more than a month past e. Core serves at O (fresh) and at N (active); the mix refuses.
- **E:** the restart keeps O's min-work result (`serve.rs:254`). Take a deep `invalidateblock` that drops N's tip work below `nMinimumChainWork`. Core at N sends the empty `headers`; the mix serves.

Both need a composite inside one expired window, like r1's site 3. The shipped code handles both correctly, because everything reads `snap`. Optional: a second `inside_the_next_getheaders` case, for example invalidating the active chain below min-work inside the window, which expects the empty reply (np:4785-4792). It would kill E. C needs the reorg-back setup.

**n-2. Unbounded loop with no `debug_assert` (`serve.rs:232-234`).**

`at_view` has `debug_assert!(tries < 1_000, "the newest view keeps expiring")` (`chain.rs:139`). The getheaders restart loop has no equivalent. Adding the same assert, or a test-only restart counter alongside `expired_reads`, would make a broken host (one whose `snapshot()` returned a stale view) fail loudly in tests instead of hanging. It is not needed for correctness (Q3).

**n-3. Note the P4/P6b inheritance.**

When P4 implements `ProcessGetBlockData` (np:2776-2784) and P6b implements `PrepareBlockFilterRequest` (np:3448-3456), they must restart their own `cs_main` sections on `ViewExpired`, as `serve_headers` does. They must not map it to `false` (which disconnects in the cfilter path) or redo only the read. The `Result` type forces them to handle it, but a sentence in D121 or a P4 note would save the next implementer a round.

**n-4. Stale `valid_scripts` redo wording.**

- `dashd-node/src/p2p/chain.rs:4-9`: the module doc lists `valid_scripts` among the reads and then says "the adapter then reads the entry again, whole, on the newest view, never `false` or `None`". That is now true only for `status`.
- D120's headline ("`status`/`valid_scripts` read at the token's view; an expired view is read again on the newest one") and (4) ("On `SnapshotExpired`, `at_view` redoes …") read as if they covered `valid_scripts`. The DEC-292 sentence later in the row corrects this, but the headline could say "`status` is read again; `valid_scripts` restarts its caller (D121)".

## Core-side notes

None.

## Addendum: citation nit (part of n-4)

The code comments (`serve.rs:229`, `:239`) and D121 put the start of Core's `cs_main` section at np:4782. At rc.3, `LOCK(cs_main)` is np:4776, and the min-work test with its empty reply is np:4785-4792. That is off by a few lines and has no effect on the code.
