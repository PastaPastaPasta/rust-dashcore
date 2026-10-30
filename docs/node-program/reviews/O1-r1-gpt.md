**Verdict: CHANGES-REQUIRED.** O1 is not ready to serve as the trusted ground truth for downstream consensus gates. The supplied plain archives currently match their recorded hashes, and all 17 smoke tests pass. However, the build/link path can silently consume an archive that does not match its pin, a clean rebuild failed the recorded reproducibility check, a valid devnet configuration is refused, and important operations have no successful independent smoke comparison.

Reviewed on 2026-10-08, independently and without modifying or committing either implementation branch. Scope: Dash Core `node/oracle-shim-v23.1` at `6d5c9ac2ed1bbd4325f72d67d21326e7307f29be`, `node/oracle-shim-v24.0` at `684cb425e2bbd2eb11b842083ac6754004495abc`, and rust-dashcore `node/o1-oracle-sys` at `c4657bc0`, including its three-commit `node/integration...HEAD` diff. Design references: A1 §4.4 and PLAN row O1. Scratch evidence is under `/work/scratch/o1-review/`.

Core references below apply to both pins unless specified: their `src/test/oracle/oracle.{h,cpp}` and `src/Makefile.oracle.include` are identical. Rust references are relative to `/work/home/agent/workspace/rdc-wt/o1-oracle-sys/`. The matching Core worktrees are `/work/home/agent/workspace/dash-wt/oracle-shim-v23.1/` and `/work/home/agent/workspace/dash-wt/oracle-shim-v24.0/`.

**B1 — BLOCKER: the archive actually linked is never authenticated against the pin.**

Files: `dash-oracle-sys/build.rs:84`, especially lines 89–96; `contrib/node/build-oracle.sh:101`, especially lines 103–106; `dash-oracle-sys/pins.txt:7`.

Scenario: replace or modify a valid cached archive while leaving its existing `MANIFEST` in place. Both the shell script and Cargo compare the expected digest with the *text in MANIFEST*, rather than computing the digest of the `.a` file. Both accept the substituted archive without a warning. Even a manifest digest that differs from the pin only produces a Cargo warning and is still linked. The fuzz digests are `-`, so that flavor has no recorded digest check at all.

Proof: in a separate scratch cache, I preserved the original manifests and substituted a linkable v23 archive whose subsidy entry point delegates to the original implementation before V20 but returns 42 satoshis when V20 is requested. The real `build.rs` exited successfully and emitted both link directives with no warning; the real shell script reported both pins as cached. The unchanged safe Rust wrapper then printed:

```text
safe_api abi=1 version=v23.1.8 post_v20_subsidy=Ok(42)
other_pin abi=1 version=v24.0.0-rc.3-1-g684cb425e2bb
```

The substituted archive's actual SHA-256 was `1e204a54aec80cb03d1a8fe0641fc6b38e1e6215586e8e81a843df37d7b028fb`, while the untouched manifest/pin said `bb99cc128b40f685547f7881834ea06a16d028bc8e911059b6e5be2b27e2180b`. This is a cache-integrity counterexample, **not a claim that the supplied cache is corrupt**: I independently hashed both supplied archives and they match the pins. Evidence: `stale-manifest-build-rs.log`, `stale-manifest-script.log`, `safe-cache-probe.rs`, `safe-cache-probe.log`, and `stale-manifest-cache/`.

Fix: hash the actual archive before cache reuse and before linking, and fail the trusted-oracle build on a mismatch. Validate manifest identity fields as well; use an immutable verified artifact or a verified snapshot so a concurrent rebuild cannot replace it between checking and linking. Provide any deliberately unpinned/toolchain-variant mode as an explicit opt-in that cannot silently satisfy a pinned consensus gate; record and review the fuzz artifacts too. Do not let a successful cache lookup or a version string stand in for artifact identity. This blocks trusting downstream differential results because a different implementation can currently present itself as the recorded Core pin.

**M1 — MAJOR: a clean build under another cache path is not reproducible.**

Files: `contrib/node/build-oracle.sh:121`, lines 124–138 and 202–204; `dash-oracle-sys/README.md:79` (the rebuild/check procedure).

Scenario: follow the README procedure with a different cache root on the same machine. I copied only the existing depends source/package caches into `/work/scratch/o1-review/repro-cache/`, then built clean `git archive` sources using the unmodified script, the same pin, and the same gcc/g++ 15.2.0 and GNU ld 2.46. The v23 plain artifact changed from the recorded `bb99cc128b40f685547f7881834ea06a16d028bc8e911059b6e5be2b27e2180b` to `4b6b06020b18c669fd050fde358e54ef996a966940758ba7b6f4de0adad66ed7`. All manifest fields other than the digest are identical. The script warns but exits successfully.

The two 19,880,048-byte relocatable objects have identical section layouts and differ in only 53 bytes: the Boost pool assertion filename is `./depends/x86_64-pc-linux-gnu/include/boost/pool/pool.hpp` in the recorded artifact and `../depends/x86_64-pc-linux-gnu/include/boost/pool/pool.hpp` in the rebuild. This experiment found a diagnostic-string difference, not a consensus arithmetic difference. Nevertheless it disproves the claimed archive reproducibility. The environment has `CCACHE_BASEDIR=/work`; Core configure automatically enables ccache, and the script never clears that setting despite explicitly identifying its path-rewriting hazard in lines 122–123.

Fix: control ccache's path rewriting, or disable ccache for this reproducibility build, and normalize relevant compiler/preprocessor paths consistently. Add an automated same-toolchain build in two different roots and require byte-identical archives. Record the recipe/environment inputs needed to reproduce a pin. Evidence: `repro-build.log`, both v23 manifests, `oracle_v23_1.o`, `rebuilt-v23.o`, and the section listings. The subsequent v24 and fuzz build results are recorded in the validation notes below.

**M2 — MAJOR: a valid Core devnet option disables the entire oracle network.**

Files: Core `src/test/oracle/oracle.cpp:433`, particularly line 435; v23 `src/chainparams.cpp:1308`; matching v24 override implementation; both pins' `src/pow.cpp:167`.

Scenario: select devnet with `-devnet=o1-review -powtargetspacing=86401`. Core accepts any parsed spacing of at least one second. The devnet dispatch uses DGW, whose arithmetic works for this value; it does not need the BTC `DifficultyAdjustmentInterval()`. The shim nevertheless rejects the network because `86400 / 86401 == 0`, sets `g_network_selected` false, and makes every network-dependent entry point unusable.

Proof on **both pins**, calling the real Core function and globals from a scratch copy of the archive with just the relevant internal symbols exposed:

```text
devnet_spacing86401 shim_status=-5 core_spacing=86401 direct_core_bits=1c055555
```

The direct call used a complete synthetic DGW ancestry with safe times/bits; Core's chain parameters were constructed by its actual `SelectParams` path. Evidence: `core-probe.cpp`, `core-probe-v24.cpp`, and their `.log` files.

Fix: retain Core's accepted network-configuration domain. Check only preconditions actually needed by the dispatched operation, with checked arithmetic for genuine UB conditions; do not require a BTC interval for a DGW-only devnet. Add independent devnet/regtest override smoke cases. The current suite never selects either network and therefore misses this behavior.

**M3 — MAJOR: the claimed per-operation smoke coverage leaves successful operations and release distinctions unverified.**

Files: `dash-oracle-sys/tests/smoke.rs:435`, `:644`, `:660`, and `:685`, especially lines 701–710; `dash-oracle-sys/scripts/gen-samples.py:43`; `dash-oracle-sys/tests/data/PROVENANCE.md:26`.

The captured data is honestly documented, but it does not meet the stated meaningful smoke coverage:

- `CoinCodec::VarIntDecode` is never called by the smoke suite. The coin/undo tests exercise `VarIntEncode`, not its standalone decode operation.
- `platform_share` is called successfully only inside the EvoReward branch. All ten payment samples are testnet Classic-era samples; mainnet has zero payment samples. Consequently the only executed platform-share call passes `i64::MAX` and returns from the shim guard **before `PlatformShare` is called**. An implementation that always returns `ORACLE_E_ARG` would pass this coverage.
- Every subsidy/superblock sample predates V20. Neither `v20=true` nor CreditPool/EvoReward payments has an independent successful comparison.
- All fixture txids equal instance hashes; sampled transaction types are mainnet `{0}` and testnet `{0,1,2,3,4,5,6}`. The v24 stable-txid asset-unlock distinction is never tested. Returning the txid from `instance_hash` for both pins would pass the current checks.
- There are no regtest/devnet/default-or-override smoke contexts.

The main captured expected values do come from stock dashd RPCs and checksum-confirmed undo records, and several other comparisons use dashcore independently. I inspected the generator (it does not call the shim) and independently cross-checked block 1 bytes and a captured subsidy against each live reference RPC. However, the suite's broad “never from the oracle” claim also needs qualification: it derives script context, superblock cycle/PoW context and some expected values from `chainparams_json`, oracle encoder round trips, or other oracle outputs. Some are anchored by separate independent assertions; they are not all independently captured per operation.

Fix: add successful, externally sourced comparisons for every operation, including standalone VARINT decoding and PlatformShare; cover both sides of V20/MN_RR and the v24 txid/instance-hash distinction. Use stock Core RPC/functional behavior or independently extracted Core test vectors with provenance where live-chain samples are unavailable. Add independent default and overridden regtest/devnet contexts. Count the required operation/era/network cases so fixture truncation cannot silently remove them. Do not treat “17 tests passed” as evidence that these branches were reached.

**M4 — MAJOR against the requested guard contract: some `ORACLE_E_ARG` guards censor defined Core results.**

Files: Core `src/test/oracle/oracle.cpp:224`, especially lines 225–233; `:776`, especially lines 778–780; `src/test/oracle/oracle.h:14`; Rust `dash-oracle-sys/tests/smoke.rs:584`.

Two confirmed examples on both pins:

```text
mn_height1 shim_status=-4 direct_core=1844674407370955161
kgw_30 shim_status=-4 direct_core_short=1b195b2f direct_core_full=1b195b2f
```

For masternode payments, Classic height 1 with `INT64_MAX` only executes `blockValue / 5`; it does not multiply by 600 or overflow. The blanket maximum-coefficient bound filters this defined result. This value is outside monetary consensus input bounds, so this example demonstrates an inaccurate *function-level guard domain*, not a reachable valid-block consensus split.

For KGW, a chain ending at height 20,000 with equal `0x1c100000` targets and one-second time spacing reaches its event horizon after a short lookback. The real Core function gives the same result with 30 ancestors and 4,032 ancestors. The shim rejects the 30-header input without letting Core evaluate it. The full-maximum KGW restriction is intentionally documented in the ABI, so this is not an undisclosed memory-safety bug; it still does not satisfy the requested exact assert/UB guard contract. The smoke test also treats some Argument errors as skipped windows rather than asserting an independently specified required lookback.

Fix: distinguish insufficient harness context/intentional domain restrictions from proven Core assert/UB preconditions. Give differential targets a complete reference context and require independently justified coverage bounds; do not silently discard defined cases as Core-invalid arguments. Narrow numeric overflow checks to the arithmetic actually executed. Avoid solving the KGW case by reimplementing its difficulty arithmetic in the shim: supply complete ancestry or otherwise establish the actual traversal requirement. If conservative exclusions are retained, document and approve them in the coverage matrix rather than claiming exact Core assert/UB correspondence. Evidence: the two direct-Core probe sources/logs cited above.

**Checks that passed and scope limits.**

The source-level shim is largely sound. Codec, hash, script, compact-target, block-proof, transaction/block-check and payment/subsidy bodies call Core implementations rather than reimplementing consensus arithmetic. The Makefile addition only exposes an explicitly requested extra library target. The two parent commits are the peeled `v23.1.8` and `v24.0.0-rc.3` tags; the shim source is identical across pins, with the intended conditional differences for instance hashes and payment API signatures.

The next-work adapter copies header time/bits/version/merkle/nonce through the real header/index types, assigns consecutive heights and stable deque-backed `pprev` links, and leaves skip pointers null. This faithfully supplies the fields the pinned BTC/KGW/DGW implementations read when the admitted window is sufficient. `nChainWork`, `nTimeMax`, block hashes and other index state remain zero/unset, but **neither pinned `GetNextWorkRequired` reads them**, so I found no present divergence from those omissions. Neither pin's next-work implementation consults deployment state. The subsidy `*_inner` API deliberately accepts the V20 decision explicitly, as specified by O1; it does not itself test versionbits/deployment-state computation. Script flags are also explicit inputs passed through unchanged. No v1 entry point decodes BLS objects or consumes `bls_legacy_scheme`; transaction payloads remain byte bags here. Initialization of future BLS/contextual entry points belongs to O2a and must be reviewed separately.

The handwritten Rust ABI declarations match the current C signatures and `oracle_validation` layout. Output ownership stays with the caller, capacities/required lengths are explicit, fixed hashes are copied, and no C++ heap-owned buffer is handed to Rust. Input buffers must obey the header's pointer/length preconditions; a null pointer with nonzero length is not promised to be checked. Core exceptions from operational bodies are caught and mapped; optional scalar/hash outputs and null/short variable-output cases follow the documented convention. I found no confirmed ownership error or escaping C++ exception. Network options are expressly trusted harness configuration in the C header: inconsistent regtest overrides can still cause Core assertions, and this is not a general fuzz-safe configuration parser.

The concurrency split is justified for the present entry points. Rust's per-pin mutex covers selection plus the complete network-sensitive operation, including buffer retries; it prevents replacing `Params()`/`g_stats_client` while they are read. Non-network operations use local objects, Core's static secp256k1 verification context and once-initialized logging. `ECC_Start()` is unnecessary for the verification operations in these pins (`pubkey.cpp` uses `secp256k1_context_static`). Fresh local blocks avoid sharing `CBlock::fChecked`. Raw unsafe FFI callers still have to obey the C header's synchronization requirements and must not invalidate the safe wrapper's cached chain selection.

For the supplied plain archives, `nm -g --defined-only` exposes exactly the 25 pin-prefixed entry points per pin. The script's partial link closes the reachable Core/dependency references, dissolves COMDAT groups, renames entry points and localizes other defined symbols; `-fno-gnu-unique` and the leaked-symbol check address GNU-unique symbols. All 17 tests linked and exercised both pins in one executable, including threaded/interleaved chain selection. Both clean fuzz builds also passed their standalone link checks and linked together into a libFuzzer executable. That executable selected mainnet in v23 and testnet in v24, checked their distinct parameter JSON, and ran 1,000 header-decode comparisons including truncated inputs. I found no symbol collision in either exercised flavor. Flavor/build checks still need artifact authentication and reproducibility independently of this symbol-isolation technique.

**Validation record.**

- `cargo test -p dash-oracle-sys --features oracle --test smoke`: **17 passed**, both supplied plain pins linked together.
- Actual supplied archive digests match `pins.txt`: v23 `bb99cc128b40f685547f7881834ea06a16d028bc8e911059b6e5be2b27e2180b`; v24 `855b9b265debd867e2623ab6dce91aad261d3b083cdee04333578991f72e41fc`.
- Independent live RPC spot checks: block 1 bytes match both fixtures; mainnet height 450,000 subsidy is 543,214,287 satoshis and testnet height 430,000 subsidy is 1,940,051,022 satoshis, matching the committed data. Evidence: `live-rpc-crosscheck.json`.
- Both direct-Core probes confirm M2/M4. They expose existing Core symbols in **scratch object copies only**; the implementation branches and original cached archives were not changed.
- Clean v23 plain build succeeds but differs from its recorded digest, as detailed in M1. Its runtime version/ABI and a genuine post-V20 subsidy remain sensible (`v23.1.8`, ABI 1, 205,304,206 satoshis for the probe input).
- Clean v24 plain build succeeds and **matches** the recorded digest `855b9b265debd867e2623ab6dce91aad261d3b083cdee04333578991f72e41fc`. Thus the reproducibility failure reproduced here is specific to v23, not both pins.
- Both clean fuzz builds succeed and pass the script's libFuzzer link/run checks. Digests: v23 `6283f42d9f2b82271bce47e93a7a5090de6076f19dd7adcddc083dbcf4bc9572`; v24 `09315f3875c5f3de2862663dde435c3368ccd1aee7b98a8e6f81fc74f6b153b5`. These are single builds, not proof of fuzz reproducibility; neither digest is recorded in the reviewed `pins.txt`.
- Combined two-pin fuzz executable: **1,000 runs completed**, seed 1, no failure. Evidence: `two-pin-fuzz.c` and `two-pin-fuzz.log`. This checks linking, isolated parameter selection and successful/failing header decoding; it is not an exhaustive ABI or differential fuzz campaign.

No claim is made here that all malformed inputs, every trusted configuration, or future O2a stateful adapters have been exhaustively tested. The blockers/findings above are supported by source traces and executable counterexamples; fixing them and rerunning independent coverage/reproducibility checks is required before approval.
