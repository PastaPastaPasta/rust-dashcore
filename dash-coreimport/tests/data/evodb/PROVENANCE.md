# evodb/*.json.gz

One fixture per regtest chain of Dash Core v24.0.0-rc.3's functional tests
`feature_asset_locks`, `feature_masternode_payout_shares`, `feature_masternode_shares`,
`feature_protx_version` and `rpc_netinfo` (node0 of each, as BS2's `conformance/scripts/regtest-v24.sh`
left it in `/work/conformance/regtest-v24`), captured 2026-10-21 with the tools and steps in
`contrib/node/ev4-serve/README.md`:

- `hashes`, `coinbases`: every block's hash and coinbase transaction (`getblockhash`,
  `getblock <hash> 0`);
- `txids`: the target blocks' txids (`getblock <hash> 1`);
- `evodb`: the `dmn_*` and `q_mc` records of the node's `evodb/` LevelDB, read with LevelDB's C
  API from a copy dashd never reopened;
- `replies`: `[base, block, protocol version, payload]` of rc.3 dashd's `mnlistdiff` reply to
  each P2P `getmnlistd`, the connection's version set to the band.

Recaptured 2026-10-22 with the same tools (DEC-331): each chain adds genesis → block 1 and a null
base → block 1 in every band (block 1 is below DIP3), and rpc_netinfo adds 204 → 260 in every band;
the 763 replies of the first capture came back byte-identical, 840 in all.

rpc_netinfo's first 199 blocks come from the functional-test cache, mined before the test's
`-dip3params=2:2`, so their coinbases carry no CbTx.

# refusals-feature_masternode_payout_shares.jsonl

rc.3 dashd's outcome for `getmnlistd` requests it refuses, captured 2026-10-22 with
`contrib/node/ev4-serve/negative.py` from a fresh copy of the same node0 datadir, started with the
test's arguments and `-debug=net`. Before the capture one side block was made:
`invalidateblock` of the tip (127), `generatetoaddress 1` (side block `6ba025e9…` at 127), then
`reconsiderblock` of the old tip, which stays active (`getchaintips`: the side block is
`valid-fork`; the test's own invalid block `5fb40c83…` at 123 is `invalid`). Each request went on
its own connection at protocol 70242 followed by a `ping`; `answered` is whether a `mnlistdiff`
came before the `pong`, and `log` holds the lines the request added to `debug.log`. Unavailable
target data (pruning) is not reachable here: the chain is 127 blocks in one block file.
