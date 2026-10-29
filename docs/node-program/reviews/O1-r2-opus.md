# O1 + O1b review, round 2 (Opus)

**Verdict: APPROVE-WITH-NITS.** Every round-1 finding (B1, M1, M2, M3, M4) is closed, and I verified each one by running something, not only by reading. The O1b entry points call Core's own functions at both pins, and their guards match Core's assert/UB conditions with one documented exception (duplicate prevouts, minor m1 below). The KGW full-lookback restriction is safe and complete: no KGW input can get a wrong verdict, only a refusal (§KGW). I found no blocker or major. The minors are all fail-closed tooling issues or documentation precision.

Reviewed on 2026-10-08, read-only:
- rust-dashcore `node/o1-oracle-sys` 465c30ca..36c2b987 (worktree `rdc-wt/o1-oracle-sys`, HEAD 36c2b987, clean).
- Dash Core `node/oracle-shim-v23.1` 506510fca8..350ce242a5 and `node/oracle-shim-v24.0` c463c5e0ee..23769258c2, four commits each. The shim files are byte-identical on both pins and on `node/o1-oracle-shim` (`git diff --stat` between the three is empty).
- `node/o1b-core-vectors` 2babb5575d and ac7e45359f.

Scratch and evidence: `/work/scratch/o1-review-r2/`. Core references are to the tags v23.1.8 and v24.0.0-rc.3.

## Validation record

| Check | Result |
|---|---|
| `cargo test -p dash-oracle-sys --features oracle` (TMPDIR and CARGO_BUILD_JOBS=12 as briefed, own target dir) | **47 passed**: archive_check 8, contexts 9, script 12, smoke 18 (`test.log`) |
| `cargo clippy -p dash-oracle-sys --features oracle --all-targets -D warnings`, `cargo fmt --check` | clean |
| `check-oracle-repro.sh --work …/repro --jobs 8 v23.1` (plain), my own two fresh roots, sources copied to scratch | **both roots `22f00737…829463` = pins.txt** (`repro-v23.1.log`). v23.1 is the pin that failed in r1. With the implementer's two roots, that makes four independent byte-identical builds. |
| Installed cache archives | v23.1 `22f00737…`, v24.0 `c9cbd756…`: both equal pins.txt |
| Direct C probe of both archives (`probe/probe.c`, logs `probe/p23.log`, `probe/p24.log`) | 0 failures on each pin (details below) |
| The implementer's workspace gate log (`/work/scratch/o1/evidence/gate-test-workspace.log`) | exit 101 only in `dash-spv`/`dash-spv-ffi` dashd tests ("DASHD_PATH must be set"). This is environmental: the diff touches only `.github/ci-groups.yml`, `Cargo.toml` (workspace member), `contrib/` and `dash-oracle-sys/`. |

## Round-1 findings

**B1 (substituted archive links): closed.** `build.rs` → `build/archive.rs::snapshot` checks the MANIFEST identity fields (pin, commit, flavor, symbol prefix). It then copies the archive into `OUT_DIR`, hashes the *copy*, and links only that copy, and only if its sha256 equals the pins.txt column for the selected flavor. Both flavors are now pinned. I attacked it this way:
- **Bit flip.** I copied the real v23.1 archive into a scratch cache and flipped one byte (`m`→`M` in `mandatory-script-verify-flag-failed`), keeping the original MANIFEST, whose sha256 line is now stale. The pinned build fails: `has sha256 699efdf3…, but pins.txt records 22f00737… refusing to link it`.
- **Opt-in at build time only.** With `DASH_ORACLE_ALLOW_UNPINNED=1` the archive links, with a cargo warning. Running the resulting test binary *without* the variable panics in `Oracle::new` ("built with unpinned archives"). With the variable set at run time too, the flipped string appears in the verdicts (`tx_invalid row 7 … "Mandatory-script-verify-flag-failed …"`), so the flip was linkable and only the hash check stopped it.

The only bypass left is deliberate. Raw `ffi::v*_*::API` calls skip the run-time check, but they still need the build-time opt-in (nit n6).

**M1 (non-reproducible v23.1): closed.** ccache is disabled and `CCACHE_*` cleared, locale and TZ are fixed, `SOURCE_DATE_EPOCH` is set, a path grep rejects any build, cache or $HOME path left in the object, and debug info is stripped. My independent two-root rerun reproduced the pinned v23.1 plain archive exactly (table above). The fuzz flavor's two-root evidence (`/work/scratch/o1/evidence/repro-fuzz.log`) is consistent; I did not rerun fuzz.

**M2 (valid devnet spacing refused): closed.** `oracle_set_network` no longer refuses anything Core accepts. A spacing above the timespan is refused only per call: `NextWorkPreconditions` (oracle.cpp:233-238) returns `ORACLE_E_ARG` at BTC-era heights, where Core computes `% DifficultyAdjustmentInterval()` with an interval of 0 (the SIGFPE recorded in `m2-dashd-sigfpe.txt`). The other overflow guards (oracle.cpp:241, 265, 290) match Core's evaluation order exactly. `contexts.rs::a_devnet_spacing_above_the_timespan_is_accepted` and the devnet-spacing captures from the release binaries cover it.

**M3 (smoke coverage gaps): closed.** These are now covered:
- `VarIntDecode` (smoke.rs:524).
- Successful `platform_share` and every reward era with V20 on, from Core's `block_reward_reallocation` regtest records (contexts.rs:227, which requires counts per era).
- V20 mainnet subsidy points (contexts.rs:292).
- The v24 txid/instance-hash split, using type-9 v2 asset unlocks whose expectations come from dashd's `getblock 2` (contexts.rs:367).
- Default regtest and devnet-spacing captures from the official release binaries.
- P2SH sigop counts from dashd block templates.

Coverage counters guard against silent truncation.

**M4 (guards excluding defined results): closed**, re-derived at both pins:
- `oracle_mn_payment` (oracle.cpp:909-918). Before the reallocation, Core only divides. Its only multiplications are `blockValue * 3` and `blockValue * vecPeriods[n]` (513..600). A probe with `blockValue = 1000` can never overflow and identifies the path, because 750 can only come from `*3/4`. On both pins the probe hit the exact boundaries `INT64_MAX/585`, `INT64_MIN/585` and `INT64_MAX/3` on the accepting side and refused one step past each. Classic height 1 with `INT64_MAX` now returns 1844674407370955161, the value from the r1 counterexample.
- `oracle_platform_share`: exact (`*375` overflow or `!MoneyRange`, which is Core's assert).
- KGW: the short-window case is now `ORACLE_E_CONTEXT`, a declared domain restriction rather than a Core-assert claim, as r1 asked. See §KGW.

## O1b entry points (DEC-25(3), DEC-29 D3, DEC-35 D7), checked against both pins

All CS1 §7.5 entry points are present, with the specified signatures, plus `convert_bits_to_double`, `superblock_payments_limit` and `split_amount_by_shares`. The four ScriptNum helpers are extras, all thin wrappers.

- **Thin script wrappers** (`eval_script`, `pubkey_verify`, `check_low_s`, `parse_der_lax`, `check_signature_encoding`, `find_and_delete`, `script_sigops`, `script_sigops_with_sig`). Each calls the Core function directly. I grepped every `assert` in `interpreter.cpp` at both pins. The ones reachable from these calls are the CLEANSTACK-without-P2SH assert (guarded, see below) and `SignatureHash`'s `nIn` assert (guarded). The rest are opcode-switch defaults, `ConditionStack` asserts behind emptiness checks, `!m_ready`, and missing-data asserts that Dash's BASE sighash never hits. An arbitrary initial stack for `EvalScript` reaches no new UB: DIP0020 ops bound sizes before arithmetic. Between the pins, `interpreter.cpp`, `script.cpp` and `pubkey.cpp` differ only cosmetically (`emplace_back`, the `Derive256` addition).
- **`check_minimal_push`**: guard = Core's `assert(0 <= opcode && opcode <= OP_PUSHDATA4)`. Exact.
- **`scriptnum_decode`** (oracle.cpp:1009-1012). Both pins build as C++20, so the left shift by 56 into the sign bit is defined, and 8-byte inputs are fine. From 9 bytes on, `set_vch` shifts by 64 or more, which is UB. The guard refuses exactly the >8-byte inputs that pass the constructor's own checks: size ≤ max_size, and minimal when minimality is required. Probe: an 8-byte `ff…ff` returns −INT64_MAX. A 9-byte non-minimal input with minimality required returns REJECT, without minimality ARG, and above max_size REJECT. `scriptnum_encode(INT64_MIN)` is defined at both pins (`~uint64 + 1`) and returns 9 bytes.
- **`split_amount_by_shares`** (oracle.cpp:1144-1177, v24 `evo/providertx_util.cpp:88-114`). The guard repeats Core's two `+=` sums and the final `total - paid` with checked arithmetic, in Core's order. `ProRataFloor` itself has no UB: it uses `uint64` casts, wrapping `arith_uint256` arithmetic, and `GetLow64` → int64, which is defined in C++20. Its division by zero throws `uint_error`, which maps to `ORACLE_E_EXCEPTION`. That happens for weights summing to 0 with n ≥ 2. With n = 1, `ProRataFloor` is never called and the result is `total`. Probe results: [0,0] → EXCEPTION; [-1,1] → EXCEPTION; [INT64_MIN,-1] → ARG; n=1 → total; n=0 → OK; (INT64_MIN; −3,5,7) → OK, a defined wrap. v23.1 → UNSUPPORTED. Exact.
- **`superblock_payments_limit`** (oracle.cpp:1179-1191). At both pins `GetPaymentsLimit` never reads `active_chain`: V20 comes from `V20Height`, and nBits is powLimit or 1. The guard applies `SubsidyHeightInRange(height-1)` only at superblock heights, which is exactly where Core calls `GetSuperblockSubsidyInner(…, height-1, …)`. `SubsidyHeightInRange` is exact. The halving loop overflows iff `prev ≥ ⌊INT_MAX/interval⌋·interval`, and `prev < nHighSubsidyBlocks` hits `assert(isDevnet)`. Probe: at 4 real mainnet superblock heights, including both sides of V20, the result equals `superblock_subsidy_inner(1, h-1, v20) × 16616` on both pins. The top superblock height below INT_MAX is refused, and so is the matching subsidy call.
- **`convert_bits_to_double`**: thin. A mantissa of 0 gives `65535/0.0 = inf` (nit n3).
- **`transaction_sigops` / `check_input_scripts`**: see the next two sections.

## The coins-view adapters (N3)

`AddSpentCoins` (oracle.cpp:340-358) works as follows:
- **Value −1 refusal**: exact. `CTxOut::IsNull()` is `Coin::IsSpent()`. `CCoinsViewCache::AddCoin` would assert on it, and so would the functions under test through `AccessCoin`. Value −2 is accepted (probe), which is correct: Core's behaviour there is defined.
- **Unspendable spent outputs** (OP_RETURN first, or larger than MAX_SCRIPT_SIZE): `AddCoin` silently drops them, and `GetP2SHSigOpCount`/`CheckInputScripts` then assert on the empty coin. Refusing is right, and no UTXO set can hold such an output.
- **Count mismatch**: refused. A coinbase's spent outputs are ignored, which is right because Core reads none.
- **Duplicate prevout**: refused even when both spent outputs are identical (probe, both pins, both adapters). See m1.

## `check_input_scripts`: concurrency and the signature cache

- **Cache setup.** Both caches are set up once behind `std::once_flag` (oracle.cpp:362-375). On v23, `InitSignatureCache()`/`InitScriptExecutionCache()` read `gArgs` under `cs_args`, so racing a concurrent `oracle_set_network` is not a data race, and `-maxsigcachesize` cannot be injected (`SetupChainParamsOptions` does not register it). The v24 `bool` returns map to an exception, and `call_once` retries after one.
- **Store nothing.** `cacheSigStore=false` makes `CachingTransactionSignatureChecker` call `Get(entry, erase=true)` without a `Set`. `cacheFullScriptStore=false` skips `g_scriptExecutionCache.insert`. Nothing is ever stored, so no lookup can hit and verdicts cannot leak between calls.
- **Locking.** `LOCK(cs_main)` around the call satisfies `AssertLockHeld`, and the execution-cache probe runs under it. `CheckInputScripts` reads no `Params()`, and `g_stats_client` is set once.
- **Stress probe.** Eight threads ran 20 000 calls each while a ninth thread switched the network 200 times. Every result stayed REJECT, with no crash and no deadlock.
- **CLEANSTACK pre-check** (oracle.cpp:1108-1118). `VerifyScript` reads CLEANSTACK only at its final assert, so a run without the flag is identical up to that point. On a failure, `CheckInputScripts` returns at input 0, and its retry uses `flags & ~STANDARD_NOT_MANDATORY_VERIFY_FLAGS`. CLEANSTACK is in STANDARD and not MANDATORY on both pins (v23 `standard.h:57`, v24 `standard.h:47`), so the retry drops it. Only input 0 can therefore reach the assert. Probe:
  - passing input 0 + CLEANSTACK without P2SH → ARG;
  - failing input 0 → REJECT, Core's own classification;
  - input 0 passes, input 1 fails → ARG, because Core would assert on input 0;
  - P2SH|CLEANSTACK → OK.
- **Expected deviation.** The classification matches the mempool path (`pvChecks=nullptr`), not ConnectBlock's parallel path. That is as designed in CS1 §7.5.

## KGW full-lookback restriction (`ORACLE_E_CONTEXT`): manager decision

**Recommendation: accept.** It is safe (no wrong verdict is possible) and complete for every configuration Core can select.

Argument, against pow.cpp at both pins (KGW is unchanged between them):
1. KGW runs only for `nPowKGWHeight ≤ height+1 < nPowDGWHeight`. That range is non-empty only on mainnet and regtest (15200..34140). Testnet (4002/4002) and devnet (4001/4001) have none, and devnet's `-powtargetspacing` is the only spacing override. So wherever KGW runs, `PastBlocksMax = 604800/150 = 4032 > 0` and `PastBlocksMin = 14`.
2. Core's loop stops after `i = PastBlocksMax`, or when it reaches height 0, or at the event horizon. It therefore reads at most heights `[max(1, h−4031), h]`. The shim accepts only `h == 0`, `h < PastBlocksMin` (Core reads nothing), `first ≤ 1`, or `len ≥ 4032`.
3. In each accepted case the window covers everything Core reads. The window's null `pprev` at `first` turns Core's "advance, then exit the loop" into a `break` *after* the same iteration, and all post-loop state is identical. With `first == 1`, real Core stops by moving to genesis, which has height 0; the window stops by `break` after processing height 1. The result is the same.
4. Empirically, I generated 3 synthetic mainnet chains (on-target spacing, which reaches the full lookback; noisy; bursty, which hits early horizons) × 196 KGW heights × both pins. The result from the full chain from height 1, from the last 4032 headers and from the last 5000 headers agreed in every case (0 mismatches). A 4031-header window was refused with CONTEXT every time.

The cost: a KGW query needs 4032 headers (322 KB) or a chain from height ≤ 1, even when Core's event horizon stops much earlier. The early stop cannot be predicted without reproducing KGW's libm-dependent floating point (DEC-29 D2 / quirk 40), so the restriction is the right trade. CS2's KGW fuzz targets must generate full ancestry. One hardening nit: if `PastBlocksMax` were 0, the guard would accept short windows over an unbounded Core walk. That is unreachable today (point 1), but cheap to close (n1).

## Findings

**m1 (minor): an identical duplicate prevout is refused although Core's result is defined.** oracle.cpp:352-354, oracle.h:277-286.
- Scenario: a tx whose inputs 0 and 1 spend the same outpoint, with identical spent outputs. Core's `GetP2SHSigOpCount`, `GetTransactionSigOpCount` and `CheckInputScripts` evaluate it deterministically: both inputs read the one coin, with no assert and no UB. The shim returns `ORACLE_E_ARG` (probe, both pins).
- Context: the refusal does follow CS1 §7.5. It is unreachable in consensus (`CheckTransaction`'s `bad-txns-inputs-duplicate` runs first on both the mempool and the block path), and it is documented. So this is a declared domain restriction, but it is labelled as an argument error.
- Fix: accept identical duplicates (add the coin once; that is exactly Core's view) and refuse only conflicting ones, which are genuinely malformed. Or keep it, record it in the coverage matrix next to KGW, and say in oracle.h that it is a domain restriction, not a Core assert.

**m2 (minor): non-atomic cache publish.** build-oracle.sh:272 and :294-295; check-oracle-repro.sh:132.
- Scenario: `rm -f "${archive}"` runs before `strip dashd` and the MANIFEST/archive moves, and `--install` removes `dest` before its `mv`. A `cargo build` that reads `/work/cache/oracle` during a `--force` rebuild or an install sees no archive (it links nothing, then fails to link) or a missing file (`fs::copy` panics).
- This is fail-closed, because B1's hash check still holds, but it breaks other agents' builds for that window.
- Fix: drop the `rm -f` (a `mv` over the old file is atomic). For `--install`, publish into a new directory and swap it in with `rename` or a symlink flip.

**m3 (minor): pin moves can leave pins.txt half-updated.**
- build-oracle.sh:299-301: `--update-pins` rewrites the commit but only the built flavor's sha column. The other flavor keeps the old commit's digest, so it is unbuildable (rejected) until edited by hand.
- check-oracle-repro.sh:66-77: `--update-pins` writes the new commit and `-` before building, and leaves them in place when the build fails.
- Both are fail-closed, but they leave the tree unable to link.
- Fix: in build-oracle.sh, reset the other column to `-` on a commit change, as the repro script does. In the repro script, write pins.txt only after both roots agree, or restore it on failure.

**m4 (minor): `pins_match_pins_txt` accepts either flavor's digest.** smoke.rs:160.
- Scenario: `fields[3..5].contains(&sha)` would pass a plain build that links the fuzz archive, or the reverse. build.rs already checks the right column, so this only weakens the regression test.
- Fix: export the flavor as a rustc-env and compare against that column.

**Nits.**
- n1: oracle.cpp:276-285. Treat `past_blocks_max == 0` like a short window (require `first ≤ 1`). The `spacing <= 0` branch (oracle.cpp:272) is unreachable: Core's `-powtargetspacing` parser rejects values below 1, and KGW never runs on devnet. Comment it as such or drop it.
- n2: oracle.cpp:914-915. The mn_payment probe is exact for both pins, but it relies on the reallocation table never yielding 750 and on the pre-reallocation sum staying at or below 500. Add a line to the O17 re-pin checklist (DEC-35 D9) to re-derive it on the next pin.
- n3: `convert_bits_to_double` and the subsidy entry points let a zero mantissa through to `65535/0.0`. The C++ standard calls that UB, but under GCC/IEC 559 it is defined as `inf`. Core's mainnet ≤ 4500 path does the same. Record in oracle.h that `inf` is the intended result, so the "UB refused" claim stays literal.
- n4: check-oracle-repro.sh:30. `nproc/2` is 0 on a 1-CPU runner; use at least 1.
- n5: archive.rs:60. Uppercase hex parses but never matches (`sha256_hex` is lowercase). Lowercase it or reject uppercase.
- n6: `ffi::v23_1::API`/`v24_0::API` are public. A caller using them directly skips `Oracle::new`'s run-time pin check (the build-time opt-in still applies). Mention in the README that consensus gates must go through `Oracle` or check `ARCHIVES_PINNED`.
- n7: build.rs:93-99. With a `-` column and `DASH_ORACLE_AUTOBUILD=1`, it announces a multi-minute rebuild that does not happen, because `cached_archive_ok` accepts unrecorded archives. Report the missing digest directly.
- n8: script.rs:462 checks only the reject-reason prefix of `check_input_scripts`. Since that is Core's own string this is fine for O1, but CS1's per-pin classification leg (§7.4 row-19 delta) has no oracle-side fixture yet.
- n9: `.github/ci-groups.yml` excludes the whole crate. `tests/archive_check.rs` needs no Core build and could run in normal CI.

O1b vector commits (2babb5575d, ac7e45359f): they only wrap existing checks with recorders, and every `BOOST_CHECK` is preserved. The scriptnum records use the test's reference `CScriptNum10`, which that test asserts equals `CScriptNum`. No issues.

The code-review skill (high effort) was one input. Its findings are folded into m2-m4 and n2/n4/n5/n7 above after I re-checked each against the code. It found no correctness issue in the shim guards, which matches my own derivation.

**Verdict: APPROVE-WITH-NITS.** No blockers, no majors. Recommend accepting the KGW `ORACLE_E_CONTEXT` restriction as safe and complete.
