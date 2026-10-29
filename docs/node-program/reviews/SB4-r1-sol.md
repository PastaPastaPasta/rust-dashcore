# SB4 implementation — Sol r1

Scope: `node/sb4-dash-chain`, head `558f54403732d55ea2fb3c7693cd7a07fc4079a6`; worktree clean at intake. No source edits, commits, pushes, or GitHub comments.

Contract: REVIEW-COMMON, approved SB4 rev7.1, SB0 rev6.1, CH1-node rev3.1, and DEC-136/147/157/164/167/172/174. PLAN §7 uses codenames only; no private files read. The task-specific scratch/TMPDIR and security-note directions supersede the common brief.

## Findings

### R1-M1 — major: SPV `chain_tips` silently loses evicted stale tips

**Location:** `dash-chain/src/chain.rs:2078` (`chain_tips_with`; the ID list is resident `self.leaves` plus the active tip), and `:1930` (eviction removes those leaves).

Approved SB4 §10.2 requires SPV to join resident leaves with `HeaderStore::leaves()`, deduplicate, read overlay-aware statuses, and fail-stop on store errors. The query never reads cold leaves. On a genesis-origin regtest SPV chain with `w = 10`, accept a stale height-2 leaf off height 1 and advance the main branch to height 40. At base 30 the stale entry is evicted. The store reports leaves `[BlockId(2), BlockId(41)]`, cold lookup returns `Ok(Some(BlockId(2)))`, but `chain_tips()` reports only active `BlockId(41)`. The stale tip should be present with height 2, branch length 1 and `HeadersOnly`.

**Evidence:** read-only standalone probe compiled against the current-head all-features library; `/work/scratch/sb4-r1-sol/probe.rs`, build driver `build_probe.py`, and `probe.log`. No branch files changed. This is a query-completeness failure, not proof of an incorrect fork target: `fork_target` has its own cold-leaf path. A-4's committed mainnet/testnet fixtures contain zero stale branches, so all six reported full replays cannot detect this omission.

The same probe also armed a `ReadKind::Leaves` error: `chain_tips()` still returned `Ok` with the latch false; a subsequent direct store read consumed the untouched fault (`coldskip-probe.log`).

**Suggested fix:** include `shared.cold_leaves()` in the SPV query, resolve statuses through the writer overlay, reconcile pending child/leaf deltas, deduplicate with resident leaves and the active tip, and preserve the specified latch/`ChainError::Storage` behavior (including the cold-work refresh contract). Add a windowed stale-leaf regression and a `ReadKind::Leaves` fault case; verify the tip remains visible before/after eviction and across a re-extension.

### R1-m1 — minor: a malformed cold skip pointer can make an ancestry query loop forever

**Location:** `dash-chain/src/shared.rs:223` passes the persisted `skip` through unchecked; `dash-chain/src/walk.rs:49`–`:62` trusts that each chosen link lowers the height.

On the same windowed fixture, healthy `reader.try_ancestor(stale_id, 0)` returns genesis. Set the cold height-2 record's skip to itself using the existing `Corrupt::BadSkip` fault. `GetSkipHeight(2) == 0`, so the walk repeatedly chooses that self-link and rereads height 2. It never reaches the target or recognizes corruption. This is a Rust persisted-ancestry robustness issue: Core rebuilds its in-memory skip links; this cold path imports persisted links.

**Evidence:** `/work/scratch/sb4-r1-sol/coldskip-probe.rs` and `coldskip-probe.log`. A deterministic watchdog injects a `ById` read error on the **100th** read. The query exhausts all 100 reads and returns only that injected error; the self-skip itself produces no failure. Without the watchdog, the loop condition and chosen link do not change. This needs malformed stored data; no healthy-store or peer-triggered consensus divergence is demonstrated.

**Suggested fix:** in the fallible cold ancestry path, reject self/nondecreasing/wrong-height skip links (and bad parent-height transitions) with a corruption/storage error that trips the latch. Keep the trusted resident node fast path if needed. Add a bounded-read fault regression. SB5's `open` validation remains necessary, but validating only its loaded window does not establish that every older cold link is safe, or cover corruption first encountered after open.

## Evidence and coverage

- Read REVIEW-COMMON and verified the branch/head.
- Read implementation and Core-scenario memory notes, all five checkpoint files, approved SB4/SB0/CH1-node contracts and the requested DEC entries. D-row scope exceptions are considered below.
- Current-head `cargo test -p dash-chain --all-features`: **151 unit tests passed, 2 ignored**; benchmark fixture test and all 5 doctests passed. Full-chain, memory, and one oracle codec integration test were ignored as marked.
- Core-linked `regen.py --check --smoke --jobs 1`: **5 exact vectors**, two ChainLock scenarios under 8 delivery schedules each passed; binary provenance verified at pin `9c6dd45e0b10`.
- Core v24 `FindForkInGlobalIndex` hypothesis rejected: it has the same first-contained-entry search as this branch, with no ahead-of-tip shortcut.

## Assessment

- **Core parity:** inspected v24 `AddToBlockIndex`, `AcceptBlockHeader`, `ProcessNewBlockHeaders`, `EnforceBlock`, `MarkConflictingBlock`, reset and best-header sites against SB4. New-header ties preserve the first seen header; reload uses the explicitly allowed D9 ID surrogate. CH1 owns candidate ordering/`nSequenceId`, block validation, ABC orchestration, and invalidate/reconsider; the scenario runner injects those operations for acceptance tests and models data/ABC in the ChainLock replays. Its 25 regtest ChainLock vectors passed in the crate run. Do not read that as testing the future CH1 implementation.
- **Harness checks:** independently regenerated 9 additional exact vectors (a17, both a18, a19, a26-ii, both a29 kept-mark forms, a35, a36); six ChainLock cases also passed two delivery schedules each. This agrees with DEC-167's fixed-point interpretation; no assumption that one pass is the final state.
- **DEC-174:** committed deep-reorg and below-window ChainLock tests passed. An additional scratch probe tested the unchanged `active.rs` directly: 7 floors around leaf/inner boundaries × 4 widths (1/10/1,024/10,000), 69,999 disconnects and reconnects per case, dropped-subtree re-extension and retained views. All 28 cases passed (`active-probe.log`).
- **Durability:** independent ignored `core_codec` test passed all 8 Core disk-index records. Reviewed the B1 codec, dirty extraction, load ID assignment/structural refusals, failed-child propagation and the memory-store transaction/undo model. Crate tests passed transaction rollback, buffered/durable prefix cuts, status and active-row invariants. `commit_mem_step` is a test stand-in that omits active/CL tail rows; it cannot prove production reopen/crash recovery.
- **Concurrency:** release seqlock stress passed. The production seqlock loom model (one writer/two readers, two-preemption bound) passed in 255.79 s. The composite restores `TipState` and trips the latch on unwind; publication happens under the held tip mutex. The lock probe, host-error, reentrant-call and abandoned-hold tests passed. `after_step`/`after_publish` are empty feed seams on this head; no feed/backpressure progress or wake-order claim is made for the absent SB5 seam commit.
- **Layering:** all-features/all-target normal closure check passed against `dash-dml`, `imbl`, `imbl-sized-chunks`. Targeted all-features/all-target clippy passed with `-D warnings`; workspace formatting check passed. Production `dash-chain` contains no filesystem/network/process operations, owned thread/pool creation, or global network parameter state. `load` installs parallel decoding in the injected pool. `dash-clock` implements the approved clock service outside rule code.

## Ownership and limits

The checkpoint exceptions matter: D-C6-8 defers A-23's every-store publication property, A-39/40/41 feed/W halves, A-30 reopen and A-33/34 step/crash tests to SB5/CH2b; D3 defers SPV `open`, the LRU, retained-snapshot cold heights and the larger cold-recovery property suite. SB13 owns the SQLite adapter. D-C10-1 requires SB5 reopen to window after load/eviction: SB4's `init_active_tip` itself builds the full path. These are integration obligations, not additional defects asserted against this branch. Full reopen/crash recovery is not established by the SB4-only tests. R1-m1 concerns the current fallible cold-walk termination, independent of the deferred reopen implementation.

## Performance and memory evidence

- **PF0 code paths:** lookup, locator and ancestor benches call the public reader/ancestry paths; tip benchmarking calls 1,000 publishing `connect_tip` steps; load decodes 262,144 records through `HeaderChain::load`. Setup is outside the measured region. The shim also records total instructions and RSS, so moving work into setup remains visible. Synthetic fixtures intentionally model index shape, not a second full historical replay.
- **A-25:** inspected the committed real DGW/KGW slices, `hash_and_accept`, workload mappings, the region counter shim and Core `bench-pnbh`. Rust performs `dash_x11::x11` and then `accept` in the region, with decoding and the cached `Header::new` hash outside. This is the approved §16 substitution for the future hasher switch. It is **not** a measurement of today's complete `Header::new` + accept pipeline, or of ABC/feed/durable-store work. The Rust prestate is synthetic below the real retarget lookback (50 DGW / 4,100 KGW historical headers); this is sufficient for these fixed header-rule windows, not literally an identical whole index to Core's historical preload.
- Read the implementer's PF0 `measure-results/head.json` at **39aef5f41**: DGW minimum **113,067,767** user instructions vs committed Core **483,104,655** (0.234×); KGW **367,374,014** vs **861,888,159** (0.426×). The recorded region, rather than the much larger setup/total count, supports those ratios. I did not independently rerun these large counter workloads at 558f54403. Their margin and scope are credible; they do not prove every later call in the node/SPV driver has the same performance.
- **A-31 method:** the counting allocator starts before chain construction and counts allocations/frees on the chain's single test thread. Only `commit_mem_step` runs uncounted; it takes `&self`, allocates temporary rows and the memory-store tables, and does not retain those allocations inside the chain. This exclusion is appropriate for a disk-store stand-in. The current head has no cold LRU, so the measurement covers SB4's chain, not the final SB4+SB5+SQLite client's full memory footprint. The least-squares trend/peak samples are post-step; they do not bound transient batch/reorg peaks or externally retained snapshots.
- Independently ran a scratch copy of the allocator assay against the exact current-head all-features **debug** library, using local H-M through **160,000**, `w = 10,000`. Post-step last **3,270,936 B**, peak over the last 40,000 sampled headers **3,272,096 B**, fitted growth over that sample **164,652 B**: below the 4,375,000 B cap and the one-chunk sample threshold. An earlier sample through 80,000 met the byte cap (last 3,271,440 B) but its short fitted-growth check was 232,596 B and failed; this short-window statistic crosses early context/residency transitions and is not the specified final-2M trend gate. Both logs are retained (`sample-memory.log`, `sample-memory-160k.log`). These samples support the byte scale; they do not independently reproduce the reported full release run's 3,037,456 B / 20,292 B final-2M growth.
- **A-4 sample:** standalone current-head replay from local files through mainnet **36,000** and testnet **12,000**, each in node, SPV w=1,000 and SPV w=10,000: all six passed. Each checked the final hash, sum of per-header work, height and window base, with SPV matching the node. Mainnet crosses BTC/KGW/DGW boundaries. `real_tolerance_era_contexts_match_complete_ancestry` also passed independently in release through **68,600**. The implementer's six entire histories were not rerun; the zero-stale fixture limitation remains R1-M1's blind spot.

## Reproduction and validation

All build/test commands used `nice -n 10`, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=` and disk `TMPDIR=/work/scratch/sb4-r1-sol/tmp` (0700). No oracle RPC, protected datadir access, private-file reads, source edits, commits, pushes or GitHub comments. Scratch Rust probes linked the freshly built exact all-features `dash_chain` dependency graph via the saved fingerprint-aware build drivers.

Relevant commands (logs under `/work/scratch/sb4-r1-sol/`):

```text
cargo test -p dash-chain --all-features
cargo clippy --locked -p dash-chain --all-features --all-targets -- -D warnings
cargo fmt --all --check
cargo test --locked -p dash-chain --all-features --test core_codec -- --ignored
cargo test --locked --release -p dash-chain --lib entry::tests::readers_never_see_a_torn_group
cargo test --locked --release -p dash-chain --lib real_tolerance_era_contexts_match_complete_ancestry -- --ignored
RUSTFLAGS='--cfg loom' cargo test --locked -p dash-chain --lib --target-dir target/loom entry::loom_tests::loom_one_writer_two_readers
cargo test --locked -p node-l1 --features oracle --test l1_block_proof
cargo test --locked -p dash-clock
python3 contrib/node/check-layering.py dash-chain dash-dml imbl imbl-sized-chunks
python3 contrib/node/sb4-core-scenarios/regen.py --bin /work/scratch/sb4-dash-chain/harness-out --check --smoke --jobs 1
python3 contrib/node/sb4-core-scenarios/regen.py --bin /work/scratch/sb4-dash-chain/harness-out --check --only a18-invalidate-walkback a18-invalid-chain-found a17-conflict-then-reload a19-first-seen-tie a26-ii-enforcement-reorg a29-x-kept-mark-trailing a29-x-prime-headers-only-kept-mark a36-conflicting-marked-order a35-resident-to-cold --jobs 1 --schedules 2
```

The targeted GetBlockProof differential test passed on both oracle pins (compact-target edges plus 4,096 deterministic random cases per pin); both dash-clock tests passed. The targeted commands above all passed. The exploratory 80K memory sample failure is qualified above. Full workspace/mobile/iOS builds, full A-4/A-31 reruns, SB5 feed behavior and production store/reopen were not independently validated in this review.

## Gate decision

The healthy-store core and DEC-174 implementation have substantial passing evidence. R1-M1 violates the approved SPV query contract and is reproduced on this exact head; it needs correction and a regression test before the Sol gate can pass. R1-m1 is a bounded, actionable cold-corruption robustness gap.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
