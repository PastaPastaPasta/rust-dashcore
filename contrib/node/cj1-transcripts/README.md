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

Coverage, measured v24 details and pending legs are in
[`PROVENANCE.md`](../../../dashd-coinjoin/tests/fixtures/dashd-v24/PROVENANCE.md).
