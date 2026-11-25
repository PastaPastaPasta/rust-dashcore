# Compact-filter serving fixtures

Captured on 2026-11-13 from Linux x86_64 **Dash Core v24.0.0 final**, protocol 70242,
using the release binary cached by `contrib/setup-dashd.py`:

- Binary: `~/.rust-dashcore-test/dashcore-24.0.0/bin/dashd`
- Binary SHA256: `2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223`
- Command: `python3 contrib/node/p6b-cfilters-transcripts/capture.py dashd-p2p/tests/fixtures/dashd-v24`
- Node arguments and mock time are recorded in each fixture. The serving node runs
  `-regtest -blockfilterindex=1 -peerblockfilters=1`; the negative-service node runs
  `-regtest -blockfilterindex=1 -peerblockfilters=0`.

The tool verifies the binary SHA256 before starting a node. It records real blocks, waits for
all peers to settle, and records large payloads by digest. Request payloads and all filter
responses are compared exactly by `dashd_cfilters_transcripts`; only the shared replayer's
existing handshake and nonce masks apply.

`serve-cfilters` starts with 1,100 blocks and extends the active chain to height 2,000 to
exercise the 2,000/2,001 header boundary and two checkpoint intervals. A formerly active
height-1,000 fork is replayed after its height-999 parent and before the replacement branch.
The replacement height-1,000 block pays `OP_2`, so it differs from the stale `OP_TRUE` block
under the frozen clock. The tool clears the temporary manual invalidation after the longer
branch wins, retaining a valid side chain for serving. It waits for the basic index to sync.

The fixtures cover 1,000/1,001 filters; 2,000/2,001 filter hashes; zero and nonzero previous
headers; empty, one-interval and two-interval checkpoints; side-chain filters, headers and
checkpoints; unknown types and block hashes; reversed ranges; and all three requests without
the compact-filter service. Valid capture steps assert their reply counts; invalid steps
assert disconnection. The test suite additionally covers missing index records, unvalidated
and old side-chain stops, and checkpoint caching across a reorg.

| Fixture | SHA256 |
| --- | --- |
| `serve-cfilters.txt` | `7b7e152a37edd3ac71923643eef42dc59913641a8f6adcab0e19e87fe2644356` |
| `serve-no-cfilters.txt` | `cf629b3148851d31f97a1fe5eec8df4965adfbe0572726fd444024bcc1aac22f` |
