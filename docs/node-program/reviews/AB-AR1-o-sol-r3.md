# A/B calibration review: AB-AR1-o, round r3

**Verdict: APPROVE.** F3-r2 and F6-r2 are closed. No new findings in the requested narrow confirmation.

Reviewed only `node/ab-ar1-o`, head **`00839cf35e378fb6bb4424ccdd66c6396a092047`**, the three commits on `cf77c41da968efd453ececcac81c810f29aec129`:

```text
b9baf97f fix(dash-args): compare msvcrt stat results in Windows equivalent
d47a99e5 fix(dash-args): spell a negative NaN as Core's Windows build does
00839cf3 docs(dash-args): describe the msvcrt stat comparison and the new probes
```

Followed `AB-REVIEW.md`, `REVIEW-COMMON.md`, `AB-AR1.md`, PLAN §4 AR1, the r2 review, and the prompt's narrow r3 scope. Used a detached worktree at `/work/scratch/ab-review-ar1-o-r3/wt`. No other arm's source or reviews were inspected. No implementation edits, commits, pushes, or GitHub comments were made. This report is uncommitted.

## Findings

None.

## F3-r2 — closed: Windows equivalence includes Core's stat comparison

**Locations:** `dash-args/src/paths.rs:121` and `:449`; regression capture in `dash-args/tests/data/core-v24/init_cases_win64.json:1372`.

The port now compares mode and device before volume serial and file index. The mode calculation includes file/directory type, read permission, write permission unless read-only, and execute permission for directories and the case-insensitive executable suffixes `.exe`, `.bat`, `.cmd`, and `.com`. The device is the path's drive number, with the current-drive fallback.

I independently rebuilt and ran `gen/msvcrt_stat_probe.cpp`. Its explicit `GetProcAddress(msvcrt.dll, "_wstat64")` call reproduced the checked-in mode/device observations: regular and executable files, uppercase suffix, all four executable suffixes, non-executable longer suffixes, read-only file, directories (including a directory ending in `.exe`), relative path with `..`, lowercase drive, and C: versus Z: device numbers. These checks cover more than ten named behaviors without reopening the rest of AR1.

The new `win_hardlinked_conf_exe` case is generated from the actual release, not a hand-authored expected error. In the fresh independent capture, hard-linking `dash.conf` as `other.exe` makes Core exit **1** with the ignored-config error; the Windows Rust replay matches its complete stderr and file effects. The `other.conf` hard-link control still succeeds. All **83/83** Windows init cases match.

Wine 10's short-relative-name extension quirk is reproducible: its `_wstat64` does not set execute bits for the short `a.exe` name, but does for the absolute path. Wine's tagged source explicitly gates this check on path length greater than six. The architecture document acknowledges this difference; Core's config comparison uses absolute paths, so it does not affect these closure cases. This review does not claim native Windows execution of the short-name behavior.

## Release linkage: msvcrt, not UCRT

Independently downloaded `dashcore-24.0.0-rc.3-win64.zip` and the release's `SHA256SUMS.asc`. The archive SHA256 is **`bd92e1d5682f8a2f3d4cd17d7aa52391317d1053146a098393242003de8aea05`**, matching the published checksum and the capture script's enforced digest. This verifies the checksum comparison, not the signing-key trust chain.

The actual release `dashd.exe` PE import table directly lists **`_wstat64` under `msvcrt.dll`**. It imports no `ucrtbase.dll`, UCRT API-set DLL, or libstdc++ DLL. Its compiler metadata says **GCC 13.3.0**.

Checked GCC 13.3.0's `libstdc++-v3/src/filesystem/ops-common.h`: Windows `posix::stat` calls `::_wstat64` with `__stat64`. Its `src/c++17/fs_ops.cc` equivalence implementation compares `st_mode` and `st_dev` before opening handles and comparing volume serial and file index. The tagged Core filesystem wrapper delegates to `std::filesystem`; its tagged Guix manifest selects GCC 13.3.0 and the MinGW POSIX-thread toolchain. Thus the explicit msvcrt probe reflects the CRT function used by the Win64 release. A host MinGW default to UCRT would not establish this by itself.

## F6-r2 — closed: platform-specific negative-NaN error

**Locations:** `dash-args/src/univalue.rs:871` and `:945`.

The Windows branch now formats signed NaN as `nan`; Linux retains `-nan`. The regression test checks the full constructor error for both signs of NaN and infinity.

I freshly extracted Core **v24.0.0-rc.3**, `da567d7dba945143eee4d2ce1621c21b32089989`, and rebuilt `gen/univalue_float_probe.cpp` against its four UniValue implementation files. Linux gives:

```text
The string '-nan' is not a valid JSON number
```

Both GCC 13 MinGW **POSIX-thread and Win32-thread** builds, run under Wine, give:

```text
The string 'nan' is not a valid JSON number
```

Both Windows probes report the input sign bit as **1**. Positive NaN and positive/negative infinity also agree with the updated tests, and the nine finite-value observations agree across the three probes. The rebuilt POSIX probe imports `msvcrt.dll`, matching Core's release CRT, rather than UCRT. The actual Windows Rust `non_finite_floats_are_refused` test passes.

As in r2, this is a probe of pinned Core UniValue source with the corresponding toolchain/CRT, not a float-construction endpoint invoked in release dashd.

## Independent validation

Rust checks ran with **`CARGO_BUILD_JOBS=6`** and **`TMPDIR=/dev/shm/ab-review-ar1-o-r3-tmp`**, mode **700**, in the detached worktree. Rust gates and Wine setup/runtime jobs used reviewer-owned user systemd units. Windows execution used Wine **10.0** in a Docker container with **`--network none`**; its prefix, profile, capture cases, and Windows temporary files lived in the private shared-memory directory. A reviewer wrapper unsets POSIX TMPDIR only for the Wine launcher.

- `cargo test -p dash-args`: **PASS, 69 passed, 0 failed**, no ignored tests; library 21, argsman 14, functional behaviors 14, getarg 8, help 7, init replay 1, settings 4. Zero doctests. Linux init replay covers 74 cases.
- `cargo fmt --all -- --check`: **PASS**.
- `cargo clippy -p dash-args --all-features --all-targets -- -D warnings`: **PASS**.
- The same clippy command targeting `x86_64-pc-windows-gnu`, `x86_64-pc-windows-msvc`, and `i686-unknown-linux-gnu`: **PASS**, each exit 0.
- `cargo test -p dash-args --target x86_64-pc-windows-gnu --no-run`: **PASS**. Executed every resulting PE test binary under Wine: **69 passed, 0 failed**; library 22, argsman 14, functional behaviors 13, getarg 8, help 7, init replay 1, settings 4. The init test explicitly reports **83 init cases match Core**.
- Fresh `capture_core_outputs.py --win-zip ...`: **PASS**. All six help files and the full 83-case init JSON are **byte-identical** to the reviewed fixtures. Compared the old fixture with the new fixture excluding the added case: all **82 earlier cases are unchanged**, confirming the Wine HOME fix did not alter their expectations.

The claimed full-workspace gate **exit 0, 3198/0** was **not independently rerun** in this narrow confirmation; it remains implementer evidence. No whole-task recertification or native Windows/macOS execution is claimed. Previously documented unrelated gaps remain outside this round; no additional deviations were found in the two fixes.

The detached worktree, reviewer scratch files, private temporary directory, and reviewer-created Wine container/image were removed after the report was written.

**Verdict: APPROVE.**

blockers=0 majors=0 minors=0 nits=0

