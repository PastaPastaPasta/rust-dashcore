# P4 closure notes (G0)

Base: `6e5e468153bca03c91419aeb1fee84377d43092b`; branch: `node/p4-close`.

The integration base already implements test-chain spork announcements. The node installs
`SporkHandler` once in its registry. Its `verack_invs` obtains `ActiveSporks` in store order,
and the handshake's `verack_announce` queues those inventories through the existing relay,
only on test chains. The ordinary inventory send section batches them and filters duplicates.
Mainnet announces none. There is no second announcement path or new overlay accessor.

G0 retains the final-Core capture and tests the existing registry implementation using the
real `SporkHandler` and manager. The support table assigns `-peerblockfilters` to P6b;
the option remains inert when off.

## Core rc.3 to final audit

Audited `git diff v24.0.0-rc.3 v24.0.0 -- src/net_processing.cpp` read-only in the Core checkout.
The 26 changed lines (22 added and four removed) comprise five hunks:

- The `MAX_BLOCKTXN_DEPTH` static assertion uses `BLOCKS_NEAR_TIP`.
- `InitializeNode` adds `NODE_BLOOM` for the bloom-filter permission.
- Platform-ban handling suppresses already-seen hashes before logging/processing.
- `ProcessMessage` VERACK adds test-chain active-spork announcements at final lines 4233–4240.
- Pruned GETBLOCKS serving uses `BLOCKS_NEAR_TIP` in its retention bound.

Only the verack hunk is G0's scope. The baseline already includes its registry implementation;
G0 verifies that path against final Core. No observable difference in announcement order,
test-chain gating or batching was found in the two-spork transcript and sim checks.

## Transcript

Binary: `~/.rust-dashcore-test/dashcore-24.0.0/bin/dashd`.
SHA-256: `2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223`.
Capture: `python3 contrib/node/g0-transcripts/capture.py dashd-p2p/tests/fixtures/dashd-v24`.
The script uses P4b's capture machinery, a fresh regtest data directory, frozen mocktime
1700000000 and Core's public functional-test `-sporkkey`. It calls `sporkupdate` twice before
an inbound peer's version/verack and asserts a two-entry `MSG_SPORK` inventory was captured.
The replay lives in `dashd-masternode/tests/spork_transcripts.rs` alongside SP0's other
transcripts. It installs the real `SporkHandler`, signs the RPC updates with the manager,
and compares the complete handshake's outgoing messages using the existing transcript masks
(version identity and ping nonce); inventory bytes are compared without masking. The fixture
remains under `dashd-p2p/tests/fixtures/dashd-v24/`. Its SHA-256 is
`3e38d604b915b64493565433e8e2ae1c54c1a23dbb48b4b533e47b3a6d94e35a`
(nine node messages). No capture gap remains.

## P4 exits

These exits are **GATED**, not credited as passing by G0. Their owners must supply the
corresponding exit evidence; this small slice does not run their acceptance campaigns.

| Exit owner | State | Remaining evidence |
| --- | --- | --- |
| O11 | GATED | Owner's P4 exit validation |
| RP1a | GATED | Owner's P4 exit validation |
| O13a | GATED | Owner's P4 exit validation |
| LK1b | GATED | Owner's P4 exit validation |
| RP | GATED | Owner's P4 exit validation |

P6b owns the compact-filter-serving option gate. G0 implements neither its serving path nor
any of these other owners' work.
