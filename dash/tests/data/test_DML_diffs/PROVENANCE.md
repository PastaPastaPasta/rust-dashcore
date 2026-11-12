# Provenance of the masternode-list fixtures

Where each file in this directory (and the two copies in `dash/artifacts/`) comes from, how it was
checked, and what is still not independently verified. Node program task O4 (finding F12 of the
R4 verification research: the `.bin` fixtures were *not* Core-produced).

## What was wrong

`mn_list_diff_0_2227096.bin`, `mn_list_diff_2227096_2241332.bin` and
`dash/artifacts/mn_list_diff_testnet_0_1296600.bin` (and the unused duplicate
`dash/artifacts/mn_list_diff_0_2227096.bin`) were re-encodings made by Rust code, not bytes from a
Core node. They differed from what Core sends in the 16-byte service address of the masternode
entries:

| | Core (`CService`) | old fixture |
|---|---|---|
| IPv4 `a.b.c.d` | IPv4-mapped `::ffff:a.b.c.d` | IPv4-compatible `::a.b.c.d` (2,800 entries in the first file, 5,600 bytes) |
| null service | all 16 bytes zero | `::ffff:0.0.0.0` (16 entries of `mn_list_diff_2227096_2241332`, 31 entries of the testnet artifact) |

Because the entry hash covers that field, the diffs' masternode merkle roots did not match their own
coinbase, and `entry_hash_matches_core_calc_hash` compared the code under test with constants taken
from the same corrupted encoding. Hashing the Core bytes gives different values (for example
`0008858d…` is `33b42553…`, which is what `calculate_entry_hash` returns for the Core bytes; the
old constant was `759c929f…`).

## Core wire captures

Every `*.bin` below is the unmodified payload of a `mnlistdiff` or `qrinfo` P2P message sent by a
real Dash Core node. dashd has no RPC that returns this encoding (`protx diff` returns JSON), so
the files are captured over P2P with `contrib/node/p2p_capture.py` and
`contrib/node/capture_fixtures.py`, which holds the request table (block hashes, base hashes) for
all of them. No file was edited after capture.

Captured 2026-10-08 (UTC) from public mainnet/testnet peers; the reference oracles under
`/work/chains` were still syncing (mainnet at height 0.48M of 2.55M) and could not serve these
heights. The bytes are a function of the chain, the request and the announced protocol version, so
independent peers agree:

| Files | Peers (user agent) | Result |
|---|---|---|
| `mn_list_diff_0_2227096.bin` | 168.119.83.14 and 136.243.29.198 (`Dash Core:23.1.8`), protocol 70240 | identical, sha256 `e3be4126…fc7a` |
| `mn_list_diff_2227096_2241332.bin` | same two peers | identical, sha256 `3fe27e02…d1e638` |
| `dash/artifacts/mn_list_diff_testnet_0_1296600.bin` | 46.224.41.173 (23.1.7), 44.249.22.218 (23.1.3), 35.166.35.250 (`24.0.0(dcg-seed-1)`) | identical, sha256 `27ea9691…1097` |
| `core_wire/mnlistdiff_<a>_<b>.bin` (31 files) | 168.119.83.14; re-requested from 136.243.29.198 | identical |
| `core_wire/mnlistdiff_tip_2518986.bin` | 168.119.83.14 and 207.148.75.95 (23.1.8) | identical |
| `core_wire/qrinfo_2240504.bin` | 168.119.83.14 and 207.148.75.95, announced protocol **70235** (legacy construction) | identical |
| `core_wire/qrinfo_2518986.bin` | same two peers, announced protocol 70240, empty base list | identical |

The block hashes of the three named diffs (and every height→hash pair of the container files below)
were checked against the heights in Core's header index (the oracle `getblockheader`). The same files regenerate with
`contrib/node/capture_fixtures.py --check --mainnet-peer HOST:9999 --testnet-peer HOST:19999`
(it re-requests each `mnlistdiff` and compares byte for byte; a `qrinfo` is not compared by the
script because of its tip diff, see below: it was checked by hand, as described there).

**The `qrinfo` tip.** Core builds `mnListDiffTip` against the responding node's *current* chain
tip, not the requested block, so that one diff in a `qrinfo` changes with every block. The rest of
the message is a function of the request. The two `core_wire/qrinfo_*.bin` captures therefore carry
a tip diff for whatever the peer's tip was on 2026-10-08, and are reproducible apart from it:
re-requesting both from a third peer (207.148.75.95) with the parameters in
`contrib/node/capture_fixtures.py` and parsing the replies with Core
(`test-vectors/golden/PROVENANCE.md`) gave identical snapshots, cycle diffs and last commitments
and a different tip diff.

**How Core's request handling shaped the request.** `BuildQuorumRotationInfo`
(`src/llmq/snapshot.cpp`) picks the base of each diff as the highest base hash at or below that
diff's work block, falling back to genesis. The legacy construction, served to peers below protocol
70236 (`EFFICIENT_QRINFO_VERSION`, `use_legacy_construction = common version < 70236`), uses the
bases of the request only (the tip diff always from the highest one). The efficient construction
also adds each diff's work block to the bases once it is built, so the cycle diffs chain upward
from genesis when the request has no base. `qrinfo_2240504.dat` has the legacy shape (one known
list at 2227096, which is below every work block, so all five diffs are based there) and
`qrinfo_2518986.dat` the efficient one (a chain from genesis), so the captures were requested the
same way: base list `[2227096]` at announced protocol 70235, and an empty base list at 70240.

## Core's own reading of the captures

`test-vectors/golden/mnlistdiff-core-ground-truth.json` and `qrinfo-core-ground-truth.json` hold
what Core itself computed from these bytes (its `CSimplifiedMNListDiff` / `CQuorumRotationInfo`
deserializers, `CalcHash`, `BuildCommitmentHash`, `CalcMerkleRoot`); see
`test-vectors/golden/PROVENANCE.md`. For all 35 diffs and both `qrinfo`:

- Core re-serializes the payload to exactly the captured bytes;
- the merkle root of the list a chain of diffs builds equals the root in that diff's own coinbase
  (`mn_list_diff_0_2227096`, `mn_list_diff_2227096_2241332` and the testnet diff).

`dash/tests/core_mnlistdiff_ground_truth.rs` and the rewritten
`entry_hash_matches_core_calc_hash` (`dash/src/sml/masternode_list_entry/hash.rs`) check that
`dashcore` decodes, re-encodes, hashes every entry and commitment and computes the masternode-list
root exactly as Core did. The second one pins the fixture's sha256 to the capture Core's numbers
came from.

## The bincode fixtures (`*.dat`)

`mnlistdiffs_2240504.dat`, `qrinfo_2240504.dat` and `qrinfo_2518986.dat` are bincode
(`bincode::config::standard()`) serializations of `dashcore` types. They are now **exactly
`bincode(decode(Core wire capture))`**, enforced by `dash-spv/tests/core_fixtures.rs`; run
`cargo test -p dash-spv --test core_fixtures -- --ignored regenerate` to rewrite them.

- `mnlistdiffs_2240504.dat`: the map `(base height, height) -> MnListDiff` of the 32 diffs
  (`mn_list_diff_0_2227096.bin` plus `core_wire/mnlistdiff_*_*.bin`).
- `qrinfo_2240504.dat`, `qrinfo_2518986.dat`: the Core `qrinfo` capture with `mnListDiffTip`
  replaced by the Core `mnlistdiff` for the fixture's own tip block (`2227096 -> 2240504` and
  `mnlistdiff_tip_2518986.bin`). That replacement is needed because the tip of a `qrinfo` is the
  server's live tip (above). Both are Core-produced bytes; the composition is the only
  synthetic step.

Comparison with the files they replace (decoded, field by field):

| File | Result |
|---|---|
| `qrinfo_2518986.dat` | **byte-identical to the previous file** (the August 2026 live-sync capture was already exact) |
| `qrinfo_2240504.dat`, 13 of the 32 diffs of `mnlistdiffs_2240504.dat` | equal except two entry fields: a masternode with a null service was `0.0.0.0:0` and is `[::]:0` (as Core sends it), and the Evo `platform_node_id` was byte-reversed |
| everything else (merkle trees, coinbases, deleted lists, quorum commitments, ChainLock signature groups, snapshots, last commitments) | identical in all files |

All `dashcore` (506 + integration) and `dash-spv` library (625) tests pass on the regenerated
files.

## Files not regenerated

| File | Status |
|---|---|
| `block_container_2240504.dat`, `block_container_2518986.dat` | height→hash maps (chain facts, not Core messages). All 186 pairs verified against Core's header index (oracle `getblockheader`) on 2026-10-08. |
| `masternode_lists_2243493.bin` | engine state (29 lists holding only their ChainLock quorums, a rotation cycle) built by the engine from diffs. All 149 height→hash pairs verified the same way. **Quorum contents not independently verified.** Follow-up: rebuild from Core captures with the engine. |
| `DML_0_2221605.hex`, `QR_INFO_0_2224359.hex` | raw Core payloads (a mainnet `mnlistdiff` and `qrinfo`) from before this task; roots checked against their coinbases by the R4 research (6 of 6). Origin and peer not recorded. |
| `qrinfo_core231_paloma.hex` | a Core 23.1 devnet/testnet `qrinfo`; origin not recorded. |

## Duplicate

`dash/artifacts/mn_list_diff_0_2227096.bin` is byte-identical to
`test_DML_diffs/mn_list_diff_0_2227096.bin`; nothing references it. It was regenerated together so
the two cannot disagree.
