# Provenance of the dashd v24.0.0-rc.4 masternode-sync fixtures

Every file here was written by `contrib/node/sp1-transcripts/capture.py` (P2a-core's transcript
format, defined in `contrib/node/p2a-transcripts/capture.py`, plus SP1's `mnsync` check and
`mnsyncreset` input). It ran against a **release** regtest dashd v24.0.0-rc.4 (the oracle pin
375c64d2317's tag, as fetched by `contrib/setup-dashd.py` into
`~/.rust-dashcore-test/dashcore-24.0.0-rc.4/`, dashd sha256 `e1be2564…f823a7`) on 2026-10-27:

```
P2A_DASHD=…/dashcore-24.0.0-rc.4/bin/dashd python3 -I contrib/node/sp1-transcripts/capture.py OUT_DIR
```

Nothing was edited by hand. Each transcript's header records the dashd version and arguments.
Every node starts from a fresh datadir with `-mocktime=1700000000` (`sim::SIM_START`) and mines one
block before any peer connects. Each `mnsync` line is dashd's `mnsync status` (AssetID, AssetName,
AssetStartTime, Attempt, IsBlockchainSynced, IsSynced) after the segment before it settled.

| Fixture | What happens |
|---|---|
| `mnsync-disabled` | `-disablegovernance=1` (rdashd's default, DEC-510); an outbound, a block-relay-only and an inbound peer; the mock time in 1 s steps. BLOCKCHAIN switches at +36 (30 s timeout with ≤ 3 peers, ticks on a 6 s grid) with `mempool` to the outbound peer only; GOVERNANCE ends at +42. `mnsync reset` at +50; the next tick (+54) disconnects all three `full-sync` peers |
| `mnsync-four-peers` | four outbound peers: the switch at +12 (the 6 s timeout with > 3 peers), `mempool` to each |
| `mnsync-blocksonly` | `-blocksonly=1` too (rdashd's node defaults): the switch sends no `mempool` |
| `mnsync-governance` | governance on (Core's regtest default): `govsync(0, empty bloom)` to peer 1 at +42 and peer 2 at +48 (Attempt 1, 2), then FINISHED at +72, the 30 s timeout after GOVERNANCE started at +36 |
| `mnsync-allow-sync` | SP1b (DEC-562), `-disablegovernance=1` and `-whitebind=noban@127.0.0.1:30960`: an outbound peer; BLOCKCHAIN switches at +36 with `mempool` to it. Before the next tick, a manual peer (`addnode onetry`), a `noban` peer through the `-whitebind` listener and a second outbound peer connect. The tick at +42 finishes after looking at peer 1 only, so FINISHED gives all four `full-sync` before the newcomers' first `allow-sync` check. `mnsync reset`; the tick at +48 removes the manual and the noban peer's requests (dashd logs `skipping mnsync restrictions` for both) and keeps them, and disconnects both outbound peers as recently synced |

`<scenario>.netfulfilled.dat` and `<scenario>.mncache.dat` are the files dashd wrote at its clean
`stop` after the transcript. The `netfulfilled.dat` files hold the `mempool-sync`, `full-sync`
and `governance-sync` requests above, with their expiries (300 s after the request, regtest's
`nFulfilledRequestExpireTime`). The `mncache.dat` files hold no masternode, since regtest has
none; the populated vectors are IM1b's rc.3 captures (`dash-coreimport/tests/data/flatfiles/`).

The noban peer dials from 127.0.0.1, source port 30973 (below the ephemeral range, as the box
requires), and is recorded as P4a's `accept 3 from 127.0.0.1 perm noban`. The replay gives it port
40003 (P4a's 40000+N); no compared byte shows the port, since `addr_recv` is all zeros for
127.0.0.1 on both sides (P2a's masks). The allow-sync capture's command was the one above, with
`mnsync-allow-sync` as the scenario, run without `-I`, on 2026-10-27.

`tests/sync_transcripts.rs` replays the five transcripts with SP1's tick and checks every `mnsync`
line. `tests/core_flatfiles.rs` reads the body of each `.dat` file (and IM1b's) with SP1's codec,
writes it back byte for byte, and compares the result with IM1b's reader.

| File | sha256 |
|---|---|
| `mnsync-allow-sync.mncache.dat` | `9986c1030eff3cb878bc9b206f14a09cd64553cab887db8b8730a22f19a0eb17` |
| `mnsync-allow-sync.netfulfilled.dat` | `d57a9a800c296aa16536e7eb555f9f7f34137f7d50b08c7710961e1e8b1d5a7e` |
| `mnsync-allow-sync.txt` | `4649220927fbd24efd91f170e6eb924f0f3a99736f9c11769c2d5654fe52713a` |
| `mnsync-blocksonly.mncache.dat` | `9986c1030eff3cb878bc9b206f14a09cd64553cab887db8b8730a22f19a0eb17` |
| `mnsync-blocksonly.netfulfilled.dat` | `d251610d9afc00148fce84555c0ffc306da1c4b2d2a8de3f537732ab7f95a78f` |
| `mnsync-blocksonly.txt` | `218da5b8bae129b2466c7808a328772e113a1e8c8e97efcf30d49f2530326423` |
| `mnsync-disabled.mncache.dat` | `9986c1030eff3cb878bc9b206f14a09cd64553cab887db8b8730a22f19a0eb17` |
| `mnsync-disabled.netfulfilled.dat` | `6e0c31ebcd303e9d65dbf5992cd072d23e2b6a13d1a6630c467081e6d4fd6b8f` |
| `mnsync-disabled.txt` | `a8ba1fc9030b16ce2c58a3713375a89d773674b3f81cbc803fccfb306d4f2684` |
| `mnsync-four-peers.mncache.dat` | `9986c1030eff3cb878bc9b206f14a09cd64553cab887db8b8730a22f19a0eb17` |
| `mnsync-four-peers.netfulfilled.dat` | `83570b2c4a407acd21c8c5a074e4d85ec88afb0667016294e45bd11cd47f031e` |
| `mnsync-four-peers.txt` | `f70e2e82d5f0efa6d9b89bfb488e0c3cc842613a9ae40fcaa257521cf91561fe` |
| `mnsync-governance.mncache.dat` | `9986c1030eff3cb878bc9b206f14a09cd64553cab887db8b8730a22f19a0eb17` |
| `mnsync-governance.netfulfilled.dat` | `66aaae1078cdc9d4b85af1c36a8c9aedf0a9909811173accb36e5c2078a6db86` |
| `mnsync-governance.txt` | `1934de53567d9ecc3f73a8aad44fda36c2a1985c9cadd286a3906a8a12105cbe` |
