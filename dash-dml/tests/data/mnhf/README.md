# Core-judged MNHF step vectors (O2a-MNHF)

Each line is one `oracle_mnhf_step` (ABI 26; the tip-cache seed ABI 28): Dash Core v24's
`CMNHFManager::ProcessBlock()` for a block (`evo/mnhftx.cpp:210-262`), then for a connected
block the next block's `GetSignalsStage()` (`:61-92`). Every answer is Core's; the inputs are
what those functions read, so a port is checked without an oracle.

- `crafted.txt`: written by `dash-oracle-sys/tests/mnhf.rs` on crafted regtest chains: every
  reject reason, the order of the checks, `fJustCheck`, a window's start and timeout, expiry,
  an unknown bit, and blocks before and at V20.
- `regtest.txt`: written by `dash-oracle-sys/tests/mnhf_regtest.rs` from the chain Core's
  `feature_mnehf.py` mined (`dash-oracle-sys/tests/data/mnhf-regtest-v24.0.json.gz`): the
  signal blocks and their neighbours, the invalidated side branch's signal, and the
  `getblocktemplate` proposals (`fJustCheck`).

Regenerate both with `DASH_ORACLE_REGENERATE=1 cargo test -p dash-oracle-sys --features oracle
--test mnhf --test mnhf_regtest`; without it those tests fail if the oracle's answers changed.

## Format

`#` lines are comments. Fields are separated by ` | `; a list is space-separated, `-` when
empty.

| Field | Meaning |
|---|---|
| name | the case |
| `deps b:s:t:e …` | every `vDeployments` entry: bit, `nStartTime`, `nTimeout`, `useEHF` |
| `v20 0\|1` | `DeploymentActiveAfter(block, V20)`: whether the next block's stage is read at all |
| `height h` | the block's height (the height a new signal is stored at) |
| `mtp m` | the block's `GetMedianTimePast()`, which `IsValidMNActivation()` reads and which is a new signal's time in the next stage |
| `stage b:h …` | `GetSignalsStage(pprev)`: the signals the block is checked against |
| `bits b …` | the version bits of the block's MNHF transactions after the coinbase, in block order |
| `check 0\|1` | `fJustCheck` |
| verdict | `ok`, or the reject reason (`BLOCK_CONSENSUS`) |
| `signals …` | `ProcessBlock()`'s result: with `fJustCheck` the stage alone; `none` on a reject |
| `after …` | `GetSignalsStage(block)` after a connect; `none` when not connected |

A signal already in `stage` passed `GetSignalsStage()`'s expiry check against the same
deployments, so only the block's new signals can expire in `after`.

At `V20Height - 1` (`v20 1` with the block below V20, as in the crafted "V20 from the next
block"), `after` comes from Core's manager cache, which holds the block's signals, while Core
stores no row for the block (O2a-MNHF D13). The crafted "on V20 from the tip's cache" cases are
the next block, at `V20Height`, on that tip with its cache entry seeded as the connecting node
left it (BL-211): their `stage` is the "V20 from the next block" case's `after`.
