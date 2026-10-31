# O1c independent Sol review, round 2

Review complete. One major isolation finding requires a Core fix and another re-pin. The requested pinned-archive suites pass, the r1 fixes are present, and no recorded SB4 or CS3 verdict moved.

Reviewed Core `node/o1c-oracle-v24.0` at `e0248f71fa8b4607ef959e79221f8ae1b80bd902`, based on O2a-EV `d694a47043`; rust-dashcore `node/o1c-oracle` at `c083e5aa5f5fab167220dbaaf009610fe634c2f1`. The pinned rdc baseline is `2c31fe338533ba8fccc4fd46e4d9b97908ab90f4`; its only two descendants are `778ec5e2a` (`check_block_index`) and `c083e5aa5` (D1 notes). ABI constants agree at 10.

Detached review worktrees: `/work/scratch/o1c-r2-sol/core` and `/work/scratch/o1c-r2-sol/rdc`. Neither reviewed tree was edited. All builds and tests ran at nice 10, with `CARGO_BUILD_JOBS=8` and mode-0700 `TMPDIR=/work/scratch/o1c-r2-sol/tmp`. Core archives and existing build trees were consumed read-only; no Core or depends rebuild was performed. Cheaper session setup was excluded. No private material was read, no commits or pushes were made, and no GitHub action was taken.

## Finding

### F1 — major: logger categories and category levels leak between inputs

Core `src/test/oracle/oracle_session.cpp:521,838`; `src/logging.cpp:86`; `src/init/common.cpp:59,79`; `src/test/util/setup_common.cpp:181`.

Session startup and teardown call `DisconnectTestLogger()`, which resets buffering, the file handle and callbacks, but retains the category mask and per-category level map. `SetLoggingCategories` treats `-debug=0` as “do not enable categories”; it does not clear categories enabled by a previous session. `SetLoggingLevel` updates specified category levels without clearing old overrides. BasicTestingSetup's default `-loglevel=trace` sets the global level and does not erase a prior `net:info` override.

Two fresh-process witnesses against the pinned plain archive, using the normal libstdc++ allocator, establish the effect:

1. Run identical seed-41 unconnecting-header scripts with `-debug=0`, then `-debug=net`, then `-debug=0`. X11-call counts are **[1, 2, 2]**. The first and third session have identical configuration and input bytes. Core's diagnostic `headers[0].GetHash()` at `net_processing.cpp:3160` is unexpectedly evaluated in the third session.
2. Run ten unconnecting-header deliveries with default logging, then `-loglevel=net:info`, then default logging. X11-call counts are **[20, 10, 10]**, and captured `Misbehaving` records are **[1, 0, 0]**. The first and third session have identical configuration and inputs. Core still increments the same misbehavior score; its NET log at `net_processing.cpp:1964` is suppressed in the third session.

Both witnesses first verify that the remaining peer/session outcomes agree after removing the affected observation records, then deliberately fail their isolation assertion. This is a history-dependent oracle transcript, including a missing misbehavior record, rather than an allocation-interposer artifact. It violates the per-input isolation requirement and invalidates a claim that raw call logs describe a fresh rc.3 node for every input. Default-only replay tests do not expose it because those sessions repeatedly enable the same categories.

Suggested fix: save the logger's prior configuration before session construction, initialize the session's category mask and category-level map from a fresh-node baseline before applying its arguments, and restore the saved configuration on every cleanup path, including failed starts. Include the global level in that ownership/reset policy. Add fresh-process quiet/debug/quiet and default/category-override/default regressions that compare complete raw transcripts. Keep the real diagnostic X11 calls; resetting logger state fixes the fidelity problem without hiding actual Core work. Re-pin both archive flavors after the Core fix.

Witness source and evidence:

- `/work/scratch/o1c-r2-sol/extra-probes.rs`, functions `independent_logger_history_witness` and `independent_category_level_history_witness`.
- `/work/scratch/o1c-r2-sol/evidence/logger-history.log` and `category-level-history.log`.
- First/third quiet raw session outcomes: `evidence/logger-0.bin` and `logger-2.bin`, SHA-256 `b581cbee4b4cda4a33d13bb85a8d6cf0f63d7787a2b168b8ec40557ae2406ddb` and `38658fb6555667fd47cd7ae3b57728f0c2ba2c9822681aead431027b2ffafcd8`.

## Requested fidelity checks

### D38: startup draw order and the SB6 note

Confirmed against rc.3's `kernel/context.cpp`, `kernel/checks.cpp`, `key.cpp`, `random.cpp`, `init.cpp`, `addrdb.cpp`, `addrman.cpp`, `addrman_impl.h` and `netaddress.h`. Relevant production routines are unchanged by this review range. The session's fresh-datadir sequence is:

1. `kernel::Context` calls `ECC_Start`: `GetRandBytes(32)` for secp256k1 blinding.
2. `AppInitSanityChecks` reaches `ECC_InitSanityCheck`: `GenerateRandomKey` consumes strong 32-byte key material, then `VerifyPubKey` consumes `GetRandBytes(8)` for the challenge. The key generator retries an invalid scalar; using the actual sanity check preserves that behavior.
3. `InitScriptExecutionCache` consumes a 32-byte nonce. The signature-cache nonce is a process-static constructor draw before this session sequence, not another draw at `InitSignatureCache`.
4. `LoadAddrman` constructs one AddrMan, then a replacement after the missing `peers.dat`: **each consumes its key/context draw and two `CServiceHash` salt draws**. `DumpPeerAddresses` then draws the temporary filename through `GetRand<uint16_t>()`.
5. BanMan's rolling bloom filter draws its tweak.
6. ConnMan's seeds are assigned with `nSeed1` drawn before `nSeed0`, matching GCC's evaluation of rc.3's constructor arguments.

The implementer's note is right: SB6 D38's table omits the ECC sanity check's **32 + 8 bytes** and the **two hash salts per constructed AddrMan**. Each salt is a real `GetRand<uint64_t>()`, with its own self-seeding FastRandomContext; it is not merely eight bytes to skip in the global stream. The oracle should retain these draws, and SB6 should reconcile its table/driver to Core.

`Random_SanityCheck` separately calls `GetOSRand` directly and mixes entropy internally (`random.cpp:741`); it does not consume `GetRandBytes`/`GetStrongRandBytes` output from the shim stream. The 32 + 8-byte correction belongs to the ECC sanity check. RandomInit's internal entropy collection is likewise outside this public draw seam.

`setup_draws_in_inits_order` passes against the pinned archive, tiles the startup records, and checks that the nonce actually uses the observed `(seed0, seed1)`. The extra deterministic test context is explicitly identified. This remains a specified fresh-datadir, GCC-style, no-RPC-cookie model. Constructor argument evaluation is not a portable seeded protocol: the source's `nSeed1` assignment is explicit in both archive flavors, rather than claiming that a separately compiled Clang dashd must choose that same argument order.

### P2a asks: X11, minimum chain work and decoded markers

**X11:** `primitives/block.cpp:16` records each real `CBlockHeader::GetHash` computation under the observer. At `oracle_session.cpp:552`, the shim's own fixture-tip verification hash runs under a null observer; snapshot/outcome paths read cached block-index hashes. The exclusion is correct. The pinned three-header witness passes with multiplicities **[2, 2, 3]**.

Keeping hashes evaluated for NET debug lines is faithful to the explicitly documented debug-enabled Core session (`oracle.h:1209`). It is not a pure validation-work count or the trace of a default non-debug dashd. A fresh quiet unconnecting-header script makes one computation; a fresh debug script makes two. P2a should compare the same logging configuration or define a separate normalized cost metric before interpreting these records as minimum processing work. The raw log should not silently suppress actual Core calls. The concrete fidelity failure is F1's inherited logging configuration.

**`min_chain_work`:** Rust renders the `[u8; 32]` value in big-endian hex and appends `-minimumchainwork`; hook-mode ChainTestingSetup calls Core's unchanged `node::ApplyArgsManOptions` before constructing a new ChainstateManager (`setup_common.cpp:343`, `node/chainstatemanager_args.cpp:26`). The real `MinimumChainWork()` controls IBD and `getheaders`. The height-two boundary test serves headers at work 6 and refuses at work 7, and the invalid-hex/recovery case passes. This also checks byte order. No separate shim threshold implementation was substituted.

**Decoded markers:** `PeerReads` feeds the real peer-side transport and updates `last_decoded` only after `GetReceivedMessage` yields a non-rejected message. Controlled peers' substitutions read that map (`oracle_session.cpp:660`); eagerly reading peers retain their stated pushed-message convenience. The controlled V1 test refuses a version marker before any drain, uses the first decoded ping while a newer ping remains unread, then uses the newer nonce after draining it. It passes. Partial-read framing and V2 transport behavior remain exercised by the transport/backpressure tests.

### `check_block_index`: acceptable for transcript fuzzing, with a precise limit

Confirmed against the complete rc.3 `ChainstateManager::CheckBlockIndex` body (`validation.cpp:5607`) and its helpers. `ShouldCheckBlockIndex` reads the per-manager option. The enabled body locks `cs_main`, constructs a local forward map, traverses the block tree, reads candidate/unlinked/chainstate data and asserts invariants. `GetAll()` allocates a local vector of usable chainstates. No body path draws randomness, queues/sends messages or calls X11. `CBlockIndex::GetBlockHash` reads its cached hash; the recomputation assertion using `GetBlockHeader().GetHash()` is commented out.

The rdc-only flag appends `-checkblockindex=0/1`; the already pinned Core `ApplyArgsManOptions` accepts it. No Core modification or ABI re-pin is needed for that Rust change. The test's five seeds × handshake, extension and reorg scripts compares raw outcomes including draws/calls, and its allocation-stack witness confirms that the enabled body executes while the disabled body does not. It passes.

Together with the source audit, that is enough to justify switching this invariant checker off for successful transcript-comparison fuzz runs. The evidence is not a universal equivalence proof for invariant-violating or allocation-failure runs: on can assert or fail allocating its temporary containers where off would continue. Campaigns with it off relinquish block-tree assertion coverage; retain on-mode coverage separately. D1's “only changes cost” claim should be understood with that limitation, not as a claim that the two modes have identical crash detection.

### Re-pin and unchanged Core verdicts

The installed archives match `pins.txt` and their MANIFESTs:

- Plain: `677db62c07aaacf137f782361ecb5f02750807d89eea919d6e020794a0a2fa32`.
- Fuzz: `516c2522f60e1a2a1bc1601d278b486009ef7e2ccd20b5243af28c716f89f678`.

`/work/scratch/o1c/logs/repro.log` records matching builds at roots `a` and `second-root` for each flavor, installation at the requested commit, and both reproduction exits 0. `check-oracle-repro.sh` drives distinct clean build roots with shared sources/downloads; both MANIFESTs name clean git source `e0248f71fa`, ABI entry packaging and the same recipe SHA-256. The recipe in this rdc tree matches that MANIFEST. The old `repro-plain.log` and `repro-fuzz.log` describe r1; they were not mistaken for this re-pin's evidence. Original reproduction roots have been removed, so this is independent verification of the installed bytes and retained two-root record, not another reproduction build.

Semantic comparison of `8d871a91b^` to `8d871a91b`, and then to the reviewed head, confirms all **46 SB4 vectors** preserve every semantic verdict, input and state field. Only provenance and these two instruction counts change: DGW **483,144,915 → 483,126,094**; KGW **861,074,211 → 861,649,266**. The full independent `regen.py --check` passes: **44 exact + 2 tolerance-checked**, with eight alternate schedules for each of 28 eligible ChainLock cases. It verifies the existing harness binary and all 23 linked archives. A scratch runpy adapter changes only TemporaryDirectory's location to the required disk TMPDIR, because the checker otherwise hardcodes `/dev/shm`; neither repository source nor expected vector was changed.

Both CS3 fixtures differ only in their provenance string: **118 check rows** and **10 primitive fixtures** retain all values. Independent `l1_check_rows` and `recorded_fixtures` suites each pass 2/2, without regeneration enabled. Evidence: `collateral.py`, `evidence/collateral.log`, `evidence/sb4-check-disk.log`, and `evidence/cs3-tests.log` under the review scratch root. **No recorded SB4 or CS3 Core verdict moved.**

### Allocation interposition and its coverage limit

The unarmed `_Znwm` replacement uses `malloc(max(size, 1))`, as ordinary libstdc++ new does on its successful path, and throws if allocation fails. It omits normal new-handler/retry behavior under genuine exhaustion. Core's dashd startup installs `new_handler_terminate` (`init.cpp:1180`), so this test mechanism should not be described as production OOM behavior.

I independently compiled a scratch copy of the peer-session test source with the allocator module absent. Dynamic-symbol inspection confirms normal `_Znwm@GLIBCXX_3.4` in that binary and a defined `_Znwm` in the interposed binary. All **22 original non-stack-witness cases** passed, along with two review probes. Identical seed-42 handshake and seed-30 reorg scripts have byte-identical raw transcripts across the two allocators; their aggregate SHA-256 values are `cdad89738bfef9c070af77496096ebe8ada3c5f91afe249abcb24bc9aed1565d` and `00fd423bf4ebf1031766b559817a98eefb3bd7f763b80a21ff32c80962a9a159`. Evidence: `probe-default.log`, `probe-alloc.log`, and `evidence/{default-raw,alloc-raw}`. F1 is independently reproducible without the replacement.

The `session_new` allocation test deliberately excludes stacks inside TestingSetup construction and validation-interface registration, and warms process initialization first. It tests the shim's guarded allocations and cleanup, not every Core setup allocation, nor aligned new/malloc/worker-thread failures. That is a disclosed ABI containment test with limited fault coverage. It does not establish exception safety for all of Core, which has aborting/noexcept paths and non-exception-safe setup. I found no evidence that successful ordinary allocation behavior is being masked. Both independent allocation-fault runs pass **5/5**.

### Isolation beyond F1

Each start clears arguments/network selection, resets time data and the IBD service cache, and constructs new chainstate options. `min_chain_work` and `check_block_index` belong to that new manager, not a retained process option. The inline validation queue runs callbacks on the hooked thread; teardown unregisters its participants and disables inline mode. Capture restoration is conditional on having saved it, including failed starts. RPC registration is once per process, avoiding the earlier repeated-registration growth.

An independent mixed-configuration probe changed services, minimum chain work, check-block-index mode, send/receive caps, peer timeout and bantime, performed removal/maintenance, then forced an invalid minimum-work start. Fresh baseline setup, handshake and reorg outputs still replayed byte-for-byte. A separate probe connected a recent block, ran maintenance, explicitly observed initial-sync completion, and then verified that a fresh baseline script replayed. The r1 IBD leak remains fixed. These successes do not cover logger category/level reset; F1 shows why identical-default replay alone is insufficient.

## R1 disposition and validation

The saved range-diff (`evidence/core-range-diff.txt`) compares `9c6dd45e0b..6efa24f215` with `d694a47043..e0248f71fa`. All nine r1 commits survive; the only rebase difference in those commits is the expected ABI/base-header context adjustment for O2a-EV. Eleven subsequent commits implement the ABI 10 fixes and requested additions.

| R1 finding | Round-two disposition |
| --- | --- |
| F1, ignored local services | Resolved: InitConnman passes the supplied mask to real InitializeNode; version masks and filterload service gating pass. |
| F2, foreign allocation unwind | Resolved in the shim: allocation-free templated Guarded encloses allocations, record failures are caught, and failed sessions refuse further verdicts. Allocation-test coverage is limited as described above. |
| F3, unreachable maintenance/inactivity | Resolved: explicit ticks call real Core routines; nine-peer eviction and silent/half-handshake timeout cases pass. Cadence/interleaving is explicitly owned by the script. |
| F4, missing backpressure domain | Resolved: room-limited sockets drive real V1/V2 transports and ConnMan accounting; controlled partial sends, many pongs, receive pause and V2 fallback cases pass. |
| F5, observer overhead in off builds | Resolved for these seams: production hooks/snapshots are enclosed by DASH_ORACLE_HOOKS. Inspected retained object-comparison evidence reports no oracle symbols/relocations or extra instructions in off-mode objects, apart from line-number immediates. Those objects were not rebuilt in this review. |
| F6, unlogged scheduler reorg draw | Resolved: no session scheduler worker services the validation queue; inline draining executes the real disconnect/reset callback under DrawScope. The reorg test observes the extra filter-reset GetRand and raw replay, and passes. |

Independent requested commands against the pinned plain archive:

```text
cargo test --locked -p dash-oracle-sys --features oracle \
  --test addrman --test peer_session --test alloc_fault --no-fail-fast
  addrman: 10 passed; peer_session: 23 passed; alloc_fault: 5 passed
cargo test --locked -p node-l1 --features oracle --test p2p
  1 passed
cargo test --locked -p dash-oracle-sys --features oracle --test alloc_fault
  second run: 5 passed
```

Autobuild was disabled and unpinned archives disallowed. Logs: `evidence/sys-tests.log`, `l1-p2p.log`, `alloc-fault-second.log`, and `test-exits.txt`. Node-l1 P2P remains the documented Core-vs-Core bootstrap/replay leg; its pass does not independently certify the downstream Rust implementation.

The retained `gate-abi10.log` reports **GATE EXIT 0, 3,587 passed / 0 failed**, its oracle-diff section sums to **174/0**, and smoke reports **43/43 with zero divergences/crashes/sink failures**. `gate-last-green` names `2c31fe338`. These are verified implementer records; the full gate and fuzz smoke were not independently rerun.

Final cleanup: the review-created Rust target was removed with `cargo clean`; neither detached reviewed tree has source changes. Scratch probes and evidence are retained for the manager. This report is uncommitted. Passing suites and unchanged fixture verdicts do not satisfy the gate while F1 permits previous inputs to change oracle observations.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
