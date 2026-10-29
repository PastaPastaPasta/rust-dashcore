# Provenance of the dashd v24 spork transcript fixtures

Every `*.txt` here was written by `contrib/node/sp0-transcripts/capture.py` (P2a-core's format,
defined in `contrib/node/p2a-transcripts/capture.py`, plus its `sporkupdate` record), run against
a **release** regtest dashd v24.0.0-rc.3 (`dashcore-24.0.0-rc.3-x86_64-linux-gnu.tar.gz`, dashd
sha256 `e204e316…a7ab5bb4`, as fetched by `DASHVERSION=24.0.0-rc.3 contrib/setup-dashd.py`), on
2026-10-15:

```
P2A_DASHD=…/dashcore-24.0.0-rc.3/bin/dashd python3 contrib/node/sp0-transcripts/capture.py OUT_DIR
```

Nothing in them was edited by hand. Each file's header records the dashd version and arguments;
every node starts from a fresh datadir with `-mocktime=1700000000` (`sim::SIM_START`). The spork
key is Core's functional-test key (`test_framework.py`), regtest's spork address's.

| Fixture | What happens |
|---|---|
| `sporks-serve` | a node with `-sporkkey`: an inbound peer's `getsporks` on an empty store, repeated; `sporkupdate` of the five v24 sporks; `getsporks` (Core's `unordered_map` order), repeated (v24 dedup); SPORK_2 replaced; `getsporks` twice; a second peer's `getsporks`, `getdata MSG_SPORK` for the replaced SPORK_2 (still served by hash) and for an all-zero hash (`notfound`) |
| `sporks-receive` | a node without a key: SPORK_19 sent twice (new, then seen) and with its header byte +8 (an alias, seen); SPORK_2 with s negated (high-S, accepted); `getdata` for both; SPORK_21 with a flipped bit in r (+100, closed); a spork signed at `SIM_START+7201` (too far in the future, closed); a 66-byte signature and a truncated payload (malformed, closed); SPORK_21 unaltered from a fifth peer, then `getsporks` |
| `sporks-sync-tick` | evidence for SP1, not replayed: an outbound peer while the mock time moves in 1 s steps; dashd's own `getsporks` goes out at `SIM_START+6` (`SyncManager::ProcessTick`'s 6 s gate) and only once |

The sporks in `sporks-receive` were signed by separate regtest dashd v24.0.0-rc.3 nodes run by the
same tool with `-sporkkey` (`signed`: `sporkupdate`, then the bytes of their `getsporks` reply), at
the mock times `SIM_START` and `SIM_START+7201`. The tool's alterations (`alias`, `high_s`,
`bad_sig`, the 66-byte signature, the cut last byte) are inputs; every `<` line is dashd's.

`tests/spork_transcripts.rs` replays `sporks-serve` and `sporks-receive` with
`dashd_p2p::sim::transcript` and SP0's handler registered, dropping dashd's spork `inv`s (P5a's
relay, SP0 D4).

## `verifymessage.txt`

Written by `contrib/node/sp0-transcripts/verifymessage.py` against the same dashd, on 2026-10-15.
For eight spork tuples (the five v24 IDs and three edge cases: negative and extreme IDs, values
and times), regtest dashd signs the spork message (`ID‖value‖time` in decimal) with
`signmessagewithprivkey` and Core's functional-test key (the `signed` lines), then judges 322
altered signatures each with `verifymessage` against regtest's spork address: all 256 header
bytes, s negated with and without the recovery ID's parity flipped, and 64 seeded random bit
flips. The tool chose the alterations; every verdict is dashd's. `CSporkMessage::Sign` and
`CheckSignature` use the same `CMessageSigner` on every network but testnet.
`tests/spork_signatures.rs` signs each tuple (byte-equal to dashd's) and checks every verdict.

## `../live/sporks.txt`

Written by `contrib/node/sp0-transcripts/live_sporks.py` on 2026-10-15 (the file's header has the
minute): `getsporks` to three public mainnet and three public testnet peers (DNS seeds and
`dash-network-seeds/seeds`), keeping each distinct spork once with the first peer that sent it.
They are what the networks' spork signers signed, relayed by the peers' dashd: 20 on mainnet
(message form) and 23 on testnet (hash form), all from uncompressed keys, unknown IDs included.
The unit test `live_sporks_verify_under_their_network_only` checks each against its network's key
and form and not the other's, with the header alias, an altered value, a flipped bit in r and the
two-hour limit.
