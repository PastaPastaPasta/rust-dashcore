# A/B calibration review: AB-AR1-o, round r2

**Verdict: CHANGES-REQUIRED.** The original major is fixed. The original reproductions for F1–F7 now pass, and the Windows evidence is reproducible. Two minor fidelity gaps remain within F3 and F6.

Reviewed only `node/ab-ar1-o`, head **`cf77c41da968efd453ececcac81c810f29aec129`**, the six commits after `3eaa005f5557a12b6643461a13585b9ed3fb6190`. Followed `AB-REVIEW.md`, `REVIEW-COMMON.md`, `AB-AR1.md`, PLAN §4 AR1 and the requested narrow r2 scope. Review used a detached worktree at `/work/scratch/ab-review-ar1-o-r2/wt`; no changes were made to the implementer's worktree. No other arm's source or review files were inspected. This report is uncommitted; no commits, pushes or GitHub comments were made.

Core reference: **v24.0.0-rc.3**, `da567d7dba945143eee4d2ce1621c21b32089989`, freshly extracted with `git archive` rather than relying on the ordinary Core checkout's HEAD.

## Findings

### F3-r2 — minor: the new Windows equivalence check omits Core's stat comparison

**Location:** `dash-args/src/paths.rs:402`, used by `dash-args/src/init.rs:124`.

The ordinary `dash.conf` / `other.conf` hard-link reproduction is fixed. However, GCC 13.3.0's `std::filesystem::equivalent` first compares `_wstat`'s `st_mode` and `st_dev`, then compares volume serial and file index. The port implements only the latter comparison. Windows stat mode depends on the filename's executable extension, so identical file identity alone does not reproduce Core's result.

**Independent reproduction against the actual SHA256-verified Win64 release under Wine 10:** create an empty `D\dash.conf`, hard-link it as `D\other.exe`, and run:

```text
dashd.exe -version -datadir=D -nosettings -conf=D\other.exe
```

Core exits **1** with the full `Data directory "D" contains a "dash.conf" file which is ignored...` error. A Windows executable linked to the unmodified reviewed Rust library, calling `Program::Dashd.setup_args`, `parse_parameters` and `init_config`, exits **0**, with no error. The control using `other.conf` exits **0** in both implementations. These were separate scratch cases, with absolute `D` paths on Wine's Z drive; neither starts a node.

**Suggested fix:** reproduce libstdc++'s stat existence/mode/device checks before its handle-identity comparison. Add a release-generated case for the executable-extension hard link alongside the successful `.conf` case. `ARCHITECTURE.md` discloses this exception, but the brief's Core-fidelity requirement still applies. F3 is therefore only partially closed.

### F6-r2 — minor: the negative-NaN error still uses Linux spelling on Windows

**Locations:** `dash-args/src/univalue.rs:869`, exposed by `from_f64` at line 94; the incorrect Windows expectation is in `dash-args/src/univalue.rs:947`.

The fallible constructor and removal of `From<f64>` fix the invalid-settings-file defect. The error text is not yet platform-faithful: the Windows MinGW implementation formats a negative NaN as `nan`, while the port unconditionally preserves its sign as `-nan`.

**Independent reproduction:** compile the exact tagged Core `univalue.cpp`, `univalue_read.cpp`, `univalue_write.cpp` and `univalue_get.cpp` with MinGW GCC 13 and run under the same isolated Wine 10 environment. Calling `UniValue(-std::numeric_limits<double>::quiet_NaN())` throws:

```text
The string 'nan' is not a valid JSON number
```

The probe explicitly confirms `std::signbit(input) == true`. Both the POSIX-thread and Win32-thread MinGW builds give `nan`; positive/negative infinity give `inf`/`-inf`. The actual Windows Rust test binary passes its assertion that `SettingsValue::from_f64(-f64::NAN)` returns `The string '-nan' is not a valid JSON number`, confirming the mismatch. This float probe is compiled pinned Core source, not an invocation of a float-construction API in the release dashd.

**Suggested fix:** use the Windows spelling for negative NaN and keep the Linux spelling on Linux. Make the regression expectations platform-specific and derive the Windows expectation from the Core probe. F6's persistence defect is closed, but its claimed byte-identical error is only partially closed.

## Closure checks

| r1 finding | r2 result |
|---|---|
| F1, Windows drive-relative resolution and joining | **Closed.** The Windows-only absolute-path test runs; drive-relative datadir/config/include/settings cases replay successfully. The UNC/verbatim claim matches the actual release, as detailed below. |
| F2, shell application-data folder | **Closed.** The port calls `SHGetSpecialFolderPathW(CSIDL_APPDATA, create=true)` and emits Core's exact failure log. The Windows-only test removes APPDATA and still obtains the absolute shell-folder datadir. |
| F3, hard-link identity | Original `.conf` reproduction **fixed**; remaining mode/device parity gap is **F3-r2**. |
| F4, Windows text I/O | **Closed.** Ctrl-Z input termination, CR LF input translation and CR LF settings output are exercised by the Windows replay, which compares the stored file bytes. Fresh Core captures reproduce these expectations. |
| F5, platform-dependent help | **Closed.** UI platform, normal font weight and pointer-size dbcache variants are source-derived, generated and tested; Windows excludes signer. All six Windows and eight Linux captured help bodies match. macOS/32-bit values have source evidence, not native release execution. |
| F6, non-finite float construction | Invalid persistence **fixed**; remaining Windows negative-NaN message gap is **F6-r2**. |
| F7, cross-target clippy | **Closed.** All-target clippy with warnings denied passes on Windows GNU, Windows MSVC and i686 Linux. |

## Actual Win64 toolchain and filesystem behavior

The independently downloaded `dashcore-24.0.0-rc.3-win64.zip` has SHA256 **`bd92e1d5682f8a2f3d4cd17d7aa52391317d1053146a098393242003de8aea05`**, matching the independently downloaded release `SHA256SUMS.asc` and the capture's enforced digest. This checks the digest comparison, not the signature's trust chain.

The release's `dashd.exe` contains `COMPILER=gcc-13.3.0` and `GCC: (GNU) 13.3.0`. Its PE imports include msvcrt and Windows system DLLs, with **no libstdc++ DLL**. The pinned release manifest selects GCC **13.3.0**, the MinGW **POSIX-thread** toolchain, and a GCC patch affecting debug-path remapping, not filesystem semantics. Core's `util/fs.h` delegates to `std::filesystem`; its wrapper does not supply another root-name parser. The release therefore uses the statically linked GCC 13.3.0 libstdc++ implementation.

I independently fetched GCC 13.3.0's `fs_path.h`, `fs_path.cc` and `fs_ops.cc` and checked the relevant implementations. The `//server` root-name parser is enabled for **Cygwin**, not MinGW. MinGW recognizes a two-unit `X:` root name; UNC and verbatim prefixes have a root directory but no root name and fail `is_absolute()`. `absolute()` skips all but one leading separator before calling `GetFullPathNameW` (PR 88884). The reviewed helpers implement these rules.

Actual release tests corroborate this beyond the implementer's local MinGW probe:

- A `-datadir=\\dev\shm\...\d` UNC-shaped name resolves to the existing **Z-drive local directory**, and both Core and the port succeed.
- An existing config named using `-conf=\\?\Z:\dev\shm\...\c.conf` fails in both with the computed path **`Z:\?\Z:\dev\shm\...\c.conf`**, rather than being treated as a verbatim absolute name.
- All 82 freshly recaptured release init cases, including same-drive relative paths, root-relative/double-separator config paths and the verbatim datadir failure, are **byte-identical** to `init_cases_win64.json`.

The UNC/verbatim claim is sound for this particular Core build. The architecture document also records broader `_wstat` versus Rust metadata/opening differences; this narrow review does not certify arbitrary long-path or access-control parity.

Direct source spot-checks for the fixes covered more than ten behaviors: Core's filesystem wrapper; root-name parsing; `is_absolute`; path concatenation; absolute resolution; `GetPathArg` normalization/trailing separators; `CheckDataDirOption`; shell-folder lookup/failure logging; equivalence; text-mode config input; settings input/output; `setFloat`/number validation; UI platform/font defaults; pointer-size dbcache limits; and configure's Windows signer exclusion. Closed parsing/precedence areas were not re-reviewed.

## Independent gates and Windows evidence

Rust builds/checks used `CARGO_BUILD_JOBS=6`, with `TMPDIR=/dev/shm/ab-review-ar1-o-r2-tmp` (mode **700**), in the detached worktree. Long jobs ran in reviewer-owned user systemd units.

- `cargo test -p dash-args`: **PASS, 68 passed, 0 failed**, no ignored tests or doctests: library 20, argsman 14, functional behaviors 14, getarg 8, help 7, init replay 1, settings 4. The Linux init replay covers 74 cases.
- `cargo fmt --all -- --check`: **PASS**.
- `cargo clippy -p dash-args --all-features --all-targets -- -D warnings`: **PASS**.
- The same clippy command with `--target x86_64-pc-windows-gnu`, `x86_64-pc-windows-msvc` and `i686-unknown-linux-gnu`: **PASS**, each exit 0.
- `cargo test -p dash-args --target x86_64-pc-windows-gnu --no-run`: **PASS**. Every resulting **PE `.exe` test binary** was then executed under Wine **10.0**, in a Docker container with **`--network none`**: **68 passed, 0 failed** (library 21, argsman 14, functional behaviors 13, getarg 8, help 7, init replay 1, settings 4). Windows-only `windows_absolute_matches_libstdcxx` and `windows_default_data_dir_uses_the_shell_folder` execute, the Unix filesystem-error test is absent, and the init test explicitly prints **`82 init cases match Core`**. This establishes that the evidence exercises Windows cfg branches, not a Linux executable or a no-op Windows suite.
- Wine's launcher crashes with the private POSIX TMPDIR set in this environment. A reviewer-only wrapper unsets TMPDIR for the launcher; Python/build processes retain the private TMPDIR. The Wine user profile was moved into that same private `/dev/shm` directory and symlinked from the isolated prefix, and all 68 Windows tests passed again there. Thus Windows tempfile paths are backed by private shared memory. No test started a node or used shared chain/oracle datadirs.
- Fresh `capture_core_outputs.py --win-zip ...` execution: **PASS**; all **six help files and all 82 init cases byte-identical** to the checked-in Windows captures. Help is compared after the documented universal-newline decoding; settings/config file contents preserve their stored CR LF.
- `gen_options.py` against the freshly archived tagged Core sources, with both help directories: **PASS**, regenerates `generated.rs` byte-identically.

No Linux fidelity regression was found in the changed paths, settings writer, finite float construction or generated option registrations. The Linux help bodies and init fixtures remain covered by the passing suite. The full-workspace gate claim (**exit 0, 3196 passed, with one BL-20 failure passing on its 54/0 rerun**) was **not independently rerun** in this narrow confirmation; it remains implementer evidence.

The detached worktree, reviewer scratch assets, temporary Wine image and private temporary directories were cleaned up after writing this report.

## Reviewed commits

```text
ad72d92b fix(dash-args): refuse non-finite floats as Core's UniValue does
f7ed974b fix(dash-args): follow Core's Windows build in paths, datadir and file I/O
d551f8e4 test(dash-args): capture the win64 release's outputs under Wine
cd6550f4 fix(dash-args): emit platform-dependent help defaults per platform
dfc3c6bf test(dash-args): replay the init cases against the win64 release
cf77c41d docs(dash-args): describe the Windows behaviour and its provenance
```

**Verdict: CHANGES-REQUIRED.**

blockers=0 majors=0 minors=2 nits=0
