# AB-AR1-s confirmation review, round 3

**Verdict: APPROVE**

Reviewed only `node/ab-ar1-s` at `66b9e5fa5cf68b4ecef8b4c413b6d04f7fcbde38`, diffed against r2 head `862ca3f6de16d603b69395c9c5cfdcc574a42958`. This is the requested narrow confirmation of F7 and F8 and regressions introduced by their fixes, against AB-REVIEW.md, REVIEW-COMMON.md, AB-AR1.md and PLAN §4 AR1. Closed findings were not reopened. No other arm's implementation or review files were inspected. The implementer's worktree was not modified.

The delta comprises two commits and six files, with 103 insertions and 13 deletions:

- `b9ec2d4c`: preserve the explicit blocksdir cache on a status exception.
- `66b9e5fa`: convert public config text, object keys and non-Unix OS text; retain internal escaped-input paths; document the boundaries and add the F7 regression.

Reference: Dash Core `v24.0.0-rc.3`, peeled commit `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`. Reference files and the UniValue writer used for independent comparison were extracted from that tag with `git show`, rather than relying on the Core checkout's current branch.

## Confirmation

**F7 — closed.** `args.rs:1074` converts both config text and its filename once. Its internal `read_escaped_config_stream` receives file contents already converted by `open_text`, and filenames already converted from paths or obtained from stored includeconf values. Both primary-file and include-file callers use that internal variant, preserving real escape-range characters and invalid UTF-8 bytes without a second conversion.

`value.rs:166` converts public `push_kv_end` keys once. `settings.rs:193` uses `push_escaped_kv_end` for the map's already encoded keys, preserving keys returned by `read_settings`. The JSON reader retains its direct encoded-key construction; it does not route decoded JSON keys back through the public text method. The generated `_warning_` key remains ASCII and safely uses the public method. The non-Unix `from_os_str` at `bytestr.rs:91` now converts the lossy Unicode result through `from_bytes`, protecting real characters in U+10FF00–U+10FFFF.

The original U+10FF22 config-to-settings reproduction and public object-key reproduction pass. Independent probes extend them to **all 256 characters** in the escape range. No unreadable JSON, byte loss or double conversion was observed. The bytestr documentation and KNOWN_GAPS.md now name the changed public text boundaries.

**F8 — closed.** `args.rs:909` distinguishes normal nondirectory status from a thrown status error. On error, it stores the explicit absolute blocksdir before returning the error; the next getter returns that cached directory without checking status or adding network/blocks suffixes. Although assignment occurs in the Rust error arm rather than literally before the status call, the tested sequential success, false and exception outcomes match Core's `util/system.cpp:336`–`:361`. A normal nondirectory result leaves the cache empty, permitting a later successful retry. Clearing the cache restores the status check. Absolute and relative ENAMETOOLONG paths behave correctly, including with regtest selected.

## Independent validation

Used a detached worktree at `/work/scratch/ab-review-ar1-s-r3/wt`, its own `target/`, `TMPDIR=/dev/shm/ab-review-ar1-s-r3` (mode **700**) and `CARGO_BUILD_JOBS=6`.

- `cargo test -p dash-args`: **99 passed, 0 failed**; 0 doctests. Both new named regression tests passed. The crate declares no feature flags.
- `cargo fmt --all --check`: PASS.
- `cargo clippy -p dash-args --all-features --all-targets -- -D warnings`: PASS.
- `git diff --check 862ca3f6 66b9e5fa`: PASS.
- Independent external Rust probe suite, linked to this head's compiled library: **6 passed, 0 failed**. Covers every escape-range character through public config values, error strings, section names and filenames, public object keys, settings read/write keys, include filenames and contents, Unix OS strings, and the blocksdir retry/cache cases. Includes invalid-byte primary config names, include names, section names and values to check that internal inputs are not converted twice.
- Pinned Core UniValue writer differential: **259 inputs, 0 differences** in either compact or pretty output. Inputs comprise every escape-range character as both an object key and a value, plus ASCII, ordinary Unicode, and quote/backslash/control-character text. Rust values were built through the public text constructors; byte serialization and byte-reader round trips passed.
- Isolated non-Unix-branch experiment: **3 passed, 0 failed**, including all 256 escape-range characters through OS-text conversion and path reconstruction. A scratch copy of bytestr selected its existing non-Unix branches on Linux. This tests the changed conversion expression, **not a native Windows build or run**.

One initial scratch-probe assertion put its primary-file `uacomment` in an unknown section, where the default-network getter correctly returned empty. Correcting that reviewer fixture produced the six-probe pass above; it was not a product regression.

Ten focused Core behavior checks support the confirmation without re-reviewing closed findings:

1. Config stream option values retain their input bytes (`GetConfigOptions`, `ReadConfigStream`).
2. Section names and source filenames retain their bytes and line numbers (`GetConfigOptions`).
3. Unknown-option and malformed-line diagnostics preserve the offending text (same functions).
4. Primary config files reach the stream parser without a second text conversion (`ReadConfigFiles`).
5. Include filenames and contents preserve bytes through resolution and parsing (`ReadConfigFiles`).
6. Object insertion preserves key bytes and insertion order (`UniValue::pushKVEnd`).
7. Compact and pretty JSON emit escape-range UTF-8 unchanged (pinned `univalue_write.cpp`, independent differential).
8. Settings map keys are passed unchanged into object insertion (`util::ReadSettings`, `util::WriteSettings`).
9. Explicit blocksdir status exceptions leave the absolute directory cached (`GetBlocksDirPath`).
10. Normal nondirectory status clears/leaves an empty cache; successful retry appends the network and blocks directories (same function).

## Limits and findings

No new blocker, major, minor or nit was found in the reviewed delta. F7 and F8 are both closed.

The documented escaped-String contract still applies: `write()` returns escaped storage, while `SettingsValue::read` consumes fresh bytes. Round trips must use `write_pretty_bytes` or decode `write()` with `bytestr::to_bytes`; feeding escaped output directly into a text boundary can convert it twice. This is the existing disclosed API limit from r2, not an outstanding instance of the repaired F7 input-boundary defect. Native Windows validation and previously disclosed platform/error limits remain outside this narrow confirmation.

The claimed full gate (**exit 0; 3,227 passed; one BL-20 dashd failure followed by a 54/0 passing rerun**) was not independently rerun. The reviewer independently ran the touched-crate gates above; this verdict does not attest to the full workspace gate.

The detached worktree, scratch probes, extracted Core sources, build artifacts and owned /dev/shm temporary directory were removed. No commits, pushes or GitHub comments were made. This uncommitted review document is the only retained output.

**Verdict: APPROVE**

blockers=0 majors=0 minors=0 nits=0
