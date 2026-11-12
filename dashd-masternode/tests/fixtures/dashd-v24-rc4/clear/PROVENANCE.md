# Provenance of the dashd v24.0.0-rc.4 `CoreRead` clear vectors

`contrib/node/sp1-transcripts/clear_vectors.py` wrote every file here on 2026-10-27, against the
release regtest dashd v24.0.0-rc.4 of `../PROVENANCE.md` (dashd sha256 `e1be2564…f823a7`):

```
P2A_DASHD=…/dashcore-24.0.0-rc.4/bin/dashd python3 -I contrib/node/sp1-transcripts/clear_vectors.py OUT_DIR
```

Each `<case>.dat` is a regtest `netfulfilled.dat`, `mncache.dat` or `sporks.dat` in Core's `CFlatDB` format
(Core's magic message, the regtest message start, the object, its SHA256d), crafted to stop at
one compact size. The tool started dashd once per case on a fresh datadir holding that file, and
`coreread-clear.txt` records the `CoreRead` line dashd's debug.log printed for it (after the
time and `[error] `). Nothing was edited by hand.

| Case | Where the read stops |
|---|---|
| `nf-short` | a 10-byte file, shorter than its hash (Core refuses it: `HashReadError`) |
| `nf-magic` | the magic message's length, `MAX_SIZE + 1` |
| `nf-outer` | the service map's count, `MAX_SIZE + 1` |
| `nf-outer-max` | the service map's count, `MAX_SIZE`, then no data |
| `nf-outer-noncanonical` | the service map's count, 252 in the 3-byte form |
| `nf-outer-noncanonical-9` | the service map's count, 5 in the 9-byte form |
| `nf-outer-over-9` | the service map's count, `MAX_SIZE + 1` in the 9-byte form: non-canonical, which Core checks first |
| `nf-inner` | one service, then its request map's count, `MAX_SIZE + 1` |
| `nf-name` | one service and one request, then the name's length, `MAX_SIZE + 1` |
| `sp-sig-65` | one by-hash spork whose signature is 65 bytes, `LIMITED_VECTOR`'s bound: the read stops at `mapSporksActive` |
| `sp-sig-66` | the same spork with a 66-byte signature: over the bound |
| `sp-sig-over` | the same spork with a signature length of `MAX_SIZE + 1`: `ReadCompactSize` refuses it before the bound |
| `mn-version` | the version string's length, `MAX_SIZE + 1` |
| `mn-infos` | the infos' count, `MAX_SIZE + 1` |
| `mn-votes` | one info, then its governance-vote map's count, `MAX_SIZE + 1` |
| `mn-used` | no infos, `nDsqCount`, then the used masternodes' count, 2^32 in the 9-byte form |
| `mn-used-max` | the used masternodes' count, `MAX_SIZE`, then no data |
| `mn-used-noncanonical` | the used masternodes' count, 65535 in the 5-byte form |

`tests/core_flatfiles.rs` puts each object in SP1's envelope (SP1's magic and version string) and
checks that SP1's codec fails the same read and logs the same line (a spork file without Core's hash keys, SP0 D3).

| File | sha256 |
|---|---|
| `coreread-clear.txt` | `c079c32618e7139e5282d84163ef312336d59d635bc2c4273213e83bc12f607c` |
| `mn-infos.dat` | `dbf5e370c45c544100f5b0ea3a41dcd5d166a80b2b6eb29d1840faf6fdebe59e` |
| `mn-used-max.dat` | `0e040c4433e5b76662ed46e238477fe0b3d6efe6173daf32cc5470e455ca839e` |
| `mn-used-noncanonical.dat` | `f73467cffbd17d63ac6e63a0cd477138b3839fe70055731a398f88a9e74d2560` |
| `mn-used.dat` | `0f8d7cd01370fb58c2b908bd243f62c6ae5be1e5b0d443c33edceb892f9beb32` |
| `mn-version.dat` | `55d98f75c8116553a49528c38a73460afeba78ef1d2d6a7601e3e0d2f120ded3` |
| `mn-votes.dat` | `f3badf27802169cf5962b90c46bf02a440c606bb9881e1fd88ecfb27da016681` |
| `nf-inner.dat` | `7963b9841432a88fdc176eff70f6ed165e3df81535bd686157f14d7ab9562366` |
| `nf-magic.dat` | `9579f086d6bc37548483258dc05a6f893c0d2086748579e81b63c49d27177964` |
| `nf-name.dat` | `4672b64634a7215bc5df571900e304519998bbff6d63bb3dfc3c0c990781e877` |
| `nf-outer-max.dat` | `a2cf1630d673a8e5366c1434ef3d90a62a10352c227c816f51923e07fc4a24ae` |
| `nf-outer-noncanonical-9.dat` | `abb6c570463ee289d2aacb058b9c7c45f173702505a6a68d5e49becea1a82329` |
| `nf-outer-noncanonical.dat` | `694166bb00834b6f9767e912b01f6aaeba69100c3f8b32232a428bd9fea71eee` |
| `nf-outer-over-9.dat` | `cb2183b775d3d5f92885e2593f1513e0e3fdb5bb8c891b9e9a75f594dd3b5af1` |
| `nf-outer.dat` | `446673e07b2d67faa21614fe6aaa84ef8bc68284fbf6474d536d5b9b2fa7c5b2` |
| `nf-short.dat` | `ffadf8d89d37b3b55fe1847b513cf92e3be87e4c168708c7851845df96fb36be` |
| `sp-sig-65.dat` | `d93974eade68bade8716933636411085a497542f61b402a160b62b2d958286cb` |
| `sp-sig-66.dat` | `0a459e5856a6ccd334db14d5f1d886cdae458f9bb67a04efdf71affa3a7f01f7` |
| `sp-sig-over.dat` | `d3c9ac510b79ad3ab0deaac9587bcd8082a2e939ea027f686759ef7c61600901` |
