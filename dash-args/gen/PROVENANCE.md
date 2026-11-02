# Provenance of dash-args' generated code and test data

Everything here comes from Dash Core **v24.0.0-rc.5** (tag object `3f3373a11eb6af4d643c187a69c14f391b8e22a8`,
commit `8163290aa0307ed4bc2fc0ba15b78c20d61dfa88`). Nothing is hand-written: the option tables are
generated from Core's sources and its release binaries' help, and the test data is what the
release binaries printed.

## Inputs

| Input | Where it came from |
|---|---|
| Core sources (`src/`, `test/functional/`) | `git archive v24.0.0-rc.5 src test/functional` from a clone of `github.com/dashpay/dash`; the tag resolves to the tag object and commit above |
| Release binaries | `dashcore-24.0.0-rc.5-x86_64-linux-gnu.tar.gz` from the GitHub release, sha256 `54cc952f79e4733d20b950c93b9c0824ae4410acf9a4972cd9361e2a107cb62d`, matching the release's signed `SHA256SUMS.asc` (`capture_core_outputs.py` refuses any other file) |
| Windows release | `dashcore-24.0.0-rc.5-win64.zip` from the GitHub release, sha256 `e6087458b618dd260a40c04257e88ed6f46da03e40f955d7a8cf1bd0937ef7f6`, matching `SHA256SUMS.asc` (the capture refuses any other file); run under Wine 10.0 (Ubuntu 26.04's `wine64`, in a container without network) |
| Extra X libraries for dash-qt (only if the host lacks them) | Ubuntu's `libxcb-icccm4 libxcb-image0 libxcb-keysyms1 libxcb-render-util0 libxcb-xinerama0`, unpacked with `dpkg -x` and passed as `--qt-libs` |

## Commands

```sh
# Core sources.
git -C <dash clone> archive v24.0.0-rc.5 src test/functional | tar -x -C <core>

# Capture the release binaries' outputs (dash-qt runs under xvfb-run).
python3 -I dash-args/gen/capture_core_outputs.py \
    --tarball dashcore-24.0.0-rc.5-x86_64-linux-gnu.tar.gz --core-src <core>/src \
    --work <empty scratch dir> --out dash-args/tests/data/core-v24 [--qt-libs <dir>]

# Capture the Windows release's outputs, with wine on PATH and a fresh WINEPREFIX.
python3 -I dash-args/gen/capture_core_outputs.py \
    --win-zip dashcore-24.0.0-rc.5-win64.zip \
    --work <empty scratch dir> --out dash-args/tests/data/core-v24

# Generate the option tables.
python3 -I dash-args/gen/gen_options.py --core-src <core>/src \
    --help-dir dash-args/tests/data/core-v24/help \
    --help-dir-windows dash-args/tests/data/core-v24/help-win64 \
    --out dash-args/src/options/generated.rs

# libstdc++'s Windows path results for src/paths.rs's tests (prints them; run under Wine
# from a directory two levels deep, as the expected `absolute` results assume `Z:\p\sub`).
x86_64-w64-mingw32-g++ -std=c++17 -static -o probe.exe dash-args/gen/windows_paths_probe.cpp
wine probe.exe x 'Z:x' 'Z:..\x' '\x' '/x' '\\\x' '\\server\share' '\\?\Z:\x' 'Z:\x' . x/

# msvcrt's _wstat st_mode/st_dev (run in an empty directory Z:\p\run with a subdirectory x)
# and Core's UniValue float texts, for src/paths.rs's and src/univalue.rs's tests.
x86_64-w64-mingw32-g++-posix -std=c++17 -O2 -static -o stat.exe dash-args/gen/msvcrt_stat_probe.cpp
U=<core>/src/univalue
x86_64-w64-mingw32-g++-posix -std=c++17 -O2 -static -I$U/include -I$U/lib \
    -o float.exe dash-args/gen/univalue_float_probe.cpp $U/lib/univalue*.cpp
```

The capture reads `gen_options.py`'s parse of the sources to know which option names to try.
The checked-in files were produced by these commands on x86_64 Ubuntu 26.04; re-running them
reproduces the files byte for byte. The path tables in `src/paths.rs` were printed by the
probe built with MinGW-w64 g++ 13 (win32 threads); the msvcrt stat table and the Windows
float texts by the other two probes (g++ 13, POSIX threads, as Core's releases use) under
Wine 10, the float probe also built for Linux with g++. The probes were last run at
v24.0.0-rc.3; rc.4 and rc.5 leave `src/univalue` unchanged. Run the Windows capture with
`XDG_RUNTIME_DIR` set, or Wine's own messages land in stderr. Wine can also crash after a case
and print `wine: Unhandled page fault …` into its stderr in `init_cases_win64.json`; re-run the
capture in fresh `--work` and `WINEPREFIX` directories if it does.

## Generated code: `src/options/generated.rs`

`gen_options.py` parses the bodies of Core's setup functions (`SetupServerArgs`,
`AddLoggingArgs`, `AddWalletOptions`, `SetupChainParamsBaseOptions`, `SetupChainParamsOptions`,
`SetupUIArgs`, `SetupCliArgs`, `SetupBitcoinTxArgs`, `SetupWalletToolArgs`,
`SetupBitcoinUtilArgs`) and emits one Rust function per Core function with the same
`AddArg` / `AddHiddenArgs` / `AddCommand` calls in the same order.

- **Build configuration.** `#if` blocks are evaluated for the release build: wallet (BDB and
  SQLite), ZMQ, UPnP, NAT-PMP, external signer, `HAVE_SYSTEM`, `sockaddr_un` and `fork` all
  on. That is the Linux and macOS configuration. Windows lacks `sockaddr_un` and `fork`, and
  `configure` turns the external signer off for mingw; the generator evaluates that
  configuration too and emits the differences under `#[cfg(windows)]` /
  `#[cfg(not(windows))]` (the `-onion` and `-proxy` parameters and help, `-daemon` /
  `-daemonwait` hidden, no `-signer`).
- **Help texts.** Core builds most help texts at runtime (`strprintf` with defaults,
  translated strings), so the generator takes the texts of visible options from the captured
  `-help` / `-help-debug` output of the Linux and Windows releases, unwrapping each by
  walking the options in the order the table prints them, and checks that `FormatParagraph`
  re-wraps it to the captured lines and that each configuration re-renders its captures
  byte for byte. Hidden options have no help text: Core never prints it.
- **Platform-dependent defaults.** `-uiplatform` (`GUIUtil::defaultUIPlatform`), the GUI
  font weights (`TARGET_WEIGHT_*` through `mapWeightArgs`) and the `-dbcache` maximum
  (`nMaxDbCache`, by pointer size) depend on the platform Core was built for. The generator
  reads every platform's value from those sources (failing if their form changes), checks
  the Linux and Windows values against the captures, and emits one registration per
  platform (`target_os = "macos"`, `windows`, `target_pointer_width`). The macOS and 32-bit
  values are taken from the source only.
- **Checks.** The generator fails on any setup call it cannot parse, on any other use of
  the `ArgsManager` in a setup function (a call to an unknown helper could hide options), on
  a preprocessor macro neither configuration names, on a help text that does not round-trip,
  and if `SetupHelpOptions` no longer matches the hand port in `src/args.rs`.

## Test data: `tests/data/core-v24/`

| File | Produced by |
|---|---|
| `help/*.txt` | `-help` and `-help-debug` of dashd and dash-qt, `-help` of dash-cli, dash-tx, dash-wallet and dash-util, with `HOME` set to an empty scratch home (so defaults that print paths are stable) |
| `args_merge.txt`, `chain_merge.txt`, `settings_merge.txt` | the release `test_dash` running `argsman_tests/util_ArgsMerge`, `argsman_tests/util_ChainMerge` and `settings_tests/Merge` with `ARGS_MERGE_TEST_OUT`, `CHAIN_MERGE_TEST_OUT` and `SETTINGS_MERGE_TEST_OUT` set; the Rust ports hash the same output and compare against these files |
| `recognized.txt` | every option name in the generated tables, run against each program as `<prog> -<name>=1 -version` (commands as `<prog> -version <command>`), with the verdict of Core's parser (a run killed by a signal stops the capture); an unknown option is checked to be rejected first |
| `help-win64/*.txt` | the same programs' help from the win64 release under Wine (no dash-qt: on Windows it shows its help in a dialog), read with universal newlines (CR LF and CR as LF) |
| `init_cases_win64.json` | the `CASES` that name `-datadir`, then `WIN_CASES` (drive-relative, root-relative and verbatim paths, configs hard-linked as `.conf` and `.exe`, Ctrl-Z and CR LF in config and settings files), run with the win64 dashd under Wine; `{T}` is the case's directory as a Windows path (`{D}` its drive, `{R}` the rest); files keep their CR LF; stderr is read with universal newlines |
| `init_cases.json` | 74 dashd command-line and config-file cases (`CASES` in `capture_core_outputs.py`), each run as `dashd -version ...` in a fresh directory with `HOME` inside it: the exit code, stderr, and the files created, changed and removed, with the directory replaced by `{T}` |

`-version` makes dashd exit right after config initialization, so no case starts a node. It
comes first on the command line because the parser stops at a lone `-` or a non-option
token.

## Regenerating for another release

Change `CORE_TAG`, `CORE_COMMIT` (gen_options.py), `TARBALL_SHA256` and `VERSION_DIR`
(capture_core_outputs.py), re-run both commands, then fix whatever `cargo test -p dash-args`
reports. Any new kind of setup call makes the generator fail rather than drop it.
