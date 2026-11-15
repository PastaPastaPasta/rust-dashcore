# EV4-serve vector capture

The tools that built `dash-coreimport/tests/data/evodb/*.json.gz`, the fixtures of
`dash-coreimport/tests/evodb_serve.rs` (EV4-serve notes D9). Everything in a fixture is produced
by Dash Core v24.0.0-rc.3 on the regtest chains its functional tests left behind (BS2,
`/work/conformance/regtest-v24/<test>-node0`); no live oracle is read.

1. Copy each datadir twice under `/work/scratch/ev4s`: `pristine/<test>-node0` (never started
   again) and `chains/<test>` (for dashd).
2. Dump the pristine copy's evodb with LevelDB's own C API:
   `cd evodump && CC=clang CXX=clang++ CARGO_TARGET_DIR=$PWD/target cargo build --release`, then
   `evodump <pristine>/regtest/evodb > /work/scratch/ev4s/<test>-node0.evodb.txt`.
   (leveldb-sys builds LevelDB from source; gcc rejects its `-Wthread-safety`.)
3. `run_all.sh` starts rc.3 `dashd` on each `chains/` copy (`node.sh`, with the consensus
   arguments the test's node0 `debug.log` records), and `capture.py` writes the fixture: block
   hashes and coinbases over RPC, the target blocks' txids, the evodb's `dmn_*` and `q_mc`
   records, and the node's `mnlistdiff` reply to a P2P `getmnlistd` (`getmnlistd.py`) for each
   chosen (base, block) pair in each of the seven serialization bands (70221, 70225, 70227,
   70228, 70229, 70230, 70242), plus a null base.
4. `gzip -9n` each fixture into `dash-coreimport/tests/data/evodb/`.

The pairs are fixed per chain (seeded `ev4s-<test>`): genesis to the tip and to half the tip,
DIP3 to the tip, a quarter and half the tip to the tip, the tip's parent and the tip itself, ten
random parent-to-child steps and four random spans; genesis to block 1 (below DIP3: a null
`blockHash`) and a null base to block 1; and for `rpc_netinfo`, 204 to 260, which deletes two
masternodes out of proTxHash order.

## Refusals

`negative.py HOST:PORT fcc1b7dc DEBUG_LOG REQUESTS.jsonl` sends each request on its own
connection, then a `ping`, and records whether Core answered and what it logged (the node runs with
`-debug=net`, so `Misbehaving` lines show the score). The steps behind
`dash-coreimport/tests/data/evodb/refusals-*.jsonl` are in that directory's PROVENANCE.md.
