# dash-rpc-spec generator

Regenerates `../src/generated.rs` from Dash Core. `../PROVENANCE.md` says what comes from where
and which cross-checks must pass.

| File | Role |
|---|---|
| `build-core.sh` | Builds dashd from a fresh `git archive` of a tag through Core's depends (wallet and ZMQ on, as in releases). The depends prefix is the host triplet depends reports (`make -C depends print-host`). |
| `rpcspec_dump.cpp` | Linked like dashd against that build; prints every registered command's `RPCHelpMan` as JSON. |
| `generate.py` | `build-dump` (compiles and links the dump tool with dashd's own commands from `make -n`), `capture` (help from a running regtest dashd), `emit` (cross-checks, writes the table), and `regenerate` (all of them, then rustfmt). |

```bash
dash-rpc-spec/generator/build-core.sh <dash-git-dir> v24.0.0-rc.4 <out-dir> [depends-sources]
python3 -I dash-rpc-spec/generator/generate.py regenerate <out-dir>/core <release dashd> \
    [--work DIR] [--rpcport N]
```

Requirements: Bash 3.2+ and POSIX tools for `build-core.sh` (set `MAKE=gmake` where GNU make is
not `make`), Python 3.8+, git, rustfmt, and Core's own build requirements. No GNU-only options,
Bash 4 features or coreutils (`realpath`, `sha256sum`) are used; hashing and paths are in Python.

The dashd that `capture` starts gets its own datadir, a random RPC password and, unless
`--rpcport` is given, a free RPC port (another if dashd cannot bind it). It is used only after
`getrpcinfo` shows its log inside that datadir, and only that process is stopped.

Platforms: validated on Linux (x86_64). **macOS: untested** (no Mac on the build box); the
scripts avoid the known GNU and Bash 4 dependencies, and Core's depends picks the Darwin
triplet itself. Windows: run under WSL or MSYS2 (untested).
