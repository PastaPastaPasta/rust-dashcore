# Provenance of the fingerprint lab's dashd transcripts

Every `fp-*.txt` here is a transcript of a real **release** dashd v24.0.0-rc.3
(`dashcore-24.0.0-rc.3-x86_64-linux-gnu.tar.gz`, dashd sha256
`e204e3163185b1bffcd3bf3532c0957d8cc11019eaf8f3dbc3b2fc82a7ab5bb4`, as fetched by
`DASHVERSION=24.0.0-rc.3 contrib/setup-dashd.py`; DEC-82), recorded on 2026-10-22 by

```sh
P2A_DASHD=~/.rust-dashcore-test/dashcore-24.0.0-rc.3/bin/dashd \
  python3 contrib/node/fingerprint/capture.py fixtures OUT_DIR
```

and copied here unedited. The script is P2a's and P2b's capture harness
(`contrib/node/p2a-transcripts/capture.py`, `contrib/node/p2b-transcripts/capture.py`) with
BIP324 peers: their encryption is Core's own functional-test implementation
(`test/functional/test_framework/v2_p2p.py` of the v24.0.0-rc.3 tag, found through
`FP_CORE_TEST_FRAMEWORK`). Each dashd runs on loopback only, from a fresh datadir with
`-mocktime=1700000000`, `-dnsseed=0 -fixedseeds=0 -listen=1 -bind=127.0.0.1` and ports below
32768 (P2a's base arguments); each file's `# args:` line is its exact command line.

| Fixture | Profile (extra arguments) | Chain | Scenario |
|---|---|---|---|
| `fp-default-{out,in}-{v1,v2}` | none | regtest | handshake |
| `fp-default-framing` | none | regtest | behaviour 10: bad checksum, invalid command, oversized header, wrong magic |
| `fp-default-rekey` | none | regtest | BIP324 rekey: 230 pings over outbound V2 |
| `fp-blocksonly-{out,in}-v1` | `-blocksonly` | regtest | handshake |
| `fp-listen0-{out,in}-v1` | `-listen=0` (no `-listen=1`, no `-bind`) | regtest | handshake; inbound refused |
| `fp-v2off-{out-v1,in-v1,in-v2}` | `-v2transport=0` | regtest | handshake; a V2 peer is closed |
| `fp-whitelist-in-{v1,v2}` | `-whitelist=127.0.0.1` | regtest | handshake |
| `fp-testnet-{out,in}-{v1,v2}` | `-testnet -connect=0` | testnet | handshake |
| `fp-mainnet-{out,in}-{v1,v2}` | `-connect=0` | mainnet | handshake |

Outbound connections are `addconnection` on regtest and `addnode … onetry` (`manual`) on
testnet and mainnet, where `addconnection` is unavailable. The V2 fixtures
(`fp-{default,testnet,mainnet}-{out,in}-v2`, `fp-whitelist-in-v2`, `fp-v2off-in-v2`) and
`fp-default-rekey` were recaptured the same day with the tool's D9 fixes (design note), by naming
them after `OUT_DIR`; `fp-v2off-in-v2` came out unchanged. `KNOWN-DIFFS.json` lists rdashd's
remaining differences from these transcripts, each with its BACKLOG row.
All 23 were recaptured once more with the tool's V1 frame validation (design note D14) and came
out equal to these under the masks, so none was replaced.
