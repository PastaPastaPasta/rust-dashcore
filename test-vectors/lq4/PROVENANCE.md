# Provenance of `test-vectors/lq4/`

LQ4a-1's Core corpora (LQ4 §5, DEC-623 Q-2): byte-for-byte copies of two files from the node
program's base-sdk fork, so dash-llmq's tests read them without a Cargo git checkout. LQ4a-2 adds
two files derived from Core v24.0.0-rc.4 captures, and LQ4b-1 three derived from Core
v24.0.0-rc.4 and v24.0.0-rc.5 captures (below). Nothing here is hand-written or edited.
`dash-llmq/src/dkg/session/vectors.rs`, `vectors/replay.rs` and `vectors/finalize.rs` check each
file's sha256 before they read it.

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

## LQ4b-1: `dkg_lq4b_dkgerrors.json5`, `dkg_lq4b_rotation.json5`, `dkg_lq4b_singlenode.json5`

Derived by `trim_lq4b1.py` (sha256 `662372e3fc53fe1f457f2e9c10ed4c35c6833c40823716bada46c21292c197c0`)
from the LQ4b C0 captures:

```
python3 -I trim_lq4b1.py <captures/out> <out dir>
```

| File | sha256 |
|---|---|
| `dkg_lq4b_dkgerrors.json5` (101,448 bytes) | `29968b2366db456b09ba2537e6d4e1beb2469bffe5eaa20c5b12dd5509059df5` |
| `dkg_lq4b_rotation.json5` (186,234 bytes) | `6815a7b4c19deba0254de0bf1d0444087eecc8364c9e94a5bd7b926f1f3a9ae6` |
| `dkg_lq4b_singlenode.json5` (31,523 bytes) | `7a30035fa6a2e515212d9331e415b6d58d20b8ad2fa9a095a106b861877e6490` |

Each file holds both runs, under `rc4` and `rc5`. The captures (2026-11-05, agentbox) ran the stock
release binaries under the base-sdk fork's `-capturemessages` wrapper, LQ4a-2's recipe:

| Tag | Commit (tag object) | `dashd` sha256 |
|---|---|---|
| v24.0.0-rc.4 | `0e7382df80d0bc6811068d076b232a5a26d34a3c` (`49caf372bb67`) | `e1be2564c5f98b7cb97e60953e1ed6817eeb3b5ab36cf5473f32ad75b4f823a7` |
| v24.0.0-rc.5 | `8163290aa0307ed4bc2fc0ba15b78c20d61dfa88` (`3f3373a11eb6`) | `fa6f61de98950d17adc1065155f783b189bc40b4694534df85354511bc8b8a3c` |

| Capture | sha256 |
|---|---|
| `dkg_rc4_dkgerrors.json5` | `4c269589bca1d3dedd1d5e09d97bd3c9c28a6deb2465bae1848e81d03d2dcf13` |
| `dkg_rc4_dkgerrors.scenarios.json` | `6215c4214ede7774f0a9d8ba58eba93d76aae0a2dc515fbc9ff76b8198fff0fa` |
| `dkg_rc5_dkgerrors.json5` | `9d06ef063250c1ce58c3d6b8284db6d65b4e68db3ae97a5131c062dd5d8da8cc` |
| `dkg_rc5_dkgerrors.scenarios.json` | `98867658684dec15739fb320428a5e2f96e3bcdf14cb5717cc9a8a97b4111cff` |
| `dkg_rc4_rotation.json5` | `ef62a39f26c7ae06d54fe092636fc7bb1faf425f14a6c32f9e785f0ad0656b0b` |
| `dkg_rc5_rotation.json5` | `e6d8c213c6d6eff4daa23253819a73e70f3ad893241990f91f809728310f24cc` |
| `dkg_rc4_singlenode.json5` | `a8081a19fb47f098a897366f1008a56d164c874d09eaedc17dc4e6bd23ad9100` |
| `dkg_rc5_singlenode.json5` | `d1accd295ec1c16595c9c95fcb1e6201324ac08c14f48a0811f2e27bab7edb66` |

- **Tests** (sha256 of each tag's blob under `test/functional/`): `feature_llmq_dkgerrors.py` rc.4
  `7af9ee71f2a4064ea57a9712f3524fe44c74bc4dbdc50849c0546140d822071c`, rc.5
  `97c74595544002f19904be29f9ea33ed85429e0b0324c3a4cac593d5243db8d4`;
  `feature_llmq_singlenode.py` `08f893bd84496b7a73f801a2058e92baabcfc8a1726a048d5a6d77289702750c`
  and `feature_llmq_rotation.py` `b8f26e31c115eb0da5e34070abfc5594cb942cce67419eb997f1d36c50b4eabb`,
  the same at both tags. The test framework is the same at both tags.
- **dkgerrors:** a labelled copy of the test (sha256
  `52213d526a7aafe16f2a4ff31b19e73f184e01e336c5c3907d7cb132e8296670`), generated from rc.4's
  `feature_llmq_dkgerrors.py` by `make_labelled.py` (sha256
  `df5aaa31bfac71ff09301ab00520b3043db397f654c61f53816b547aed6e5795`), which only adds log lines
  naming each mined quorum's scenario and erring masternode. The same copy runs unchanged at both
  tags; it does not run the scenario rc.5's stock test adds. 4 nodes, 3 masternodes; `llmq_test` at
  Core's values: size 3, minSize = threshold = dkgBadVotesThreshold = 2. PRNG seeds: rc.4
  3243386188382568394, rc.5 8100480099356422146. Eight sessions per run, in the test's order:
  `honest`, `contribution-omit`, `contribution-lie-justified`, `contribution-lie-justify-omit`,
  `contribution-lie-justify-lie`, `complain-lie`, `commit-omit`, `commit-lie`, each error at rate
  100 on mninfo[0].
- **rotation:** Core's `feature_llmq_rotation.py`, 9 nodes, 8 masternodes,
  `-llmqtest{,instantsend,platform}params=4:4` (size 4, minSize = threshold =
  dkgBadVotesThreshold = 4); `llmq_test_dip0024` at Core's values (size 4, minSize 4, threshold 3,
  dkgBadVotesThreshold 2). PRNG seeds: rc.4 5125890220894745135, rc.5 49753468885712369. Eleven
  sessions per run: 5 of type 100 (version 3) and 6 of type 103 (version 4), 3 at quorumIndex 0 and
  3 at quorumIndex 1.
- **singlenode:** Core's `feature_llmq_singlenode.py`, `-llmqtest{,instantsend,platform}params=1:1`,
  one regular masternode and one EvoNode added at runtime. PRNG seeds: rc.4 4506192660463283726,
  rc.5 4388340673142575952. Seventeen sessions per run: 6 of type 100, 5 of type 104, 6 of type
  106, each version 3 at quorumIndex 0. V19 is buried at height 1 on regtest, so every base has V19
  active.
- **Harness:** each test ran as a subclass with a capture mixin (`lq4b_capture.py`, sha256
  `52807057130f7346c62cfc71aee8608c1052505852991a56c80ec2a26e16be03`) that only reads: after each
  mined quorum it logs `quorum list`, `quorum info`, the mined block's type-6 transactions,
  `protx info` of each member and `getdeploymentinfo`. Subclasses: dkgerrors
  `2bacb604ba8fc5715e32b328f3cc30eac267c8de5c11464c3a516fb1916b27d9`, singlenode
  `27b82444fba7949b90a0bb0a7d92609e7f2e76f9e8ee4e5f5e3b6ca4ff97c0bf`, rotation
  `1bed006660103c57a4d328c7b1fb59e40a7d0db858856a92fba202298f798e78`; runner `run.sh`
  `4d0e03bd33f96fc9eedc93294f72626962c5a63543891a9824e93394baa02865`.
- **Extraction:** multi-member sessions by the fork's `contrib/vectors/bls_oracle/dkg.py` at
  `b6853466b7f52f002873ccb4c8817b67f2ad54f5`, unmodified (sha256
  `8560b4ebd1d2ab824ff66ff5a0cf2b952b243b31a535ce6b65e021bdbd87add7`; `oracle.py`
  `a92a2ee77ce6b32ba3dae1db12b98a9a2e026d0525e795ba5b7deac171ba4439`, `gen.py`
  `88b6a1069603b62e5ed8bb36d2ad365c669a3ee40ed9b36f1cbfdc5c575fff0b`). Every final check it
  records is true. The labels reach only `scenarios.json` (written by `sessions.py`, sha256
  `c8766b476042c92adcf0f599d0d37e6a3b76abd262eaf17342671dc62ef7227a`), never `dkg.py`'s output.
  `sessions.py` also checks each session against the logged RPC output: its quorum is listed, the
  final's quorumIndex equals `quorum info`'s and its bytes equal the mined final.
- **Single-member adapter:** `dkg.py` cannot extract a single-member session (no contributions, no
  verification vectors), so `single_member.py` (sha256
  `4f020f8dadb0cd03ec46e09f6182ee8cf6b5b87fbd21e7d091edc8b6e222484b`) takes the member from
  `quorum info` / `protx info`, its operator secret key from the run's argv log (the key whose
  public key is the member's `pubKeyOperator`), and the final from the captured `qfcommit`,
  byte-equal to the mined one. It checks, for every session: `membersSig` verifies under the
  operator public key through the fork's BLS probe; `quorumSig` equals `membersSig`;
  `quorumPublicKey` is the operator public key; signers and validMembers are `[1]`; quorumHash and
  quorumIndex equal `quorum info`'s; the version is `GetVersion(false, V19 active after the base)`.
- **Kept per session:** members (`pro_tx`, `operator_sk`), each message's sender and raw bytes in
  capture order (contributions, complaints, justifications, premature commitments), the final's
  raw bytes; for dkgerrors the scenario label and the erring member's index; for a single-member
  session its member, base height and V19 state. Dropped: what the tests rebuild from the raw bytes
  or the operator keys. Effective parameters are in each file's `params`; the script checks their
  `size` against every final's bitset sizes.
- **Byte order:** `quorum_hash` and `pro_tx` are the stored bytes.
