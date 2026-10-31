# AB-O5-s calibration review

Verdict: **CHANGES-REQUIRED**.

Reviewed only `node/ab-o5-s` at `660315fd61921abcc428a5776d2cc8d68ec90ad1`, against base `76391f21`, in a detached scratch worktree. No other arm was inspected. The implementer's worktree was not modified; this report is uncommitted.

Commits:

- `5ee8cbe5` — docs(node-program): add the quirk register (O5)
- `638ddb45` — test: add one named test per register quirk (O5)
- `660315fd` — chore(node): check that the quirk register and its tests agree (O5)

## Findings

### F1 — major: source-text matches do not establish that registered tests exist in the compiled suite

`contrib/node/check-quirks.py:42` (particularly lines 51–64 and 148–157).

`find_tests` scans lines without Rust lexical, module, target or conditional-compilation context. I copied the register and its referenced test files into a scratch fixture and applied each mutation independently to `conformance/tests/quirks.rs`:

1. Surround the entire file with `/*` and `*/` on separate lines.
2. Prepend `#![cfg(any())]`.
3. Place its items in `#[cfg(any())] mod disabled { ... }` (remove the inner documentation comments for this experiment).
4. Prepend `#[cfg(feature = "not_default")]` to every test, leaving that feature disabled.

For **all four**, the checker exits **0** and still reports `38 registered ... ignored count 26`. Compiling the target with `rustc --edition=2024 --test` and running the resulting binary with `--list` reports **0 tests**, versus **19** before each mutation. Thus a complete target can disappear without changing the reported agreement or ignored count.

I also declared `not_default = []` in a minimal Cargo package and kept it out of default features: `cargo test -- --list` gives **0 tests**, `cargo test --all-features -- --list` gives **19 tests**, and the checker still reports agreement. This is valid declared-feature configuration, not an unknown-cfg compiler-warning trick.

A separate Cargo experiment gives the same false green: a minimal test package containing the unchanged conformance file, with `[[test]] required-features = ["slow"]` and no default `slow` feature, has an empty default `cargo test -- --list` inventory and checker exit 0. This confirms the Cargo target-membership blind spot in addition to item-level cfg.

The same scanner also misses conditional ignore attributes: insert `#[cfg_attr(all(), ignore = "quirk 22: owner BS11a-t")]` immediately before the existing implemented `quirk_22_...` function. The unchanged register and checker still pass. In a separately compiled standalone pending target, changing quirk 13 to implemented and replacing its ordinary ignore attribute with `cfg_attr(all(), ignore = ...)` passes the checker while `--list --ignored quirk_13` proves that Rust ignores it.

**Fix:** validate the compiled test inventory and effective ignored status for the required/default configuration, including Cargo target/required-feature membership; use proper Rust token/AST handling for source association and reasons. A smaller conservative solution may reject cfg/cfg_attr on registered tests, their enclosing modules and targets, and correctly exclude comments, but must also cover crate-level cfg and Cargo target configuration. Add regression selftests for all these cases. Keep owner-reason and duplicate/unregistered checks.

### F2 — major: already registered quirks 39–42 are omitted and forbidden by the checker

`docs/node-program/quirks.toml:21`, `docs/node-program/quirks.toml:25`, and `contrib/node/check-quirks.py:102`.

The register says quirks 39+ are outside its scope until their owners land tests, and the checker hard-codes 38. However DEC-25(4) **registered 39** (malformed-tail sighash serialization, CS1), DEC-29 D2 **registered 40** (KGW floating-point variation, CS2), and DEC-50(4) **registered 41 and 42** (CHAMP order, EV2a; in-block order-dependent exceptions, EV2b). A1 §4.1 principle 5 explicitly requires later discoveries to be appended. These decisions existed before this arm's base. Deferred implementation is exactly what the pending scaffolding is for. Quirk 40 even already has `dash-consensus/tests/quirk_40_kgw_fp_platform_invariance.rs`, with five behavioural tests, and its release gate runs via `check-kgw-fp.sh`; it only lacks a registered named wrapper/reference.

The literal 38 in AB-O5/PLAN is stale relative to those decisions and to this review's explicit scope probe. The implementation satisfies that literal initial count, but does not reconcile the binding additions. Adding a `quirk_39_...` test is rejected as unregistered; raising the register count to 42 is also rejected by the hard-coded count rule.

**Fix:** register all four with their owners, Core anchors and named tests/references or pending stubs; update the completeness check and relevant documentation/selftests. Preserve 1–38. Record the updated counts rather than declaring 39–42 out of scope.

### F3 — minor: several new descriptions give future owners incorrect Core expectations

`dash-consensus/tests/quirks_pending.rs:13`, plus the register titles at `docs/node-program/quirks.toml:104`, `:159`, `:297`, `:334`, `:361`, and `conformance/tests/quirks.rs:46`.

These are scaffolding/documentation defects, not changes to runtime consensus code:

- **2:** the stub says AssetUnlock is exempt from empty outputs alongside QcTx/MNHF. Core `consensus/tx_check.cpp:18–30` instead exempts AssetUnlock's **inputs only**. Empty outputs still return `bad-txns-vout-empty`. Correct the comment to distinguish the two sets. The register's general title is acceptable.
- **6:** `quirks_pending.rs:32` says an out-of-range input index returns one. At the v24 pin, `script/interpreter.cpp:1514` asserts `nIn < vin.size()`; only an existing input without a matching SINGLE output returns one. Remove the obsolete input-index assertion from the description.
- **9:** “within half of the expected difficulty” uses the wrong denominator. Core `validation.cpp:4479–4482` accepts `abs(n1-n2) <= n1*0.5`, where `n1` is the submitted header's difficulty. For example expected 100/submitted 60 is within half of expected but fails Core; expected 100/submitted 180 fails that prose rule but passes Core. State the actual comparison.
- **15:** Core `governance/superblock.cpp:156` passes **compact nBits = 1**, not difficulty 1, when minimum difficulty is disallowed. These are different values. State the compact-bits rule and its `fPowAllowMinDifficultyBlocks` condition.
- **17:** omitting the version field from the hash does not mean arbitrary entries differing only in version hash equally. Core `evo/simplifiedmns.h:78–92` still uses the version to select serialized fields (e.g. v2 adds `nType`). Qualify the proposed fixture or assert the serialization rule directly.
- **30:** bits 58–62 is the 32-active-quorum case only. Core `llmq/quorumsman.cpp:739–743` uses `n=log2(count)` and bits `(63-n)..62`; four active quorums use 61–62. Keep the formula already present in the stub and qualify the title.
- **34:** the title says these pins are outside chain parameters, but the superblock hash is populated in `chainparams.cpp:172` and read through `GetConsensus()` at the registered anchor. Describe network pins without that blanket claim.
- **37:** serialization under another scheme loses unresolved raw bytes, while a successful earlier `Get()` permits valid conversion. The arm's own lines 129–133 demonstrate the counterexample to its unconditional title. Add that condition.

**Fix:** correct these descriptions against v24, so future tasks do not turn inaccurate prose into acceptance tests. No change to the already working quirk 9/15 implementations is requested.

### F4 — minor: some Core anchors point at a different operation, including the opposite null-commitment result

`docs/node-program/quirks.toml:197` (quirk 19 core field) and `docs/node-program/quirks.toml:344` (quirk 35 core field).

Quirk 19 anchors `llmq/blockprocessor.cpp:124`, the **P2P** path which assigns misbehaviour 100 and rejects a null commitment. Its registered behaviour is the **block** path: `ProcessCommitment`, lines 325–380, accepts a valid null commitment after the type/hash checks, with `VerifyNull` at 369–379. The present anchor sends an implementer to the opposite result.

Quirk 35 anchors `bls/bls.h:100`, `SetBytes`, which catches decode failures and resets. The four-step eager decode and throwing malleability outcome occur in **Unserialize**, lines 183–201. Quirk 22's opposite-scheme fallback also deserves this latter anchor; quirk 20's global-scheme commitment hash is in `llmq/commitment.cpp:312–322`, rather than the per-version serialization shown at its header anchor.

The optional anchor check passes all present entries and also passes changing quirk 19's anchor to `pow.cpp:1`. Its implementation honestly checks only syntax/file existence/line count; that does not validate semantic location and is not independently reported as another checker bug.

**Fix:** use the actual behavioural anchors (and optional additional anchors for compound quirks); retain the lightweight existence check without treating it as semantic validation.

### F5 — nit: strengthen the named quirk 37 test with the direct failed-Get corpus row

`dash-consensus/tests/quirks_bls.rs:114`.

The test covers the explicit-other-scheme serialization trap and successful Get followed by conversion. Its sole failed Get comes **after serialization has already overwritten the buffer with zeros**, and no serialization follows that Get. Thus it does not test A1's named quirk “lazy re-serialization after a failed Get()”. A regression that fails to discard nonzero raw bytes on a failed Get, leaving subsequent same-scheme serialization unchanged, can pass every assertion in this test.

The pinned Core-produced corpus already supplies the precise missing row: `pk.lazy`, class `sub_000`, `unser:B,get,ser:B` → `I,zero`, using the same input already copied here. The base-sdk test `failed_get_zeroes_the_lazy_object` checks this directly. The file-level reference to that existing suite is allowed by the brief, so this is an optional test-strengthening nit rather than a failed acceptance item. Dependency test suites are not run by rust-dashcore's workspace test command, and pinning the direct row locally would make the named test more useful.

**Suggested improvement:** add this row to the one named quirk 37 test (raw serialization equals input before Get; failed Get; serialization equals zeros afterwards), or identify `failed_get_zeroes_the_lazy_object` explicitly in the existing reference.

## Acceptance and adversarial evidence

- TOML parses and has ids 1–38 exactly once: 11 implemented, 26 pending, 1 private, 0 dropped. Existing named tests plus the additions match that initial register. No non-private entry among 1–38 was identified as v23-only, so no DEC-82 drop is required for that set.
- Pending stubs are split between seven dash-consensus tests and nineteen conformance tests; all have the exact owner reason and `unimplemented!` bodies. Un-ignoring them does not silently produce a passing test.
- Three new implemented BLS tests use the pinned base-sdk corpus. I checked `sub_000`, `sub_101`, `tors_11`, `tors_3`, zero and the lazy scripts against `481f8c351c4f00dfad08a11063d2852e74e06016:pkgs/pkc/corpus/bls_wrapper.json5`; the copied bytes/outcomes agree. Provenance names Core-built probes and the deterministic generator. No invented vectors were found.
- The checker is wired into `selftest-checks.sh`; PROCESS documents its direct invocation and ignored-count output. There are 17 added selftest cases. The full gate copy invokes the selftest because this arm changes contrib/node.
- Direct baseline `DASH_CORE_REPO=~/workspace/dash python3 -I contrib/node/check-quirks.py` passes with the claimed summary. This establishes structural agreement, not compiled coverage.
- Missing id 13: rejected. Wrong ignore owner: rejected. Wrong registered test file: rejected. Owner changed consistently in register and tests: accepted (the register is the owner authority). Semantically unrelated but valid Core anchor: accepted even with DASH_CORE_REPO. All four test-disappearance mutations and cfg_attr ignore: accepted, as detailed in F1.
- Register/private-gate references stay at the permitted PLAN §7 level. I did not inspect private vectors or either private report. Gate evidence records receipt counts only.

## Core audit

Reference: `git -C ~/workspace/dash show v24.0.0-rc.3:src/...`; the annotated tag peels to Core commit `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`. I inspected every non-private title and primary anchor (2–38), with additional behavioural locations for compound entries, exceeding the required ten spot checks. No live oracle was needed for this scaffolding review.

- 2–7: tx_check exemptions/coinbase lengths; validation size comparison; interpreter CHECKDATASIG/NUM2BIN/SINGLE and FindAndDelete; pubkey lax DER/normalization. Corrections in F3.
- 8–12, 15: pow.cpp's 24-block/23-interval average and truncation; historical difficulty comparison; BIP34 byte prefix; subsidy truncation/decline/treasury; separately truncated MN schedule and cycle rounding; superblock nBits choice. Corrections in F3.
- 13–14, 16–18: deterministic MN payee selection; old-budget NoCheck path; CbTx activation; SML hashing serialization; quorum-root helper and `GetMinedAndActiveCommitmentsUntilBlock`'s configured-type iteration. Quirk 17 fixture qualification in F3.
- 19–27: block versus P2P null commitment paths; commitment version/minSize and hash serialization; global ProTx verification; BLS wrapper SetBytes/Unserialize; serialized unique-property hash; banned-MN punishment; fixed rebuild phases/replacement/previous-view collateral; network-derived sign string. Anchor corrections in F4.
- 28–34: rotation skip/removal/exception paths; work-block modifier; signer-bit extraction; MNHF request id/activation and quorum lookup; versionbits floor and int multiplication; coinstats precedence; validation/chainparams pins. Corrections in F3.
- 35–38: eager Unserialize outcome; IsNull's bit/hash/validity predicate; lazy Serialize/Get buffer state; recovered-signature deserialization hash with lazy wire bytes, ChainLock eager serialization, commitment SerializeHash and InstantSend lazy field serialization. Corrections in F3–F5.

## Gate results

I copied `/work/scratch/gate.sh` to the review scratch, pointed it at the detached head, set `CARGO_BUILD_JOBS=6` and `TMPDIR=/dev/shm/ab-review-o5-s` (mode 700), isolated its output/retained-datadir paths, and set the selftest trigger base to `76391f21`. It ran under `systemd-run --user`.

- **Full gate: PASS, `GATE EXIT 0`.** No failed step and no test rerun was needed.
- `cargo fmt --check`: PASS.
- `cargo clippy --workspace --all-features --all-targets -- -D warnings`: PASS.
- Workspace tests: **3,132 passed, 0 failed, 63 ignored** (unit/integration/doc-test summaries combined).
- Compiled registered inventory at this head: **38 names present, 12 passed, 26 ignored**; the 12 include the tracked private hook, while the real private gate is recorded separately below. This validates the clean baseline; it does not fix F1's false greens on mutations.
- All **105 node-check selftests passed**, including the **17 new O5 cases**. Counter-gate unit tests: **107 passed**.
- Pins, cargo-deny, R2, pure-crate checks, CI-group verification, no-libm and KGW release/host-variant checks: PASS.
- Private tests: **2 receipt PASS lines**, and the private-check selftest passed. The implementer's reported BL-49 one-receipt flake did **not** reproduce in this gate run.
- Fuzz registry: **9 targets / 14 smoke legs** valid; fuzz selftest passed; smoke **14 passed, 0 failed**, with zero divergences/crashes/sink failures and no triage error.
- The independent checker probes produced the false greens and correct rejections detailed above; standalone Rust/Cargo compilations confirmed the effective test inventories and ignore attribute.


No production implementation or performance-sensitive code changed; O5 has no benchmark gate. The detached worktree, probe fixture and temporary data are removed after validation. The acceptance gaps above remain even if the gate is green.

Verdict: **CHANGES-REQUIRED**.

blockers=0 majors=2 minors=2 nits=1
