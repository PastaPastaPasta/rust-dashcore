# Provenance of `test-vectors/lq4/`

LQ4a-1's Core corpora (LQ4 §5, DEC-623 Q-2): byte-for-byte copies of two files from the node
program's base-sdk fork, so dash-llmq's tests read them without a Cargo git checkout. Nothing here
is hand-written or edited. `dash-llmq/src/dkg/session/vectors.rs` checks each file's sha256 below
before it reads it.

| File | Copied from (fork revision `f27bc4f55ccc0ff1534b7ca6572fb237dc0909b6`) | sha256 |
|---|---|---|
| `bls_llmq_100.json5` | `pkgs/pkc/corpus/bls_llmq_100.json5` | `72bec3e551d22a6b511b87e73bbc8f0215736a91bf9ee7efe79ac514c8c33557` |
| `bls_oracle_dkg.json5` | `pkgs/pkc/corpus/bls_oracle/dkg.json5` | `e5d2163fefe7090b7c5357d57ffd98396053b1459e0b42bf038e2f7731bdcaa2` |

The fork's `pkgs/pkc/corpus/PROVENANCE.md` and `pkgs/pkc/corpus/bls_oracle/PROVENANCE.md` at that
revision are the full records; in short:

## `bls_llmq_100.json5`

Per scheme (`chia` = Core's legacy scheme, `ietf` = its basic scheme), a three-member,
threshold-2 DKG of LLMQ type 100 as Core runs it: each member's verification vector and secret key
shares (`contribute`), every member's check of the shares it received (`verify`), and the
premature and final commitments. Ids and hashes are written as `uint256::GetHex` prints them, the
stored bytes reversed.

- Source: upstream base-sdk `ff3499594d2b` (Kittywhiskers Van Gogh, 2026-04-22), merged in
  `5d3e57d4674a`; `c9eefdff3423` and `9b022b5c2e73` re-spelled the ids in GetHex order.
- Values reproduced by Core v24.0.0-rc.3 through the consensus oracle shim
  (`contrib/vectors/bls_oracle/check_upstream.py`). The shares are checked by Core's own
  verification instead, as the file does not record the polynomial coefficients.

## `bls_oracle_dkg.json5`

A regtest DKG transcript: the type-100 sessions' `qcontrib`, `qcomplaint`, `qjustify`, `qpcommit`,
`qfcommit` and `qsigrec` messages as captured, the members in Core's order, the operators'
regtest keys, and every share, aggregate and verdict Core derives. Here `pro_tx` and the share ids
are the stored bytes.

- Run: Core's `test/functional/feature_llmq_dkgerrors.py` from the v24.0.0-rc.3 tree
  (`0364ed626e`), PRNG seed 5249215831811370464, against the release `dashd` 24.0.0-rc.3 (sha256
  `e204e3163185b1bffcd3bf3532c0957d8cc11019eaf8f3dbc3b2fc82a7ab5bb4`) run with
  `-capturemessages`.
- Generator: the fork's `contrib/vectors/bls_oracle/dkg.py --probe <probe> --run <run dir>
  --argv <argv log> --out dkg.json5`, which has Core check every step through the oracle.
- Its eight sessions follow the functional test's order: an honest quorum, then
  `contribution-omit`, `contribution-lie` with a correct justification, `contribution-lie` with
  `justify-omit`, `contribution-lie` with `justify-lie`, `complain-lie`, `commit-omit` and
  `commit-lie`, each at rate 100 on one masternode.
