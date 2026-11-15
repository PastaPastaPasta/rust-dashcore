# Core flat files, key files and dash.conf vectors (IM1b)

Every file here was written by Dash Core, none was edited. The regtest vectors come from a real
**v24.0.0-rc.3** `dashd` (tag `v24.0.0-rc.3`, commit `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`;
binary `/work/chains/bin/dashcore-24.0.0-rc.3/bin/dashd`, sha256
`e204e3163185b1bffcd3bf3532c0957d8cc11019eaf8f3dbc3b2fc82a7ab5bb4`), captured on 2026-10-15. The
files are copied byte for byte from the node's datadir after a clean stop.

## `empty/`

`governance.dat`, `mncache.dat`, `netfulfilled.dat`, `sporks.dat` of a plain one-node regtest node
that was started and stopped at once (Core's framework, `--empty`): the envelope with nothing in it.

## `populated/`

The same four files from node 0 (the framework's controller) of a seven-node regtest network at
height 179 (mock time `1417713337` onwards), produced by
`dash-coreimport/scripts/im1b-capture-governance.py` run inside Core's own functional-test tree
(`git archive v24.0.0-rc.3 test/ share/rpcauth`, a venv with `dash_hash` and `py_ecc`):

```sh
B=/work/chains/bin/dashcore-24.0.0-rc.3/bin
DASHD=$B/dashd DASHCLI=$B/dash-cli PYTHONPATH=. <venv>/bin/python im1b-capture-governance.py \
  --configfile=<core>/test/config.ini --tmpdir=<run>/populated --nocleanup --outdir=<out> \
  --timeout-factor=6 --portseed=732          # the empty set: add --empty
```

- six masternodes (four regular, two evonodes), all `ENABLED`; node 0 is the controller, not a
  masternode itself;
- governance: `Proposal_0` with mixed votes, `Proposal_1` that failed funding, `Proposal_2` that
  every masternode funded, `Proposal_3` that was voted for deletion and then erased by
  `CheckAndRemove` (so `mapErasedGovernanceObjects` holds it and its votes are gone), and one
  superblock trigger the masternodes created and voted on themselves; 30 votes are held at the end
  (the script cast 33 through RPC, 8 of them on the erased proposal). The valid, delete and endorsed
  votes are signed with each masternode's BLS operator key by the script (`voteraw`), the funding
  votes with the voting key (`gobject vote-alias`);
- sporks: the five v24 sporks (`SPORK_2`, `17`, `19`, `21`, `23`) set to distinct values and
  updated more than once, so `sporks.dat` also holds the two superseded messages `mapSporksByHash`
  keeps, all signed with the regtest spork key;
- `netfulfilled.dat`: six peers on `127.0.0.1`, five with `full-sync`, `governance-sync` and
  `spork-sync` requests and one with only `full-sync`.

The bytes differ on every regeneration (random keys and hashes), which is why the tests read the
ground truth from the committed `expected.json` and not from constants.

`expected.json` is ground truth read from the same node over RPC just before the stop
(`spork show`, `spork active`, `gobject count/list all/get/getcurrentvotes`, `masternodelist`,
`protx list valid`, `masternode count`, `getpeerinfo`) together with what the script itself did
(`votes_cast_by_script`, `proposal_hashes_submitted`, `masternodes`). `tests/flatfiles.rs`
compares the parsed files with it. It also records each file's sha256, which are:

| file | sha256 |
|---|---|
| `populated/governance.dat` | `8970bca91edb219e02a522f2ac6145b89275cb6252d072124fc681aae88d3425` |
| `populated/mncache.dat` | `fd20186cd091cced2482871b2a29f6b9287ad9e097dc06b3c5ef149fc2b4aab0` |
| `populated/netfulfilled.dat` | `47abf81d617fec426d5670b6446789470ebee22108367eec8897eff73882f9de` |
| `populated/sporks.dat` | `44acc61091150ee6dcc276da389384cc745fb8354a2f40b4fbe549a133d47c59` |
| `empty/governance.dat` | `5692b99ebc0e1099ee5e970ee879b3f6a86770e067f3af50839fa4b8fba1af3c` |
| `empty/mncache.dat` | `9986c1030eff3cb878bc9b206f14a09cd64553cab887db8b8730a22f19a0eb17` |
| `empty/netfulfilled.dat` | `723dcd105d40f970e6f3066327c326ee53ae81bb58e46eabf927de047824158c` |
| `empty/sporks.dat` | `9961d71a63a97ad7346de85a2ba7eb42b72c5bfbaedd46728777bb71e84428b8` |

## `oracle-mainnet/`, `oracle-testnet/`

The flat files and the `dash.conf` of the BS2 reference oracle datadirs (a Dash Core v24 mainnet node
and testnet node, see `node-program/ORACLE.md`), copied with a plain `cp` while the nodes ran (pure
reads; the datadirs were neither locked nor modified), 2026-10-15. They are what real, long-lived
datadirs hold: mainnet's `governance.dat` and `mncache.dat` are nearly empty (141 and 96 bytes),
`sporks.dat` carries the mainnet spork history, and testnet's `governance.dat` (136 KB) embeds a
real testnet masternode list. The `dash.conf` files carry no secret (`rpccookiefile` only); the
`[test]` section of the testnet one is why the section tests use it.

| file | sha256 |
|---|---|
| `oracle-mainnet/dash.conf` | `541503fa1d9f8b418b3e0dbba5966628d2f64f5c0d53e88e3f128cd072414770` |
| `oracle-mainnet/governance.dat` | `616ec74d9f45c9b46069648dae806aecf81038faadacfcaf7d59fa3bdad4b161` |
| `oracle-mainnet/mncache.dat` | `c98b253b527ff416957fa8cc4e648bae448b313c2c36bd76e71129286cc07746` |
| `oracle-mainnet/netfulfilled.dat` | `7da8e83fa2a4088f5842182e15b2e65cd3ff8da87973c09376baa21c5dc0987e` |
| `oracle-mainnet/sporks.dat` | `a6a9a99d9f4ee2a9ef211af1ca3374c66e024b8a0be6bd5dfaaf01b923fcd109` |
| `oracle-testnet/dash.conf` | `d5fb648bddd1be4bdfc105b2828b2c9eec390820e5f843b3bed89e99145d3c53` |
| `oracle-testnet/governance.dat` | `b87ee58561d9bdbe9f257d1492fad67b8b4027338e272a49bb1ee18949a60a2e` |
| `oracle-testnet/mncache.dat` | `411afdbd6606d20b373a3b9872b96baefae28f3f7d9256f99eaecc69fdf0988b` |
| `oracle-testnet/netfulfilled.dat` | `d4eb05a5a2857929c437f45d6e4c4b732bb5e3bc7b7b2370401530a4f7727aaf` |
| `oracle-testnet/sporks.dat` | `0b9482fc2ea62530aba48e19079e8ed4e059c59252bcd6977e9dfdc3dc95c38b` |

## `keys/`

`onion_v3_private_key` and `i2p_private_key` as the v24.0.0-rc.3 `dashd` itself wrote them
(`WriteBinaryFile` in `torcontrol.cpp` and `i2p.cpp`), produced by
`dash-coreimport/scripts/im1b-capture-keys.py`. There is no Tor or i2pd on the capture host, so the
node talked to two in-process mock servers: a Tor control port that answers `ADD_ONION
NEW:ED25519-V3` with a freshly generated, valid expanded key, and a SAM bridge that answers `DEST
GENERATE SIGNATURE_TYPE=7` with a well-formed 679-byte private destination. The mock keys are real
Ed25519 keys; the encryption half of the I2P destination is random bytes (Core never checks it).
`dashd` was run twice on the same datadir: the second run read the files back and sent the
same key to the mock (the script asserts both), which is what `expected.json` records together with
the address Core logged (`onion_address_core_logged`, `b32_address_core_logged`) that the tests
match against the address the reader computes.

**These are throwaway keys that exist only in this repository's test data. They protect nothing.**

| file | sha256 |
|---|---|
| `keys/onion_v3_private_key` | `1495d44598f93aaee792ee3cba61bacd7f4c297064585a46fd0dc9d03fc79eaf` |
| `keys/i2p_private_key` | `1fb1ca54cbb9beda261879ca2c85962cd55636fad2b6dc4bb0bc48dd793c35c3` |
| `keys/expected.json` | `923e054efe20f2c07db5a6fcbec7baee8d16cea0758350f6942431778de954c3` |
