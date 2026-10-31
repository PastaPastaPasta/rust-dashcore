**Verdict: CHANGES-REQUIRED**

Sol review of **CH2b b1**, pinned to **03b562cd5** on base **96f9b2cc7**, against Dash Core **v24.0.0-rc.3** (`da567d7dba945143eee4d2ce1621c21b32089989`). The reviewed source is archived at `/work/scratch/ch2b-b1-sol/src/`. The working branch advanced during the review; this report concerns the requested checkpoint. I did not edit, commit or push branch files.

### R1-1 — major: Recovery does not check the block-file header's proof of work

**Location:** [dashd-chainstate/src/recovery/replay.rs:424](/work/scratch/ch2b-b1-sol/src/dashd-chainstate/src/recovery/replay.rs:424), in `Recovery::read_block`, shared by the undo and forward legs.

After decoding a block, recovery checks only `block.hash() == b.hash`. Its comment assumes the index-load PoW check is sufficient. That check uses the stored hash and the **index record's** `nBits` ([dash-chain/src/chain.rs:309](/work/scratch/ch2b-b1-sol/src/dash-chain/src/chain.rs:309)); recovery must also check the **decoded block header's** `nBits`. The two targets can differ in inconsistent disk metadata, without requiring a hash collision.

**Core evidence:** At the pinned tag, `src/node/blockstorage.cpp:825–828` checks `CheckProofOfWork(hash, block.nBits, consensusParams)` and logs `ReadBlockFromDisk: Errors in block header at %s` on failure. Only afterwards does the index overload compare the block hash with the index hash (`:834–847`). Core also retains a cached hash independently of the serialized header (`src/chain.h:404–417`), and index loading checks that hash against the index's `nBits` (`src/txdb.cpp:314–330`). Thus Core's index-load check does not replace its block-file check either.

**Concrete reproduction:** [pow_repro.rs](/work/scratch/ch2b-b1-sol/pow_repro.rs) uses the real `HeaderChain::load`, real block files and production startup recovery:

1. Construct a regtest height-1 block whose actual header has `nBits = 0`, which always fails PoW. Select a nonce whose actual hash passes regtest's easy target `0x207fffff`.
2. Store an index record with that actual hash and data position, but with the independently serialized index header's `nBits = 0x207fffff`. The real index loader accepts it.
3. Store a first-flush marker `[new, null]`, `evo_tip = new`, and the block-file metadata. Call `ChainstateOpen::open(...).recover(...)` with the loaded index.
4. Recovery succeeds, erases the marker, sets the tip and installs the block's output coin.

Run the preserved executable with:

```sh
TMPDIR=/work/scratch/ch2b-b1-sol/tmp RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  nice -n 10 /work/scratch/ch2b-b1-sol/build/pow_repro
```

Observed on two runs ([log](/work/scratch/ch2b-b1-sol/pow-repro.log)):

```text
Real HeaderChain::load accepted stored hash 29b15c7699b061d6e985120d4f9638d8aa5d47785b31eeb957bd5509b68b1e68 with index nBits=207fffff
Core CheckProofOfWork with block-file nBits=00000000: false
Recovery completed, erased marker, and installed one coin from the invalid-PoW block
```

The executable checks the PoW result using the Rust consensus primitive; the Core comparison is established by the cited source, rather than an end-to-end Core execution. Its build arguments are preserved in [pow-build-command.json](/work/scratch/ch2b-b1-sol/pow-build-command.json).

**Impact:** Startup silently finalizes coins from a header Core rejects. On the forward leg, Core's read failure reaches `RollforwardBlock(): ReadBlockFromDisk failed ...` (`src/validation.cpp:5188–5190`) and then `Unable to replay blocks. You will need to rebuild the database using -reindex-chainstate.` (`src/node/chainstate.cpp:251–252`). The shared helper also omits the check on the undo leg. This reproduction requires inconsistent disk metadata; it does not demonstrate that an ordinary crash or network input produces that metadata. No accepted D-b1 row authorizes omitting the check.

**Suggested fix:** Supply recovery with the network PoW parameters and check the decoded header's PoW before the index-hash comparison or coin operations. Preserve Core's header-error log and the caller-specific replay failure. Add regressions using the real index loader, covering both legs; adjust synthetic recovery fixtures to contain deterministic, valid-PoW headers.

### Review coverage and validation

The accepted D-b1-a…i rulings were treated as binding. No additional finding was established in these areas:

| Area | Evidence examined |
|---|---|
| Flush triggers | Modes, cache-state mapping, strict periodic timer boundaries, timestamp accounting, pending causes and prune dry-run conditions, compared with `src/validation.cpp:2969–3102`. |
| Durability | Files before B1; one-batch versus split marker; presumed-effect and pre-Bk WAL barriers; final marker removal; copy-point hook before release/snapshot. Subset convergence was checked from blind operation ordering and existing recovery tests. |
| Recovery | Marker checks, null old tip, LCA, reverse undo and forward ordering, malformed undo, read-error routing and final durable batch. R1-1 is the outstanding discrepancy. |
| Signals | Captured site locator handed over at release; ALWAYS shortcut; suppression with null coins best block; disconnect-site exception under DEC-166. |
| Concurrency | Failure ownership and refusal after another failure, V's registered wakeups, W panic handling, and crash exit before latch publication. |
| PF0 | Seeded synthetic profiles, setup/region boundaries, asserted split steps, recovery of a B1 image, and open of a B1 retained in the WAL. Quiet databases and synthetic profiles follow the accepted rulings. |

The pinned **dashd-chainstate unit harness passed all 177 tests**, with `rocksdb`, `model`, `alloc-tag` and `planted-bugs` enabled ([test log](/work/scratch/ch2b-b1-sol/pinned-tests.log)). This includes the flush, recovery, driver, signal and RocksDB model tests. The harness was compiled from the archived checkpoint using cached dependencies; build arguments are preserved in [pinned-rustc-command.json](/work/scratch/ch2b-b1-sol/pinned-rustc-command.json). All runs use disk scratch, `TMPDIR` within it, an empty `RUSTC_WRAPPER`, six build jobs and nice 10.

**PF0 diagnostics:** Compiled the pinned bench against the repository's criterion shim and ran each of its six rows twice, sequentially, with one measured iteration. All 12 executions passed their assertions and exited successfully. Build arguments are in [bench-build-commands.json](/work/scratch/ch2b-b1-sol/bench-build-commands.json), and raw counter results are in `/work/scratch/ch2b-b1-sol/pf0/`.

| Row | Rust allocations, run 1 → run 2 | Instructions change | Peak RSS change |
|---|---:|---:|---:|
| `replay/synth-mainnet-453371-p300` | 144,091 → 144,091 | −0.367% | +7.861% |
| `replay/synth-testnet-435990-p300` | 151,182 → 151,182 | +0.527% | −0.335% |
| `flush/split-64m` | 122 → 122 | +0.238% | +3.564% |
| `shutdown/always-p300` | 134 → 252 | +0.186% | −0.346% |
| `recovery/mainnet-2000` | 150,823 → 150,823 | −0.104% | +0.091% |
| `chainstate/open-b1-256k` | 148 → 148 | +1.016% | −0.223% |

Inputs and asserted outcomes are deterministic; **the measured counters are not bit-stable**. Shutdown's additional 118 allocations total 3,304 bytes (+0.274% allocated bytes). Its measured region uses the engine's scheduling-dependent write-stall retry loop (`dashd-store/src/rocks.rs:659–673`), which allocates error/status data on refused attempts. Other rows' allocated bytes repeated exactly. I/O repeated exactly except recovery's small variation (174 fewer read bytes, 188 fewer write bytes, two fewer read syscalls and one fewer write syscall). Page faults are explicitly ungated by the workload definitions.

These are **diagnostic counter runs**, without the full PF0 sandbox, external instruction/syscall passes, base/anchor comparison or escalation. They establish execution and expose noise; they do not establish a PF0 PASS or a formal PF0 regression. The shutdown allocation and RSS variation should be assessed with that gate's repeated-run policy before claiming measurement reproducibility. The accepted quiet/synthetic fixture choices are not disputed.

Commit 6 and the **ST3a CRASH suite are explicitly excluded**. This review does not claim an exhaustive crash-model result or completion of CH2b's later checkpoints.

**Sol gate: not satisfied. R1-1 requires correction and re-review.**
