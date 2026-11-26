# LK1b-s S1e captures

Ground truth for the IS store's direct-ingestion differential (`dashd-node/tests/lk1b_is_diff.rs`) and
its power-loss matrix (`dashd-node/tests/lk1b_is_crash.rs`). It records, op by op, what Dash Core
v24.0.0's InstantSend DB and its O8 records do when blocks, ChainLocks, InstantSend locks, spork
changes and restarts reach one node in a fixed order.

Each run is one file: `dashd-node/tests/data/lk1b-is/main.json` (the scenarios below) and `rot.json`
(the rotated `llmq_test_dip0024` run). Fixture correspondence holds by construction. There is one
run per file, and each IS DB checkpoint is keyed by the step number it was taken at.

Core paths and lines below are of v24.0.0's source (`git show v24.0.0:src/<path>`).

## Design

- **Producer.** A `DashTestFramework` network of 5 nodes and 4 masternodes: `llmq_test_instantsend`
  (`-llmqtestinstantsenddip0024=llmq_test_instantsend`), spork 2 on. It makes the blocks, quorums,
  ChainLocks and locks. Its own state is never recorded.
- **Recorder.** One extra dashd, the O8-patched v24.0.0 from `contrib/node/statedigest/build.sh`. It
  is started from `self.nodes` and popped out of it, so the framework never connects, syncs or
  stops it. Its arguments are the producer's chain arguments, without `-watchquorums`, plus:

  ```
  -connect=0 -listen=1 -txindex=1 -persistmempool=0 -disablewallet=1 -debug=chainlocks
  -debug=instantsend -sporkkey=<node0's key> -statedigestlog=digest.jsonl
  -recordstatedecisions=decisions.jsonl
  ```

  It learns only what the harness gives it:
  - blocks by `submitblock`;
  - `clsig` and `isdlock` messages from a `Feeder` P2P peer, which announces each by `inv` and
    answers the recorder's `getdata` with the exact payload bytes;
  - sporks by `sporkupdate`.

  The network's payloads are node0's serialized objects: `getislocks`' and `getbestchainlock`'s
  `hex`. A `Source` P2P observer on node0 fetches every `clsig`/`isdlock` inv the moment it is
  announced, because Core serves only its best ChainLock. Where it got the object, its bytes must
  equal the RPC's. Blocks come from the observer by `getdata`, checked against `getblock`. Some
  payloads are built by the harness and signed by the quorum members through `quorum sign`:
  - `build_clsig`;
  - `build_islock`, a lock of a chosen tx. `llmq_test_instantsend` is not rotated, so its cycle
    hash is the signing quorum's hash (`net_instantsend.cpp:143-146`).
- **One op, one step.** After each op the harness settles the recorder:
  - `syncwithvalidationinterfacequeue`;
  - the tip, best ChainLock, `getchaintips` and both O8 files are unchanged over four polls 0.25 s
    apart.

  It then takes the digest and decision lines written since the previous step. The O8 output is
  never rewritten or merged. Each step's lines are byte-for-byte Core's.
- **Timer output.** A ChainLocked block that fails validation, as in (d), is retried by the
  ChainLock scheduler every 5 s of real time (`chainlock/handler.cpp:63-71, 296`). `mockscheduler`
  does not reach that scheduler. So how many retries land in a step depends on wall time. The
  harness keeps the first `R` and `A` of that block in the step and moves the later ones into the
  step's `periodic` lines. The restart that follows ends the retries, because the best ChainLock
  is held in memory only.
- **Checkpoints.** A `RESTART` op stops the recorder (cleanly, or by SIGKILL for `KILL`). It copies
  `regtest/llmq/isdb` to `<work>/checkpoints/step-NNNN.isdb`, dumps the copy with `isdump` to
  `step-NNNN.isdump.json`, puts the rows in the step and starts the recorder again with that op's
  arguments.
- **Mined windows.** Conflict blocks and IS-relevant blocks are mined outside DKG mining windows:
  - not at `h % 24` in 10..20 (`llmq_test`/`llmq_test_instantsend`/platform 10–18, dip0024 12–20);
  - and with no masternode list change (no ProTx, no PoSe change).

  So a block's attempt carries only payee and IS effects.
- **Built blocks.** v24's template pays the platform share (OP_RETURN), the owner and, with an
  operator reward, the operator (`masternode/payments.cpp:131-198`). The harness builds a block
  from the template's payees, its quorum commitments and its CbTx payload, as Core's own tests do
  (`test/functional/feature_masternode_payout_shares.py:36-70`). It recomputes the owner and
  operator amounts for the block's own fees, after checking that the same computation gives the
  template's amounts. That check covers only the template's own fees; Core connecting the built
  blocks of (c), (c′), (e) and (f) is what proves the amounts. Superblock heights are refused.
- **Core-only paths avoided.** Core removes a lock when a conflicting mempool tx is removed for a
  conflict (`TransactionIsRemoved`, `instantsend/instantsend.cpp:132-147`). It also invalidates
  blocks from `ResolveBlockConflicts` (`net_instantsend.cpp:600-640`). LK1b-s has neither (S2/S5).
  The harness keeps both from happening:
  - the recorder's mempool is cleared by a restart before each conflict block;
  - a conflicting tx is only ever in a block, never in the recorder's mempool;
  - (c′)'s txB pays a bare `OP_TRUE` output. When CL(X) disconnects Bblk, the reorg cannot put txB
    back in the mempool (`validation.cpp:450-456, 877`; `policy/policy.cpp:57, 130`), so U's
    connect does not evict it and drop lock B through `TransactionIsRemoved`;
  - the run fails if the recorder's debug.log shows `was removed from mempool`,
    `ResolveBlockConflicts -- invalidating block` or `conflicts with islock` (`FORBIDDEN_LOG`).
- **Reconsidered invalid blocks.** On restart, at the default `-checklevel` (3), Core reconsiders a
  best invalid block that is ahead of its tip (`node/chainstate.cpp:463-465`,
  `validation.cpp:4166-4170`). It retries the block while still importing, so in IBD
  (`node/blockstorage.cpp:898`, `validation.cpp:1990`), before the IS check
  (`validation.cpp:2812`). After (b) a producer block moves the tip past the
  rejected block. In (d) the restart after the ChainLock retries the bad-payee block on purpose.
  It is rejected again (`bad-cb-payee`), and that `R` is part of the RESTART step.
- **Kill.** Core flushes its block index lazily, so (g)'s SIGKILL loses the newest block while the
  IS DB keeps its rows. `catch_up` asks the recorder which blocks it has and delivers the missing
  ones again (PLAN-LK1b §2.4 "Process crash").
- **`core_tx`.** Core stores a lock only once it knows the tx (`net_instantsend.cpp:369-398`), from
  the mempool or, with `-txindex`, a block. An `ISLOCK` op whose tx is in neither sends `core_tx`
  to the recorder's mempool by `sendrawtransaction` first. It is Core-side preparation. The Rust
  side ingests `bytes` with `mined_height` and ignores `core_tx`.

## Scenarios (main run)

| | ops | what it shows |
|---|---|---|
| a | ISLOCK, BLOCK (mines it), CL_RECORD | lock stored; `is_m` mined mark; ChainLock → confirmed, `is_a1`/`is_a2` archive |
| b | ISLOCK, RESTART, BLOCK conflicting, a producer block | `R` `conflict-tx-lock`, `cls` `chainlock`, `fx` `[]`; the lock stays |
| c | ISLOCK on x, ISLOCK on its child, RESTART, BLOCK conflicting, CL_RECORD on it, RESTART | Core's `feature_llmq_is_cl_conflicts` order: `R` first, then the ChainLock's enforcement connects it: `C` with `is_rm`, chained child removed |
| c′ | lock A on x,y; RESTART; spork 2 off; block with txB; spork 2 on; lock B (txB mined); fork X [T spends y, U spends x]; CL(X) | PLAN-LK1b §5.3 A/B: `D` of B's block, `C`(X) with `is_rm(A, T)`, B survives |
| d | (c) where the conflicting block also pays the wrong masternode | `R` `conflict-tx-lock`, then after CL `R` `bad-cb-payee` **with** `is_rm` (retries in `periodic`); RESTART (the block retried and rejected again), producer blocks, CL |
| f | `prioritisetransaction −COIN`; a recorder-only block mines the locked tx; 2 producer blocks reorg it out; priority back; re-mined; CL | mined mark across a reorg |
| g | KILL, restart, the lost block re-delivered | the IS DB after SIGKILL holds every committed batch |
| e | RESTART `-maxtipage=1` (IBD), conflicting block accepted, RESTART with defaults, producer reorg | no IS check in IBD |
| h | mine to (a)'s archive height + 101, CL | archive pruned at h − 100 |
| i | not captured | see open questions |

The rotated run (`capture_lk1b_rot.py`) is `contrib/node/lq3a-captures/capture_lq3_rotation.py`'s
setup with the recorder attached. Every block is delivered to the recorder, each rotated lock is
fed as an `ISLOCK` op, the tip's ChainLock follows, and a closing RESTART checkpoints the DB. Its
fixture also carries `lq3r`: the rotation capture's quorum list, quorums, cycles and locks, with
`step` naming the `ISLOCK` step of each lock.

## Running

The binaries come from `contrib/node/statedigest/build.sh` (O8-patched v24.0.0). This directory does
not build dashd.

```
contrib/node/lk1b-captures/run.sh ~/workspace/dash /work/scratch/statedigest/out \
    /work/scratch/lk1b-capture dashd-node/tests/data/lk1b-is/main.json main
contrib/node/lk1b-captures/run.sh ~/workspace/dash /work/scratch/statedigest/out \
    /work/scratch/lk1b-capture-rot dashd-node/tests/data/lk1b-is/rot.json rot
```

`run.sh` does the following:
- checks every `sha256` line of `build-info.txt` against the binaries and that `describe` is a
  v24.0.0 build;
- exports `test/` from build-info's commit;
- copies in these scripts and the two lq3a captures they import;
- builds `isdump` unless `ISDUMP` names one;
- makes a venv with `dash_hash`;
- runs with `--nocleanup`.

`LK1B_PORTSEED` (2411) and `LK1B_TIMEOUT_FACTOR` (8) tune it. The datadirs stay in
`<work>/ft/run`. A run is one functional test of a few minutes and does not need the heavy lock.

`isdump/` is a standalone crate (its own workspace and `Cargo.lock`) after `ev4-serve/evodump`:
`isdump <db-dir>` prints the DB's rows in key order as JSON (`isdump/1`). Build and test it with
`cd isdump && CC=clang CXX=clang++ cargo test --locked`.

## Fixture schema `lk1b-is/1`

The top level:

| field | |
|---|---|
| `schema` | `"lk1b-is/1"` |
| `run` | `"main"` or `"rot"` (`"sample"` for the parser tests' hand-made `sample.json`) |
| `network` | `"regtest"` |
| `provenance` | `dashd_version`, `dashd_sha256`, `build_describe`, `build_commit`, `tool`, `producer_args`, `recorder_args` (without `-sporkkey`), `captured_utc`, `llmq_size`, `llmq_threshold` |
| `steps` | the ops, in order |
| `lq3r` | `rot` only: `tip_height`, `bls_scheme`, `quorum_list`, `quorums`, `cycles`, `locks` |

Each step:

| field | |
|---|---|
| `step` | 1-based, consecutive |
| `scenario`, `label` | `setup`, `a`…`h`, `c'`, `rot`; free text |
| `mocktime` | in the committed fixtures, the recorder's clock at its last start, not when the op ran (they predate the capture recording the harness's mocktime as each op starts); the driver uses the step's first `A` record's `t` when it has one, and never sets its clock back |
| `op` | one of the ops below |
| `digest` | the `-statedigestlog` lines written during the op: `H`, `C`, `R`, `D`; each a canonical O8 line without its `\n`. Step 1's lines also hold the recorder's start-up records (the genesis `C` and its `A`), so the Rust side emits its start-up records in its first `apply` |
| `decisions` | the `-recordstatedecisions` lines: `H`, `A` |
| `conflict` | `null`, or the block hashes this op attempted that conflict with a lock; each has a `C` or `R` record in `digest` |
| `conflict_fx_kinds` | capture-only note: the `fx` kinds Core wrote on the conflict attempts |
| `periodic` | optional: digest and decision lines of the ChainLock timer's retries (above); parsed, not compared |
| `checkpoint` | `null`, or the IS DB after the op's stop: `rows` (`[key hex, value hex, table]`, strict key order), `is_locks`, `counts`; on every `RESTART` (`CLEAN` or `KILL`) and the last step, nowhere else |

The ops (hashes are display-order hex, as RPC prints them). The loader checks that a `BLOCK`'s
`hash` is the X11 of its first 80 bytes and that a `CL_RECORD`'s `inv_hash` is the sha256d of the
`clsig` payload its `height`, `hash` and `sig` make:

| `op` | fields | Rust side |
|---|---|---|
| `BLOCK` | `hash`, `bytes`, `source`, `core_reply` (`submitblock`'s answer) | NodeSut's block op |
| `CL_RECORD` | `height`, `hash`, `sig` (96 bytes), `inv_hash`, `source` | `cl_record`, then the enforcement Core schedules |
| `ISLOCK` | `bytes` (the `isdlock` payload), `inv_hash` (its sha256d), `txid` (the payload's), `mined_height` (`null` if unmined), `core_tx`, `source` | S1d's `IsLock(bytes, mined_height)` |
| `SPORK` | `id`, `name`, `value` | `sporks().update_spork` |
| `RESTART` | `kind` (`CLEAN` or `KILL`), `args` (the restarted recorder's extra arguments) | `restart_report` |

Table names are those of `instantsend/db.cpp:12-18`: `is_i`, `is_tx`, `is_in`, `is_m`, `is_a1`,
`is_a2`, `is_v`, or `null` for any other key.

**`is_locks`** (O2b §2.6) is the SHA-256 over the keys of the `is_i`, `is_m`, `is_a1` and `is_a2`
rows, sorted, each written as its length (u32 little-endian) followed by its bytes. `is_tx`,
`is_in`, `is_v` and the values are not part of it, so the index rows that §2.4 does not hold
consistent do not enter the digest. They are compared row by row at checkpoints. Python
(`capture_lk1b.is_locks_digest`) and Rust (`lk1b_is::is_locks_digest`) compute it the same way. The
loader recomputes it from the rows.

## Comparison (dashd-node/tests/lk1b_is)

The driver sets the clock before each op, to the `t` of the step's first `A` record (the
recorder's adjusted time when that attempt opened) or, in a step without one, to the step's
`mocktime`, but never earlier than the previous step's clock (Core's never ran backwards). It then applies the op and compares the following. VerifyDB's records (`org` `verifydb`), written at every Core start, are left out, as in
node-l1's comparator.
- **Outcome.** Per attempt (`C`/`R`/`D` by block), the kind and `s`: `res`, `cls`, `why`.
- **`is_conflict` queries.** Each attempt's `A` `is_conflict` sequence.
- **`fx`.** Only its IS keys (`is_rm`), on every `C`, `R` and `D`, and only where both sides wrote
  it. The non-IS keys (`mn_changed`, `snap`, `qc_unmine`, `ban_reset`) are left out on both sides,
  so the Rust side may write IS-only `fx`. On a conflict attempt `fx` must be present on both
  sides.
- **Checkpoints.** The rows (key by key) and `is_locks`.
- **Not compared, on purpose.** A `C`'s `s` fields, the `A` fields other than its `is_conflict`
  queries, an `R`'s `dbg`, and every `x`: the chain fields belong to a later slice (O2b-5).
- **The kill-dependent step.** The loader marks the first `BLOCK` after a KILL whose hash a `C`
  connected before the KILL: in main, (g)'s re-delivery (step 257); main has exactly one, rot none.
  Core lost that block in the kill and connects it again; a Rust node that kept it may skip it as
  a duplicate. There the Rust side may write Core's `C` with its `A`, or no attempt at all, and the
  driver compares the full IS state after the step instead of its records: the rows against the
  KILL step's checkpoint (which already holds the block's `is_m` row, the only key the
  re-delivery rewrites) and the Rust tip against the block's hash.

The power-loss matrix (`lk1b_is_crash.rs`) cuts at every IS commit of each op of (a)–(f). It
accepts a recovered state when some subset of the pending batches, each whole and in order, gives
it (PLAN-LK1b §2.4). The base and the batches are the durable base and the pending batches the
FaultyEngine reports at the cut. A clean restart is a durability barrier: everything before a
clean reopen is durable. The oracle decides membership directly and does not enumerate subsets.

## Rulings

MN0's rulings on the S1e questions:
1. **`mn_changed`.** Core's conflict connects carry `mn_changed` next to `is_rm`, because every
   paid block updates `nLastPaidHeight` (`evo/specialtxman.cpp:773`). `mn_changed` parity belongs
   to O2b-5.
2. **Scenario (i)**, the R18/MN4-08 outpoint index, is not part of LK1b-s. It moved to S5.
3. **`CL_RECORD`** includes Core's scheduled `EnforceBestChainLock`. The Rust side enforces after
   recording (S1d).
4. **`-maxtipage=1`** in (e)'s RESTART args needs a NodeSut equivalent (S1d).
5. **`core_tx`** is Core-only preparation (above).
6. **Core-only paths** are kept out and checked absent (`FORBIDDEN_LOG`).
7. **`periodic`** is not compared. The loader checks that every periodic record is a retry of
   (d)'s ChainLocked block: a reject with the first reject's outcome and no `is_rm`, or its `A`
   record, before the restart that ends the retries.
8. **VerifyDB records** stay out of the comparison, as in node-l1.
9. **(d)'s restart reject.** (d)'s RESTART step carries Core's start-up retry of the bad-payee
   block (above). It is compared like any other step, so the Rust side has to reconsider its best
   invalid block on restart.
10. **IS keys only.** On every `C`, `R` and `D` the driver compares only the IS keys of `fx`
    (`is_rm`) and ignores the non-IS keys on both sides; NodeSut may write IS-only `fx`. A conflict
    attempt still needs `fx` on both sides.
11. **The kill-dependent step.** (g)'s re-delivery after the KILL (main step 257) may be Core's `C`
    with its `A` or no attempt at all; the driver compares the IS rows after it with the KILL
    checkpoint and the Rust tip with the block (Comparison, above).
