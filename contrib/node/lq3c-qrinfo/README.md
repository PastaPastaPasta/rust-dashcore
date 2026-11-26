# LQ3c qrinfo capture

The tools that built `dash-coreimport/tests/data/qrinfo/`, the fixtures of
`dash-coreimport/tests/qrinfo_serve.rs`: Dash Core v24.0.0's `qrinfo` replies and `getqrinfo`
refusals on the regtest chain its functional test `feature_llmq_rotation` leaves, branched so that
some request blocks lie off the active chain. The steps and what came out are in that directory's
PROVENANCE.md. Everything runs under `/work/scratch/lq3c` (`$W` below); `D=$W/main`.

1. Download `dashcore-24.0.0-x86_64-linux-gnu.tar.gz` and `SHA256SUMS.asc` from the v24.0.0
   release into `$W/bin`, check the tarball with `sha256sum -c --ignore-missing SHA256SUMS.asc`
   and unpack it (`dashd --version` must say v24.0.0).
2. Export the tag's tests (`git archive v24.0.0 test/ share/rpcauth`) into `$W/core`, write
   `core/test/config.ini` as `contrib/node/lq3a-captures/run.sh` does, make a venv with
   `dash_hash`, and run `feature_llmq_rotation.py --nocleanup --tmpdir=$W/run
   --timeout-factor=20` with `DASHD`/`DASHCLI` pointing at `$W/bin`.
3. Copy `$W/run/node0` to `$D` and `node.sh start $D`: dashd with node0's `dash.conf`, its other
   consensus arguments, no mocktime and `-debug=net`, P2P on 47710 (`node.sh cli $D …` is its RPC).
4. `capture.py build $W/work/state.json node.sh cli $D` branches the chain (A, B and L; see
   `capture.py`'s doc) with SPORK_19 off throughout, and checks each branch's tip status and that
   the tip is back at T.
5. `node.sh stop $D`, then dump the stopped copy's evodb with LevelDB's C API:
   `contrib/node/ev4-serve/evodump` (built with `CC=clang CXX=clang++`),
   `evodump $D/regtest/evodb > $W/work/evodb.txt`.
6. `node.sh start $D`, then
   `capture.py capture $W/work/state.json 47710 $D/regtest/debug.log $W/work/evodb.txt
   $W/work/refusals.jsonl node.sh cli $D > $W/work/fixture.json`. It sends every request through
   `getqrinfo.py` (one connection per protocol version; each refusal on its own connection, with
   a `ping` after it and the node's `debug.log` lines), checks each outcome against the one the
   case expects, and writes the block tree, the evodb rows and the replies.
7. `gzip -9n` the fixture into `dash-coreimport/tests/data/qrinfo/feature_llmq_rotation.json.gz`
   and copy `refusals.jsonl` to `refusals-feature_llmq_rotation.jsonl`.

`getqrinfo.py` is `contrib/node/ev4-serve/getmnlistd.py`'s peer with `getqrinfo` added, and the
refusal handling of `ev4-serve/negative.py`.
