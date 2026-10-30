# A/B calibration review: AB-AR1-o

**Verdict: CHANGES-REQUIRED.** The Linux parsing and fixture claims withstand independent verification. A valid Windows drive-relative datadir crashes configuration initialization, and six smaller parity/portability defects remain.

Reviewed only `node/ab-ar1-o`, head **`3eaa005f5557a12b6643461a13585b9ed3fb6190`**, against base `76391f21`, `briefs/AB-AR1.md`, `AB-REVIEW.md`, `REVIEW-COMMON.md`, PLAN §4 AR1, and DEC-84. Review used a detached worktree at `/work/scratch/ab-review-ar1-o/wt`; the implementer's worktree was not used or modified. No other arm was inspected. No commits, pushes, or GitHub comments were made.

Core reference: **v24.0.0-rc.3**, `da567d7dba945143eee4d2ce1621c21b32089989`. The ordinary `~/workspace/dash` checkout is not at that tag, so reference sources were extracted with `git archive v24.0.0-rc.3`. The release tarball was independently downloaded; its SHA256 `9327f00018afd12f11676ac66e688c76b253e4783bf0d595f08476de4269ff52` matches the release's `SHA256SUMS.asc`. This verifies the digest comparison, not the signature's trust chain.

## Findings

### F1 — major: Windows drive-relative paths can panic or resolve against the wrong directory

**Locations:** `dash-args/src/paths.rs:136` (`absolute`), `dash-args/src/paths.rs:145` (`abs_path_join`), with the panic at `dash-args/src/paths.rs:141`.

`PathBuf::join` does not implement Windows `std::filesystem::absolute` or C++ path concatenation. On Windows, a path with a drive prefix and no root replaces the Rust base, even when the drive matches. Consequently, `absolute("C:data")` can return the still-relative `C:data`. Core resolves it through Windows absolute-path handling. This is more serious than the documented difference about normalizing `.` and `..`.

**Reproduction, verified with Windows-compiled Rust and MinGW filesystem probes under Wine 10:** use current directory `Z:\work\scratch\ab-review-ar1-o`, with an existing `wt` directory. The exact Rust crate returns:

```text
absolute(Z:wt)=Ok("Z:wt")
join same drive="Z:wt"
```

The C++ operations Core uses both return `Z:\work\scratch\ab-review-ar1-o\wt`. Register `Program::Dashd`, parse `["prog", "-datadir=Z:wt"]`, and call `init_config(&args, &select_base_params, None)`. Rust panics:

```text
AbsPathJoin: base path Z:wt is not absolute
```

A related case is an absolute `-datadir=C:\data` with `-conf=C:dash.conf`: C++ joins the same-drive filename under the datadir, while Rust discards the datadir and opens a drive-relative filename. The same helper handles includes and settings paths.

**Fix:** provide a Windows implementation of absolute resolution (including drive-relative paths and per-drive current directories), and implement Core-compatible Windows path concatenation. Add tests for same-drive and different-drive relative prefixes, root-relative paths, and UNC paths. DEC-84 requires portable behavior now; documenting this failure does not satisfy it.

### F2 — minor: Windows default datadir depends on APPDATA instead of the shell folder

**Location:** `dash-args/src/paths.rs:157`.

Core's `GetDefaultDataDir` uses `GetSpecialFolderPath(CSIDL_APPDATA)`, implemented with `SHGetSpecialFolderPathW`. The port reads a mutable environment variable and silently uses an empty base when it is absent. These are not equivalent APIs.

**Reproduction:** remove `APPDATA` inside a Windows process and call `default_data_dir()`. The reviewed crate returns relative `"DashCore"`. In the same Windows environment, the shell call succeeds and returns `C:\users\agent\AppData\Roaming`; Core therefore uses its absolute `DashCore` child. The port can then panic at the absolute-base assertion during default config resolution. An overridden APPDATA also redirects the port without redirecting Core's configured shell folder.

**Fix:** obtain the Windows roaming application-data folder using the same OS facility as Core, with explicit error handling. Add an environment-independent regression test. This is disclosed in ARCHITECTURE.md, but remains a behavioral deviation rather than merely unperformed Windows QA.

### F3 — minor: Windows hard-linked configuration files are falsely treated as different files

**Location:** `dash-args/src/paths.rs:189`, used by `dash-args/src/init.rs:124`.

Canonical path equality does not establish file identity. The ignored-config check must recognize that two hard links refer to the same file.

**Reproduction, verified under Wine:** create `D\dash.conf`, hard-link it to `D\other.conf`, then initialize with `-datadir=D -conf=D\other.conf -nosettings`. Core's `fs::equivalent` returns true. The port reports the multi-line “contains a \"dash.conf\" file which is ignored” error and fails. Canonicalizing the two links preserves their different names.

**Fix:** compare Windows file identity using volume/file identifiers from opened handles, matching Core's equivalent check. Test a hard-linked explicit config and a genuinely different config. ARCHITECTURE.md records this limitation, but legitimate Core configurations still fail.

### F4 — minor: Windows text-mode settings I/O is not preserved

**Locations:** `dash-args/src/settings.rs:230` (file read) and `dash-args/src/settings.rs:319` (file write); analogous config reading is in `dash-args/src/args.rs:988`.

Core opens settings with ordinary C++ text-mode streams. Rust `fs::File` reads/writes bytes. On Windows this changes both accepted file contents and the bytes written, beyond JSON's ordinary whitespace equivalence.

**Reproduction:** write the bytes `b"{}\x1a garbage"` to a Windows settings file. A Windows build of the actual pinned Core `src/util/settings.cpp` and UniValue sources returns `ReadSettings=true`, because text-mode input ends at Ctrl-Z. The port returns `Ok(false)` with the invalid-JSON error. Also, the Windows C++ writer emits `\r\n` for pretty-print line breaks; the Rust writer emits only `\n`, confirmed by inspecting both output files.

**Fix:** reproduce Core's Windows text-stream transformations for config/settings input and settings output, behind Windows cfgs. Add platform-specific Core-generated fixtures for Ctrl-Z termination and CRLF writes. The existing Linux replay cannot validate these cases.

### F5 — minor: generator freezes a Linux-only GUI help default on every platform

**Locations:** `dash-args/gen/gen_options.py:44` (platform configurations and later shared capture text), `dash-args/src/options/generated.rs:324`.

The generated `-uiplatform` help always says `default: other`. Core's `SetupUIArgs` calls `GUIUtil::defaultUIPlatform()`; pinned `src/qt/guiutil.cpp:171` returns `macosx` on macOS and `windows` on Windows. The generator varies some registrations by platform but reuses the Linux captured text here.

**Reproduction:** register `Program::DashQt` on either platform and render `-help-debug`. Its `-uiplatform` paragraph says `default: other`, unlike Core. This mismatch also exists on macOS, where the Linux help comparison remains enabled.

**Fix:** have the generator resolve and emit platform-specific runtime defaults, including this one, without leaving unresolved placeholders. Add platform-specific assertions/captures. All eight Linux help outputs genuinely match, and no unintended placeholders were found; that evidence does not establish native-platform fidelity.

### F6 — minor: non-finite float construction can write an unreadable settings.json

**Location:** `dash-args/src/univalue.rs:93`, also exposed through `From<f64>` at line 509.

`from_f64` directly creates `Num(format_g16(value))` and skips Core's JSON-number validation. For infinity and NaN it constructs `inf` and `nan`. Inserting either value into `rw_settings` makes `write_settings_file` report success while writing invalid JSON, which the next startup cannot read.

**Reproduction:** `SettingsValue::from(f64::INFINITY).write()` returns `inf`; NaN returns `nan`. A probe compiled from pinned Core UniValue sources throws respectively:

```text
The string 'inf' is not a valid JSON number
The string 'nan' is not a valid JSON number
```

**Fix:** use a fallible float constructor that applies Core's validation and returns its runtime-error text; remove or adjust the infallible conversion. Add tests proving these inputs cannot be persisted successfully. This deviation is not recorded in ARCHITECTURE.md.

### F7 — minor: Windows all-target clippy fails with warnings denied

**Locations:** `dash-args/tests/help_tests.rs:16` and `dash-args/tests/help_tests.rs:28`.

The help comparison is cfg-disabled on Windows, but its two private helpers are not. The required warning policy therefore fails on this supported target.

**Reproduction:**

```sh
TMPDIR=/dev/shm/ab-review-ar1-o CARGO_BUILD_JOBS=6 \
  cargo clippy -p dash-args --target x86_64-pc-windows-gnu \
  --all-features --all-targets -- -D warnings
```

Exit 101: `help_message` and `captures` are never used. Windows library `cargo check` and `cargo build` both succeed, so a library-only cross-target gate does not catch this.

**Fix:** cfg the helpers together with their test, or use them in appropriate Windows help tests. Re-run the command above.

## Acceptance and verification evidence

All reviewer Rust checks used the detached worktree, `TMPDIR=/dev/shm/ab-review-ar1-o` (mode 700), and `CARGO_BUILD_JOBS=6`. Long validation/capture jobs ran in separate user systemd units.

- `cargo fmt --all -- --check`: **PASS**.
- `cargo clippy -p dash-args --all-features --all-targets -- -D warnings`: **PASS** on Linux.
- `cargo test -p dash-args --all-features`: **62 passed, 0 failed**: 15 library tests, 14 argsman, 14 functional-behavior tests, 8 getarg, 6 help, 1 init replay, 4 settings. No doctests.
- The required Core case counts really are **getarg 8/8, settings 4/4**, with argsman additionally **14/14**. The Rust cases and source expectations were compared against the tagged Core tests; the three generated merge transcripts match their Core outputs and hash expectations.
- Downloaded the pinned Linux release and independently ran `capture_core_outputs.py` into new scratch paths. **All three merge transcripts, all six non-GUI help files, recognized.txt, and all 74 init cases were byte-identical** to the checked-in fixtures. The two GUI outputs were separately recaptured under Xvfb with scratch-extracted XCB libraries: **both byte-identical**. Thus all eight Linux help captures are independently reproduced. Rendered help-body tests pass; version/usage banners are intentionally outside this library. Notification-command `%s`/`%w` etc. are literal Core documentation, not unexpanded generator placeholders.
- `recognized.txt` represents **345 program/name parser trials, 302 distinct names**, across dashd (274), dash-cli (28), dash-tx (11), dash-wallet (23), dash-util (9). It is not 345 distinct options. Every row reproduced. GUI-only parser recognition is not captured; GUI registration was inspected against the source and the GUI help outputs.
- `gen/gen_options.py`, run against a fresh archive of the exact Core tag with the checked-in help captures, regenerated **generated.rs byte-identically**. It has no dependency on the implementer's worktree or a particular input-path layout. Its platform help-default gap is F5.
- A separate JSON comparison used the pinned Core UniValue implementation and the reviewed crate on **15,006 deterministic valid/mutated documents: zero differences**. Non-finite typed construction is separately covered by F6.
- `python3 .github/scripts/ci_config.py verify-groups`: **PASS**, all 29 workspace crates assigned and matrix consistent.
- `contrib/node/check-r2.sh`: **PASS**. dash-args is dependency-free in production, correctly stays outside both stacks, and its shared-library classification in deny.toml does not need an exception entry.
- `cargo check -p dash-args --target x86_64-pc-windows-gnu --all-features` and corresponding build: **PASS**. Windows all-target clippy: **FAIL**, F7.
- Windows runtime reproductions used the unmodified Rust crate compiled for `x86_64-pc-windows-gnu`, pinned Core settings/UniValue code compiled with MinGW, and the same filesystem/shell calls Core uses, under an isolated Wine 10 prefix. This is supporting evidence, not a claim of native Windows release QA. No node was started and no shared oracle/benchmark datadir was touched.

The implementer's full-workspace gate claim is **GATE EXIT 0, 3190 passed after one BL-20 retry**. This review independently reran the required touched-crate checks and the checks above, **not the entire workspace gate**; it does not independently certify the 3190 count.

## Adversarial Core spot-checks

These behaviors were checked against the exact tagged source and/or independently reproduced release outputs, exceeding the ten required spot-checks:

1. `-nofoo` produces a false setting, cancels earlier values, and yields an empty list.
2. `-nofoo=0` re-enables with true and logs Core's exact double-negative warning.
3. `DISALLOW_NEGATION` and `DISALLOW_ELISION` yield Core's exact errors; `-proxy=` differs from elided `-proxy`.
4. Only one `no` prefix is removed; boolean text uses Core's atoi semantics rather than Rust boolean parsing.
5. `--foo` is normalized; dotted CLI keys with a nonempty section are rejected; a lone `-` stops parsing.
6. Config sections and dotted config keys follow Core's section parsing, including warnings and line numbers for unrecognized sections.
7. NETWORK_ONLY default-section values are ignored outside mainnet; default-section negations retain Core's exceptional behavior.
8. CLI scalar values use last-wins, ordinary config scalars use first-wins, and chain-selection config values use last-wins.
9. Forced > CLI > settings.json > network config > default config precedence, null overrides, and multi-value “zombie” config values follow the Core merge code.
10. `-includeconf` and `-noincludeconf=0` are refused on argv; `-noincludeconf` suppresses config includes.
11. Chain-section includes precede default-section includes; multiple includes retain ordering and first-value precedence.
12. Recursive includes are ignored with Core's exact stderr warning, including a chain change caused by an included file.
13. `conf=` inside base/included config is forbidden; relative `-conf` is based on the original datadir.
14. A config `datadir=` changes the eventual data/blocks directory without changing the recorded config path; CLI datadir wins. Ignored secondary dash.conf detection follows Core on POSIX.
15. Conflict errors for regtest/testnet/devnet/chain, named/unnamed devnet directories, and ignored CLI chain negations match Core.
16. settings.json default warning, unknown-key retention/logging, duplicate-key rejection, invalid/non-object JSON, empty-file removal, alternate paths, and disabled settings match the Linux reference.
17. Sensitive argv/config logging masks values, while settings logging follows Core's separate behavior.
18. Full Linux help, categories, flags, recognition, and regeneration were checked independently as above; the Windows edge probes produced F1–F4 and F7.

The functional-behaviors table covers the three named scripts. Deferring connection-manager behavior, full chain-parameter interaction, actual shutdown, and Qt dialogs is justified without rdashd. The library-specific versions of those checks are present. Per-manager base parameters, result-returning runtime errors, and panic equivalents for Core assertions/programmer errors are reasonable Rust API adaptations. The one-release table follows DEC-82.

The disclosed invalid-UTF-8/above-U+10FFFF limitations mean unrestricted byte-string parity is not established; this review does not extend the Linux success claims to such inputs. Non-reentrant callbacks are explicitly documented. The disclosed Windows path/default-folder/identity substitutions do not justify claiming Core-equivalent handling: F1–F3 demonstrate actual failures. No unrelated implementation changes are requested.

## Reviewed commits

```text
835bfef5 feat(dash-args): capture Dash Core v24 argument handling outputs
33305c1e feat(dash-args): port Dash Core's ArgsManager
1b784c63 test(dash-args): port Core's getarg, settings and argsman unit tests
7bde5f7b test(dash-args): check help, options and init against Core's binaries
1ad2d4eb ci: run dash-args in the node-app group and note it in deny.toml
37a9ae2c fix(dash-args): follow Core on filesystem errors and failed parses
cc6d3b4c fix(dash-args): fail the option generator on what it cannot account for
3eaa005f refactor(dash-args): share test helpers and trim duplication
```

**Verdict: CHANGES-REQUIRED.**

blockers=0 majors=1 minors=6 nits=0
