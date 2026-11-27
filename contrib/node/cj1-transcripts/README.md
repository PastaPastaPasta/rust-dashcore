# CJ1 transcripts

`capture.py` records **release Dash Core v24.0.0** on pre-V24 regtest. It imports
P2a's checked V1 framing, peers and RPC client, and P2b's version/handshake code.
The network adapter follows `dash-spv/src/test_utils/masternode_network.rs`: copy
the `regtest-mn` image, load `network.json`, start its controller and four actual
masternodes, update their registered services, mine, bump mocktime and finish
regtest masternode sync. It uses the image's real operator keys with BLS basic
signatures, checked against the registered public keys. Private keys stay in
the downloaded image; command records substitute `<network.json>` for them.

```sh
mkdir -m 700 -p /work/scratch/cj1/impl-transcripts
uv venv /work/scratch/cj1/impl-transcripts/venv
uv pip install --python /work/scratch/cj1/impl-transcripts/venv/bin/python \
  -r contrib/node/cj1-transcripts/requirements.txt
/work/scratch/cj1/impl-transcripts/venv/bin/python \
  contrib/node/cj1-transcripts/capture.py setup
```

`setup` calls `contrib/setup-dashd.py` for version 24.0.0 and the v0.0.4 MN
image, storing their paths in the scratch `setup.json`. It checks the binary's
version before fetching the image; capture checks again before starting nodes.
A different version stops recording. `--setup PATH` selects another path file.

Run long recordings under a user service, for example from the repository root:

```sh
systemd-run --user --unit=cj1r-capture --same-dir --collect bash -c \
  'P2A_QUIET=0.3 /work/scratch/cj1/impl-transcripts/venv/bin/python -u \
  contrib/node/cj1-transcripts/capture.py capture \
  > /work/scratch/cj1/impl-transcripts/capture.log 2>&1; \
  echo $? > /work/scratch/cj1/impl-transcripts/capture.rc'
```

Pass scenario names after `capture` to record a subset: `server`, `empty`,
`timeout`, `timeout-charge`, `mn-list`, `recent`, `dstx`, `removed-mn`,
`lifecycle`, `chainlock`, `dsq-modes`, `dsq-intake`, `dsq-server`,
`dsq-blocksonly`, `dsq-threshold`. Each uses a separate copy of the network and high random
loopback ports. Normal exit and Python exceptions attempt to stop and reap every owned process
before deleting datadirs, collecting cleanup errors after all nodes are handled.
A datadir is retained if its process could not be stopped. `CJ1_SCRATCH` changes the scratch root;
`--fixtures PATH` changes the output directory. `CJ1_CL_WARMUP_BLOCKS=48` can
warm fresh DKG cycles before the ChainLock attempt. V24 must still be inactive. `chainlock` is opt-in while the image cannot produce
a ChainLock; default capture records all other scenarios.

```sh
/work/scratch/cj1/impl-transcripts/venv/bin/python \
  contrib/node/cj1-transcripts/capture.py selfcheck
/work/scratch/cj1/impl-transcripts/venv/bin/python \
  contrib/node/cj1-transcripts/capture.py selftest
```

`selfcheck` and `selftest` require `py-ecc==8.0.0` from `requirements.txt`:
use the venv commands above to verify BLS signatures. Neither needs dashd or a
running network. Without `py_ecc`, run the structural checks with system Python:

```sh
python3 -B contrib/node/cj1-transcripts/capture.py structural-selfcheck
```

This mode checks framing structure, known MN identities, signature lengths,
raw-record integrity, projections and scenario evidence. It does **not** verify
BLS signatures; run `selfcheck` with the dependency for cryptographic validation.

The format extends P2a's line grammar. `accept`, `>`, `<`, `mocktime`, `time`
and `ibd` retain their meanings. Empty payloads omit the trailing separator;
this preserves their zero bytes without trailing whitespace. Additional records contain JSON after the tag:

| Record | Meaning |
| --- | --- |
| `format cj1-v1` | Versioned grammar identifier. |
| `fact` | Setup, exact command argv, binary hash, public MN list, coins, collateral test-accept results, observed signed queue bodies, participant aliases, eligibility, integrity manifest, signing/fork facts. |
| `mask` | Comparison rules for random IDs, queue times/signatures, background traffic and collateral eligibility. |
| `rpc` | Real RPC method, parameters and result; also an input for lifecycle replay. |
| `log` | Original Core CoinJoin and relevant net log lines, including Misbehaving. |
| `expect` | Deterministic projection of a received wire message; raw `<` bytes remain available. |
| `observe` | Score delta, DSTX verdict/map size/confirmation height, completion IDs, collateral charge count or charged participants. |

Compare `expect` and the relevant `observe` records; use `fact` to prime the
replay. Raw signatures, timestamps, complete RPC snapshots and logs are evidence,
not byte-for-byte comparisons. `dsf` retains the exact unsigned transaction bytes.
The `queues` fact maps hashes of observed DSQ bodies to queue aliases; the
`queue_bodies` fact retains each body and its fields. Server scenarios fetch every
announced queue through observer peer 90 while cached. Queue labels preserve
same-queue relationships, denomination, ProTx identity and readiness. The separate
`collaterals` fact maps each participant's collateral to its participant alias. Session zero stays zero; other IDs map consistently by first use.
Explicit inputs still preserve their timestamps (including expired DSQs).

Self-check re-parses every fixture, reconstructs wire projections and log
observations, derives completion IDs and charge counts/participants from raw
evidence, verifies received DSQ/DSTX signatures, and checks scenario milestones.
A SHA256/count manifest covers all raw wire, RPC and log records. Self-test rejects
missing inputs, closure and queue bodies, altered replies/signatures/scores,
inconsistent mempool observations and charges to cooperating participants; it
also exercises cleanup failures and P2a's framing controls. A fixture is published
only after self-check passes. All random charging attempts are retained; replay
must prime their zero/one decisions and compare both count and eligibility.
The charged timeout has one offender, so its charged identity is deterministic.
An empty entry must consume that entry's collateral and no other participant's.
Selection may be masked only among multiple eligible offenders; no current
fixture charges from such a set.

## Target mode (CJ1-W7)

`capture.py target [SCENARIO...]` runs the clients against rdashd instead of recording dashd.
It is started by `dashd-node`'s `node::tests::coinjoin::accept` tests, which run rdashd in their
own process: the harness asks for it on stdout (`CJ1-TARGET {json}` lines) and reads the
answer on stdin. On each copy of the network only the controller and rdashd run; rdashd takes
masternode 0's operator key, and the controller moves that masternode's service to rdashd's
port. `getcoinjoininfo`, which rdashd does not serve yet, is answered by the test from the
server. Each scenario's transcript is then compared with its fixture instead of being written:

- per peer, every projected message in order (the order across peers is timing);
- session IDs, queue aliases and transaction hashes renamed by first use, and the final mix as
  `mix` when it is the harness's own BIP69 reconstruction;
- the wire and RPC observations, the score deltas, and the `accepted` DSTX verdict (the log
  lines rdashd writes in Core's format), plus whether each `dstx-removed-mn` DSTX is pooled.

Not compared: the queue observer's own `inv` against its `getdata`'s `dsq` (peer order), and
MSG_DSQ inventory in `dstx-removed-mn`, which dashd recorded on the non-masternode controller.
A run on the other branch of one of Core's random draws (a random fee after the session, a
charge on the first timeout) is repeated, up to three times. The default scenarios are
`server empty recent mn-list removed-mn timeout timeout-charge`; `wallet` (two dashd wallets
mixing a round through rdashd) runs only when named.

```sh
export CJ1_PYTHON=<scratch>/venv/bin/python CJ1_SCRATCH=<scratch>
# The Python leg, then the wallet leg:
cargo test -p dashd-node --features dashd-node/test-support --lib -- --exact \
  node::tests::coinjoin::accept::dashd_clients_see_the_rdashd_server_answer_as_dashd
cargo test -p dashd-node --features dashd-node/test-support --lib -- --include-ignored --exact \
  node::tests::coinjoin::accept::dashd_wallets_mix_a_round_through_the_rdashd_server
# Both legs; they take turns, one in-process rdashd at a time:
cargo test -p dashd-node --features dashd-node/test-support --lib \
  node::tests::coinjoin::accept -- --include-ignored
```

`<scratch>` is the directory `capture.py setup` wrote `setup.json` to, next to the venv.

Without `CJ1_PYTHON` and `CJ1_SCRATCH/setup.json` the tests skip. `CJ1_TARGET_SCENARIOS` picks
scenarios for the Python leg; `CJ1_WALLET_LEG_SECONDS` (default 900) bounds the wallet leg.

### Nightly

`contrib/node/cj1-nightly.sh OUT_DIR [COMPONENT...]` (and `--selftest`) builds `dashd-node`'s
lib test binary once at `HEAD` and runs `python_leg` (1200 s limit) then `wallet_leg` (1500 s
limit) one after the other at `nice 10`, failing closed on skips (`skipped:`) or missing
`1 passed` / `summary ` lines. Run `eval $(python3 contrib/setup-dashd.py)` first (`DASHD_PATH`
must be set). Knobs: `CJ1_NIGHTLY_SECONDS` overrides both wall-clock limits; `CJ1_NIGHTLY_BIN`
skips the build and runs a prebuilt lib test binary; `CJ1_PYTHON` and `CJ1_SCRATCH` reuse an
existing venv and `setup.json` (otherwise the script creates them under `OUT_DIR/cj1`, mode 700,
logging to `OUT_DIR/setup.log`). Failed runs copy `$CJ1_SCRATCH/*-target-run.txt` and
`$DASHD_TEST_RETAIN_DIR` (if set) into `OUT_DIR/<component>-evidence/` (capped at ~200 MB).

Coverage, measured v24 details and pending legs are in
[`PROVENANCE.md`](../../../dashd-coinjoin/tests/fixtures/dashd-v24/PROVENANCE.md).
