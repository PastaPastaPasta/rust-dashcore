# Provenance of the dash-oracle-sys smoke samples

`mainnet.json` and `testnet.json` were captured on 2026-10-08 by
`dash-oracle-sys/scripts/gen-samples.py` from the BS2 reference oracles on agentbox
(`/work/chains/{mainnet,testnet}`, dashd v24.0.0-rc.3, RPC ports 27998/37998).
Only read-only RPCs were used, plus read-only access to the oracles' undo files
(`blocks/rev*.dat`). Each file's `provenance` object records the
dashd version, the tip height and hash at capture time, and whether the node was
still in initial block download.

| Field | Source RPC |
|---|---|
| `blocks[].hex`, `hash`, `merkle_root` | `getblock <hash> 0` / `getblock <hash> 2` |
| `blocks[].txs[].txid`, `instance_hash`, `type` | `getblock <hash> 2` (`instanceHash` falls back to `txid` when absent) |
| `blocks[].txs[].prevouts` | `getrawtransaction <txid> 1` of every spent output, `getblockheader` for its height |
| `header_ranges[].hex` | `getblockheader <hash> false` for every height in the range |
| `subsidy[]` | `getblockstats <h>` (`subsidy`, `totalfee`) and `getblockheader` of `h-1` (`bits`) |
| `superblocks[]` | `getsuperblockbudget <h>` |
| `payments[]` | `masternode payments <hash> 1` (sum of `masternodes[].amount`) and `getblockstats` |
| `deployments` | `getdeploymentinfo` |
| `blocks[].undo_hex` | the block's `CBlockUndo` record in `blocks/rev*.dat`, matched by transaction and input count and confirmed by the record's checksum `SHA256d(prev block hash ‖ CBlockUndo)`; absent for blocks without inputs |
| `special_tx_blocks` | testnet only: the first, middle and last sampled height per special transaction type, found by scanning every 331st block from 7000 |

Both oracles were still in IBD at capture time (mainnet tip 672642, testnet tip
676252). The samples therefore stop before DIP3 enforcement on mainnet and
before V19/V20/MN_RR/V24 on testnet: the payment samples (testnet only) exercise
the Classic reward era, and there are no mainnet payment samples. Re-run the
generator once the oracles reach tip to extend them; heights above the tip are
skipped automatically. The later reward eras, V20 subsidies and successful
PlatformShare() calls are covered meanwhile by the regtest chain of Core's
block_reward_reallocation test (tests/contexts.rs).

To regenerate:

```bash
python3 -I dash-oracle-sys/scripts/gen-samples.py --network mainnet --out dash-oracle-sys/tests/data/mainnet.json
python3 -I dash-oracle-sys/scripts/gen-samples.py --network testnet --out dash-oracle-sys/tests/data/testnet.json
```

## Regtest and devnet samples (`regtest-<pin>.json`, `devnet-spacing-<pin>.json`)

DEC-82 dropped the v23.1 pin: the `-v23.1.json` files of this and the following sections are
deleted, and the v23.1.8 provenance below is kept as history of how the surviving files were
chosen. Only the `v24.0` files remain.

Captured on 2026-10-08 by `dash-oracle-sys/scripts/gen-chain-samples.py` from the
official release binaries of each pin, each mining its own chain on a fresh datadir:
v23.1.8 (`dashcore-23.1.8-x86_64-linux-gnu.tar.gz` from the GitHub release, sha256
`e664cad0fe860afa3d005be0db1eb06da598f2f83421083afae77191e3115825`, as listed in the
release's `SHA256SUMS.asc`) and v24.0.0-rc.3 (`/work/chains/bin/dashcore-24.0.0-rc.3`,
the BS2 oracles' binary). The `provenance` object of each file records the dashd
version and the chain arguments; the tests select the same chain on the oracle.

| Field | Source |
|---|---|
| `header_ranges[].hex` | `getblockheader <hash> false`; outside mainnet Core requires every header's nBits to equal `GetNextWorkRequired()` |
| `templates[]` (devnet) | `getblocktemplate` `height`, `curtime`, `bits`: Core's `GetNextWorkRequired()` for the next block at that time |
| `subsidy[]` | `getblockstats <h>` `subsidy`, `getblockheader` of `h-1` `bits` |
| `superblocks[]` | `getsuperblockbudget <h>` (`CSuperblock::GetPaymentsLimit()`) |
| `deployments` | `getdeploymentinfo` (v24) or `getblockchaininfo` `softforks` (v23) heights |

`regtest` uses default regtest parameters (600 blocks; V20 and MN_RR are active from 432).
`devnet-spacing` uses `-devnet=o1spacing -powtargetspacing=86401
-minimumdifficultyblocks=4001 -highsubsidyblocks=500 -highsubsidyfactor=10`. A pow target
spacing above `nPowTargetTimespan` makes `DifficultyAdjustmentInterval()` zero, and
Core's BTC-era code divides by it: the same release started without
`-minimumdifficultyblocks` aborts with SIGFPE ("Floating point exception") when asked to
mine block 1, which is why the shim refuses those heights and nothing else. After block
4001 each of 12 segments mines one block more than 2 hours after the previous one
(min-difficulty) and records the template 1 s to 2 h later (DGW). Those DGW blocks
themselves cannot be mined: DGW's average of powLimit-sized targets wraps around
`arith_uint256`. The high-subsidy window is needed because the devnet genesis block
(height 1) pays 50 DASH.

## P2SH spends (`regtest-p2sh-<pin>.json`)

`gen-chain-samples.py --scenario regtest-p2sh`, with the same two release binaries and a
legacy wallet on default regtest: three transactions spending 2-of-3, 1-of-2 (plus a
P2PKH output) and 3-of-4 P2SH multisig outputs made with `addmultisigaddress`, signed by
`signrawtransactionwithwallet` and accepted into the mempool. `sigops` is
`getblocktemplate`'s per-transaction count, `GetTransactionSigOpCount(tx, view,
STANDARD_SCRIPT_VERIFY_FLAGS)` as the mempool computed it; `spent` is
`getrawtransaction` of each input's previous output.

## Version-2 asset unlocks (`v24-asset-unlocks.json`)

The four asset-unlock transactions (type 9) of `dash/tests/data/core_v24_regtest.json` on
rust-dashcore branch `node/v24a-dashcore-wire` (commit in the file's `provenance`),
which dashd v24.0.0-rc.3 produced running Core's `feature_asset_locks.py` functional test.
`txid` and `instance_hash` are dashd's `getblock 2` `txid` and `instanceHash`. Two have
payload version 2, whose v24 txid omits the quorum signature.

## dashd difficulty (`difficulty.json`)

`dash-oracle-sys/scripts/gen-difficulty-samples.py`: for the first header with each
distinct nBits in `mainnet.json`/`testnet.json`, `getblockheader`'s `difficulty` from the
BS2 reference nodes, kept as printed (`GetDifficulty()`, rpc/blockchain.cpp's own copy of
the `ConvertBitsToDouble()` formula, printed with 16 significant digits).

## Core unit-test records (`core-tests/`)

`script_tests.json` (only `script_FindAndDelete`; not Core's data file of that name),
`sigopcount_tests.json` and `scriptnum_tests.json` are what Dash Core's `script_tests.cpp`,
`sigopcount_tests.cpp` and `scriptnum_tests.cpp` compute at each of their checks. They were recorded with O4's
recorder (`src/test/util/o4_dump.h`, see `test-vectors/core/0364ed626ec2/PROVENANCE.md`)
by commit `2babb5575d` on Dash Core branch `node/o1b-core-vectors` (on top of `node/o4-core-vectors`,
Core commit `0364ed626ec2`, the v24.0.0-rc.3 pin's parent), which only adds recording calls
next to the existing checks: `O4_VECTOR_OUT=<dir> test_dash
--run_test=script_tests/script_FindAndDelete:sigopcount_tests:scriptnum_tests` (the binary
built from that commit with depends; every check passed).
`evo_sharedmn_tests.json` (the two `SplitAmountByShares()` calls of its
`split_amount_by_shares` case) comes from commit `ac7e45359f` on the same branch, run
the same way with `--run_test=evo_sharedmn_tests/split_amount_by_shares`.
`scriptnum_tests` records the reference encoding (`CScriptNum10`) of each value the test
creates; `sigopcount_tests` uses freshly generated keys, so its multisig scripts differ
between runs while the counts do not.

`asmap.raw` is Dash Core's `src/test/data/asmap.raw` at the v24.0 pin (the file
`addrman_tests.cpp` embeds as `asmap_raw`; sha256
`55dcec00c72b8a33a271dad20271e14cfbe5526aa1dc12e2559f30c376d85a35`). `tests/addrman.rs`
checks the oracle's bucket and netgroup entry points against the known answers Core's
`addrman_tests.cpp` (`caddrinfo_get_{tried,new}_bucket{,_legacy}`) and `netbase_tests.cpp`
(`netbase_getgroup`) assert at that pin, and replays `addrman_simple` and
`addrman_evictionworks`; each value cites its line.

Other Core-produced expectations come from dash-core-vectors directly
(`transcribed/{block_reward_reallocation,subsidy,pow,dip0020opcodes,checkdatasig}_tests.json`,
`src/test/data/{script_tests,tx_valid,tx_invalid}.json`).

## Contextual samples (`contextual-mainnet.json`, O2a)

Captured on 2026-10-08 by `dash-oracle-sys/scripts/gen-contextual-samples.py` from
the mainnet reference oracle (dashd v24.0.0-rc.3, still in IBD at height ~1.30M),
read-only RPC, one request at a time. Every sample is chain data Core accepted, so the
expected verdicts are "accept"; the expected values are the RPC's:

| Field | Source |
|---|---|
| `dip3_block` | the first block after DIP0003 activation (1,028,160) holding a ProRegTx with an external collateral: 1,028,162. `block`, `header`, `headers` (heights `first_height` .. 1,028,161): `getblock 0`, `getblockheader false`. `prev_list`: `protx listdiff 1 1028161` re-encoded as evodb stores a list (empty here). `expected_listdiff`: `protx listdiff 1028161 1028162`. `coins`: `getrawtransaction <collateral> 1` (value, script, height). `normal_tx`: a non-coinbase transaction of the block with its spent outputs from `getblock 3` (`prevout`) and `fee` |
| `commitment` | the LLMQ_50_60 commitment mined at 1,298,963 (49 of 50 members valid): the commitment from the QcTx's `extraPayload`, the headers from its quorum base block (1,298,952), and the quorum's 50 members, computed from `protx listdiff 1 1298952` as `dml-listdiff-check.py` does (CalculateQuorum before V20; the commitment's BLS members signature verifies only over the right operator keys) |
| `proupserv` | the first ProUpServTx after DIP0003 activation (1,028,257, `8e5bdc5f…`): `tx` (`getblock 2`), `headers` (heights 1,028,227 .. 1,028,256), `prev_list` (`protx listdiff 1 1028256` re-encoded), its payload, the operator key from that list, its BLS signature (the payload's last 96 bytes) and the signed hash, SHA256d of the payload without the signature (`SerializeHash` with `SER_GETHASH`, which skips the signature) |

The `proupserv` context fields were added in fix round 1 (the same day); the regenerated
file is identical to the first capture in every other field.

Derived inputs in `tests/contextual.rs`, built from these samples by the oracle itself:
the list after 1,028,162 is `oracle_dml_apply`'s output for the sampled block (Core's
`RebuildListFromBlock`), and the duplicate-registration case resubmits one of that
block's ProRegTxs on it. The list-state guard cases patch one field of that list
(registered, last paid, revived and ban heights, consecutive payments, PoSe penalty) and
apply a coinbase-only block on top; they are adversarial inputs the adapter must refuse
or pass without overflow, not chain data. The checkpoint cases use mainnet's checkpoints
from `chainparams.cpp` at both pins (1,067,570 is the first above the sampled block,
2,487,500 the last) and Core's reject string for them, `bad-fork-prior-to-checkpoint`;
one case replaces the sampled commitment's quorum hash with the checkpoint at 1,850,400
(an LLMQ_50_60 base height) to check that a base known only as a checkpoint above the
tip is missing context.

## Contextual regtest verdicts (`contextual-regtest-<pin>.json`, O2a)

Captured on 2026-10-09 by `dash-oracle-sys/scripts/gen-contextual-regtest.py` from the
same two release binaries as the regtest samples above (v23.1.8's `dashd` from the
GitHub tarball; v24.0.0-rc.3's copy of `/work/chains/bin/dashcore-24.0.0-rc.3`, dashd
sha256 `e204e316…5bb4`), each on a fresh regtest datadir with default parameters, a
mock clock, and RPC only. At a tip before DIP0003 (300) and one after (440) (`tips`)
each file records:

| Field | Source |
|---|---|
| `headers`, `adjusted_time` | `getblockheader false` for 0 .. tip; the node's mock time when it judged the proposals |
| `deployments` | the version-bits state for the block after the tip: `getdeploymentinfo` `status_next` (v24), `getblockchaininfo` `softforks` `status` (v23.1.8, computed from the tip as well) |
| `mined_block` | a block `generatetoaddress` mined on the tip (so Core accepted it), then taken back with `invalidateblock` |
| `blocks` | mutations of that block, each submitted as a `getblocktemplate` `proposal`; `core_reason` is Core's answer (TestBlockValidity: ContextualCheckBlockHeader, CheckBlock, ContextualCheckBlock, then ConnectBlock, whose first step for these cases is CheckSpecialTx) |
| `sequence_locks` | version-2 spends of a mature coinbase coin with relative height and time locks on both sides of the boundary, signed by the node's wallet; `satisfied` is `testmempoolaccept`'s verdict (allowed, or `non-BIP68-final`) for the block after the tip |

The mutations only aim at both outcomes; every expected value is the release's answer.

The rejection cases of `tests/contextual.rs` are Core's own `src/test/data/trivially_invalid.json`
(dash-core-vectors, v24.0.0-rc.3), run through `CheckSpecialTx()` on a synthetic regtest
chain as `evo_trivialvalidation.cpp` runs them (V19 at 100, the tip at 98 or 100;
`-dip3params=2:2` because `CheckSpecialTx()` gates on DIP0003 first). v23.1.8's copy of
the file differs in one row only: it labels the payload failure of transaction
`95cb5e1a…` `gettxpayload-fail`, its test helper's marker for `GetTxPayload()`
failing, where v24's copy records the reason `CheckProUpRegTx()` returns at both pins,
`bad-protx-payload`.

To regenerate:

```bash
python3 -I dash-oracle-sys/scripts/gen-contextual-samples.py > dash-oracle-sys/tests/data/contextual-mainnet.json
```

## BS8 overlay verdicts (`bs8-core-overlay.json5`, O2c)

A verbatim copy of base-sdk's `pkgs/p2p_core/corpus/core_overlay.json5` at fork revision
`dbe245f7e95a88b0de37ee857f9f02e0de33d275` (`node/base-sdk`, BS8's merge, which the root
pins): 445 overlay payloads with Dash Core's reads of them, recorded by BS8's `bs8-judge`
(`contrib/node/dde-probe/bs8-judge`; judge.cpp 48597ba511a78653, binary 73a3b6c17199ebec), a
separate binary linked against a clean build of the v24.0 pin a9a483e51efd. Its header comment
describes the fields; `tests/p2p.rs` does not compare the `hash` field (the oracle does not
report hashes).
`tests/p2p.rs` checks that `oracle_p2p_decode` returns the judge's verdict, bytes read,
write-back and qdata fields for every case, so the in-process entry point stands for the
judge. To refresh it, copy the file again from the fork revision the root pins.

## Provider signatures (`special-tx-signatures.json`, O2a-EV)

The provider transactions (types 1, 2, 3, 4, 10, 11 and 12) of base-sdk's BS6b corpus,
`pkgs/primitives/corpus/sighash.json5` at `node/bsdk-bs6b-sighash` 84886b9, unchanged.
That corpus is Core-produced and its header states how: dashd v24.0.0-rc.3 (release
tarball) on replicas of the regtest chains Core's functional tests built
(feature_masternode_payout_shares, feature_masternode_shares, feature_protx_version,
rpc_netinfo) and on a mainnet node (blocks 1,028,160 to 2,094,670, sampled). It has `getblock 2`
for the transactions and `protx info` / `protx listdiff` at the block before for the
signers' keys. Nothing in it is hand-written.

| Field | Source |
|---|---|
| `txid`, `source`, `network`, `raw` | the corpus entry (its key, `source`, `network`, `raw`) |
| `type` | the transaction's `nType` (bytes 2..4 of `raw`) |
| `signer` | the corpus entry's `signer`: `key_ids` (the ECDSA signers' key ids; every share owner for a shared masternode), `shares` (a shared registration, signed by its share owners), or `operator` with `operator_legacy` (the operator key and the masternode's key encoding) |

`tests/mnlist.rs` checks every signature over `oracle_special_tx_sign_hash`'s hashes.
The test does not re-encode anything Core signed: it recovers ECDSA keys from the
signatures and verifies BLS signatures under the scheme Core applies.

To regenerate (the corpus has JSON5 trailing commas and `//` comments only):

```bash
git -C <base-sdk> show 84886b9:pkgs/primitives/corpus/sighash.json5 | python3 -I -c '
import json, re, sys
text = re.sub(r",(\s*[}\]])", r"\1", re.sub(r"(?m)^\s*//.*$", "", sys.stdin.read()))
corpus = json.loads(text)
records = [
    {"txid": txid, "source": e["source"], "network": e["network"],
     "type": int.from_bytes(bytes.fromhex(e["raw"])[2:4], "little"), "raw": e["raw"],
     "signer": e["signer"]}
    for txid, e in corpus["sighash"].items()
    if int.from_bytes(bytes.fromhex(e["raw"])[2:4], "little") in (1, 2, 3, 4, 10, 11, 12)]
print(json.dumps({"source": "base-sdk node/bsdk-bs6b-sighash 84886b9, "
                  "pkgs/primitives/corpus/sighash.json5 (provider transactions only)",
                  "records": records}, indent=1))' > dash-oracle-sys/tests/data/special-tx-signatures.json
```

## Crafted masternode-list blocks (`dml-crafted-v24.0.json`, O2a-EV)

These are Core's verdicts on one crafted regtest block per masternode-list operator of EV2 §6.5,
for `tests/dml_crafted.rs`. `scripts/gen-dml-crafted.py` produced them with the release
dashd v24.0.0-rc.3 (`dashd` SHA-256 e204e316…ab5bb4, the binary of
`/work/chains/bin/dashcore-24.0.0-rc.3`). It ran two fresh regtest chains with default
parameters, the second with V24 always active (`-vbparams=v24:-1:999999999999`). The
provider transactions are Core's own (`protx register_fund`, `register_fund_evo`,
`register_prepare`, `update_service`, `update_service_evo`, `revoke` with `submit=false`),
signed by its wallet. Each block is the one Core mined on the tip, taken back with
`invalidateblock`, with the case's transactions added after its coinbase.

| Field | Source |
|---|---|
| `chains` | `getblockheader false`, 0 .. the chain's last tip |
| `cases[].context` | at the tip: `tip` (height), `list` (`protx listdiff 1 <tip>` encoded as evodb stores it, by dml-listdiff-check.py), `coins` (the block's ProRegTx collaterals that exist at the tip, from `getrawtransaction`), `deployments` (`getdeploymentinfo` V24 `status_next`), `adjusted_time` (the mock time) |
| `cases[].core_reason` | the `getblocktemplate` `proposal` answer for `block` |
| `cases[].stage` | where in ProcessSpecialTxsInBlock Core gives that reason: the CheckSpecialTxInner loop (`check`), BuildNewListFromBlock (`build`), or null for a step oracle_dml_block leaves out. That is the quorum block processor (`bad-qc-block`) or the CbTx merkle roots: the coinbase was made for the block without the case's transactions, so a list update Core accepts ends in `bad-cbtx-mnmerkleroot`. Read from the pin's specialtxman.cpp; the test checks the oracle reports it |

Some cases are mutations of what Core built. They are listed with what each does to Core's
verdict:
- `collateral_same_block`: `register_prepare` reads only confirmed collaterals. The ProRegTx
  was prepared for a confirmed coin, then pointed at the funding output and signed again by
  that output's key over the new sign string.
- `proupserv_bad_ntype`: the nType of an evonode's ProUpServTx was set to 2. The payload
  decode refuses it before any signature check.
- The `qc_*` cases: the quorumHash (or nHeight) of a null commitment in Core's block was
  replaced. Null commitments carry no signature.

ConnectBlock runs ProcessSpecialTxsInBlock before it checks scripts, so the input signatures
these mutations break are never reached. The addresses and keys are fresh in every run, so
a regenerated file differs byte for byte; the verdicts do not. EV2 §6.5's
"negative computed height" has no case of its own. Core compares a QcTx's nHeight with the
block's before any lookup (`qc_bad_height`), and only a rotated commitment's quorumIndex
could make the computed height negative.

To regenerate:

```bash
python3 -I dash-oracle-sys/scripts/gen-dml-crafted.py --dashd <dashcore-24.0.0-rc.3>/bin/dashd \
    --pin v24.0 --datadir <scratch>/dd --port 29400 > dash-oracle-sys/tests/data/dml-crafted-v24.0.json
```

## LLMQ regtest fixture (`llmq-regtest-<pin>.fixture.gz`, `llmq-regtest-<pin>.json`, O2a-LQ1)

The chain is the one Dash Core's own functional test `feature_llmq_rotation` builds, run with
`--nocleanup` against the pin's release binary (v24.0.0-rc.3) by
`dash/contrib/core-vectors/run-functional-tests.sh` (branch node/v24a-dashcore-wire). Its nine
nodes use `-llmqtestparams=4:4`, `-dip3params=2:2` and `-testactivationheight=v20@300`. They mine
LLMQ_TEST quorums before V20, at 144 and 168, where members come from the base block's list with
the block-hash modifier. They mine LLMQ_TEST quorums after V20, at 384, 408 and 432, where
members come from the work block's list with the CbTx chainlock modifier. They also mine rotated
LLMQ_TEST_DIP0024 cycles at 384, 408 and 432, each with quorum indexes 0 and 1.
`scripts/gen-llmq-regtest.py` restarts node0 alone on a copy, with the chain arguments debug.log
recorded, and reads everything from that private node.

| File | Content |
|---|---|
| `.fixture.gz` | an `llmq-fixture v1`, gzipped. It holds the chain arguments, headers 0 .. 452, the list after every block from DIP0003 (`protx listdiff 1 h`, re-encoded by dml-listdiff-check.py's `encode_list`), every block's coinbase, and every non-null commitment (getblock 2) with its mined height. It holds no snapshot rows: the context builds them as the node did |
| `.json` | the provenance and the fixture's sha256. `quorums`: `quorum info` members of every mined quorum, in order. `rotation_snapshots`: the CQuorumSnapshot rows `quorum rotationinfo` reports at H - C, H - 2C and H - 3C, requested at each block that mined a rotated cycle. Each row is keyed by the cycle's base block, which is the work block of its `mnListDiffAtHMinus*C` plus 8. `scans`: `quorum list <count>` at the tip for counts 1, 2 and 4 |

`tests/llmq.rs` loads the fixture and connects every block with `precompute`, as a node does. It
then compares the rows the context stored, every quorum's members, and the scans with these
answers. The internal ids and `nTotalRegisteredCount` of the re-encoded lists are not Core's,
but no answer depends on them.

To regenerate:

```bash
contrib/core-vectors/run-functional-tests.sh <dash> v24.0.0-rc.3 <dashcore-24.0.0-rc.3>/bin <work> feature_llmq_rotation
python3 -I dash-oracle-sys/scripts/gen-llmq-regtest.py --dashd <dashcore-24.0.0-rc.3>/bin/dashd --pin v24.0 \
    --node0 <work>/ft/feature_llmq_rotation/node0 --datadir <scratch>/dd --port 29881 \
    --out-dir dash-oracle-sys/tests/data
```

## Lock captures and their chain (`lq3-regtest-<pin>.captures.json`, `lq3-regtest-<pin>.fixture.gz`, `lq3-regtest-<pin>.json`, O2a-LQ3)

One run of `contrib/node/lq3a-captures` (its README describes the capture: five nodes, four
masternodes, three cycles of `llmq_test` and `llmq_test_instantsend` quorums of 4, threshold 3, at
base heights 144, 168 and 192, tip 214) against the pin's release binary, kept with `--nocleanup`.
`.captures.json` is that run's output, byte for byte the same file as
`dash-evo/tests/data/lq3a-regtest-captures.json`: 13 `qsigrec`, 3 `isdlock` and 3 `clsig` payloads
with the inventory hash Core announced for each. One `qsigrec` is signed by the third-newest
`llmq_test` quorum, active only through `keepOldConnections` (`provenance.keep_old_connections_recsig`).
`scripts/gen-llmq-regtest.py --stem lq3-regtest` then restarts the run's node0 alone on a copy and
writes the same chain's `.fixture.gz` (headers 0 .. 214, every list from DIP0003, every coinbase,
the six non-null commitments) and `.json` (as for the LLMQ regtest fixture above). `node-l1/tests/l1_llmq.rs` loads both into Core's context and LQ3a's
view and compares the verdicts on every captured message and its changes.

To regenerate (the three files together):

```bash
contrib/node/lq3a-captures/run.sh <dash> v24.0.0-rc.3 <dashcore-24.0.0-rc.3>/bin <work> \
    dash-oracle-sys/tests/data/lq3-regtest-v24.0.captures.json
python3 -I dash-oracle-sys/scripts/gen-llmq-regtest.py --dashd <dashcore-24.0.0-rc.3>/bin/dashd --pin v24.0 \
    --node0 <work>/ft/run/node0 --datadir <scratch>/dd --port 29891 --out-dir dash-oracle-sys/tests/data \
    --stem lq3-regtest --source "contrib/node/lq3a-captures (--nocleanup), node0 restarted alone"
```

## Rotated and legacy lock captures (`lq3r-regtest-<pin>.captures.json`, `.fixture.gz`, `.json`, BL-218)

One run of `contrib/node/lq3a-captures` with `LQ3A_SCRIPT=capture_lq3_rotation.py` (its docstring
describes the chain): nine nodes, eight masternodes, V19 deferred to height 2000 (with V20 and MN_RR),
so every key, quorum and signature is legacy-scheme (V19's height is in `provenance.extra_args`; the framework writes the V20 and MN_RR heights to each node's `dash.conf`, not to `extra_args`, and `provenance.v19_deployment` records V19 still inactive at the tip). InstantSend uses the rotated `llmq_test_dip0024`.
Two `llmq_test` quorums (bases 144 and 168, each with an `llmq_test_platform` quorum at the same base), then rotation cycles at F = 264 and C = 288, each with
both quorum indexes and an `llmq_test` and `llmq_test_platform` quorum at the same base; tip 311 =
C + 23. `.captures.json` holds 29 `qsigrec` (26 `llmq_test`, 3 `llmq_test_dip0024`), 3 `isdlock`
and 5 `clsig` payloads with their inventory hashes. `provenance.locks` records each lock's signing
tip and the cycle Core reports for it (`getislocks`), which is the signing quorum's cycle: A at 284
and B at 288 (C's quorums were not mined yet) carry F, D at 308 carries C. `.fixture.gz` (headers 0 .. 311, the lists, the coinbases,
twelve non-null commitments, four of them rotated) and `.json` (with the four rotation snapshot rows)
come from the run's node0 as above.

To regenerate (the three files together):

```bash
LQ3A_SCRIPT=capture_lq3_rotation.py contrib/node/lq3a-captures/run.sh <dash> v24.0.0-rc.3 \
    <dashcore-24.0.0-rc.3>/bin <work> dash-oracle-sys/tests/data/lq3r-regtest-v24.0.captures.json
python3 -I dash-oracle-sys/scripts/gen-llmq-regtest.py --dashd <dashcore-24.0.0-rc.3>/bin/dashd --pin v24.0 \
    --node0 <work>/ft/run/node0 --datadir <scratch>/dd --port 29893 --out-dir dash-oracle-sys/tests/data \
    --stem lq3r-regtest \
    --source "contrib/node/lq3a-captures capture_lq3_rotation.py (--nocleanup), node0 restarted alone"
```

## LLMQ mainnet and testnet samples (`llmq-samples.json.gz`, O2a-LQ1)

`scripts/gen-llmq-samples.py` reads the shared reference dashds (v24.0.0-rc.3) with pure reads
only, one request at a time: getblockhash, getblockheader, getblock 2 and `protx listdiff`. No
`quorum` RPC, `protx diff` or rotationinfo is used, since those can write rows on these nodes.
Mainnet heights stay at or below 2,090,000.

A sample is a group of non-rotated quorums whose members come from one list: the base block's
list before V20, or the work block's list (base - 8) once V20 is active after the base. Each
sample holds:
- the headers from that list block to the last mined commitment;
- the list (`protx listdiff 1 h`, re-encoded by dml-listdiff-check.py's `encode_list`);
- the block's coinbase transaction;
- each quorum's non-null commitment with its mined height.

The file copies nothing the RPC says about members.

| Network | Base | Types | What it covers |
|---|---|---|---|
| mainnet | 1,737,792 (DIP0024) | 50_60, 400_60, 100_67 | legacy BLS; the base block's list; the block-hash modifier; every masternode for 100_67 |
| mainnet | 1,899,072 (V19) | 400_60 | basic BLS (50_60 and 100_67 mined null commitments there) |
| mainnet | 1,987,776 (V20) | 400_60, 400_85, 100_67 | the work block's list, which is still before V20, with the block-hash modifier; evonode-only 100_67 |
| mainnet | 1,987,800 | 100_67 | the first work block after V20: the CbTx chainlock modifier |
| testnet | 1,100,016 .. 1,100,232 (10 cycles, 216 blocks) | 50_60, 25_67 | every non-null non-rotated commitment of these types in the window (400_60 and 400_85 mined none) |

Mainnet 50_60 is gated off once DIP0024 quorums exist, so it mines only null commitments
after 1,738,698. `tests/llmq.rs` computes each quorum's members in an LLMQ context and checks
the commitment with `oracle_verify_commitment` with signatures, which must accept. It then
checks the commitment again with a signer swapped for a non-signer and with a signer replaced
by a masternode outside the quorum, and each of those must be rejected.

To regenerate:

```bash
python3 -I dash-oracle-sys/scripts/gen-llmq-samples.py --out dash-oracle-sys/tests/data/llmq-samples.json.gz
```

## Block payments and credit-pool steps (`payments-v24.0.json.gz`)

`dash-oracle-sys/scripts/gen-payments-fixture.py` wrote this file from the reference dashd v24.0.0
nodes' RPC. It used pure reads only, one at a time, at mainnet heights ≤ 2,090,000: `getblock`,
`getblockheader`, `getblockstats totalfee`, `protx listdiff 1 <h - 1>` and `getcreditpoolinfo`.
The file's `provenance` names the generator, the rust-dashcore commit it ran at and the node versions.

`payments` holds the samples `payments-check.py` builds (see the `payments_check` example):

| Name | Network | Height | Why |
|---|---|---|---|
| pre-budget | mainnet | 100,000 | below nBudgetPaymentsStartBlock |
| budget-window | mainnet | 332,320 | offset 0 of a budget cycle; the mined block paid a budget |
| budget-outside | mainnet | 332,420 | offset 100: past the window |
| superblock | mainnet | 2,077,000 | a superblock with 13 payments |
| regular | mainnet | 2,077,001 | a regular block |
| banned | mainnet | 2,077,001 | `regular` with every masternode PoSe-banned |
| testnet-superblock | testnet | 1,500,000 | a superblock height in the MN_RR era |
| platform | testnet | 1,500,001 | the MN_RR (EvoReward) era: the platform share |
| strict | testnet | 1,500,001 | `platform` with each paid masternode's operator reward set to 50%, paid to its own payout script |
| superblock-start-prev | mainnet | 614,819 | the last block below nSuperblockStartBlock (old-budget rules) |
| superblock-start | mainnet | 614,820 | nSuperblockStartBlock: superblock rules, not a superblock height |
| first-superblock | mainnet | 631,408 | the first superblock height (38 × 16,616), before DIP3: no list, so no trigger |
| dip3-enforcement-prev | mainnet | 1,047,199 | the last block without the masternode payee check |
| dip3-enforcement | mainnet | 1,047,200 | DIP0003EnforcementHeight: the first payee-checked block (Classic era) |
| classic-realloc | mainnet | 1,700,000 | the Classic era inside the reallocation schedule |
| pre-v20-superblock | mainnet | 1,977,304 | the last superblock before V20 (119 × 16,616): GetPaymentsLimit without V20 |
| v20-prev | mainnet | 1,987,775 | the last Classic-era block |
| v20 | mainnet | 1,987,776 | V20's activation: the first CreditPool-era block |
| testnet-mn-rr-prev | testnet | 1,066,899 | the last CreditPool-era block |
| testnet-mn-rr | testnet | 1,066,900 | MN_RR's activation: the first EvoReward-era block (the platform share) |

The samples from `superblock-start-prev` on were added for the EV3-1 independent review (N1);
the nine above them are unchanged. Mainnet's MN_RR (2,128,896) is above the 2,090,000 cap on
mainnet reads (DEC-276), so mainnet's EvoReward era has no sample.

Each sample's masternode list is the parent's (`protx listdiff`, encoded as evodb stores it,
as `dml-listdiff-check.py` does). It is trimmed to the masternodes the coinbase pays plus the
first 12 valid ones by registration. `GetMNPayee()` picks the minimum of the list under its
order, so a subset that holds the real payee picks the same one (O2a-GV-notes D13). Superblock
samples carry those 12 as `voters` and the coinbase's output addresses.

`credit` holds `oracle_credit_pool_step` inputs, each with the block, the headers of the 576
heights below it, the window-start block and `getcreditpoolinfo`'s pool after the parent and
after the block:

| Name | Network | Height | Why |
|---|---|---|---|
| v20-activation | mainnet | 1,987,776 | V20's activation: the first checked step, from the empty pool |
| unlock | testnet | 1,153,152 | the generator's search from WITHDRAWALS (1,148,500): the first block with an asset lock and an unlock, else the first with an unlock |

To regenerate:

```bash
python3 -I dash-oracle-sys/scripts/gen-payments-fixture.py
```

## MNHF signals regtest chain (`mnhf-regtest-v24.0.json.gz`, O2a-MNHF)

The chain is the one Dash Core's own functional test `feature_mnehf.py` (git tag `v24.0.0-rc.3`,
sha256 `58b4a1b1…256ef`) builds, run with `--nocleanup --timeout-factor=8 --portseed=731` against the
release `dashd` v24.0.0-rc.3 (sha256 `e204e316…ab5bb4`), through a read-only wrapper
`feature_mnehf_capture.py` placed next to the test (its source is in the fixture's
`provenance.capture_wrapper`). The test runs two nodes and one masternode with `-llmqtestparams=1:1`,
`-dip3params=2:2`, `-testactivationheight=v20@100` and `mn_rr@100`. It mines an LLMQ_TEST quorum, then
two EHF signals for `testdummy` (bit 28) and an unknown bit (27), invalidates the signal block and
reconsiders it, sends the same bit-28 signal again, and finally restarts with a newer `testdummy`
start time so that the old signal is out of its window and a new one is mined. The test passed.

The wrapper subclasses the test class and only reads: after every successful `send_tx()` and at the
end of `run_test()` it records node0's mempool (`getrawmempool`, `getrawtransaction` hex and decoded
`mnhfTx`). The test sends the duplicate same-bit signal at height 198 and restarts with
`-persistmempool=0`, so the duplicate exists only in the snapshot taken right after it was sent. The
subclass has to define `set_test_params` (delegating to the test's) because the framework's
metaclass demands it.

`scripts/gen-mnhf-regtest.py` restarts node0 alone on a copy of its datadir. It starts with the
chain arguments from the last start of node0 in its debug.log (`Command-line arg` and
`Config file arg` lines; the last start is the one with the newer `-vbparams`, the logged
`-mocktime`s are not passed), `-connect=0`, `-listen=0`, its own RPC port, `-rpcdoccheck=0`,
`-checkblocks=0` and `-persistmempool=0`. The copy's `dash.conf` loses its `bind`, `port`,
`rpcport` and `rpcdoccheck` lines (`-bind` cannot be combined with `-listen=0`). It then reads everything
over RPC; an argument it does not know is an error. The mock time is set to the tip's time + 150 s.

| Field | Source |
|---|---|
| `chain`, `chain_args` | `regtest` and the consensus arguments of that last start: `-dip3params=2:2`, `-testactivationheight=v20@100`, `-testactivationheight=mn_rr@100`, `-llmqtestparams=1:1`, `-llmqtestinstantsendparams=1:1`, `-llmqtestplatformparams=1:1`, `-vbparams=testdummy:1418923591:1419923591:0:4:4:4:5:1`. The sporks are in the datadir, not an argument (they do not change block validity here) |
| `headers` | `getblockheader <hash> false`, heights 0 .. 256 (tip `262af307…debe86`) |
| `blocks[]` | per height 1 .. 256: `getblock <hash> 0`; `mnhf` from `getblock <hash> 2` (type-7 transactions: `txid`, `index_in_block`, `versionBit`, `quorumHash`, `sig`) |
| `deployments[h]` | `getdeploymentinfo <hash of h>` reduced to the BIP9 entries (`evo_shares`, `testdummy`, `v24`) as `{bit, ehf, ehf_height, status, status_next}`; the names are Core's. `bit` is absent (null) while a deployment is `defined`; `ehf_height` is absent (null) until a signal is in force |
| `invalidated[]` | every block that is not on the active chain, from `getchaintips` and `getblockheader` (`confirmations` -1): `hash`, `height`, `previousblockhash`, `branch_tip`, `branch_tip_status`, `branchlen`, `forkpoint`, `hex`, `mnhf` |
| `mempool_at_capture` | the wrapper's snapshots, verbatim (`label`, `height`, `tip`, `txs[{txid, hex, decoded_mnhfTx}]`) |
| `proposals[]` | `getblocktemplate` `{"mode":"proposal"}` on the restarted node (see below) |

MNHF transactions on the chain (all signed by the one quorum `5a9f3c15…3fa50`):

| Height | Block | Transaction | versionBit |
|---|---|---|---|
| 187 | `19fd5b0c…864c` (active) | `ffc2e010…c24f` (index 3), `28c4305d…e3bb` (index 4) | 28 (`testdummy`), 27 (unknown) |
| 191 | `07dcad2b…8871` (side branch) | `ffc2e010…c24f` (index 1) | 28 |
| 247 | `3c2a42d4…716d` (active) | `ffc2e010…c24f` (index 1), `5585483e…3fc9` (index 2) | 28, 13 (`v24`'s own signal) |

The same transaction `ffc2e010…` is mined three times, because a quorum member signs a request id
once and the test's second and third signals are byte-identical to the first. The "duplicate" in the
mempool at height 198 is that same transaction too. Heights 187 .. 192 of the first window are
`defined` under the final `-vbparams`, which start the window after block 246: `testdummy` never
starts there, and `ehf_height` is null for them. `testdummy`'s `ehf_height` over the heights: null
for 0 .. 246; 247 from 247 (`defined`, `status_next` `started`) through 256 (`started` 248 .. 251,
`locked_in` 252 .. 255, `active` 256). `v24` has `ehf_height` 247 from 247, `bit` 13 from 250, and
stays `started` (its signalling needs 200 of 250 blocks). `evo_shares` stays `defined` and never
has a signal. The side branch is heights 187 .. 192, `valid-fork`: the chain `reconsiderblock` took
back to the original, whose block 187 has both signals.

`proposals` build blocks from the chain's own blocks, and each records `base_height`/`base_hash`
(the tip the proposal is judged on), `source_block_*`, the transactions `added` (with txid and hex),
`block_hex`, Core's answer `core_result` (null = accepted) and `header_nonce_flipped`. Appended
transactions go after the block's own, with the merkle root recomputed (proof of work is not
checked). A block taken as mined has its nonce XORed with `0x5a5a5a5a` (a new hash), because Core
answers `duplicate-invalid` for the invalidated original. The base is reached with
`invalidateblock` and the tip restored with `reconsiderblock` (checked against the tip hash).
Bases below the side branch (186) are judged on a second node started from a fresh copy of the
datadir instead. No second *valid* bit-28 signal exists (a quorum member
signs a request once), so the two-signals-of-one-bit cases below use crafted transactions: the
mined signal's bytes with an all-zero signature, and the same with the other mined quorum's hash.

| Name | Base | Block | Core's answer |
|---|---|---|---|
| `control_tip_block` | 255 | block 256 | accepted |
| `tip_block_plus_duplicate_same_bit` | 255 | block 256 + the mempool duplicate of bit 28 (signal in force since 247) | `bad-mnhf-duplicate` |
| `control_signal_block_187` | 186 | block 187 (bits 28 and 27) | accepted |
| `control_signal_block_247` | 246 | block 247 (bits 28 and 13) | accepted |
| `signal_block_247_plus_same_tx_again` | 246 | block 247 + its own bit 28 signal again | `bad-mnhf-duplicates-in-block` |
| `signal_block_247_plus_bit28_zero_signature` | 246 | block 247 + a crafted bit 28 signal, same quorum, zero signature | `bad-mnhf-invalid` |
| `signal_block_247_plus_bit28_other_quorum_zero_signature` | 246 | block 247 + a crafted bit 28 signal, the other quorum, zero signature | `bad-mnhf-invalid` |

To regenerate (the test needs a venv with `dash_hash`; `config.ini` is hand-written as in
`conformance/scripts/regtest-v24.sh` of rust-dashcore, commit 8d382926a):

```bash
git -C <dash> archive v24.0.0-rc.3 test/ | tar -x -C <work>/core   # + test/config.ini, feature_mnehf_capture.py
(cd <work>/core/test/functional && MNHF_CAPTURE_OUT=<work>/mempool-capture.json \
    DASHD=<bin>/dashd DASHCLI=<bin>/dash-cli DASHUTIL=<bin>/dash-util DASHWALLET=<bin>/dash-wallet \
    <work>/venv/bin/python feature_mnehf_capture.py --configfile=<work>/core/test/config.ini \
    --nocleanup --tmpdir=<work>/ft/feature_mnehf --timeout-factor=8 --portseed=731)
python3 -I dash-oracle-sys/scripts/gen-mnhf-regtest.py --dashd <bin>/dashd --pin v24.0 \
    --node0 <work>/ft/feature_mnehf/node0 --capture <work>/mempool-capture.json \
    --wrapper <work>/core/test/functional/feature_mnehf_capture.py \
    --test-file <work>/core/test/functional/feature_mnehf.py --datadir <scratch>/dd --port 29931 \
    --out dash-oracle-sys/tests/data/mnhf-regtest-v24.0.json.gz
```

The file is gzip with mtime 0 and sorted keys. Block hashes and quorum hashes differ from run to run
(fresh keys), so a regenerated file differs byte for byte; the verdicts and the heights above do not.
## Governance regtest network (`gov-regtest-v24.0.json.gz`, `gov-regtest-v24.0.governance.dat`, O2a-GOV)

`dash-oracle-sys/scripts/gov_capture.py` is a test of Dash Core's own functional-test framework
(`test/functional/test_framework` of v24.0.0-rc.3, byte-identical to the tag), run against the
release binaries (dashd v24.0.0-rc.3, sha256 `e204e316…5bb4`). Its five nodes run four regular
masternodes, the framework's defaults (`-dip3params=2:2`, `-testactivationheight=v20@100`,
`-testactivationheight=mn_rr@100`) and `-budgetparams=10:10:10 -debug=gobject`. It turns
InstantSend off (as `feature_governance.py` does, so the collaterals are mined), adds two
evonodes, submits four proposals with `gobject prepare`/`submit`, and has the masternodes vote on
three of them (`gobject vote-alias` for funding, `voteraw` with operator signatures for the other
signals). The masternodes create a superblock trigger themselves and vote on it. At the block
before the superblock (179) it reads from node 0: the headers and block hashes, `protx listdiff 1
179`, the collateral transactions, `gobject list`/`get`/`getcurrentvotes`, `getgovernanceinfo`,
`getblocktemplate` and `mnsync status`.

It then delivers twelve votes to node 0 over P2P, each from a fresh inbound peer: it announces the
vote with an `inv`, waits for node 0's `getdata`, sends the `govobjvote`, and keeps node 0's
`debug.log` lines for it (receipt, verdict, misbehaviour). The votes cover an unknown masternode
with a known and an unknown parent, a known masternode with an unknown parent, operator-signed
funding votes with an unknown and a known parent, an accepted vote, the same signal again, an
obsolete vote, a vote an hour ahead, an unknown signal, an evonode's vote on the trigger and a
10-byte signature. It reads the objects again, mines the superblock (kept as raw hex), stops
node 0 and copies its `governance.dat`.

`dash-oracle-sys/scripts/gen-gov-regtest.py` writes the JSON (the capture, plus the list after
block 179 re-encoded by `dml-listdiff-check.py`'s `encode_list` over the captured block hashes)
and copies `governance.dat` byte for byte; the JSON keeps the file's sha256. `tests/gov_regtest.rs`
replays node 0's objects and votes from its `governance.dat`, then the twelve probes at the times
node 0 logged, and compares every verdict, tally, current vote, local validity and the superblock's
payments with the captured answers. The re-encoded list's internal ids and
`nTotalRegisteredCount` are not Core's; no governance answer reads them.

To regenerate (keys, hashes and times change on every run):

```bash
# <core-test>: v24.0.0-rc.3's test/ tree with config.ini pointing at itself; py_ecc installed.
cp dash-oracle-sys/scripts/gov_capture.py <core-test>/functional/
DASHD=<dashcore-24.0.0-rc.3>/bin/dashd DASHCLI=<...>/dash-cli python3 <core-test>/functional/gov_capture.py \
    --configfile=<core-test>/config.ini --nocleanup --tmpdir=<work>/ft --outdir=<work>/out --portseed=733
python3 -I dash-oracle-sys/scripts/gen-gov-regtest.py <work>/out
```
