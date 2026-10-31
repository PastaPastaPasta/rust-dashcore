# AB-O5-s calibration confirmation — r2

Verdict: **CHANGES-REQUIRED**.

Reviewed only `node/ab-o5-s` at `1d259191b9b07e4f36c6d465909fe53aa769f63b`, diff `660315fd..1d259191`, in detached `/work/scratch/ab-review-o5-s-r2/wt`. This is the requested narrow confirmation of r1 F1–F5. The implementer's worktree was not edited. The report is uncommitted.

Commits reviewed:

- `83a92135` — test(quirks): register quirks 39-42 and correct register text
- `1d259191` — test(quirks): check the register against the compiled test inventory

## Finding

### F1-R2 — major: the compiled inventory discards Cargo-target and feature-configuration identity

`contrib/node/check-quirks.py:93`, `:124`, and `:191`.

The original text-only disappearance attacks are rejected now, but the replacement inventory is pooled across every requested feature build and deduplicated by `(src_path, ident)`. A source file is not a test binary, and a test compiled in one feature configuration does not establish its presence in another entry's declared configuration. Two concrete false greens remain:

1. **A test compiled twice:** declare two Cargo test targets with different names and the same source path:

   ```toml
   [[test]]
   name = "quirks"
   path = "tests/quirks.rs"

   [[test]]
   name = "quirks_twin"
   path = "tests/quirks.rs"
   ```

   Cargo emits two different executables, both containing every named test in that file. The checker overwrites the first inventory with the second and exits **0**, reporting the unchanged registered/ignored counts. A dependency-free 42-entry probe produces **42 named quirk tests in each executable (84 compiled occurrences)** and still reports 42 registered / 29 ignored. The same manifest mutation in the actual arm's `conformance/Cargo.toml` produces **21 quirk tests in each of `quirks` and `quirks_twin`**, with different executable paths and the same `conformance/tests/quirks.rs` source. The checker still exits 0 with 42 registered / 29 ignored, although there are 63 compiled quirk occurrences, 50 ignored. This violates the required exactly-one compiled-test check. A same-named test in a different source file is correctly rejected; the collision specifically requires the shared `src_path`.

2. **The r1 disabled-feature attack reappears when another entry requests that feature:** declare `not_default = []`, put `#[cfg(feature = "not_default")]` on quirk 13, leave quirk 13's register entry without `features`, and add `features = ["not_default"]` to quirk 41's entry in the same package. The checker exits **0**. Independent Cargo listings show quirk 13 **absent with default features** and **present with `not_default`**. This was reproduced both in the dependency-free probe and in the actual reviewed arm. Quirk 41's build has supplied evidence for quirk 13's different declared configuration. The clean register's summary masks the missing default test.

**Suggested fix:** retain Cargo target identity (package, target kind/name and executable) within each feature-set inventory. Validate each entry against its own declared configuration. When the same target/test is rebuilt for several requested configurations, deduplicate only that intentional repetition; retain separate target occurrences and reject duplicates. Add selftests for both reproductions. The original F1 coverage requirement is therefore **not fully closed**.

## r1 closure checks

- **F1 / major 1: partially fixed, remains open as F1-R2.** Block comments, crate-level `#![cfg(any())]`, disabled enclosing modules, declared but disabled features, `required-features`, `test = false`, and effective `cfg_attr` ignores are rejected when tested independently. Both an implemented quirk made ignored through `cfg_attr` and a pending quirk promoted to implemented while still effectively ignored are rejected. Macro-generated registered tests are correctly accepted. The target/configuration false greens above prevent closure.
- **F2 / major 2: closed.** The register contains ids **1–42** exactly once. Quirks **39 / CS1**, **41 / EV2a**, and **42 / EV2b** have pending stubs with the exact owner reason; **40 / CS2** references the existing Core-derived KGW proof under the renamed `quirk_40_kgw_decisions_invariant_under_pow_error`. The minimum-count rule accepts appended registrations instead of forbidding 39–42. DEC-25(4), DEC-29 D2, and DEC-50(4) are reconciled with the stale 38-item brief.
- **F3 / minor: closed.** Corrected AssetUnlock input/output exemptions (2), out-of-range input assertion versus SINGLE missing output (6), submitted-difficulty denominator (9), compact nBits 1 with the min-difficulty condition (15), version-selected SML fields (17), active-quorum-dependent bit range (30), chain-parameter versus rule-site pins (34), and successful-Get qualification (37) agree with the v24 reference.
- **F4 / minor: closed.** Quirk 19 points to block-path null acceptance at `llmq/blockprocessor.cpp:369`; 20 to `BuildCommitmentHash` at `llmq/commitment.cpp:312` with the versioned serialization as an additional anchor; 22 and 35 to eager `Unserialize` at `bls/bls.h:184`. The checker documents that its optional anchor validation establishes existence only.
- **F5 / nit: closed.** The named quirk 37 test now checks the pinned `sub_000` input under Basic, verifies raw serialization, performs a failed `Get`, then requires same-scheme serialization to be zero. It directly catches preservation of the original nonzero buffer after failed decoding.

## Evidence and validation

TMPDIR was `/dev/shm/ab-review-o5-s-r2-tmp`, mode **700**; `CARGO_BUILD_JOBS=6`. Long validation ran under a dedicated user systemd unit.

- `DASH_CORE_REPO=/work/home/agent/workspace/dash python3 -I contrib/node/check-quirks.py`: **PASS**, exit 0; **42 registered (38 R4/A1, 4 appended), 12 implemented, 1 private, 0 dropped, ignored count 29**.
- `cargo fmt --check`: **PASS**.
- `cargo clippy -p dash-consensus -p dash-conformance --all-features --all-targets -- -D warnings`: **PASS**.
- `cargo test -p dash-consensus -p dash-conformance --all-features`: **PASS**, **303 passed, 0 failed, 29 ignored**, across unit, integration, and doc-test summaries.
- Quirk section of `contrib/node/selftest-checks.sh`: **PASS, 28/28 cases**, 0 failures. I ran its 28 case bodies unchanged in the detached disk worktree, with an isolated wrapper implementing the same `expect`/reset logic. This avoids rerunning unrelated node-check selftests and creating its normal nested worktree in RAM.
- The combined local validation completed with **VALIDATION EXIT 0**.
- `git diff --check 660315fd..1d259191`: **PASS**.
- Independent dependency-free Cargo probes: the **eight** r1/configuration exclusion mutations and a same-named test in a different-source binary are rejected; macro-generated registered tests are accepted; the two new false greens are reproduced with independently listed compiled binaries/configurations, both in the small probe and in the actual arm.

Core reference was `git -C /work/home/agent/workspace/dash show v24.0.0-rc.3:src/...`. I spot-checked quirks **2, 6, 9, 15, 17, 19, 20, 22/35, 30, 34, 37, 39, 40, 41, and 42** against the relevant source, exceeding the ten-behaviour requirement. Quirk 41's 64-bit online-from-genesis qualification agrees with DEC-54. No live dashd or private vectors/reports were inspected.

The implementer's **full gate exit 0, 3,131 tests passed, BL-20 rerun 54/0** is a reported result, not independently rerun here. This round reran the mandatory checker, all quirk selftests, and touched-crate format/lint/tests; the unrelated full workspace gate was outside this narrow confirmation. There is no production consensus implementation change in this diff. The gate's success does not cover the new inventory evasions.

The detached worktree, probes, build products, and private TMPDIR were removed after validation. No commit, push, or GitHub comment was made.

Verdict: **CHANGES-REQUIRED**.

blockers=0 majors=1 minors=0 nits=0
