# Provenance of `test-vectors/golden/`

What Dash Core computed from **Core-produced wire captures**. Node program task O4 (finding F12):
these files are the ground truth the Rust masternode-list code is checked against, replacing
constants that had been derived from a corrupted fixture.

| File | Records |
|---|---|
| `mnlistdiff-core-ground-truth.json` | 35 `mnlistdiff` payloads (2 whole-list diffs and 33 incremental ones, the last of them a tip diff) |
| `qrinfo-core-ground-truth.json` | 2 `qrinfo` payloads |
| `unique-property-hash.json` | 20 unique-property keys (see the section below) |

## Inputs

The payloads are in the repository, byte for byte as a Core node sent them, under
`dash/tests/data/test_DML_diffs/` (see its `PROVENANCE.md` for peers and capture details). Each
record names its payload (`repoPath`) and pins it by size and sha256 (`wireSha256`).

## Producer

`src/test/o4_vector_dump_tests.cpp` on the local Dash Core branch `node/o4-core-vectors`
(commit `c89375bdbf`, on top of `v24.0.0-rc.3`; the change is also in
`../core/0364ed626ec2/instrumentation/o4-core.patch`). It does nothing unless the `O4_*`
environment variables are set. For every payload it uses only Core's own code:

1. deserializes with `CSimplifiedMNListDiff` / `CQuorumRotationInfo` (`SER_NETWORK`, the current
   protocol version, BLS in the basic scheme: all payloads are from chains past V19; a signature
   read in the wrong scheme loses its identity, which collapses `quorumsCLSigs`);
2. serializes the object back and requires **exactly the input bytes** (`coreRoundTripExact`);
3. hashes every list entry with `CSimplifiedMNListEntry::CalcHash` (the pre-image Core uses for the
   masternode merkle root) and every quorum commitment with `llmq::BuildCommitmentHash` (BLS
   scheme chosen by the commitment's own version: 1 and 2 legacy, 3 and 4 basic, because the key
   encoding in that hash follows Core's global scheme flag);
4. for the diffs of one chain (`group`), applies each diff to a running list and compares
   `CSimplifiedMNList::CalcMerkleRoot` of the result with `merkleRootMNList` in the diff's own
   coinbase (`CCbTx`); `rootsMatch` is true for the three chain heads (mainnet 2227096,
   mainnet 2241332 after applying the second diff to the first, testnet 1296600). The
   independent diffs (`group: wire`) are not a chain and only get steps 1 to 3.

Hashes are printed in Core's display (`uint256::ToString`) form. Entry rows are
`[proRegTxHash, nVersion, nType, isValid, CalcHash]`; commitment rows are
`[llmqType, quorumHash, quorumIndex, nVersion, BuildCommitmentHash]`.

## Regeneration

`contrib/node/regen-core-vectors.sh golden <test_dash>` (see the script for how to build
`test_dash`). The output is byte-identical on a second run.

## Consumers

`dash-core-vectors::golden` parses them. `dash/tests/core_mnlistdiff_ground_truth.rs` checks
`dashcore` against every record; `dash/src/sml/masternode_list_entry/hash.rs`
(`entry_hash_matches_core_calc_hash`) checks entry hashes against the whole-list diff.

## `unique-property-hash.json`

Core's unique-property keys (`CDeterministicMNList::GetUniquePropertyHash<T>`, the keys of
`mnUniquePropertyMap`), computed by node program task O2a-EV through the oracle shim entry
point `oracle_unique_property_hash` (`src/test/oracle/oracle.cpp` on the local Dash Core
branch `node/o2a-ev-oracle-v24.0`, Core v24.0). The shim reads each value with Core's
deserializer, takes the private template through an explicit-instantiation accessor, and
writes both the hash and the bytes Core hashes; it fails unless SHA256d of those bytes is
Core's hash.

Each record is `{label, kind, legacy, value, preimage, hash}`: `value` is the input (a
service in ADDRv2 format), `legacy` the BLS scheme an operator key was read with, `hash` in
internal byte order. The inputs are synthetic values of every key type (collateral, services
on each network, an ExtNetInfo domain, key id, platform node id) and the first legacy operator
key of Core's `evo_simplifiedmns_tests` (`../core/0364ed626ec2/transcribed/`) in its legacy
and basic encodings. The vectors show that an ADDRv1-compatible service hashes its 18-byte
ADDRv1 form, that a key id and a platform node id with the same bytes share a key, and that
a lazy operator key hashes the bytes it was read with, so the two encodings of one key are
different keys.

Regenerate with `DASH_ORACLE_REGENERATE=1 cargo test -p dash-oracle-sys --features oracle
--test mnlist unique_property_vectors`; `dash-oracle-sys/tests/mnlist.rs` checks the oracle
against this file on every run.
