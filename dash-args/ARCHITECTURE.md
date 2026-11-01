# dash-args architecture

A port of Dash Core's argument and configuration handling at **v24.0.0-rc.3** (DEC-82: v24
only). It is a shared library (A1 §1.1): it has no dependencies and uses neither the node nor
the SPV stack, so programs of either kind can read `dash.conf`, `settings.json` and the command
line exactly as Core does.

## Mapping to Core

| Module | Core source | What it holds |
|---|---|---|
| `args` | `util/system.{h,cpp}` | `ArgsManager`: parsing, getters and setters, config files and `includeconf`, `settings.json` paths and I/O, datadir and config-path resolution, help output, `LogArgs`, `HelpRequested`, `SetupHelpOptions`, `CheckDataDirOption`, `AbsPathForConfigVal` |
| `settings` | `util/settings.{h,cpp}` | `Settings` and the merge functions (`GetSetting`, `GetSettingsList`, `OnlyHasDefaultSectionSetting`), `ReadSettings`, `WriteSettings` |
| `univalue` | `univalue/` | the part of `UniValue` settings use: the JSON reader and writer and the strict getters |
| `init` | `common/init.cpp` (`InitConfig`), `init.cpp` (`AppInitParameterInteraction`'s config checks), `noui.cpp` | `init_config` and the error and warning texts programs print |
| `chain` | `chainparamsbase.{h,cpp}` | chain names, the datadir subdirectory and the default ports |
| `paths` | `util/fs.h`, `fs_helpers`, libstdc++ `std::filesystem` | `lexically_normal`, `absolute`, `GetDefaultDataDir`, `equivalent` and libstdc++'s `filesystem_error` text |
| `strencodings` | `util/strencodings.*`, `util/string.h` | `TrimString`, `LocaleIndependentAtoi`, `InterpretBool`, `FormatParagraph`, `MakeUnorderedList` |
| `options` | `init.cpp`, `init/common.cpp`, `wallet/init.cpp`, `chainparamsbase.cpp`, `chainparams.cpp`, `qt/bitcoin.cpp`, `bitcoin-cli.cpp`, `bitcoin-tx.cpp`, `bitcoin-wallet.cpp`, `bitcoin-util.cpp` | the option tables, generated (`gen/PROVENANCE.md`), and `Program::setup_args` for dashd, dash-qt, dash-cli, dash-tx, dash-wallet and dash-util |

Function names follow Core in snake case, and each public item names its Core counterpart.

## Design

- **Errors.** Where Core throws `std::runtime_error`, the port returns
  `Err(RuntimeError(what()))` with the same text; where Core returns `false` with an error
  string, the port returns `Err(String)`. Where Core asserts or throws `std::logic_error`
  (programming errors such as registering an option twice), the port panics.
  `ReadConfigFiles` keeps the distinction Core's callers see: `ReadConfigError::Failed` for a
  `false` return and `ReadConfigError::Exception` for a throw (InitConfig words them
  differently).
- **Byte-identical messages.** Every message, log line and file Core writes is reproduced
  byte for byte, including libstdc++'s `filesystem_error` text
  (`filesystem error: <op>: <strerror> [<path>]`) and Core's quirks (an empty reason when the
  default datadir is missing and Core cannot create its empty `dash.conf`).
- **Locking.** `ArgsManager` keeps its state behind one `Mutex`, as Core's `cs_args`; all
  methods take `&self`. Core's `cs_args` is recursive and this `Mutex` is not, so the
  closure passed to `lock_settings` and the log and stderr sinks must not call back into the
  same `ArgsManager` (they would deadlock where Core would not). The chain's base parameters, a global in Core, are stored in the
  `ArgsManager` (`select_base_params`, `base_params`).
- **Output.** `LogPrintf` lines go to a log sink that buffers until one is installed, as
  Core's logger does before it opens `debug.log`. What Core writes to `std::cerr`
  (`includeconf` warnings) goes to a stderr sink, `eprint!` by default.
- **Callbacks.** `init_config` takes the chain-parameter selection as a callback, so a
  program can select its full parameters (`dash-params`) and fail as Core's `SelectParams`
  does; `init::select_base_params` covers programs that only need the base parameters. The
  GUI's settings-error prompt is another optional callback.
- **Option tables.** Generated, never edited by hand (`#[rustfmt::skip]`). The generator
  checks every help text round-trips through `FormatParagraph`, and the tests rebuild every
  program's help from the tables and compare it with the release binaries' output.
- **Portability (DEC-84).** Platform differences are `cfg`s with an alternative for each
  platform: the default datadir (the shell's application data folder on Windows, macOS,
  other Unix), `equivalent` (file identity: device and inode, or msvcrt's stat mode and
  drive and then volume serial and file index), Windows path handling, text-mode file I/O on Windows, and the option tables'
  Windows variants and platform-dependent help defaults.
- **Windows paths** follow MinGW's libstdc++, which Core's Windows releases use: only `X:`
  is a root name (so `\\server\share` and `\\?\` paths are relative), `absolute` resolves a
  relative path with `GetFullPathNameW` (per-drive current directories), and `AbsPathJoin`
  uses libstdc++'s `operator/` (`C:\d` / `C:x` is `C:\d\x`). `paths::win` implements these
  for any host, so their tests run on Linux against libstdc++'s own results
  (`gen/windows_paths_probe.cpp`).
- **Windows text mode.** Core reads config and settings files and writes `settings.json`
  with text-mode streams; on Windows the port reads up to the first Ctrl-Z with CR LF as
  LF, and writes LF as CR LF.

## Tests

| Test | Checks |
|---|---|
| `tests/getarg_tests.rs`, `tests/argsman_tests.rs`, `tests/settings_tests.rs` | ports of Core's unit tests with the same cases and expectations; the merge tests compare their output against the release `test_dash`'s |
| `tests/help_tests.rs` | every program's help matches the release binaries byte for byte (so the visible options are exactly Core's): the Linux release's on Unix, the win64 release's on Windows (except dash-qt); the platform-dependent defaults on every platform; every registered option name is one Core's parser accepts |
| `tests/init_cases.rs` | 74 command-line and config-file cases replayed against the Linux release dashd's exit code, stderr and file changes; on Windows, 83 cases (the 69 that name `-datadir` plus Windows path, hard-link and text-mode cases) against the win64 release |
| `tests/functional_behaviours.rs` | each behaviour `feature_config_args.py`, `feature_includeconf.py` and `feature_settings.py` check, with a test or the reason it needs a node |

## Deviations from Core

- **Invalid UTF-8.** Rust strings hold UTF-8. Config files are decoded lossily (invalid bytes
  become U+FFFD), and argv and environment values must be UTF-8. Core passes the bytes
  through.
- **UniValue above U+10FFFF.** Core's UTF-8 filter accepts `F4`–`F7` lead bytes encoding code
  points above U+10FFFF; such a `settings.json` is refused here (Core would read it).
- **`equivalent` on Windows** compares msvcrt's `st_mode` and `st_dev` as Windows' msvcrt
  computes them, extension check for any path length; Wine's msvcrt (where the tests ran)
  checks the extension only of paths longer than six characters. Core compares absolute
  paths, where the two agree.
- **`status` on Windows** reports every failure as "not found", as msvcrt's `_wstat` does,
  but succeeds where `_wstat` fails on paths over `MAX_PATH` (Rust adds `\\?\`) and on a
  directory with a trailing separator (`GetPathArg` strips those), and can fail with access
  denied where `_wstat` succeeds. Files are opened with Rust's long-path handling too.
- **Platform values not captured.** The macOS and 32-bit help defaults (`-uiplatform`,
  `-font-weight-normal`, the `-dbcache` maximum) come from Core's sources; no macOS or 32-bit
  release was run. The Windows dash-qt help (a dialog) was not captured either.
- **Windows tests** ran under Wine 10 (`cargo test --target x86_64-pc-windows-gnu`), not on
  a Windows host.
- **Hidden options** carry no help text, since Core never prints it. Their names come from
  the source parse alone: no binary output lists them, so the tests can show each is
  accepted by Core's parser but not that none is missing. dash-qt's GUI-only names are not
  run against its parser (dash-qt needs a display).
- **`WriteSettingsFile`** with settings disabled panics instead of throwing `logic_error`.
- **One release.** A1 sketches per-release tables (`options(Release)`); only v24 exists
  (DEC-82).
- **Not ported:** the parts of `AppInitParameterInteraction` and `AppInitMain` beyond the
  config checks (they need the node), Qt's settings dialog, and `-?`'s version and usage
  banner (each program prints its own).
