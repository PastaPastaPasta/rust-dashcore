# Provenance of the `dash-mnlist` regtest captures

`sb11-regtest-v24.0.captures.json.gz` holds a Dash Core regtest masternode network's own replies:
headers, `mnlistdiff` and `qrinfo` payloads, the `clsig` and `isdlock` messages Core relayed, Core's
verdict on each lock, and `quorum info` (members included) of every quorum. Nothing in it is computed
by this repository; every payload is the exact wire bytes Core sent to a raw P2P peer.

| | |
|---|---|
| Producer | `dashd` of `dashcore-24.0.0-rc.3-x86_64-linux-gnu.tar.gz`, `Dash Core version v24.0.0-rc.3`, fetched by `DASHVERSION=24.0.0-rc.3 python3 contrib/setup-dashd.py` |
| Binary sha256 | `e204e3163185b1bffcd3bf3532c0957d8cc11019eaf8f3dbc3b2fc82a7ab5bb4` (`bin/dashd`) |
| Harness | `contrib/node/sb11-captures/capture_sb11.py`, a `DashTestFramework` functional test copied into the exported `test/functional/` of Dash Core tag `v24.0.0-rc.3` next to `contrib/node/lq3a-captures/capture_lq3a.py` (whose observers and P2P message capture it reuses) |
| Command | `contrib/node/sb11-captures/run.sh ~/workspace/dash v24.0.0-rc.3 ~/.rust-dashcore-test/dashcore-24.0.0-rc.3/bin /work/scratch/sb11/capture dash-mnlist/tests/data/sb11-regtest-v24.0.captures.json.gz` |
| Captured | 2026-10-22T09:15:23Z (`provenance.captured_utc`) |
| Chain arguments | `-vbparams=testdummy:999999999999:999999999999 -llmqtestparams=4:3 -llmqtestinstantsendparams=4:3 -llmqtestplatformparams=4:3 -watchquorums=1 -dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@100` (`provenance.chain_args`, from which the tests build their chain parameters) |
| File sha256 | `4c7e694a3457204c7fb585850d144c20c95e68089b7eae43133b6b98941e6425` |

## The chain

Nine nodes (node0 and eight masternodes), the basic BLS scheme, V20 and MN_RR at height 100, tip 311:

- two `llmq_test` (100) quorums, regtest's ChainLock type, then a ChainLocked block;
- rotation cycle F (264) of `llmq_test_dip0024` (103), regtest's DIP0024 InstantSend type, and lock A;
- rotation cycle C = F + 24 (288), with lock B signed at C's first block (by F's quorum) and lock D after
  C's quorums are eligible;
- three ChainLocked blocks; the best ChainLock is at 311.

## Fields

- `block_hashes`, `headers`: every header from genesis to the tip (`getblockheader <hash> false`).
- `diffs.step`: `getmnlistd(h - 1, h)` for every h in 1..=311 (the diff to 1 is pre-DIP3: Core sends a
  null block hash and no CbTx).
- `diffs.from_genesis`: `getmnlistd(null, h)` for the tip, every cycle base and every quorum's work
  block (21).
- `qrinfos`: `getqrinfo` at the tip and at each lock's signing tip, with no base and with a base hash
  (8, all asked with `extraShare` set, field `extra_share`).
- `messages`: the 8 `isdlock` / `clsig` payloads by inv hash, each with `core_verify` (Core's
  `verifyislock` / `verifychainlock`). `fields` is a debugging copy of the lock's ids and is not read.
- `quorums`, `quorum_list`: `quorum list` and `quorum info <type> <hash>` per type.
- `provenance`: the binary, the arguments, the deployments (`getblockchaininfo`), the cycles, the locks
  and the best ChainLock.

## Regenerating

Keys, txids and timestamps differ between runs, so a regenerated file differs byte-wise. The tests
read heights and verdicts from the file, except the constants in `src/tests/regtest.rs` (`FIRST`,
the height of the diff from genesis the step diffs continue from) and the floor heights of the
anchored tests, which follow from the chain arguments and the harness's mining schedule.

# `sb11-regtest-transitions-v24.0.captures.json.gz`

A second Dash Core regtest network, across the CbTx version 1 → 2 quorum-root transition (DIP0008)
and then V19 (the basic BLS scheme): headers, `mnlistdiff` payloads, the best ChainLock at five
points with Core's verdict, and `quorum info` of every quorum. As above, nothing in it is computed by
this repository; every payload is the exact wire bytes Core sent to a raw P2P peer, or Core's RPC
answer.

| | |
|---|---|
| Producer | the same `dashd` (`Dash Core version v24.0.0-rc.3`) |
| Binary sha256 | `e204e3163185b1bffcd3bf3532c0957d8cc11019eaf8f3dbc3b2fc82a7ab5bb4` (`bin/dashd`) |
| Harness | `contrib/node/sb11-captures/capture_sb11_transitions.py` (after Core's `feature_dip4_coinbasemerkleroots.py`), run by `run.sh` with `SB11_SCRIPT` set |
| Command | `SB11_SCRIPT=capture_sb11_transitions.py LQ3A_PORTSEED=2413 contrib/node/sb11-captures/run.sh ~/workspace/dash v24.0.0-rc.3 ~/.rust-dashcore-test/dashcore-24.0.0-rc.3/bin /work/scratch/sb11/capture-trans dash-mnlist/tests/data/sb11-regtest-transitions-v24.0.captures.json.gz` |
| Captured | 2026-10-22T14:01:36Z (`provenance.captured_utc`) |
| Chain arguments | `-testactivationheight=v19@340 -testactivationheight=dip0008@300 -testactivationheight=dip0024@900 -vbparams=testdummy:999999999999:999999999999 -dip3params=2:2 -testactivationheight=v20@900 -testactivationheight=mn_rr@900` (`provenance.chain_args`; the nodes also ran with `-deprecatedrpc=legacy_mn`, which lets the framework register masternodes with legacy BLS keys before V19 and is not a chain parameter) |
| File sha256 | `a626c63e2e7dbc178a9bb71b0724a65d13f2f76d6d44bf5ded96b07a2ebabcbc` |

## The chain

Four nodes (node0 and three masternodes), `llmq_test` size 3, threshold 2; no rotation (DIP0024 at
900), V20 at 900, tip 340:

- two `llmq_test` (100) quorums at 144 and 168, mined at 155 and 179 under CbTx version 1
  (`provenance.early_quorums`);
- ChainLocks "before" at 299 (CbTx version 1), "first rooted" at 300 (DIP0008, the first version-2
  CbTx), "later rooted" at 301, the active set still the two early quorums;
- a third quorum at 312, mined at 323 under version 2 and the legacy scheme
  (`provenance.late_quorum`), and the ChainLock "after" at 332;
- V19 at 340 and the ChainLock "after v19" at 340.

## Fields

- `block_hashes`, `headers`: every header from genesis to the tip.
- `diffs.step`: `getmnlistd(h - 1, h)` for every h in 1..=340.
- `diffs.from_genesis`: `getmnlistd(null, h)` for every quorum's work block (136, 160, 304), the
  blocks before DIP0008 and V19 (299, 339), DIP0008's first block (300) and the tip (340).
- `chainlocks`: `getbestchainlock` at each point (its `hex` is the `clsig` payload), Core's
  `verifychainlock` (`core_verify`, all true) and `quorum list` of `llmq_test` (`active`).
- `quorums`, `quorum_list`: `quorum list` and `quorum info <type> <hash>` per type, with
  `minedHeight`.
- `provenance`: the binary, the arguments, the deployments (`getdeploymentinfo`), `dip8_height`,
  `v19_height` and the quorums.

Regenerating gives a byte-wise different file for the same reasons; `src/tests/transitions.rs`
reads the heights from `provenance`, except the work and base blocks of the three quorums, which
follow from the harness's mining schedule.

# sb11-testnet-v24.0.captures.json.gz

A private Dash Core testnet node's own replies to a raw P2P peer, and its RPC answers. The node
served a reflink copy of a synced testnet datadir (manager ruling Q1 (a)) with no outbound
networking: `-testnet -connect=0 -dnsseed=0 -listen=1 -bind=127.0.0.1 -port=19511 -listenonion=0
-upnp=0 -natpmp=0 -discover=0` (`provenance.connections` is 0 after the harness disconnected).

| | |
|---|---|
| Producer | `dashd` (`Dash Core version v24.0.0-rc.3`, subversion `/Dash Core:24.0.0/`) |
| Binary sha256 | `e204e3163185b1bffcd3bf3532c0957d8cc11019eaf8f3dbc3b2fc82a7ab5bb4` (`bin/dashd`) |
| Harness | `contrib/node/sb11-captures/capture_sb11_testnet.py`, run by `run-testnet.sh` from Core's exported `test/functional/` |
| Command | `contrib/node/sb11-captures/run-testnet.sh ~/workspace/dash v24.0.0-rc.3 ~/.rust-dashcore-test/dashcore-24.0.0-rc.3/bin /work/scratch/sb11/capture-testnet http://sb11:sb11@127.0.0.1:19512 19511 dash-mnlist/tests/data/sb11-testnet-v24.0.captures.json.gz` |
| Captured | 2026-10-22T17:18:14Z (`provenance.captured_utc`) |
| File sha256 | `3dec075958bdd74bc858b828c999532cf43d15cb5c955412e6ad3cb8a01c0091` |

## Fields

- `block_hashes`, `headers`: every header from `floor` (1569533) to the tip (1574533),
  `getblockheader <hash> false`.
- `diffs.from_genesis`: `getmnlistd(null, 1573233)`, 1300 blocks below the tip.
- `diffs.step`: `getmnlistd(h - 1, h)` for every h in 1573234..=1574533.
- `qrinfos`: `getqrinfo(no base, cycle)` for the last three `llmq_60_75` cycles (1574496, 1574208,
  1573920), each with `extraShare` unset and set (`extra_share`).
- `chainlocks`: the ChainLocks the last 200 blocks' CbTx carry (`bestCLHeightDiff`,
  `bestCLSignature`), one per locked height (187, at 1574333..=1574532), each with Core's
  `verifychainlock` (`core_verify`, all true).
- `quorums`, `quorum_list`: `quorum list` and `quorum info <type> <hash>` (members included, with
  `minedHeight`) for the used types: 24 `llmq_50_60` (1), 32 `llmq_60_75` (5), 24 `llmq_25_67` (6).
- `provenance`: the binary, the deployments (`getdeploymentinfo`) and the connection count.

The datadir's ISLocks are gone (`getislocks` answers none once a block is ChainLocked), so the
capture has none. Regenerating needs a datadir at the same tip; a later one gives other heights.

# sb11-testnet-fetch-v24.0.captures.json.gz

Core's one `mnlistdiff` for each fetch item 12's restart test makes (SB11 notes D55, DEC-481):
the same kind of node as `sb11-testnet-v24.0.captures.json.gz`, a private Dash Core testnet node
on a reflink copy of the same synced testnet datadir (manager ruling Q1 (a), DEC-481), at the same
tip (1574533, asserted), with no outbound networking: `-testnet -connect=0 -dnsseed=0 -listen=1
-bind=127.0.0.1 -port=19511 -listenonion=0 -upnp=0 -natpmp=0 -discover=0 -disablewallet`
(`provenance.connections` is 0 after the harness disconnected). The pairs are the engine's own
requests, read in two steps: the first the lookup asks, then, with Core's reply to it applied, the
one step 5 asks.

| | |
|---|---|
| Producer | `dashd` (`Dash Core version v24.0.0-rc.3`, subversion `/Dash Core:24.0.0/`) |
| Binary sha256 | `e204e3163185b1bffcd3bf3532c0957d8cc11019eaf8f3dbc3b2fc82a7ab5bb4` (`bin/dashd`) |
| Request | P2P `getmnlistd(baseBlockHash, blockHash)` to a raw peer, the exact `mnlistdiff` bytes kept |
| Harness | `contrib/node/sb11-captures/capture_sb11_testnet_fetch.py`, run by `run-testnet.sh` (`SB11_CAPTURE`) from Core's exported `test/functional/` |
| Command | `SB11_CAPTURE=capture_sb11_testnet_fetch.py contrib/node/sb11-captures/run-testnet.sh ~/workspace/dash v24.0.0-rc.3 ~/.rust-dashcore-test/dashcore-24.0.0-rc.3/bin /work/scratch/sb11/capture-s9/harness http://sb11:sb11@127.0.0.1:19512 19511 dash-mnlist/tests/data/sb11-testnet-fetch-v24.0.captures.json.gz --expect-tip=1574533 --pair=1573336:1574194 --pair=1573336:1573600` |
| Captured | 2026-10-26T13:42:59Z (`provenance.captured_utc`) |
| File sha256 | `c32ea2e2607ca141c046b40c04183910744ff12a042ff26e11f02ad740717055` |

## Fields

- `diffs`: one entry per pair, in request order, with `base_height`, `base_hash`, `height`,
  `block_hash`, the `payload` hex and its `payload_sha256`:

  | Base | Target | Bytes | Payload sha256 |
  |---|---|---|---|
  | 1573336 `000000b4bf12…f133f450` | 1574194 `0000011592…31b81875` | 40,499 | `9b137032ba22caaae2717c8662d3b59c72757310d9c9686475eb890221000d81` |
  | 1573336 `000000b4bf12…f133f450` | 1573600 `0000001bf5…986a9c7b` | 25,108 | `244d713cc5b7756f0a958d47b704ef34356543143350bd97267908547607fb18` |

- `provenance`: the binary, the request form and the connection count.

The first pair was read twice (once alone to find the second request, then with it); both reads
gave the same payload sha256. Regenerating needs a datadir at the same tip.
