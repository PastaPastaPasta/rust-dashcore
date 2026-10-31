# AB-O5, arm o — Sol confirmation review, round 2

Verdict: **CHANGES-REQUIRED**

Reviewed only `node/ab-o5-o`, head `83188bfc416ab538ba21fa7afaa22f9e185eb446`, against the shared `AB-REVIEW.md`, `REVIEW-COMMON.md`, `AB-O5.md`, PLAN §4 O5, and the round-one report `AB-O5-o-sol.md`. The confirmation range is `8ce4a4af..83188bfc`:

- `d7ecbe8b`: correct quirks 9, 19 and 37 and the scaffold placement comments.
- `83188bfc`: validate the compiled test inventory and add three regression selftests.

The three original major-finding reproductions now fail correctly, and all four original minors are closed. New adversarial cases expose one major and two minor gaps in the checker. These are gate-tool correctness issues; the follow-up commits change no production consensus behavior.

## Findings

1. **major — `contrib/node/check-quirks.py:199–211,364–365`: another test binary can satisfy a registered test whose own binary contains no tests.** The inventory discards the compiler artifact's `target.src_path` and target identity, retaining only the package directory, test name and ignored flag. Matching then compares the package and the final function-name segment. This does not establish that the registered source function compiled.

   Reproduced on a detached copy of the actual reviewed head: prepend `#![cfg(any())]` to `dash-consensus/tests/quirks.rs`, then create `dash-consensus/tests/review_substitute.rs`:

   ```rust
   macro_rules! substitute {
       ($name:ident) => {
           #[test]
           fn $name() { assert_eq!(2 + 2, 4); }
       };
   }
   substitute!(quirk_04_checkdatasig);
   substitute!(quirk_05_num2bin_exceptions_and_unexecuted_failures);
   substitute!(quirk_07_ecdsa);
   ```

   `python3 -I contrib/node/check-quirks.py --core ~/workspace/dash` exits **0** and prints `42 quirks, 42 tests: 12 tested, 30 ignored (scaffold), 0 dropped (DEC-82)`. Independently listing the compiler artifacts proves that the registered `quirks` target has **0 tests**, while `review_substitute` has those **3 tests**. The register still points at the original, compiled-out functions and their real Core-vector assertions. The macro's generic `fn $name` is absent from the source scanner's function records, so the second source file creates no duplicate-source error.

   **Fix:** retain target identity/source paths in the inventory and require a match to the registered function's actual target and module, including legitimate `#[path]` modules. An equal leaf name elsewhere in the package must not substitute for it. Add this hidden-canonical-file/second-target case to the selftests. This is narrower than proving a test's body is correct: it proves the particular test named by the register is present.

2. **minor — `contrib/node/check-quirks.py:305–312`: macro-generated quirk tests in other workspace packages escape the unregistered-test check.** Packages are selected from parsed `fn quirk_NN_*` declarations and register paths. A macro-generated name is in neither list, so a package without an existing registered quirk test is never inventoried.

   Reproduced on the actual head by adding `dash-sha256/tests/review_inventory.rs` with the macro above and `substitute!(quirk_99_unregistered);`. The checker exits **0** with the unchanged 42/12/30 summary, while `cargo test --locked -p dash-sha256 --test review_inventory -- --list` exits **0** and lists `quirk_99_unregistered: test`. The same macro in an already selected package is correctly rejected; package selection is the gap. This violates the stated no-unregistered-quirk-test invariant, but does not remove an existing registered test, hence minor severity.

   **Fix:** obtain the compiled inventory across the workspace for the unregistered-test check, preferably reusing the gate's existing test build, or enforce a documented fail-closed rule that ensures every macro-generated quirk's package is included. Add a regression in a package with no registered quirk tests.

3. **minor — `contrib/node/check-quirks.py:321–324,341–345`: a dropped entry may still have a compiled quirk test.** Compiled IDs are rejected only when absent from the register; the dropped branch checks source records and returns before checking compiled records. Consequently, a macro-generated test for a known dropped ID is accepted.

   Reproduced on the actual head: change entry 33 to `status = "dropped (DEC-82)"`, delete its `test` field and its original source function, and add an ordinary `dash-consensus/tests/review_dropped.rs` target using the macro above with `substitute!(quirk_33_hash_serialized_2_varint);`. The checker exits **0**, reporting `42 quirks, 41 tests: 12 tested, 29 ignored (scaffold), 1 dropped (DEC-82)`, although the new target's `--list` explicitly contains `quirk_33_hash_serialized_2_varint: test`. This is a prospective gap: the unmodified register currently has zero dropped entries.

   **Fix:** require `compiled.get(qid, [])` to be empty for dropped entries before that branch continues. Add a compiled, macro-generated dropped-ID selftest.

## Round-one closure and Core verification

Verified against `git show v24.0.0-rc.3:<path>` in `~/workspace/dash`; the tag resolves to commit `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`. The current checkout was not assumed to equal the tag.

- **Quirk 9:** `quirks.toml:104–105` now states `abs(claimed − computed) > 0.5 × claimed`, decoded to doubles, and the inclusive mainnet height limit 68589. This matches `src/validation.cpp:4476–4485`, including the comparison's ordinary floating-point/non-finite behavior. The existing oracle-backed test remains unchanged.
- **Quirk 19:** the register and scaffold now cite `src/llmq/commitment.cpp:240–310` and `src/llmq/blockprocessor.cpp:369–378` for the null acceptance paths. The added `src/serialize.h:452–476,620–634` anchors correctly cover fixed-bitset padding rejection and its dynamic-bitset caller. `commitment.cpp:153–160` supplies the minimum member/signer counts.
- **Quirk 37:** the title now qualifies the buffered object's cross-scheme zeroing as before a successful `Get()`. `src/bls/bls.h:444–464,480–511` matches the failed-Get invalidation, unresolved-buffer scheme switch and resolved-object re-encoding. The existing pinned base-SDK `explicit_scheme_serialization_trap` test independently distinguishes the unresolved and resolved cases.
- **Scaffolds 22/34:** accepted the manager's explicit ruling. They remain in conformance under DEC-86. The opening comment now accurately says `dash-params` and `dash-pkc` exist in base-sdk, explains why they carry no node scaffolds, and describes the eventual `base-sdk:` registration. The quirk-34 note explains that the existing fork test is not at the current pin. No placement finding is carried forward.

Additional source spot-checks satisfy the shared brief's minimum ten behaviors: 2 (`src/consensus/tx_check.cpp:18–24,60–67`), 3 (`src/validation.cpp:382`, `src/policy/policy.cpp:88`), 8 (`src/pow.cpp:81–132`), 10 (`src/validation.cpp:4578–4586`), 16 (`src/evo/cbtx.cpp:30–38`), 17 (`src/evo/simplifiedmns.h:72–77`), 32 (`src/versionbits.cpp:64–67,256–265`), 33 (`src/kernel/coinstats.cpp:72–81`), 34 (`src/chainparams.cpp:171–172,186–187`, `src/evo/deterministicmns.cpp:773–777`), 35 (`src/bls/bls.h:100–122,184–216`), 36 (`src/llmq/commitment.h:118–131`) and 38 (`src/llmq/blockprocessor.cpp:115`, `src/llmq/signing.h:90–100`). Together with the three corrected entries, these cover fifteen quirks. No new discrepancy was found in those checks.

## Adversarial validation

Used both isolated minimal Cargo workspaces and a second detached copy of the exact head; each worktree kept its own `target/`. The complete-tree experiments were restored before the targeted gates. No implementer worktree or other A/B arm was inspected or edited.

- Original block-comment case: **rejected**, quirk 4 has zero source tests. Nested block comments are also rejected correctly.
- Original file-level `#![cfg(any())]`: **rejected**, quirks 4/5/7 have zero compiled tests.
- Original `#[cfg(any())] mod hidden`: **rejected**, the affected quirk has zero compiled tests. Also confirmed with an ignored scaffold in the minimal fixture; the branch selftest covers scaffold 10.
- `#[cfg_attr(all(), ignore)]` on quirk 4: **rejected**, with both compiled ignored-status and source-attribute diagnostics.
- Test in a non-default-feature module: **rejected** in the minimal workspace. A non-default-feature file containing the existing 4/5/7 tests is also rejected in the complete tree.
- Test in a `#[path]` module: a valid inclusion **passes**; a configured-out inclusion **fails**; including the same file twice **fails** with two compiled instances.
- Strings/raw strings containing comment markers, a character literal and ordinary Rust lifetimes preserve a following valid test in the positive fixture.
- Macro-generated unregistered test in an inventoried package: **rejected**. The three successful evasions are findings 1–3.

Synthetic fixtures were only for checker behavior. No consensus vectors were invented, regenerated or changed. The unchanged base-SDK quirk tests were inspected at pin `481f8c351c4f00dfad08a11063d2852e74e06016`; as documented, the workspace's Cargo test inventory does not execute dependency-crate tests.

## Gate cost

The copied gate used `TMPDIR=/dev/shm/ab-review-o5-o-r2-tmp` (mode 700), `CARGO_BUILD_JOBS=6`, isolated paths, and `RUSTC_WRAPPER=` for this run. The copied gate was pointed at the detached head, its selftest trigger was based on the r1 head, and explicit timed cold/warm/source-only checker steps were added. The review's gates and complete-tree probe runs were user-systemd jobs.

- Fresh-target compiled checker, including `--core`: **96.47 s**, peak child RSS **615,436 KiB**, PASS.
- Warm compiled checker: **1.44 s**, PASS.
- Warm `--source-only`: **0.31 s**, PASS, with its explicit compiled-inventory-not-checked label.
- On the separate complete-tree mutation worktree: first build **111.34 s**; ordinary single-file mutations **7.51–8.00 s**; adding a non-default feature **33.88 s**; same-name substitute target **5.83 s**.

The new mode therefore adds about **1.13 s** over source-only on this warm tree. Fresh and mutated trees incur real compilation cost. That cost is reasonable for the guarantee sought and is now described accurately in PROCESS rather than as “under a second.” Artifact reuse is subject to Cargo's feature/dependency fingerprints; the timing is evidence for this run, not a universal bound.

## Gates

All checks completed on the exact reviewed head; both detached worktrees were clean afterward.

- Untouched compiled checker, including all Core anchor bounds: **PASS**, 42 quirks / 42 referenced test functions / 12 tested / 30 ignored / 0 dropped. IDs 1–38 retain 11 tested and 27 ignored. The two base-SDK references have the source-only limitation described above.
- `cargo fmt --check`: **PASS**.
- `cargo clippy --workspace --all-features --all-targets -- -D warnings`: **PASS**. The separate targeted run for `dash-consensus` and `dash-conformance`, with `--locked --all-features --all-targets -- -D warnings`, also passes.
- `cargo test --locked -p dash-consensus -p dash-conformance --all-features --no-fail-fast`: **PASS**, 300 passed / 0 failed / 30 ignored, including doctests.
- Base-SDK pins, cargo-deny, R2, pure crates, CI groups, no-libm and KGW floating point: **PASS**. Cargo-deny reports the existing unused-wrapper warnings.
- Performance-harness unit tests: **PASS**, 107 tests.
- `contrib/node/selftest-checks.sh`: **PASS**, **118/118** cases overall, **30/30** quirk-register cases. The three added round-one regression cases all pass their expected rejection checks.
- Private-test gate: **PASS**, two `receipt PASS` lines; private-checker selftest: **PASS**. No private issue details were inspected or reproduced.
- Fuzz registry and tool selftests: **PASS**. Fuzz smoke: **14/14** legs, zero divergences, crashes, sink failures or triage errors; the smoke metadata identifies head `83188bfc416a` and a clean source tree.
- `cargo test --workspace --no-fail-fast`: **PASS**, **3,129 passed / 0 failed / 67 ignored**. Copied gate: **GATE EXIT 0**, no rerun required.

The complete fresh-worktree gate took approximately **90 min 39 s** with six jobs and the Rust compiler cache wrapper disabled. Major measured steps were workspace clippy **739.24 s**, the full selftest suite **782.84 s**, and fuzz smoke **1,881.85 s**, including its fresh RocksDB build. The final workspace test build also compiled a native dependency variant. These are whole-gate costs; they must not be mistaken for the isolated checker timings above. O5 has no benchmark gate and these commits alter no production runtime behavior.

The report is uncommitted. The detached review and experiment worktrees, synthetic checker fixtures, logs and temporary directories were removed after validation. No branch was committed, pushed or modified.

Verdict: **CHANGES-REQUIRED**

blockers=0 majors=1 minors=2 nits=0
