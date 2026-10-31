# SB4 Core scenarios — Sol r2 confirmation

**Verdict: APPROVE.** All three r1 majors and all three r1 minors are resolved for the supplied corpus under DEC-147. No new blocker, major, minor or nit finding. Two numerical claims in the review brief need correction: this revision checks **28**, rather than 26, SPORK_19 scenarios, and the new headers-only x′ case reaches **11** enforcement passes under the sampled schedules. All schedules still agree on state and the required trace.

## Scope and method

- Reviewed `node/sb4-core-scenarios`, **42737ddf → 9b0223e3**, at worktree `/work/home/agent/workspace/rdc-wt/sb4-core-scenarios`; HEAD remained `9b0223e359eb0b52c5511a4c4d8c98a278925e05`.
- Read REVIEW-COMMON, the integration r1 report, the program context, SB4 §15/§15.1 and DEC-147. Applied the code-review-validator passes. PLAN §7 handling was codenames only; no private material was read, and no GitHub action was taken.
- Reference: Core **v24.0.0-rc.3**, `da567d7dba945143eee4d2ce1621c21b32089989`; oracle pin **a9a483e51efdadeee37cf40ff8697da62b3cd012**. The eleven reviewed Core source files in the supplied build tree equal their pinned blobs. Pin-to-release production code in the relevant paths is unchanged; the appended contextual-check trampolines are unused here.
- Used the supplied `/work/scratch/sb4-core-scenarios/out` binary. Subject sources and vectors were unchanged. Experiments, logs and scratch executables are retained in `/work/scratch/sb4-core-scenarios-r2-sol/`. Tests and scratch builds used `RUSTC_WRAPPER=`, `nice -n 10`, and `TMPDIR=/dev/shm/sb4-core-scenarios-r2-sol-tmp`, mode **700**. The temporary directory was removed after testing. This report is uncommitted.

## Findings by severity

**Blocker: none. Major: none. Minor: none. Nit: none.** The count corrections above concern the brief's summary, not a false assertion in the revised implementation. Enforcement counts are explicitly outside DEC-147's acceptance equivalence.

## R1-M1 — real callbacks and schedule equivalence: resolved

`contrib/node/sb4-core-scenarios/harness.cpp:266`, `:302`, `:318`, `:549`, `:571`, `:626`:

The emission-time prediction, `armed` filter and access to `tryLockChainTipScheduled` are gone. Core's registered validation listener captures **every queued callback that the handler overrides**: `UpdatedBlockTip`, `BlockConnected` and `TransactionAddedToMempool`. Their original arguments are retained, callbacks enter one FIFO inbox, and delivery calls the handler's real methods. `AcceptedBlockHeader` remains synchronous and goes straight through, matching `validationinterface.cpp:269`. `BlockDisconnected` is recorded but is not forwarded because the handler does not override it. The only null-handler exclusion is during setup/reload/teardown, outside an installed handler's scenario lifetime; reload first drains the old inbox.

The real handler's compare-exchange still decides at **delivery** (`chainlock/handler.cpp:197`). `boundary-flush` delivers the complete waiting inbox at each boundary and idle pump. `random:SEED` delivers prefixes in the same queue order, with progress when idle and a flush after the configured pass cap. Successful drains require both an empty scheduler and an empty inbox. Provenance records the actual policy; regeneration checks that the executable reports the requested one.

Independent accounting instrumentation, linked against the same untouched archives and two seams, exercised A-17, A-36, A-21, x and x′ under boundary-flush/random delivery. All **1,119 captured callbacks were delivered**, no inbox remained pending, and every probe matched the committed semantic projection. See `run-probes.log` and the `*-inbox` artifacts.

Repeated passes are bracketed, numbered per lock per drain, and identified by `trace_repeat_spans` (`harness.cpp:1225`). `regen.py:172` removes only those spans and repeated boundaries: the first enforcement boundary, its ABC disconnect/connect order, other trace events, operation results and state remain compared. `StateSnapshot`/`ClosePass` (`harness.cpp:596`, `:609`) reject changes to tip, best header, floor/known lock, failed-block membership or any status. A scratch second-pass status mutation was independently rejected with `EnforceBlock(A100) #2, a repeated pass, changed the state`; it was not accepted as another removable trace span.

**Random-state question:** all **224 random runs** (28 scenarios × eight seeds) matched boundary-flush under that comparison. Positive per-drain pass counts range from **1 to 11**; x′ supplies the larger range. A-21b's logged `30` is thirty separately drained block-connect stimuli, each with pass #1, not thirty repeats of one enforcement.

The missing delivery point between ABC and the tip-task flag clear is expressly documented at `README.md:156`. Dashd can deliver those callbacks while the flag remains set and omit the extra pass the harness schedules. That omission preserves these cases' state: later passes are idempotent, and callbacks arriving mid-reorg can only queue a later task or meet the already-set flag. Coinbase-only `BlockConnected` callbacks introduce no transaction-map difference. The relevant Core paths support this explanation (`handler.cpp:187-205`, `:254-310`; `validation.cpp:3714`, `:4057-4142`). This confirms the present scripted state oracles, not all possible interleavings with concurrent receipts, spork changes or future non-coinbase fixtures. Eight seeds are sampled evidence, not exhaustive scheduling proof.

## R1-M2 — missing rows and semantic gates: resolved

- **A-29(x):** Core produces A81…A95=157 after the B100 lock, then retains those marks after the observed off/on transition and A90 lock. B81 becomes 130; tip remains A80, best header B110 and floor A90. This is the kept-mark witness, distinct from the original (ix) missing-data case.
- **A-29(x′):** with A81…A95 header-only, A81 becomes and remains 130; after A90, B81 is also 130, tip A80 and best header B110. Since header-only A90 is below transaction validity, Core runs `ResetBlockFailureFlags(A90, false)`; that call preserves CONFLICT (`validation.cpp:4075`, `:4152-4228`; `validation.h:731`). The connected (x) case needs no such reset. Both vectors reflect Core's actual paths.
- **A-21b:** A100 is locked before growth to A130; the following direct enforcement and timer keep floor A100, tip/best A130, and all statuses unchanged.
- The gate's `has_kept_mark_on_floor_path` and `has_ancestor_floor_repeat` check those state preconditions (`dash-oracle-sys/tests/sb4_core_scenarios.rs:166`, `:182`). Removing the kept mark from either vector, changing the depth to 29, or removing the later enforcement boundary makes the copied gate fail at the corresponding assertion. A-26 is required in `HARNESS_ROWS`; removing both A-26 row declarations also fails. See `negative-controls.log` and the individual failure logs.

## R1-M3 — network acceptance paths: resolved

The candidates are mined with genuine Core PoW and passed through `ProcessNewBlockHeaders`, rather than judged solely by a contextual-rule helper.

| Case | Independently confirmed Core path and outcome |
|---|---|
| Devnet window | `-minimumdifficultyblocks=1152` makes parent 1151 satisfy the strict `<` window test (`pow.cpp:177`): B1152 at `207fffff` is accepted. With 1151 the test fails, height 1152 is a BTC retarget, B1152 is `bad-diffbits`, and C1152 at Core's computed `1f00c1ee` is accepted. Both cases reject X1 as `devnet-genesis`. |
| Testnet prefix | **5,001** genuine headers, genesis through R5000; the prefix SHA-256 matches provenance. At R3500, parent bits are `1e0ffffc`. T3501 at +301 s/powLimit is accepted; U3501 at exactly +300 s/powLimit is `bad-diffbits`; V3501 at +300 s/`1e0ffffc` is accepted. W3502 at +300 s/powLimit is rejected, while X3502 uses the last non-special bits and is accepted (`pow.cpp:142-156`). |
| Testnet later boundary | At height 5001, +7201 s allows powLimit; exactly +7200 s does not and rejects powLimit. The latter takes the four-spacing, 10× target relaxation branch, below powLimit (`pow.cpp:191-204`). This exercises the minimum-difficulty threshold in the DGW era, not a DGW retarget calculation. |
| Mainnet stale branch | **301** genuine headers, genesis through R300, plus seeded floor R250. Mined S201 is `bad-chainlock` and stored as status 130; resubmission is **`duplicate`**, with `BLOCK_CHAINLOCK` result class. S202 is `bad-prevblk-chainlock`; L250 is `bad-chainlock`; F261 is accepted. These are the separate cache, parent-conflict and floor checks at `validation.cpp:4616`, `:4644`, `:4691`. |

The brief's statement that the duplicate gets reject reason `bad-chainlock` is inaccurate: Core returns `duplicate` for a cached conflicting header. The committed vector and gate correctly preserve that distinction. Network labels and required verdicts are checked by the gate; wrong-network and substituted-reason negative controls fail.

The mainnet floor is an explicitly allowed **seed**, not a claimed receipt. As an additional fixture check, I replaced `seed-cl` with the real `ProcessNewChainLock` receipt path through `cl`, retaining the declared signature seam. All five subsequent header results, statuses, tip, best header and floor were identical. The observed flag correctly became true. See `mainnet-receipt-probe.log` and its scenario/vector.

## Fixture deviations and remaining r1 minors

The declared fixture choices do not change these verdicts:

- `ScenarioSetup` follows the wallet-disabled `TestingSetup` initialization, replacing its unconditional index checker with dashd's `DefaultConsistencyChecks()` (`harness.cpp:358`; Core `validation.cpp:6768`). This is the daemon's default off regtest, and the explicit disable survives reload. It removes fixture assertions, not acceptance checks. Current scenario arguments affect chain parameters; arbitrary future chainstate-option arguments still require separate application.
- Devnet's recorded `-highsubsidyblocks=500` and `-highsubsidyfactor=10` are real Core devnet configuration inputs. They keep the declared block-1 load valid and do not change the minimum-difficulty branch being exercised; subsequent synthetic devnet blocks are submitted as headers. I verified their Core interpretation, not a separate dashmate checkout.
- The non-regtest harness spork key replaces the signer, not the spork-value logic. Mainnet's `GetSporkValue` is hardened to 0 for SPORK_19 (`spork.cpp:184`); its verdicts therefore do not depend on the harness message. Testnet's difficulty case performs no spork operation.
- Signature success remains the declared cryptographic exemption. RPC signature decoding/the separate verifier, whole-node restart persistence, automatic timers and arbitrary concurrent node activity remain outside this oracle. None is newly presented as tested.

**R1-m1:** binary/source/pin and available archive hashes are checked before regeneration, binary identity is recorded, and seam step 0 verifies the binary and all **23** archives. Corrupt binary/archive MANIFEST digests make **both CLIs exit 1**. Source and commit corruption are also rejected by `verify_build`. Actual source SHA-256: `a42db9c798fc8320cb75deaf02553999ce3ce0ee06e6b1d3894acbdac1bedc55`; binary SHA-256: `45f069dc7b5714d34c1ed006f377ea72945eeb4b2e41e6d43a11cd85b02d5944`.

**R1-m2:** benchmark comparison removes only `instructions_u`, compares the remainder exactly, then applies 0.5% count tolerance. A within-tolerance count change is allowed; altered commit or batch-count metadata is rejected. The reported **44 exact + 2 tolerance-checked** is accurate.

**R1-m3:** the RPC replay now rejects height ≤0 before the best-height check (`harness.cpp:960`), matching `rpc/quorums.cpp:1366`. The committed negative/zero-height controls return `invalid block height`, leave the floor empty, and a positive-height control installs A5.

**Per-block headers:** all **126 build steps** contain one 80-byte header per constructed block, **42,508 headers** total. The gate decodes and X11-hashes every header, checks listed hashes and every predecessor link whose parent is another built block (`sb4_core_scenarios.rs:236`). Genesis/devnet-genesis/preloaded parent edges rely on the verified Core generation and preload inputs. Missing-header, altered-X11-hash and altered-predecessor controls all fail. The long-run endpoint hash projection remains intentional; the full headers are available for replay.

## Required executions and evidence

All commands below exited **0**, with the environment/niceness stated above:

1. `python3 contrib/node/sb4-core-scenarios/regen.py --check --bin /work/scratch/sb4-core-scenarios/out --jobs 8` — **44 exact + 2 tolerance-checked**, and **28 × 8** matching random schedules (`regen.log`).
2. `contrib/node/sb4-core-scenarios/check-seam.sh /work/scratch/sb4-core-scenarios/out` — two wrappers, 23 verified archives, the sole handler referrer, 69 byte-identical text sections per seam, one retargeted relocation per seam with every other relocation unchanged, the expected wrapper calls and no outside direct call to either real target (`seam.log`).
3. `SB4_CORE_SCENARIOS_BIN=/work/scratch/sb4-core-scenarios/out CARGO_BUILD_JOBS=8 cargo test -j8 -p dash-oracle-sys --test sb4_core_scenarios -- --nocapture` — **2 passed**, including five exact smoke regenerations and their random schedules (`gate.log`).

The scratch gate used the reviewed test source with only `repo()` redirected to copied fixture data. Its positive baseline passed before the corruptions were tested. Both C++ probes were locally compiled and linked without modifying the supplied binary, shared archives or branch source. Negative controls and the additional receipt probe passed as described above.

**Final verdict: APPROVE.** The current corpus is suitable for SB4's DEC-147 state-oracle comparison. No required r1 correction remains open.

**Sol gate satisfied: yes**
