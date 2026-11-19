# G3 transaction intake captures

Recorded on regtest with Dash Core **v24.0.0 final**, binary SHA256
`2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223`.
The capture tool refuses any other binary. Each fixture records that SHA and
its exact node arguments. Blocks are mined into a private datadir and recorded
as `submitblock` inputs; fixture UTXOs identify the spendable outputs.

```sh
python3 contrib/node/g3-transcripts/capture.py dashd-p2p/tests/fixtures/dashd-v24
```

`P2A_DASHD`, `P2A_CAPTURE_DIR`, and `P2A_PORT_BASE` select the binary, private
data directory, and port range. The defaults use the release binary cached by
`contrib/setup-dashd.py` and `/work/scratch/p3/laneg/tmp-g3/capture`.

| Fixture | SHA256 |
| --- | --- |
| `tx-accept-relay.txt` | `3d24fcdc42eb698abfccffcdf0f33f7df550863c7daf2a5dde1db3f24addc6a6` |
| `tx-blocksonly-relay-perm.txt` | `c1b93dd6765b2153300db9034ad5b8da8252d20374e5f23a71ee36ddb8cfe2ec` |
| `tx-orphans-retry.txt` | `14c24583cb9864f7638ebe638ae88ebc6761e82bac20e6b38efe0de47ec3ac7e` |
| `tx-orphans.txt` | `9f933d7f36ad1fa3e703e714e015929da7240d096ca8e61731fae2c6fffe4b18` |
| `tx-reject.txt` | `df6d2399b9dbc0ed99c2f8380049c6ac66bbd36623633e33598e9da1337541e4` |

`tx-accept-relay` requires a valid transaction to enter the mempool and reach
two other peers, then sends the duplicate. `tx-orphans` sets the byte limit to
zero: a child requests its parent as MSG_DSTX but is evicted and needs a resend.
`tx-orphans-retry` retains the child, observes its admission after its parent,
then disconnects the sender of another orphan. `tx-reject` covers a script
failure (+100 and local disconnect), a nonstandard short transaction (zero),
a fee-policy rejection (zero), a repeated inv, and a forcerelay sender.
`tx-blocksonly-relay-perm` compares ordinary and relay-permission senders.

The Rust replays use the existing SimTxPool with Core-recorded admission
outcomes. Their hooks seed SimTxPool with the fixture UTXOs, enabling missing-input
checks on unscripted retries against those UTXOs and accepted parents. They compare parent getdata messages and disconnects exactly.
Transaction inv emission belongs to G4 and is excluded via Hooks::later;
separate G3 simulations assert the queued relay txids. Thus these captures prove
Core's relay output, while G3's replay proves its intake and queueing boundary.

Signed DSTX captures are deferred under Q7: this checkout has no production
CoinJoin intake adapter or masternode setup for it. The consumer seam is tested
with scripted accept/reject/stop outcomes, including default unread behavior,
priority application, validation before AlreadyHave, and storage after admission.
G5 wires the real adapter over CJ1; dashd-p2p does not depend on dash-coinjoin.
