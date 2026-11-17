# Public seeds of the `diff_payload_<type>` targets (BS6b)

`diff_payload_<type>/` holds the public seeds of the L1 target for special-transaction type
`<type>` (1–12). `node-fuzz/targets.toml` names each directory as the target's `public_seeds`, and
`node-l1/tests/l1_payload.rs` replays every file against the oracle's v24.0 pin and mutates them
in its proptest twin.

A seed is one flags byte (bit 0 set: Core's process-wide BLS flag is legacy) followed by an extra
payload. Seeds carry no expected outcome; the oracle judges each one when a target or test runs.
File names are the SHA-1 of the contents.

## Sources

Every payload comes from Dash Core; nothing is hand-written.

- The `extraPayload` of each transaction in `dash/tests/data/core_v24_regtest.json`: dashd
  v24.0.0-rc.3 on regtest chains built by Core's functional tests (provenance in
  `dash/contrib/core-vectors/README.md`).
- The extra payloads of the transactions in base-sdk's `pkgs/primitives/corpus/sighash.json5`
  (fork branch `node/bsdk-bs6b-sighash`): transactions dashd v24.0.0-rc.3 returned from
  `getblock 2` on replicas of the same regtest chains and on the mainnet reference node. These add
  the legacy-scheme shapes the regtest chains lack: provider payloads at version 1 and quorum
  commitments at versions 1 and 2.
- `core_cbtx_v1.json`: the version-1 coinbase payloads of mainnet blocks 1028160 and 1088639, as
  dashd v24.0.0-rc.3 (the mainnet reference oracle) returned them from `getblockhash` and
  `getblock <hash> 2` (the file records the heights, hashes and txids). No other source holds a
  CbTx v1.
- BLS samples: one sample per class label and (legacy, basic) outcome pair of the eager samples
  of base-sdk's `pkgs/pkc/corpus/bls_wrapper.json5` (50 for G1 keys over 42 labels, 74 for G2
  signatures over 57 labels; an outcome is its kind: throws, invalid or valid), whose outcomes
  Core's own BLS wrapper produced (provenance in that file's header).

## Construction

For each supported type and version (the commitment version for a quorum commitment), the first
payload becomes a seed under both flags. Then each BLS field of that payload takes every BLS sample
in turn, A1 §1.4.1's "seeded per field":

| Type | Fields | Flags |
|---|---|---|
| 1 ProRegTx | `pubKeyOperator` (lazy), found by the key Core printed | basic |
| 2 ProUpServTx, 4 ProUpRevTx | `sig` (version scheme) | basic |
| 3 ProUpRegTx | `pubKeyOperator` (lazy) | basic |
| 5 CbTx (v3+; v1 and v2 have none) | `bestCLSignature` (global flag) | both |
| 6 QcTx | `quorumPublicKey`, `quorumSig`, `membersSig` (version scheme) | basic |
| 7 MnHfTx | `sig` | basic |
| 9 AssetUnlockTx | `quorumSig` (global flag) | both |
| 12 ProUpSharedRegTx | `pubKeyOperator` (lazy, always basic) | basic |
| 8, 10, 11 | no BLS field | — |

The generator fails when a supported type and version (types 1–4 at versions 1–3, CbTx 1–4,
commitments 1–4, MnHF 1, asset lock 1–2, asset unlock 1–2, types 10–12 at version 1) has no Core
payload, when a BLS field cannot be located, or when a field lacks a seed for some (label, outcome
pair) of the source. `COVERAGE.json` lists the samples and, per field, its base seed and its
seeds by label and outcome pair. `seeds_cover_every_field_and_outcome_pair` in
`node-l1/tests/l1_payload.rs` checks, against its own table of the supported versions and BLS
fields, that every supported version has a seed under each flag, that each field holds every
pair, and that each listed seed is its base seed with the sample in the field, which the test
locates itself (operator keys by decoding, signatures and quorum keys from the payload's end).

Regenerate with:

```sh
python3 -I node-l1/seeds/gen.py --regtest dash/tests/data/core_v24_regtest.json \
    --signed <base-sdk>/pkgs/primitives/corpus/sighash.json5 \
    --extra node-l1/seeds/core_cbtx_v1.json \
    --bls <base-sdk>/pkgs/pkc/corpus/bls_wrapper.json5 --out node-l1/seeds
```
