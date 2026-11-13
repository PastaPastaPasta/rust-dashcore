# Dash v24 index fixture provenance

The files in `dashd-indexes/tests/fixtures/dashd-v24/` are recorded from release
Dash Core on a fresh regtest chain. The tool obtains every RPC answer from dashd
and copies the node's block and undo files after clean shutdown.

| Item | Value |
|---|---|
| Producer | `dashcore-24.0.0-x86_64-linux-gnu.tar.gz`, `Dash Core version v24.0.0`, RPC subversion `/Dash Core:24.0.0/` |
| Binary sha256 | `2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223` (`bin/dashd`) |
| Fetch command | `DASHVERSION=24.0.0 python3 -I contrib/setup-dashd.py` |
| Cached binary | `~/.rust-dashcore-test/dashcore-24.0.0/bin/dashd` |
| Capture command | `python3 -I contrib/node/ix1-captures/capture.py` |
| Reproduction command | `python3 -I contrib/node/ix1-captures/capture.py --check` |
| Initial mock time | `1700000000` (`SIM_START` in the shared P2A capture module); mining sets it to `SIM_START + height` |
| Index settings | `-txindex=1 -blockfilterindex=1 -coinstatsindex=1` |
| Fixture | 173 blocks, 417 unique transactions, 11 phase snapshots; 1,688,010 bytes |

`capture.py` imports `contrib/node/p2a-transcripts/capture.py` for node startup,
RPC, shutdown, `SIM_START`, transaction hashing, CompactSize encoding, and the
version/argument header lines. Both daemons use temporary datadirs under
`/work/scratch/ix1-captures/` (mode `0700`), with free listener/RPC port pairs
below 32768. The second daemon acts as the registered EvoNode and signs the
asset unlock over RPC. All processes started by the tool are stopped before it
returns, including on failure.

Additional chain settings are `-dip3params=2:2 -testactivationheight=v20@100
-testactivationheight=mn_rr@1000 -llmqtestplatformparams=1:1`. Both daemons also
use `-keypool=1 -par=1 -parbls=1 -persistmempool=0 -tinyblk=1`. Core's `tinyblk`
test setting uses 64 KiB allocation chunks for both block and undo files,
keeping the fixture small without stripping any file bytes. The EvoNode uses the fixed
regtest operator scalar 1, `-externalip=127.0.0.1:19899`, and Core's functional
test spork signer. Its advertised endpoint is independent of the chosen
listener port. Only the one-member Platform quorum can form; its public key
and signatures are derived by dashd from that fixed operator key. ChainLocks
and InstantSend are disabled through their sporks, and quorum DKG is enabled.
The wallet uses Core's deterministic test seed, set through `sethdseed` in a
blank legacy wallet. Explicit inputs and outputs avoid wallet coin selection
in the plain spends and asset lock.

The ProTx RPC calls follow the patterns in Core's
`test/functional/feature_dip3_deterministicmns.py` at `v24.0.0`; the EvoNode
variant allows the same registration to supply the Platform quorum. The asset
lock/unlock serialization and `plwdtx` signing request follow
`test/functional/feature_asset_locks.py` at that tag. The tool constructs only
the asset transaction requests; dashd signs, validates, mines and reports them.

## Recorded files

- `blocks/blk*.dat` and `blocks/rev*.dat`: the node's exact block and undo files,
  including both branches. Their original offsets and framing are retained.
- `blocks.json`: every known block, including genesis and the stale branch,
  with its hash, transaction IDs, complete `getblockheader`, `getblockfilter`,
  and `gettxoutsetinfo muhash <hash> true` responses, plus a coverage label.
- `transactions.json`: every unique transaction ID with its complete verbose
  `getrawtransaction <txid> true` response and the response with each containing
  block hash. Fields identical across successful responses are stored once in
  `common_result`. Reconstruct each successful answer as
  `{**row["common_result"], **reply["result"]}`; the query-specific `result`
  objects retain every differing field. The same encoding is used in phase
  snapshots. Errors are recorded with the exact RPC error code and message;
  in particular, the genesis coinbase lookup is an RPC error.
- `chain.json`: producer version/arguments, binary sha256, initial mock time,
  ordered connect/disconnect events, and phase snapshots. Each phase records
  `getindexinfo` and the active tip; reorg snapshots also retain full block and
  transaction answers at that phase.

JSON uses compact encoding and sorted object keys. All RPC response fields are
retained; common transaction fields are factored out losslessly as above.
Header argument lines omit runtime datadirs and RPC settings, as the shared
capture module does, and replace runtime listener ports with placeholders.
These are the only normalizations; the tool prints them at startup. Initial
keys, advertised addresses, mock times and transaction inputs are fixed.
`--check` records afresh under `/work/scratch/ix1-captures/` and compares every
JSON and binary file byte for byte, including the file set.

## Chain coverage

The tool records a coverage label for each block and the exact replay order in
`chain.json`:

| Height | Coverage |
|---|---|
| 0 | Genesis |
| 1–110 | Coinbase maturity and DIP3/v20 activation (DIP3 at 2, v20 at 100) |
| 111 | Plain spend funding EvoNode collateral and fees |
| 112 | ProRegTx, with 4000 DASH EvoNode collateral |
| 113–144 | Coinbase-only extension through the quorum's membership lookback; Platform quorum base at 144 |
| 145–153 | Extension to the quorum mining window |
| 154 | Signed one-member Platform quorum commitment |
| 155–162 | Extension through the quorum signing offset |
| 163 | Asset lock of 10 DASH |
| 164 | Asset unlock of 1 DASH |
| 165 | ProUpServTx changing Platform endpoints |
| 166 | ProUpRegTx changing the voting key |
| 167 | ProUpRevTx with reason 1 |
| 168 (original) | Plain spend, later re-mined on the competing branch |
| 169 (original) | Spend with OP_RETURN data `495831` |
| 170 (original) | Empty tip |
| 168 (competing) | Same plain transaction re-mined |
| 169 (competing) | Empty competing tip |

"Empty" and "coinbase-only" here allow the automatic quorum commitment
transactions required by dashd's block template.
The asset-unlock block is required to have negative
`block_info.unspendables.unclaimed_rewards` in dashd's coinstats answer;
the recorded value at height 164 is `-1.0` DASH.

The final original branch has three blocks: a plain spend, an OP_RETURN spend,
and an empty tip. `invalidateblock` disconnects all three; a two-block competing
branch re-mines the same plain transaction and leaves the OP_RETURN transaction
unmined. `reconsiderblock` restores the longer original branch. The snapshots
cover the active/stale answers before invalidation, after invalidation, on the
competing branch, and after reconsideration.
