# SB4 Core scenarios — independent tool review, Sol r1

**Verdict: CHANGES-REQUIRED.** No blocker; three majors and three minors. The supplied binary, archive hashes, receipt order and linker seams check out. The selected traces are reproducible samples of a Core schedule, but are not the unique traces an SB4 implementation must reproduce. Semantic coverage is incomplete.

## Scope and reference revisions

- Subject: `node/sb4-core-scenarios`, **e2fc19ad → 42737ddf**, three commits; worktree `/work/home/agent/workspace/rdc-wt/sb4-core-scenarios`. No subject source or vectors were modified.
- Specification: `SB4-dash-chain.md` §15/§15.1, DEC-128, DEC-136 and the R2-M2 counterexamples. Integration was **25617ebf** when this review began and advanced to **6a625933** (rev7) during the review. The coverage table below uses rev7, with the corresponding rev6 gaps identified. In particular, rev7 correctly separates the original A-29(ix) from the kept-mark witness A-29(x), and rewrites A-17 to match the unpersisted-mark vector.
- Core release: **v24.0.0-rc.3, da567d7dba945143eee4d2ce1621c21b32089989**. Oracle shim pin and binary `--commit`: **a9a483e51efdadeee37cf40ff8697da62b3cd012**. Core citations below are `src/` paths at that pin; the cited production logic is identical to the release tag.
- REVIEW-COMMON and PLAN §7 handling followed. No private material was read, no GitHub action was taken, and this review is uncommitted.
- Scratch evidence: `/work/scratch/sb4-core-scenarios-r1-sol/`. Tests used `RUSTC_WRAPPER=`, `nice -n 10`, at most eight jobs, and `/dev/shm/sb4-core-scenarios-r1-sol-tmp` with mode **700**. The supplied output and Core build trees were preserved.

## Major findings

### R1-M1 — The fixed trace must not become an exclusive acceptance oracle

**Severity: major.** `contrib/node/sb4-core-scenarios/harness.cpp:274`, `:298`, `:453`; `README.md:76`; design rev7 `SB4-dash-chain.md:996` (exact comparisons) and `:997` (`ScriptedV`'s exact hook/trace comparison). **Core:** `chainlock/handler.cpp:170`, `:197`, `:203`; `validation.cpp:4057`, `:4141`; `validationinterface.cpp:193`.

The listener decides whether to forward an asynchronous `UpdatedBlockTip` at its **synchronous emission**, using the emitting thread, the current tip-task flag and its own `armed` flag. It then suppresses selected callbacks when the validation queue eventually delivers them. Core instead delivers every callback and tests `tryLockChainTipScheduled` **at delivery**. That is an additional behavioral seam, despite there being exactly two linker wraps.

I built a scratch executable against the **same unmodified archives and the same two wrappers**, removing only the four-line emission-time filter (`harness-natural.cpp`; build script and binary are in scratch). For `a17-conflict-then-reload`, five runs gave **2, 4, 6, 2, 2** `EnforceBlock` boundaries for the lock, and **4, 8, 12, 4, 4** trace events. The final tip A95, best header B110, floor A100 and statuses agreed. See `natural.log` and `a17-conflict-then-reload-natural-{0..4}.json`.

This is the expected race: a header-only floor remains outside the active chain, so every enforcement revisits B96 and `MarkConflictingBlock` emits another `(A95, nullptr)` tip notification. If delivery occurs after the preceding tip task clears its flag, it schedules another pass. Core imposes no fixed finite bound on repetitions. The implementer's observed 2–81 range is plausible; my experiment independently confirms multiple outcomes, without claiming to reproduce those exact endpoints.

**Reachability ruling:** two passes are a reachable outcome for the affected scripts, and the natural-delivery probe reproduced it. Emission-time filtering does not itself prove a globally realizable delivery schedule for every future script, however. Nor does every lock get a second pass: A-16/A-22 and the first B100 lock of A-29(ix) already lie on the active chain and emit no qualifying tip notification, so they have **one** boundary; pending unknown locks have none. The literal README claim that a lock “always gets that second pass” is false even in the committed vectors.

**Fix:** identify the delivery policy in vector provenance and define the acceptance equivalence explicitly. Compare deterministic state and the required first-enforcement/ABC disconnect-connect sequence; represent subsequent idempotent enforcement notifications as allowed repetitions or a trace-language constraint. If multiple schedules change semantic state, record those allowed states rather than choosing one silently. A finite list of all complete traces is unsuitable because repetition is unbounded. A deterministic schedule-specific fixture may remain for exact replay, provided SB4 deliberately runs that same explicit schedule and it is not presented as dashd's only permitted output. Prefer controlled delivery of real callbacks over discarding them on an emission-time prediction. Correct the README's universal second-pass claim.

### R1-M2 — Row labels certify missing kept-mark and ancestor-floor coverage

**Severity: major.** `contrib/node/sb4-core-scenarios/scenarios/a29-ix-reset-then-lower-lock.json:3`; `scenarios/a21-three-branches.json:3`; `dash-oracle-sys/tests/sb4_core_scenarios.rs:19`, `:125`. **Spec rev7:** `SB4-dash-chain.md:988`, `:1030`, `:1038`. **Core:** `validation.cpp:4057`, `:4075`, `:4114`; `chain.h:299`.

At the reviewed branch head, **`a29-x-kept-mark-trailing` does not exist**. The original A-29(ix) script cannot substitute: B100 is already in the active B110 chain, so its first lock marks nothing. A81 remains status 2. After the reset and A90 lock, the node trails at A80 because A81…A95 have no block data, while the SPV predicate can select the unmarked A95. It does not test a CONFLICT mark on the floor's own ancestry. Rev6 incorrectly combined these mechanisms; rev7 correctly requests a separate (x), but this branch and gate have not added it.

I ran rev7's requested script with the supplied binary in scratch: A1…A95 connected, B81…B110 headers from A80, on/timer, B100 lock, off/timer/on, A90 lock. After B100, A81…A95 are **157**, tip A80, best header B110, floor B100. After A90, **A81 stays 157**, B81 is 130, B82…B110 remain 2, tip A80, best header B110, floor A90, observed true. `IsValid` ignores CONFLICT, so A90 does not trigger `ResetBlockFailureFlags`. This is the actual DEC-136 kept-mark witness. Evidence: `a29-x-kept-mark-trailing.json`, its `-output.json`, and `coverage-probes.log` in scratch.

There is also no exact A-21b scenario with the floor **30 below the active tip** followed by another enforcement. The case labelled A-21b in `a21-three-branches` finishes with floor A100 **equal to** tip A100 and then repeats enforcement. It establishes idempotence, but not ancestry membership below the tip/window. A-29(ix)'s B100/B110 lock partially exercises an ancestor floor, without the requested subsequent timer at depth 30.

The gate passes by collecting `vector.rows` strings; it cannot detect either substitution. Its purported complete `HARNESS_ROWS` list additionally omits **A-26**, although the two current A-26 vectors exist.

**Fix:** commit the Core-generated (x) vector, add an ancestor-floor repeat scenario (e.g. advance the active A chain to A130 after locking A100, then enforce/timer), and update the gate for A-29(x) and A-26. Validate the important preconditions in these coverage checks: a retained CONFLICT on the floor path for (x), and the requested floor/tip depth for A-21b. Retain (ix) as its distinct missing-data case.

### R1-M3 — A-1a's declared network acceptance coverage is absent

**Severity: major.** `contrib/node/sb4-core-scenarios/harness.cpp:1253`; `regen.py:31`; `dash-oracle-sys/tests/sb4_core_scenarios.rs:20`. **Spec rev7:** `SB4-dash-chain.md:994`, `:1007` (A-1a).

The scenario runner explicitly rejects every network except regtest. The six real-header jobs provide mainnet/testnet **valid** batches, a mainnet locator and benchmarks. They cannot supply the declared devnet `nMinimumDifficultyBlocks` acceptance cases, mined testnet min-difficulty candidates, or mainnet duplicate/stale-branch `bad-chainlock` acceptance cases. The presence of a regtest vector carrying `"A-1a"` makes the gate report full coverage anyway.

The regtest genesis and anchored vectors are useful, mined full-acceptance verdicts; this finding does not turn their outcomes into rule-level checks. The missing network-specific paths are nevertheless an explicit part of §15's oracle promise and remain unavailable from this tool's current interface.

**Fix:** implement and generate those named network cases, with their real or synthetic inputs and provenance, and distinguish their coverage in the gate. Alternatively obtain an explicit spec scope reduction; do not call the current row label complete coverage of the existing §15.1 requirement.

## Minor findings

### R1-m1 — Regeneration and the seam checker trust MANIFEST rather than validating it

**Severity: minor.** `contrib/node/sb4-core-scenarios/regen.py:52`, `:68`, `:84`; `check-seam.sh:46`.

My independent audit found **all 25 recorded digests correct**: 23 archives, harness source and binary. But neither regeneration nor the seam checker checks those digests before using the files. Regeneration also omits the binary digest from emitted provenance. As a concrete negative control, I copied MANIFEST to `manifest-probe/`, replaced only its binary hash with 64 zeroes, and symlinked the unchanged executable. `regen.py --check --smoke` still passed all five cases (`manifest-probe.log`). This is an unchecked attestation, not a failure of the supplied binary's current provenance.

**Fix:** verify the binary hash and `--commit`, current harness-source hash, pin and available archive hashes once before running jobs; validate archive hashes in the seam checker. Include the verified binary identity in provenance, or document and enforce an equivalent trusted-artifact identity.

### R1-m2 — Benchmark checks ignore every field except the instruction count

**Severity: minor.** `contrib/node/sb4-core-scenarios/regen.py:118`.

For `bench-*`, `compare()` reads only `vector.instructions_u`. A changed Core commit, archive/source/input hash, network, `from` or `count` is accepted if the count happens to remain within 0.5%. Thus “38/38 byte-identical” is not what this command proves: **36 files are compared byte-for-byte and two by count tolerance only**.

**Fix:** compare the whole parsed document exactly after removing only `instructions_u`, and apply tolerance solely to that field. Describe the result accurately as 36 exact vectors plus two tolerance-checked measurements. Retain the documented CPU/build-path restrictions.

### R1-m3 — The RPC replay omits a non-cryptographic height guard

**Severity: minor.** `contrib/node/sb4-core-scenarios/harness.cpp:742`. **Core:** `rpc/quorums.cpp:1366`.

The replay checks height against the current best, but skips the preceding `nBlockHeight <= 0` error. Scratch input `{"op":"cl","block":"G","height":0,"via":"submitchainlock"}` successfully installs **G@0 with floor G**; real `submitchainlock` rejects it as `invalid block height` before signature verification. See `rpc-zero-height-output.json`. None of the committed RPC cases uses a nonpositive height, so their existing state trajectories are unaffected.

**Fix:** preserve the positive-height check and label signature decoding/verification as the intentionally bypassed portion. Better, share or exercise the actual RPC path with an explicit verification seam rather than maintaining an incomplete handwritten replay.

## Audit of bypasses, initialization and scheduling

“Exactly two wraps” is true; “nothing else changes” is too broad. The complete relevant inventory is:

| Mechanism | Assessment |
|---|---|
| `TestingSetup`, disk-backed coins/block-tree, memory-backed Dash databases (`harness.cpp:341`) | Uses Core's fixture, not dashd's entire init. For current regtest args, chain parameters and default chainstate options agree: `setup_common.cpp:166-178`, `:266`; `kernel/chainstatemanager_opts.h:27`; `validation.cpp:6767`; dashd `init.cpp:1978-1981`. In-memory Evo/IS databases do not give a restart/durability oracle for those databases; the tested chains never activate DIP3. |
| Deployment overrides (`-dip3params=2000:2000`, V20/MN_RR at 2000) | Actual configurable regtest parameters, explicitly present in each scenario; coinbase-only connected chains stay below activation. The 34,200-header anchored fixture supplies no connected special-transaction blocks. This is not default-regtest consensus at height 432. |
| Chainstate argument application | Fixture constructs `Options` directly; dashd additionally calls `node::ApplyArgsManOptions` (`node/chainstatemanager_args.cpp:20`). The current scenario args change chainparams, not these options. Arbitrary future `-maxtipage`, `-minimumchainwork`, `-assumevalid`, `-checkpoints` options in `args` will not automatically be honored; reload constructs fresh default options too. The real-header mode explicitly recreates options with index checking disabled. |
| Mocktime (`harness.cpp:615`, `:780`, `:1257`, `:1179`) | Deterministic node time, also affecting sporks through adjusted time with zero peer offset. Regtest builds begin one hour before mocktime, keeping connected tips fresh and allowing Core's IBD latch to clear (`validation.cpp:1970-1992`). The observed non-IBD notifications are explained by the fixture, not a patched IBD predicate. Automatic clock advances and signed-toggle seconds are input conventions and should be treated as such. Header jobs use maximum input time + 3600. |
| Signature wrapper (`harness.cpp:86`) | Returns `Valid`, never calls the original; expressly permitted by DEC-128. These are post-verification state-machine oracles. An unmodified dashd will not literally accept the default-constructed signatures or conjure quorums on these fixtures. Reachability claims must retain this declared cryptographic exemption. |
| RPC replay (`harness.cpp:742`) | Also skips the separate chain-based verifier (`clsig.cpp:253-260`) and signature decoding. This bypass is outside the wrapped handler call and not proven by the wrap checker. For current positive-height cases, the RPC stale-height check before the handler is preserved, including the distinction from the handler's initial reset. R1-m3 identifies the omitted guard. |
| P2P receipt | Applies the live spork gate before calling the handler, matching `net_processing.cpp:5794-5795`; this is a receipt/state oracle, not a full peer/wire-processing test. No enforcement is manufactured for locks dropped while off. |
| `EnforceBlock` observation wrapper (`harness.cpp:102`) | ABI and void return match. Calls the real member exactly once and then reads under `cs_main` before ABC. The original releases its locks before returning, so the wrapper reacquires `cs_main`; it is not an atomic observation against an independently mutating writer. The parked, serialized scripts have no such writer. Allocation/observation can change timing but not the recorded single-writer boundary state. |
| Validation listener, parking and missing automatic 5-second task | Substitutes callback delivery and chooses when scheduled work runs. `Start()` is deliberately never called; `timer` runs its body manually. Mocktime does not advance the scheduler's steady clock. This defines a finite scripted schedule; it neither explores periodic/spork races nor reproduces arbitrary wall-clock dashd timing. See R1-M1. |
| `seed-cl`, `mark-conflicting`, `notify`, patched checkpoint | Deliberate state seeds/unit stimuli. `checkpoint` mutates `CChainParams`, as §15 explicitly authorizes; unmodified regtest cannot acquire that synthetic checkpoint from ordinary network input. `seed-cl` is not a receipt vector. Chosen `notify` arguments test the literal guard, rather than claiming every notification is generated by the scripted blocks. |
| Reload (`harness.cpp:891`) | Flushes disk index/coins, destroys chainstate, CL state and handler, and recreates them through Core loading. It retains the spork manager instead of round-tripping `sporks.dat`, and retains other fixture managers. This is sufficient for the current index/CL restart cases, not a general whole-node restart oracle. Spork signature persistence and Dash-DB restarts are outside these vectors. |
| Output projections | Equal-height RPC tips are sorted by hash and block-data statuses projected as the design allows. D9 reload ties are reported as a set; no committed tie vector chooses a deterministic address winner, and scenarios stop after a best-header tie. |

The genuine `ProcessNewChainLock` preserves **initial CheckActiveState → seen/hash and height checks → lookup/update → scheduled CheckActiveState/enforcement**, including the height-mismatch behavior that updates remembered best CL before declining the known-block update. The smoke vectors cover both R2-M2 receipt counterexamples. The separate `r2m2-*` seed vectors demonstrate the wrong ordering as negative controls; their states should never be interpreted as the receipt oracle.

For the present scripts, the selected one/two-pass state observations have an ordinary serial Core execution explanation once the declared verification/fixture exemptions are applied. The natural probe agreed on final state in all 20 runs across A-17, A-35, A-21 and A-36. That is evidence for these scripts, not proof that every future filtered trace is reachable or that a full unmodified dashd has only these outcomes. SB4 should match the semantic outcomes and allowable trace order/repetitions, or explicitly share the fixture's schedule.

## Seam-proof assessment

I ran `check-seam.sh` successfully. `seam.log` reports:

- Exactly **two** symbols, with the same set in `wrap-symbols.txt`, MANIFEST and the binary.
- The handler member is the sole undefined referrer across **23 archives**, and the extracted archive member matches its on-disk object.
- **69 text sections** are identical under `ld -r --wrap` for each seam. This alone says nothing about binding, but the subsequent comparison also checks **every relocation section**, retaining section, offset, relocation type, symbol name and addend. Each seam retargets **one** relocation, and every other normalized relocation is unchanged. Dropped symbol-table indices and symbol values are renumbering noise for this check; the target references are undefined imports.
- `ProcessNewChainLock` calls the verifier wrapper, and `EnforceBestChainLock` calls the enforcement wrapper.
- The original verifier has **no direct call/jump**; the original enforcement function has **one**, from its wrapper.

The `.cold` exclusion is only used to select the single hot named caller at `check-seam.sh:91-95`. It does **not** exclude cold text from the 69-section byte comparison, the relocation comparison, or the full-binary original-target call/jump scan. It is sound for this object/binary; there is no cold-path bypass hidden by that exclusion.

Limits: `nm`'s undefined-referrer check cannot by itself exclude same-object references to a defined symbol, and a direct call/jump scan cannot establish the absence of function-pointer/dynamic lookup paths. Source inspection finds no such alternative in the present call graph: the wrapped verifier definition at `clsig.cpp:263` only delegates to recovered-signature verification, and `EnforceBlock` is invoked from the handler. Neither caller is localized or inlined across the relevant object boundary. The proof is good evidence for **these two bindings in this binary**, not proof of fixture equivalence, callback fidelity, original-source identity, or “the only changes” in the broader oracle.

## §15.1 coverage

“Covered” below means the requested **Core portion** exists. Rust-only windowing, SPV publication, persistence transactions and `ScriptedV` mutex checks cannot be judged by this C++ tool. Schedule-dependent exact trace matching remains subject to R1-M1 even for a covered case. “Uncovered” includes rows with only partial required coverage.

| Row naming the harness | Verdict | Evidence / gap |
|---|---|---|
| A-1a | **Uncovered (partial)** | `a1a-pnbh-rules-genesis`, `smoke-pnbh`, real-header PNBH jobs; no devnet, mined testnet boundary or mainnet stale-lock cases (R1-M3). |
| A-2 | Covered | `a2-pnbh-rules-anchored`: 34,200-header prefix and newly mined contextual/full-acceptance controls above the anchor threshold. |
| A-16 | Covered | `a16-cl-on-active-heavier-header`: active A1000 lock retains best B1005 and locator0 B1005. |
| A-17 | Covered **as revised in rev7** | B96 130 → 2 on reload, best B110. Rev6's persisted-CONFLICT best-header case was not supplied; a separate positive persistence probe is described below. |
| A-18 | Covered | `a18-invalidate-walkback`, `a18-invalid-chain-found`, plus CL boundary vectors for MarkConflictingBlock; reconsider does not restore best header. |
| A-19 | Covered | `a19-first-seen-tie`: acceptance keeps first greater/equal-work choice; reload reports C101/D101 address tie without asserting a winner. |
| A-20 | Covered | A-16/A-17/A-21 supply statuses; SPV target comparison is the separate A-27 predicate, not Core's candidate set. |
| A-21 | Covered | `a21-three-branches`: first boundary tip/best A80, B81/C81 marked, B82…B110 disconnected/marked, C82…C105 unflagged. |
| A-21a | Covered | Same vector: post-ABC tip A100, best A80; subsequent enforce/timer have no disconnect/notification. SPV same-step publication is Rust-owned. |
| A-21b | **Uncovered (partial)** | Repeat is present, but floor equals tip; the requested floor 30 below tip + repeat is absent (R1-M2). |
| A-22 | Covered | `a22-lock-on-active-heavier-below`: no marks, best B110. |
| A-24 | Covered | `a24-checkpoint`: patched checkpoint rejects the deep fork; reload with checkpoints disabled accepts it. Declared parameter seam. |
| A-25 | Covered as a measurement | Both `bench-pnbh-mainnet-*` jobs; counts use tolerance, not byte equality, and do not themselves prove SB4's performance bound. |
| A-26 | Covered | `a26-i-lock-on-header-only`, `a26-ii-enforcement-reorg`: first boundary and post-ABC trace. Gate's required-row list omits A-26. |
| A-28 | Covered | Known FAILED/CONFLICT/both, precedence, immediate/lazy invalid ancestry, batch-prefix failure, unknown/mismatched/late locks across genesis/index-state/invalid-chain vectors. |
| A-29(i) | Covered | Both RPC-recorded-off and P2P-dropped-off scenarios; lower post-reset lock accepted. |
| A-29(ii) | Covered | Late headers before/after observed transition; RPC supplies the off-state pending lock. |
| A-29(iii) | Covered | `a29-iii-on-off`: live off disables new CL conflict checks/enforcement while stored marks retain their effect. |
| A-29(iv), node half | Covered | `a29-iv-restart`: no CL and observed false after reload, followed by timer. SPV restoration has no harness counterpart. |
| A-29(v) | Covered | `a29-v-marks-survive`: existing B marks survive off/on and floor reset. |
| A-29(vii) | Covered | Fresh first lock smoke: initial empty reset, A100 installed and kept. |
| A-29(viii) | Covered | `a29-viii-notify`: non-IBD `(tip,nullptr)` schedules; IBD and `new==fork` do not. |
| A-29(ix), rev7 | Covered | Both checked-off reset and no-observed-off stale-lock controls. Original rev6's kept-mark assertion is not covered by these. |
| A-29(x), rev7 | **Uncovered** | No scenario/vector in this branch; reproduced successfully in reviewer scratch (R1-M2). |
| A-30 | Covered, Core statuses | Same A-21 index statuses; cold storage, extensions and reopen behavior belong to the Rust test. |
| A-35 | Covered, Core statuses | `a35-resident-to-cold`: B101…B110 marks, tip A100 below header-only floor A120. Residency/commit failure belongs to Rust. |
| A-36 | Covered, selected trace | First enforcement has `(B110,null)` for C90, B110…B81 disconnects, then `(A80,null)`; full output also records ABC. |
| A-38 | Covered | `a38-deep-fork-time-too-old`: deep fork, `mtp+0` rejection and `mtp+1` acceptance. Cold/resident parity and derived time-max checks belong to Rust. |

A-29(vi) and the SPV half of (iv) are expressly non-harness checks. Additional §15 mode/control artifacts exist for codec (`smoke-diskindex`), locator (`locator-mainnet`) and R2-M2's seed-order counterexamples. Their existence does not fill the missing rows above.

## Verification of the claimed Core quirks

| Claim | Core basis and result |
|---|---|
| Original A-29(ix): A81 is never marked | **Confirmed for that script.** `EnforceBlock` only scans while the floor is outside the active chain (`validation.cpp:4057`). B100 is an ancestor of active B110. The vector correctly retains A81=2. The later A90 lock disconnects B to A80; the absence of A blocks explains the trailing node tip. The kept-mark mechanism is a distinct reachable case, now rev7 (x). |
| CONFLICT marks are unpersisted | **Needs qualification.** `MarkConflictingBlock`'s own writes at `validation.cpp:4114-4122` do not dirty the entries. If the entries were already dirty or another path dirties them, the marks can persist. Candidate conflict propagation explicitly dirties entries (`:3558-3561`); rejected new CL-conflicting headers are inserted by `AcceptBlockHeader`/`AddToBlockIndex` and dirtied. The A-17 vector proves only its particular B96 mark disappears. |
| Best header walks back only on identity, not every conflicting descendant | **Confirmed.** `validation.cpp:4089-4090` checks the marked entry itself; `:4107-4108` checks each disconnected tip. Header-only B110 remains best when only B96 is marked. `ConflictingChainFound` (`:2058-2073`) does not recompute it. |
| Best header remains at the fork after the enforcement reorg | **Confirmed.** `a26-ii-enforcement-reorg` leaves best A990 while ABC reconnects through A1000. The header entries already exist, so no fresh `AddToBlockIndex` strict-work update runs (`node/blockstorage.cpp:111-115`). A-21 similarly ends at best A80 with tip A100. |
| Reconsider does not restore best header | **Confirmed.** `ResetBlockFailureFlags` (`validation.cpp:4152-4230`) clears flags/updates candidates without recomputing best header. `a18-invalidate-walkback` returns tip A100 but keeps best A98 after reconsidering A99/B105. `InvalidChainFound` separately moves an affected best header to the active tip (`:2043-2044`). |
| Reload may pick a persisted CONFLICT header | **Confirmed by source and an additional positive probe.** Load uses `IsValid(BLOCK_VALID_TREE)` and the work comparator (`validation.cpp:5359`); `IsValid` excludes FAILED, not CONFLICT (`chain.h:305-307`). Scratch probe: active A80, A81…A130/B81…B109 headers; lock A130; a new B110 is rejected `bad-chainlock` and persisted with status 130; off/timer; connect independent D81…D95; invalidate A110; reload. Tip stays D95, best becomes **B110 and B110 remains 130**. See `a17-persisted-conflict-best-output.json`. This refutes an absolute “CONFLICT is never persisted” claim while agreeing with rev7's narrowed A-17. |
| Recorded while off is reachable only via RPC receipt | **Confirmed for ordinary receipt routes, after successful verification.** P2P has the live gate at `net_processing.cpp:5794-5795`; coinbase receipt is also gated (`chainlock/handler.cpp:107`); RPC `quorums.cpp:1397` has no spork gate. The handler/update and late-header methods themselves have no such gate (`chainlock/chainlock.cpp:106`, `:152`). Direct seeds/internal calls are fixture operations, not another public receipt path. The real RPC can still fail for absent quorum/signature; bypassing that is material. |

## Commands, determinism and provenance results

1. `env TMPDIR=/dev/shm/sb4-core-scenarios-r1-sol-tmp RUSTC_WRAPPER= nice -n 10 python3 contrib/node/sb4-core-scenarios/regen.py --check --bin /work/scratch/sb4-core-scenarios/out --jobs 8` — **exit 0, 38/38 checks passed** (`regen.log`). Thirty-two scenario vectors plus four non-benchmark real-header vectors matched byte-for-byte. The two benchmark vectors passed the documented 0.5% count test; this run does not prove their complete documents byte-identical.
2. Same environment/niceness, `contrib/node/sb4-core-scenarios/check-seam.sh /work/scratch/sb4-core-scenarios/out` — **exit 0**, evidence summarized above (`seam.log`).
3. Independent SHA-256 audit — all **23 archives**, current `harness.cpp` and binary match MANIFEST. Source digest **2b8f41735cc8ff5639d6854380327aa65afb75f99e10ae711b74e44cbda17590**; binary digest **31dc7fa16b22b24ba4d7719f77a9c6aa27fa21c8b8b749a755a085cbdb295d15**. Binary `--commit` matches the pin. Configure flags in MANIFEST match the archived build's configuration.
4. Audited **3,757 tracked Core `src/` files** against the pin. The sole textual `git show` difference is `clientversion.cpp`'s expected `export-subst`; it matches **`git archive` exactly**. Pin-to-release diff consists of oracle build additions and three unused contextual-check trampolines appended to `validation.cpp`; production routines are unchanged. The harness does not link the oracle shim archive.
5. `env -u SB4_CORE_SCENARIOS_BIN TMPDIR=/dev/shm/sb4-core-scenarios-r1-sol-tmp RUSTC_WRAPPER= CARGO_BUILD_JOBS=8 nice -n 10 cargo test -j8 -p dash-oracle-sys --test sb4_core_scenarios -- --nocapture` — **2 passed**; smoke prints the intended skip and returns cleanly (`gate-absent.log`). This checks the unconfigured/absent harness case, without moving or deleting the supplied binary.
6. Same test with `SB4_CORE_SCENARIOS_BIN=/work/scratch/sb4-core-scenarios/out` — **2 passed**, including all five byte-identical smoke regenerations (`gate-present.log`).
7. Scratch natural-delivery build/runs, added coverage/persistence probes, and invalid MANIFEST negative control — locally built and run, with artifacts retained in reviewer scratch. No changes were linked over the supplied binary or archived Core objects.

The current committed vectors are deterministic under their prescribed fixture and delivery policy. Cryptographic success, patched checkpoint parameters, seeded pre-states and the chosen scheduling policy remain explicit assumptions; neither regeneration nor seam checks turns them into literal full-dashd executions. Resolve the trace contract and missing coverage before treating this tool's entire output as SB4's acceptance oracle.

**Final verdict: CHANGES-REQUIRED.**
