# Dash v24 index fixture provenance

The three fixture directories named below are recorded from release
Dash Core on a fresh regtest chain. The tool obtains every RPC answer from dashd
and, for `dashd-v24` and `dashd-v24-reorg`, copies the observed node's block and
undo files after clean shutdown.

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
| Original fixture (`dashd-v24`) | 173 blocks, 417 unique transactions, 11 phase snapshots; 1,688,010 bytes |
| Reorg fixture (`dashd-v24-reorg`) | 118 blocks, 279 unique transactions, 122 ordered events, 7 phase snapshots; 1,343,855 bytes |
| Checkpoint fixture (`dashd-v24-cfcheckpt`) | 2001 ordered connects, 6 boundary answers, 7 phase snapshots; 180,556 bytes |

`capture.py` imports `contrib/node/p2a-transcripts/capture.py` for node startup,
RPC, shutdown, `SIM_START`, transaction hashing, CompactSize encoding, and the
version/argument header lines. All runs use temporary datadirs under
`/work/scratch/ix1-captures/` (mode `0700`), with free listener/RPC port pairs
below 32768. In the original fixture, the second daemon acts as the registered
EvoNode and signs the asset unlock over RPC. All processes started by the tool
are stopped before it returns, including on failure.

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
JSON and binary file byte for byte, including the file set, for all three
fixtures. `--fixture g`, `--fixture reorg`, or `--fixture cfcheckpt` selects one
fixture for recording or checking. The default records or checks all three.

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

## Additional fixture: ordinary reorgs and underclaim

`dashd-indexes/tests/fixtures/dashd-v24-reorg/` uses the same flags, fixed seed,
mock times, JSON encoding and lossless transaction encoding as the original.
It retains `chain.json`, `blocks.json`, `transactions.json`, and exact
`blocks/blk*.dat` and `blocks/rev*.dat` files. `chain.json` includes the observed
node's producer header and the headers for both isolated miner runs.

The A/B/C block numbers are relative to a 100-block maturity prefix. Thus A10
has absolute height 110, B9 has height 109, and C12 has height 112;
`branch_height_offset` in `chain.json` records 100. Every mining call sets mock
time to `1700000000 + absolute height`.

1. At height 1, a block is assembled from `getblocktemplate` before DIP3 is
   active. Its ordinary coinbase claims exactly one DASH less than the
   template's `coinbasevalue`. The tool constructs the transaction and merkle
   root, tries increasing nonces through `submitblock`, and lets dashd check
   X11 proof of work. The phase stores the complete template and
   `gettxoutsetinfo muhash <hash> true` before and after the block, with the
   accepted block's full RPC answers. The recorded unclaimed reward is 1 DASH.
2. The observed node mines through absolute height 100, then the common branch
   prefix through height 108. T spends the mature height-2 coinbase.
3. The observed node mines A9 without T and A10 with T. The A10 phase records
   T both without a block hash and with A10's hash.
4. A second, isolated dashd imports only the common prefix, mines B9 with the
   same signed T, then B10 and B11. Its blocks reach the observed node only
   through `submitblock`. B11 has more work: A10 and A9 disconnect, and B9,
   B10 and B11 connect in that order. The B11 phase records T without a block
   hash and with each of A10 and B9.
5. The isolated miner is stopped and a fresh isolated datadir imports the same
   common prefix. C9 includes a different spend of T's input; C10 through C12
   omit T. This conflict prevents T returning to the observed mempool, so the
   lookup exercises txindex. C12 wins by work: B11, B10 and B9 disconnect,
   then C9 through C12 connect. No invalidation or reconsideration RPC is used
   anywhere in this fixture. All branches use distinct addresses derived from
   the same fixed seed.
6. The C12 phase records T without a block hash and with A10, B9, and C9. The
   observed node then stops cleanly and restarts with the same arguments and
   datadir, and the same queries are repeated. Both phases show the lookup
   without a block hash returning stale B9 with zero confirmations, despite
   A10 being higher. The supplied C9 hash returns the exact RPC miss.

The genesis, underclaim, mature, A10, B11, C12, and restart phases record the
active tip and all three index summaries. Reorg phases retain full block,
filter, coinstats, and verbose transaction responses at that point. The ordered
events cover all connects and both reorgs; the restart adds no chain event.
Side-branch `submitblock` calls may return `inconclusive` before their branch
wins; the tool verifies those blocks are stored, verifies that neither branch
switches early, and requires its final block to become the active tip.

## Additional fixture: compact-filter checkpoint boundaries

`dashd-indexes/tests/fixtures/dashd-v24-cfcheckpt/` records a fresh 2001-block
chain with the same fixed wallet seed, all three indexes enabled, and mock
time set to `1700000000 + height` before every block. Its additional flags are
`-dip3params=3000:3000 -testactivationheight=v20@3000
-testactivationheight=mn_rr@4000`, overriding the earlier activation settings.
Deferring DIP3 avoids automatic quorum commitment transactions; the tool
checks that every mined block contains exactly one transaction.

- `chain.json` keeps the 2001 ordered connect hashes and seven actual phase
  snapshots: genesis, then heights 999, 1000, 1001, 1999, 2000 and 2001. Each
  boundary snapshot is taken while that block is the active tip, with index
  summaries and complete block header, filter, coinstats and coinbase lookup
  responses. There are no disconnects in this chain.
- `checkpoints.json` keeps each of those six heights and hashes with the full
  `getblockheader` and `getblockfilter` responses, including both filter bytes
  and filter header. The responses retain their phase-time confirmations.
- No raw blocks, undo files, full header chain, or answers for the other
  heights are retained. This is an RPC boundary oracle for the 1000-block
  checkpoint interval; it does not record the `cfcheckpt` P2P message itself.
