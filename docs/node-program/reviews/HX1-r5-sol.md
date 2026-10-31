# HX1 — Sol narrow round 5

Reviewed exact head `dcaae94de235f404d0379cbecc694d5e67ec17b9` on `node/sb4-core-scenarios-hx1`: one commit over r4's `b4cc23dd8878687e1a91f422b69fdc6e496e0770`. Review checkout: detached `/work/scratch/hx1-r5-sol/wt`. This round verifies R4-1 through R4-3 and the narrower perturbation scope under DEC-147, DEC-167, DEC-172 and DEC-175, including the manager's mandatory sequence-derived seeded replay. Earlier closed findings are not reopened.

All test commands used nice 10, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=` and disk-backed `TMPDIR=/work/scratch/hx1-r5-sol/tmp` (0700). Evidence is under `/work/scratch/hx1-r5-sol/`. No reviewed source, commit or branch was changed; no push, GitHub action, systemd operation or access to `/work/chains` or `/work/bench` occurred. PLAN §7 handling was observed; no private finding directory or signatures were read.

## Findings

No new findings.

## R4 closure status

### R4-1 — closed by the shared admission mechanism

`contrib/node/sb4-core-scenarios/regen.py:548` constructs the mandatory replay modes before running the plain case, then compares every pair of plain/up/down/seeded results through `perturbed_difference()`. The gate is shared by `scenario_job()` (`:213`), `header_job()` (`:252`), `random_job()` (`:633`), each address-sensitive regression (`:761`) and each additional delivery schedule (`:795`). Ordinary generation now refuses an unstable case before writing (`:813`); check mode treats instability as failure. Generated sequences still cut before the earliest difference and re-admit the retained prefix. There is no diagnostic-label pre-filter.

Evidence:

- All **76** fuzzctl tests pass, including the nine new tests that drive real `regen.main()` with a fake harness. They cover ordinary writing, seeded-only instability, schedule checks, header admission, random generation, address-sensitive and reproducibility regressions, and the minimum replay count. `fuzzctl-tests.log`.
- An independent probe redirects only the input/output paths of the ordinary real `main()` write path to review-owned fixtures. The original precious-reset sequence runs under `[plain, up, down, 765839764970893907]`, is refused at step **7**, exits unsuccessfully and writes no vector. This directly closes the old write-path escape using the unchanged production binary. `review-probes.py`, `unstable-generation.log`, `unstable-generation-calls.json`.
- The independent fake-harness probe drives **all six** header jobs through both writing and checking: each gets plain/up/down and its own derived seed. It uses the real `compare()` function: differing perturbed instruction counts are allowed, a committed-count deviation of 0.501% fails both benchmarks, and a changed non-count field fails admission and writes nothing. `review-probe-summary.json`.
- Real `pnbh-mainnet-kgw` and `bench-pnbh-mainnet-kgw` each pass their four-run admission and committed-vector comparison, exercising a **22,000-header** chain across multiple allocator blocks. The plain benchmark count is **870,276,026**, within 0.5% of committed **870,801,218**. `header-probe.py`, `header-probe-calls.json`, `header-probe-summary.json`.
- Focused real `--check` passes five byte-exact scenarios: `hx1-dump-orders`, `hx1-enforce-fixed-point`, `hx1-failed-walk-early-reject`, `hx1-pnb-force` and `hx1-precious`. Both address-sensitive regressions retain their required first differences, steps **5** and **7**. The fixed-point case also passes `random:1` and `random:2`, each now routed through admission, with **3–4** enforcement passes and matching semantic state. `targeted-regen.log`.

### R4-2 — closed at both boundaries

`regen.py:483` rejects a non-integer or fewer than one seeded replay. `admission()` calls this before any harness run, so direct `random_job()` callers cannot bypass it. `main()` rejects `--perturb-replays 0` with a usage error before build verification (`:719`). The tests verify CLI exit **2** with no calls or outputs, rejection of zero and negative counts through `perturbations()`, `admission()` and `random_job()`, and acceptance of two distinct optional seeded replays. This enforces the manager's minimum rather than merely supplying a default. `fuzzctl-tests.log`, `node-fuzz/fuzzctl_test.py:1444`.

### R4-3 — closed by isolating the node allocation ordinal

`harness.cpp:307` gates region allocation on both the actual BlockMap-node size and its caller. `PerturbedNode()` increments the ordinal only for a recognised node (`:299`); unrelated main-thread allocations no longer consume placements. `PerturbSlot()` (`:259`) gives ordinal k slot k, the reverse slot, or a seeded two-level bijection. The Feistel permutation maps each block and each within-block ordinal uniquely. Thus unrelated allocator traffic cannot advance the seeded placement stream. The main-thread restriction remains explicit; an index allocated elsewhere fails coverage rather than silently escaping perturbation.

The inspected production disassembly confirms the three selected **192-byte** allocation sites: `AddToBlockIndex`, `InsertBlockIndex` and the BlockMap `operator[]` instantiated for `ReplayBlocks`. Other selected-function allocations are bucket arrays or other-sized nodes; bucket arrays use prime bucket counts, so their size cannot be 192 = 24 × 8. Other 192-byte allocations in the binary have different callers and are excluded. `allocation-sites.json` preserves the call sites and surrounding instructions.

`CheckRegion()` (`harness.cpp:483`) checks every index entry and requires live region slots to equal the index size. It runs on every dump (`:1325`) and at the end of every header job (`:2259`), independently of `--audit-index`. The standalone flag is refused with exit **1** and `--audit-index needs --perturb-heap`. `audit-without-perturb.log`.

The committed three-way regression passes in the normal, unaudited production runs of `--check`: each original Sol seed gives one complete vector in **four independent processes**. Seed `1006220522186421180` first differs from both extremes at step **5**; `1066088033188675652` and `314087182294972517` never differ from those extremes. This preserves the middle-sensitive measurement while fixing same-seed repeatability. `targeted-regen.log`.

The allocation audit passes all four cases: **552 + 168 + 140 + 168 = 1,028** index snapshots inside the region, **1,050 + 159 + 69 + 162 = 1,440** pairs reversed under up/down, and identical per-label region offsets for seeds **17** and **29** in two processes per case. Sizes remain `CBlockIndex=144`, BlockMap node **192**, alignment **8**, slot **256**. `allocation-audit.log`.

## Scope-cut assessment

**Safe for Core v24.0.0-rc.3 and the supported harness operations.** Ordered containers can still decide by address, but the addresses they compare are those of the stored block-index objects, not their own tree/hash/list nodes. Those objects live inside the BlockMap allocations now being perturbed. I found no supported decision that requires perturbing another object's address.

The following references are to **Core tag v24.0.0-rc.3**, extracted into `core-tag/`, not a newer checkout. The oracle pin `a9a483e51efdadeee37cf40ff8697da62b3cd012` has identical production code in these paths; its `validation.cpp` additions are test-only trampolines appended after the release implementation.

- **Object identity:** `src/node/blockstorage.h:58` defines BlockMap as `unordered_map<uint256, CBlockIndex, BlockHasher>`. `src/node/blockstorage.cpp:95` and `:261` take `&mi->second` after the two insertion paths. Perturbing the enclosing node therefore perturbs the exact `CBlockIndex*` keys consumed below.
- **Failed-block walk:** `src/validation.h:1031` declares `std::set<CBlockIndex*> m_failed_blocks`; `src/validation.cpp:4678` iterates its pointer order and returns after the first matching failed ancestor. Its set nodes are irrelevant to the comparator; its index-pointer order remains perturbed.
- **Candidate and best-header ties:** `src/validation.h:594` declares `setBlockIndexCandidates` with `CBlockIndexWorkComparator`. The work/sequence comparison and address fallback are at `src/node/blockstorage.cpp:32` and `:43`. `FindMostWorkChain` selects the reverse-begin candidate at `src/validation.cpp:3527`; reload uses the same comparator for `m_best_header` at `:5359`. These compare index addresses. The live precious reset at `:3868` is covered too, not just loaded id-zero ties.
- **Unlinked and dirty entries:** `src/node/blockstorage.h:173` declares `multimap<CBlockIndex*, CBlockIndex*> m_blocks_unlinked`; `src/validation.cpp:4292` processes each parent's equal range, preserving insertion order among that parent's children. Parent ordering depends on index addresses. `m_dirty_blockindex` is another `set<CBlockIndex*>` (`src/node/blockstorage.h:146`), and its iteration builds the DB write batch (`src/node/blockstorage.cpp:344`). Both remain perturbed without relocating their container nodes. The temporary `forward` multimap in `CheckBlockIndex` (`src/validation.cpp:5624`) and versionbits' pointer-keyed cache (`src/versionbits.h:40`) likewise refer to these same index objects.
- **Related maps with pointer values:** `m_prev_block_index` is keyed by block hash (`src/node/blockstorage.h:59`), not by the address of its nodes or pointer values. `mapBlocksInFlight` is `multimap<uint256, pair<NodeId, list<QueuedBlock>::iterator>>` (`src/net_processing.cpp:1075`); requests are inserted by hash (`:1353`) and equal-hash entries retain insertion order. Stored iterators and queued-block pointers do not provide its sort key. Mempool iterator sets use transaction-hash comparison, not iterator addresses (`src/txmempool.h:405`, `src/kernel/mempool_entry.h:39`).
- **Callbacks and scheduler:** validation callbacks are registered into a list (`src/validationinterface.cpp:39`, `:54`) and delivered by iterating that list (`:84`). The pointer-keyed unordered map is a registration lookup, not the delivery order. Scheduler tasks are keyed by time (`src/scheduler.h:106`); pending validation callbacks are a FIFO list (`src/scheduler.cpp:155`, `:180`). Neither orders its own allocations by address.
- **A real excluded address-order example:** Core socket handling does have `std::set<CNode*>` receive/send/error sets (`src/net.cpp:2448`) and iterates them to service sockets (`:2499`, `:2522`, `:2531`). This is outside the harnessed paths: `ScenarioSetup` initializes the connection manager and peer manager (`harness.cpp:735`) but starts no socket/message loop and creates no network peers; the supported `cl` operation calls ChainlockHandler directly. Extending the harness to real peer/socket operations would require reassessing this exclusion.

The audit alone does not establish this scope conclusion; the comparator and reachability inspection does. The README's re-pin requirement to inspect pointer-keyed containers is appropriate. New operations, a re-pin or a toolchain changing allocation sites must repeat both scope inspection and allocation audit. One seeded permutation can sample a middle entry; it does not exhaust all multi-way permutations, as already accepted in r4.

## Validation and limits

The tested production artifact is SHA-256 `cda7f3e5170a292200d87759727d50081a11caa2ce37142c58f22a84388ea4d1`. `verify_build()` independently verifies the exact reviewed harness source, v24.0 pin and **24 linked inputs**. A review-owned copy is retained in `out/`; this round used the verified existing build rather than rebuilding Core. `check-seam.sh` passes after the caller-lookup refactor: the wrappers and interpositions remain the only seams. `check-seam.log`, `review-probes.log`.

`cargo test -p dash-oracle-sys --test sb4_core_scenarios -- --nocapture` builds and reports **3 passed**. Its optional smoke-regeneration test returns early because `SB4_CORE_SCENARIOS_BIN` is unset; the vector/provenance and records-completeness tests execute, and actual C++ regeneration is covered by the focused runs above. `rust-gate.log`.

Comparing all **54** committed vectors with r4 finds no semantic vector changes after removing benchmark instruction counts; the remaining changes are provenance. The two changed committed counts move by **0.1465%** and **0.1669%**. `review-probe-summary.json`.

The full regeneration/check, 1,000-sequence smoke, 300-sequence random-delivery smoke and extra 16 × 4 seeded campaign were not independently rerun. This review reproduces the targeted admission checks, original three-seed regression, allocation audit, two real-header jobs, seam check and fast test suites. The detached worktree remains clean, and this report is uncommitted.

**Verdict:** APPROVE

Sol gate satisfied: yes
