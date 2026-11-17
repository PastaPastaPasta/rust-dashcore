# Dash Core wire vectors

Generates `dash/tests/data/core_v24_regtest.json` and `core_v24_regtest_mnlistdiff.json`, the
Dash Core v24 vectors used by `dash/tests/core_v24_vectors.rs`. Everything in them is produced by
dashd: Core's own functional tests build regtest chains with every v24 payload (asset lock and
unlock version 2, ProTx version 3 with extended addresses and DIP-0026 payouts, shared
masternodes and special transaction types 10-12, CbTx version 4), and the extractor reads the
raw transactions, blocks and masternode list diffs back with Core's JSON for them.

```bash
./run-functional-tests.sh ~/workspace/dash v24.0.0-rc.3 /path/to/dashcore-24.0.0-rc.3/bin /tmp/v24-vectors
python3 -I extract.py /path/to/dashcore-24.0.0-rc.3/bin /tmp/v24-vectors
python3 -I slim.py /tmp/v24-vectors/vectors-raw.json ../../tests/data/core_v24_regtest.json
cp /tmp/v24-vectors/mnlistdiffs.json ../../tests/data/core_v24_regtest_mnlistdiff.json
```

Regtest keys, txids and timestamps differ between runs, so regenerated files differ byte-wise
but cover the same layouts (`core_v24_coverage` checks that).

## Extended addresses (`core_v24_netinfo.json`)

`craft_netinfo.py` tests how Core reads the `netInfo` of version 3 ProRegTx and ProUpServTx
payloads it did not write itself. On a copy of the `feature_masternode_payout_shares` chain it
registers a regular masternode and an Evo node and updates both, letting dashd build and sign
every payload (`protx register_prepare`, `register_prepare_evo`, `update_service` and
`update_service_evo` with submit=false). It then rewrites only the `netInfo` bytes, keeping
Core's payload signatures, and records dashd's `testmempoolaccept` verdict and
`decoderawtransaction` JSON for each variant. Core's own payloads are mined, and their blocks
kept; since v24.0.0-rc.4 no rewritten netInfo is accepted (a repeated, out-of-order or unknown
purpose fails the read, #7888).

```bash
python3 -I craft_netinfo.py /path/to/dashcore-24.0.0-rc.4/bin \
    /tmp/v24-vectors/ft/feature_masternode_payout_shares/node0 /tmp/v24-netinfo \
    ../../tests/data/core_v24_netinfo.json
```

The committed file was generated with the release binary of `dashcore-24.0.0-rc.4-x86_64-linux-gnu.tar.gz`
(sha256 `dd91084338e2415f4c4b8307e62668530c1da0bb8875f07b5662e797945d7b4c`) on a chain built by
that binary running Core's functional tests at tag v24.0.0-rc.4 (`feature_masternode_payout_shares`
alone).

## Quorum rotation info (`core_v24_regtest_qrinfo.json`)

`capture_qrinfo.py` asks a v24 node for `qrinfo` over P2P (`getqrinfo`), with and without the
extra share and with one or two base blocks, and keeps each answer with Core's
`quorum rotationinfo` JSON for the same request. The chain comes from Core's functional test
`feature_llmq_rotation`, which builds rotating (DIP-0024) quorums:

```bash
./run-functional-tests.sh ~/workspace/dash v24.0.0-rc.3 /path/to/dashcore-24.0.0-rc.3/bin \
    /tmp/v24-qrinfo feature_llmq_rotation
python3 -I capture_qrinfo.py /path/to/dashcore-24.0.0-rc.3/bin \
    /tmp/v24-qrinfo/ft/feature_llmq_rotation/node0 /tmp/v24-qrinfo/capture \
    ../../tests/data/core_v24_regtest_qrinfo.json
```

Capturing again from the same chain gives byte-identical messages.
