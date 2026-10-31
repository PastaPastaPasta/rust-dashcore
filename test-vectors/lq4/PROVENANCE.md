# Provenance of `test-vectors/lq4/`

LQ4a-1's Core corpora (LQ4 §5, DEC-623 Q-2): byte-for-byte copies of two files from the node
program's base-sdk fork, so dash-llmq's tests read them without a Cargo git checkout. LQ4a-2 adds
two files derived from Core v24.0.0-rc.4 captures (below). Nothing here is hand-written or edited.
`dash-llmq/src/dkg/session/vectors.rs` and `vectors/replay.rs` check each file's sha256 before
they read it.

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

## LQ4a-2: `dkg_12_7.json5`, `dkg_simplepose.json5`

Derived by `trim_lq4a2.py` (sha256 `ed036c0f89b5eeda62ba6358036ad593166476e99bc05da696afea4b1ba42e88`)
from the DEC-638 captures, so the tests carry only what a replay reads:

```
python3 -I trim_lq4a2.py <captures/out> <captures/run-sp> <out dir>
```

| File | sha256 |
|---|---|
| `dkg_12_7.json5` (105,140 bytes) | `4832e68ebf8b7a985cf12b68f821055a9ff2548b87689791a6b04a4fa6da9b08` |
| `dkg_simplepose.json5` (97,580 bytes) | `d2277fbb0a0fd3295ffda7f426770f6c5438eb876806d4d9338a3fdc733205bc` |

The captures (2026-10-28, agentbox), all against Core v24.0.0-rc.4 (`49caf372bb`); every check
value came from the base-sdk fork's BLS probe (`f27bc4f55ccc`, `contrib/vectors/bls_oracle`) and
its unmodified `dkg.py`:

| Capture | sha256 |
|---|---|
| `dkg_12_7.json5` | `11f627e733ff926cf1042d86441f14f62f0f37e15ae1012c4fdb7331418924b4` |
| `dkg_12_7.scenarios.json` | `01754f5fedd5adf19a4da95470f13746720f888af1ac0771b78252f35edef401` |
| `dkg_simplepose_spork23.json5` | `6f5781fcae2f417300bbaf33cac79cc7ee1b289863a721c4b486aa98c687c02e` |
| `folds_simplepose_spork23.json` | `1ca95fb4e9c255c165749a11646c8177b49b41108c38e25b6ddbdcc9d9733689` |
| `dkg_simplepose_nospork23.json5` | `0aa5af61ff9b100dabd875dc06d98460335239de3dff6ffcabec2e70d42c7713` |
| `folds_simplepose_nospork23.json` | `be3ed2775afa7451d974b57ce0c6b91c530a723af8b5d33d326129cfa29d6580` |
| `lq4cap-fold-log.patch` | `4ada45e8fa63a3dabb7ee228135f5dd797c3f435f6db408ee56c045e36e1e7da` |

- **12/7:** `feature_llmq_dkgerrors.py` with `set_dash_test_params(13, 12)` and
  `set_dash_llmq_test_params(12, 7)` (sha256 `5ad0f1d6…4430`), against the release `dashd`
  24.0.0-rc.4 (sha256 `e1be2564c5f98b7cb97e60953e1ed6817eeb3b5ab36cf5473f32ad75b4f823a7`), PRNG
  seed 622003510592456939. `llmq_test` is size 12 with minSize = threshold =
  dkgBadVotesThreshold = 7 (`chainparams.cpp:1019-1027`), as the file's `llmq_test` says. Three
  sessions, labelled from `dkg_12_7.scenarios.json` (`mn` = each member's `mninfo` number): `honest`;
  `omit5`, contribution-omit on mn7–mn11; `mixed`, mn0 contribution-lie then a correct
  justification, mn1 contribution-lie plus justify-omit, mn2 complain-lie. Every rate is 100.
  The capture's two unlabelled type-103 sessions are left out.
- **simplepose:** Core's unmodified `feature_llmq_simplepose.py` (`set_dash_llmq_test_params(5,
  3)`) against rc.4 rebuilt with `lq4cap-fold-log.patch`, which only logs the inputs and result of
  `VerifyConnectionAndMinProtoVersions` (`dashd` sha256
  `1672fd75728f87a764e42002a7e2d8f8b42a91c40b16119f3e8d90684d1e03d6`). `spork23` ran with default
  arguments (PRNG seed 1713558704895188658), `nospork23` with `--disable-spork23` (PRNG seed
  3240767898587969431). Of each, the script keeps the first session of each kind it has not kept
  yet (a fault branch, all members connected or not, a missing contribution, a type, with or
  without complaints): 8 of 40 distinct sessions and 4 of 27.
- **Kept per session:** members (`pro_tx`, `operator_sk`), each message's sender and raw bytes in
  capture order, and the 12/7 contributions' decrypted shares. Dropped: what the tests recompute
  from the raw bytes (vectors, IES blobs, hashes, verdicts).
- **Derived values:** a premature commitment's `validMembers`, parsed out of its raw bytes. And each
  simplepose fold's `sent`: the `badCount` and `complaintCount` of the node's "sending complaint"
  line for that session in its `debug.log` (run directories `run-sp/ft-{spork23,nospork23}`), or
  null when there is none. One complaint Core logged sending is not in the
  transcript (spork23, type 102, `01ab70c6…`, member 2), so the replay checks its counts only. The
  script checks every captured complaint against its line, and recomputes every logged
  `badConnection` from the fold before it writes anything.
- **Byte order:** `quorum_hash` and `pro_tx` are the stored bytes.
