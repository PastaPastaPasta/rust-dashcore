# AB-AR1-s confirmation review, round 2

**Verdict: CHANGES-REQUIRED**

Reviewed only `node/ab-ar1-s` at `862ca3f6de16d603b69395c9c5cfdcc574a42958`, against round-one head `1dde8b14`, AB-AR1.md, AB-REVIEW.md, REVIEW-COMMON.md and PLAN §4 AR1. Used the requested detached worktree under `/work/scratch/ab-review-ar1-s-r2/`. No other arm's implementation or review was inspected. No commits, pushes or GitHub comments were made.

Reference sources were extracted with `git show v24.0.0-rc.3:<file>` from the Dash Core repository. The peeled reference commit is `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`. Live comparisons used the Linux x86_64 rc.3 release binaries; all six help-binary SHA-256 hashes match the committed provenance.

## Result

The two original majors are closed. The original UniValue, settings-directory and filesystem-status reproductions are fixed. The original invalid-UTF-8 config/argv reproductions are also fixed, but the new escaped-string representation is not consistently enforced at public input boundaries. That leaves a concrete valid-text-to-unreadable-settings regression (F7). The path-cache follow-up also misses one exception path (F8).

Both remaining findings are **minor**, using the same scale as round one: F7 requires characters in the reserved U+10FF00–U+10FFFF range, and F8 requires retrying a getter after a filesystem exception. Neither is an ordinary precedence, option-recognition, consensus or startup-path defect. They nevertheless fail the faithful-port acceptance and should be corrected before claiming complete AR1 parity.

## Original findings and follow-ups

| Finding | Confirmation |
|---|---|
| F1, major: help placeholders | **Closed.** The table has 371 per-program registrations and 33 entries supplied from Core help dumps. Regeneration succeeds, rendered Options sections match all six dumps, and independently recapturing all six binaries reproduces every fixture and PROVENANCE.txt byte for byte. |
| F2, major: Windows UNC root | **Closed for the reported defect.** `Flavor::Windows` preserves `\\server`, drive-relative roots, and device/extended prefixes; it normalizes root-name separators too. The Linux-executed path tests pass. This is not a claim of native Windows validation. |
| F3, minor: UniValue reader | **Closed.** The original split-surrogate, overlong-UTF-8 and lone-minus cases match Core. The lexer, UTF-8 filter and expectation-mask transitions follow the pinned implementations. Independently regenerated Core fixtures match, and the larger independent differential finds no reader/writer mismatch. |
| F4, minor: settings directory | **Closed on the reference platform.** Live Core and Rust remove/recreate an empty settings directory and produce the same nonempty-directory error. |
| F5, minor: status error treated as absence | **Closed for the reported defect.** ENOENT/ENOTDIR are distinguished from other status errors. The deterministic ENAMETOOLONG diagnostic matches live Core. |
| F6, minor: lossy config/argv | **Original reproductions fixed; broader lossless-input closure incomplete.** Raw FF reaches config errors unchanged, `_os` argv and non-UTF-8 paths retain their bytes on Unix. F7 shows that valid Unicode text supplied through another public input method can still be mistaken for escaped bytes. |

The follow-up separation of text from escaped bytes fixes `parse_parameters`, `force_set_arg` and `SettingsValue::from`, but misses `read_config_stream` and the non-Unix `from_os_str` branch. ENOTDIR-on-create and caching before a create failure are fixed. The datadir status-failure cache is fixed; the explicit blocksdir status-failure cache is not (F8).

## Independent checks

Reviewer gates ran with `TMPDIR=/dev/shm/ab-review-ar1-s-r2` (mode 700), `CARGO_BUILD_JOBS=6`, and the detached worktree's own `target/`:

- `cargo fmt --all --check`: PASS.
- `cargo clippy -p dash-args --all-features --all-targets -- -D warnings`: PASS.
- `cargo test -p dash-args --all-features`: **97 passed, 0 failed**; 0 doctests.
- `python3 -I dash-args/tools/gen_option_table.py --check`: PASS.
- `.github/scripts/ci_config.py verify-groups`: PASS; all 29 crates assigned.
- `contrib/node/check-r2.sh`: PASS.
- Pinned release `test_dash --run_test=getarg_tests,settings_tests,argsman_tests`: **26 cases passed**, no errors.
- Regenerated `univalue_cases.txt` with `gen_univalue_fixture.py` against the pinned Core sources: **1,506 cases, 244 accepted**; the entire generated file is identical to the committed fixture.
- Independently expanded UniValue differential: **100,566 distinct inputs, 12,104 accepted by Core, 0 differences** in acceptance, compact bytes or pretty bytes. This includes all two-byte combinations in JSON strings, every character in the escape range as both keys and values, surrogate escapes, and seeded mutations of structured documents. All 12,104 accepted inputs satisfy `read(write_pretty_bytes(0, 0)) == original_value` in Rust. This confirms the bytes path; it does not establish the different `read(write())` path (F7).
- Live Core/Rust initialization differential: **15 cases, 0 differences** in normalized error text and resulting settings-file bytes; additionally, the ENAMETOOLONG error matches. Cases cover unknown arguments, forbidden elision/negation, conflicting chains, includeconf and its double-negation error, a raw FF config error, both settings-directory cases, split surrogates, overlong UTF-8, a real private-use character in an on-disk settings file, lone minus, duplicate keys and `-nosettings`. A final loose token stops initialization before node startup; owned datadirs were on /dev/shm and port arguments were 41478/41479.

The reported full-workspace gate of **3,226 passed, 0 failed** was not independently rerun. The required reviewer touched-crate gates and the checks above were independently run. The test suite's increase from 72 to 97 is consistent with the claimed 25-test increase, but that arithmetic is not independent full-workspace gate evidence.

### Help reproducibility and provenance

Ran `python3 -I dash-args/tools/capture_help.py --bin <rc.3-release-bin> --out <scratch-help>` with `LC_ALL=C`, a private Xvfb display, and the required xcb libraries unpacked into scratch and supplied through `LD_LIBRARY_PATH`. No machine packages were installed. **dashd, dash-cli, dash-tx, dash-util, dash-wallet, dash-qt and PROVENANCE.txt all reproduce exactly.**

`PROVENANCE.txt` pins binary hashes, version headings and invocation flags. OPTION_TABLE.md and the generated table pin the release tag and source commit. `--report` ties dumped substitutions to the source expressions; `--check` regenerates the table. The help tests correctly compare the Options section, because the program version/license/usage preambles are outside ArgsManager's help output. The reference build's Linux/platform defaults are explicitly documented. The capturer requires its documented display/runtime environment; it does not provision those dependencies itself.

### Core behavior spot checks

These were rechecked against the pinned source and passing Rust ports, with the live evidence above where applicable:

1. Bare values, `-no` negation and double-negative warnings: `util/system.cpp::InterpretValue`.
2. DISALLOW_ELISION and DISALLOW_NEGATION errors: the same function; live errors match.
3. CLI includeconf rejection and `-noincludeconf` suppression: `ParseParameters` and the functional ports.
4. Ordinary CLI last-wins versus config first-wins, including list negation resets: `util/settings.cpp::GetSetting/GetSettingsList` and the merge transcript tests.
5. NETWORK_ONLY default-section suppression and the retained negation exception: `GetSetting`, `GetUnsuitableSectionOnlyArgs`, and the section tests.
6. The four network sections, dotted keys, unknown-section source/line reporting, ASCII trimming and `#` handling: `GetConfigOptions` and the parser ports.
7. Config `conf=` rejection and reindex warning: `IsConfSupported`.
8. Network includes before default includes, nonrecursive include warnings, and final-chain include warnings: `ReadConfigFiles` and the includeconf tests.
9. Chain conflicts, special config last-wins selection and skipped negated selectors: `GetChainName` and the chain merge tests; live conflict text matches.
10. Relative conf paths, config datadir changes and ignored-default-conf handling: `common/init.cpp::InitConfig` and functional ports.
11. Settings precedence, unknown-key preservation, duplicate-key errors, number lexemes, `_warning_`, alternate paths and `-nosettings`: `util/settings.cpp`, `ReadSettingsFile/WriteSettingsFile`, and the settings ports.
12. Empty settings files/directories and nonempty-directory removal errors: `ReadSettings` plus live differential.
13. Fallible status, ENOTDIR-on-create and the cache assignment order: pinned `GetDataDir/GetBlocksDirPath`; the remaining mismatch is F8.
14. Byte preservation in file config/Unix argv, and public text inputs versus escaped storage: file/argv probes, UniValue differential and F7.
15. Help categories, debug filtering, wrap/layout and all dumped expressions: source/table regeneration and the six live captures.

## Remaining findings

### F7 — minor — Public text boundaries can confuse Unicode with escaped bytes and write unreadable settings

**Locations:** `dash-args/src/args.rs:1068` (`read_config_stream`); related boundaries `dash-args/src/value.rs:165` (`push_kv_end`) and `dash-args/src/bytestr.rs:87` (non-Unix `from_os_str`). The writer at `value.rs:298` escapes Unicode characters before `write_pretty_bytes` decodes private-use escape markers.

**Concrete reproduction, independently compiled against this head:**

```rust
use dash_args::{ArgsManager, Program, write_settings};
use std::{collections::BTreeMap, path::Path};

let mut args = ArgsManager::for_program(Program::Dashd);
args.read_config_stream("uacomment=\u{10ff22}\n", "x", false).unwrap();
let value = args.get_setting("-uacomment");
let values = BTreeMap::from([("uacomment".into(), value)]);
write_settings(Path::new("settings.json"), &values).unwrap();
```

The successfully written file contains `"uacomment": """`. Both Core's reader and this crate's `read_settings` reject it. No manual construction of an invalid enum value or escaped string is needed: the value comes directly from the public config parser and getter. `read_config_stream` is documented as taking config **text**, but forwards it to the internal parser without `bytestr::from_bytes`. The real U+10FF22 character (UTF-8 F4 8F BC A2) is consequently treated as the marker for byte 22. The JSON escaper does not see a quote until after it has already escaped the string. Core's config parser preserves the original bytes, and a separately compiled pinned UniValue writer produces readable, stable JSON containing that character.

The same inconsistency exists for a plain-text key passed to `push_kv_end("\u{10ff22}", ...)`: the serialized object is `{""":...}`. On non-Unix platforms, `from_os_str` returns valid text without encoding the escape range, unlike the Unix branch and `parse_parameters`; a valid Unicode argv/path in that range is thus also misinterpreted. The latter is source-backed reasoning, not a native Windows run.

**Round-trip limit:** file/byte reader-writer round trips pass, including real private-use characters. However, for `v = SettingsValue::read("{\"k\":\"\u{10ff22}\"}").unwrap()`, `SettingsValue::read(&v.write()).unwrap() != v`. `write()` returns an escaped `String`, while `read(&str)` consumes its UTF-8 as fresh text. The documented double-conversion convention explains that API hazard, but does not fix the unencoded public inputs above. The existing `text_that_looks_like_an_escaped_byte_stays_text` test covers the repaired constructors, not these boundaries.

**Suggested fix:** distinguish ordinary text from the escaped representation consistently. For example, expose a text/bytes config reader that encodes once and use a separate internal escaped reader for `read_config_files`; protect valid Unicode in the non-Unix OS conversion too. Give object-key construction separate text and encoded paths, or use a byte-string wrapper so the compiler enforces the distinction. Preserve encoded keys from `read_settings` without encoding them a second time. Add regressions for text-to-config-value-to-settings, keys, OS conversions, and both documented round-trip APIs. Escaping the decoded bytes in the writer would also prevent unescaped quote/control-byte emission, but does not alone recover the original Unicode value after the input was misinterpreted.

**Severity rationale:** this can corrupt persistence, but the demonstrated trigger is confined to the reserved private-use range, so it remains minor under the round-one scale for unusual input compatibility. The original FF input tests pass; this is a remaining boundary defect, not a failure of the main byte reader.

### F8 — minor — Explicit blocksdir status exceptions still have the wrong cache semantics

**Location:** `dash-args/src/args.rs:907`–`:909` (`get_blocks_dir_path`).

**Reproduction:** force `-blocksdir` to `format!("/{}", "x".repeat(300))` on Linux and call `get_blocks_dir_path()` twice. This head returns `filesystem error: status: File name too long [...]` both times. The pinned `util/system.cpp:346` assigns `m_cached_blocks_path = fs::absolute(GetPathArg("-blocksdir"))` **before** calling `fs::is_directory`. Its first call throws, but the next call returns the cached absolute blocksdir itself, without the network/blocks suffix or another status check.

Rust's explicit blocksdir branch uses a local `abs` and propagates the status exception before assigning the cache. The new fixes correctly preserve Core's cache for datadir status failures and blocks-directory creation failures; this third exception path was missed. Source assignment order proves the Core result; the repeated Rust errors were independently reproduced.

**Suggested fix:** assign/cache the explicit absolute blocksdir before the fallible status check, preserve it on an exception, and clear it when the status normally reports a nondirectory. Add a repeated-call regression alongside the existing datadir-status and blocks-create cache tests.

**Severity rationale:** this affects retry after an exceptional filesystem failure; ordinary initialization aborts on that first failure. It is a minor observable parity defect.

## Judgment on KNOWN_GAPS.md and acceptance

The scope exclusions for full consensus chain parameters, activation validation, AppInitParameterInteraction and node/network-only functional checks remain reasonable AR1 boundaries. Buffered log/stderr output is an appropriate integration adaptation. A reproducible Linux release option/help surface, with explicitly disclosed platform defaults, is supported by the new evidence. The full option table, workspace classification, CI grouping and the named Core test ports remain in place.

The escaped-byte representation can be an acceptable implementation choice if its boundaries consistently preserve bytes. The warning that plain `String` outputs require decoding, and that text constructors encode fresh text, makes intentional conversions reviewable; it does not excuse F7. Correct file/bytes round trips have strong evidence, but a plain `String` should not silently mean text at one public input and an already encoded value at another without a distinct contract.

The remaining OS-specific error wording, middle-of-stream I/O exception differences, NUL-path behavior and replacement of unpaired Windows UTF-16 are disclosed compatibility limits, not evidence of universal byte parity. The Linux reference validation does not settle macOS/libc++ or native Windows behavior. They may be retained as explicitly tracked exceptional/platform limitations for a reference-build acceptance, but KNOWN_GAPS.md itself grants no waiver for an all-platform, all-error byte-identical claim. Native platform validation and any intended narrowing should remain explicit in the acceptance decision. Neither F7 nor F8 is covered by a justified scope exclusion.

The implementation is substantially improved and the new Core-generated fixtures are credible. No blocker, major, or additional stylistic nit is found in this round. Correct the two concrete minors before recording AR1 as a complete faithful port.

The detached worktree, test temporary directories, private X display and reviewer scratch files were removed after the review. The review document is the only retained output; it is not committed.

**Verdict: CHANGES-REQUIRED**

blockers=0 majors=0 minors=2 nits=0
