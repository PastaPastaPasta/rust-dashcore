# Provenance of dash-rpc-spec's table

`src/generated.rs` (with `CORE_VERSION`, taken from the dashd's `-version`) is generated from Dash Core v24.0.0-rc.4 (tag object `49caf372`, commit
`0e7382df80d0bc6811068d076b232a5a26d34a3c`, DEC-82) and never edited by hand. Its header records
the inputs' hashes.

| Data | Source |
|---|---|
| `help` text of every command, verbatim | `help <cmd>` of a running regtest **release** dashd v24.0.0-rc.4 (`dashcore-24.0.0-rc.4-x86_64-linux-gnu.tar.gz`, dashd sha256 `e1be2564…b4f823a7`, as fetched by `DASHVERSION=24.0.0-rc.4 contrib/setup-dashd.py`), with a wallet loaded; sub-commands requested as two parameters (`help protx register`) |
| names, categories, arguments (aliases, types, fallbacks, `skip_type_check`, hidden), wallet gating | `generator/rpcspec_dump.cpp`, linked against dashd built from `git archive` of the tag (`generator/build-core.sh`). It replays `init.cpp`'s registration with the wallet disabled and enabled and reads each command's `RPCHelpMan` through `CRPCCommand::unique_id` |
| `CONVERT_PARAMS` | `vRPCConvertParams` of the tag's `src/rpc/client.cpp` (git blob `260e3875`), in file order, `#if ENABLE_MINER` entries marked; a verbatim copy is `tests/data/client.cpp` |
| `class` | PRD §5.4 (`docs/node-program/PRD.md`), with the two gaps below |
| `whitelist_name` | the name without its space, as `-rpcwhitelist` matches it (A1 §2.17) |

## Cross-checks

`generate.py emit` writes nothing unless all of these hold:

- the dump's `RPCHelpMan::ToString()` equals the live help of every command (289 of 289);
- the help of every command registered both with and without the wallet is the same in both
  modes (180 commands);
- `help dump_all_command_conversions`, recomputed from the dumped arguments, equals the live
  one with the wallet and under `-disablewallet`;
- `client.cpp`'s table equals the server's non-string arguments, with `rpc_help.py`'s filters
  (no `echojson`, no sub-commands);
- the `help` overview recomputed from the dump equals the live one in both modes (so the release
  registers no command the dump's build lacks), and `help foo` answers "unknown command";
- every command has a PRD class, and every PRD row names a command;
- no handler is registered under two names (the table's `help()` relies on it).

`tests/dashd_help.rs` repeats the comparison against a live dashd on every test run: `help`, `help foo`
and `help <cmd>` for all 289 commands with the wallet (291 outputs) and again under
`-disablewallet` (291, wallet-only commands answering "unknown command"), both argument maps, and the
client/server conversion check. `tests/table.rs` compares `CONVERT_PARAMS` with
`tests/data/client.cpp` entry by entry.

## PRD gaps

PRD §5.4 and Core's v24 command list differ in two places (`PRD_GAPS` in `generator/generate.py`):

- `protx shared_register_prepare_evo` is registered by v24 but not classified. It gets MUST, the
  class of its non-evo twin `protx shared_register_prepare` (US-SHR-01/02).
- `getprioritisedtransactions` (mining, MUST) is not registered by v24.0.0-rc.4 and is not in the table.

The totals stay 289 / MUST 266 / SHOULD 22 / WON'T 1; per category, evo is 50 and mining 6 (PRD: 49
and 7).

## Regenerating

See `generator/README.md`. In short:

```bash
# 1. Build the tag (writes <out>/core; ~30 min)
dash-rpc-spec/generator/build-core.sh ~/workspace/dash v24.0.0-rc.4 /work/scratch/<slug>/core-build
# 2. The release dashd
DASHVERSION=24.0.0-rc.4 python3 contrib/setup-dashd.py
# 3. Dump, capture (regtest dashd on a free RPC port, or --rpcport N), emit, rustfmt
TMPDIR=/dev/shm/<slug> python3 -I dash-rpc-spec/generator/generate.py regenerate \
    /work/scratch/<slug>/core-build/core ~/.rust-dashcore-test/dashcore-24.0.0-rc.4/bin/dashd
```

Regeneration from the same inputs is byte-identical.
