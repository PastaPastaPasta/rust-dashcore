# Provenance of `test-vectors/core/0364ed626ec2/`

Test vectors for the Rust Dash node, taken from or computed by **Dash Core**. Node program task O4
(NFR-CEQ-01/02). Nothing in this directory is hand-written.

| | |
|---|---|
| Source repository | `dashpay/dash` (local clone `~/workspace/dash`) |
| Commit | `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18` (`Merge #7808: backport: v24.0.x backports for v24.0.0-rc.3`) |
| Tag | `v24.0.0-rc.3` (the release the reference oracles run) |
| Files | 62 verbatim copies, 13 generated files, 1 patch |

The mainnet pin of the program is v23.1.8 (DEC-05). Between v23.1.8 and this commit the data files
differ only in `trivially_invalid.json` (one line: the reject reason of the "version set to null"
row is `bad-protx-payload`, it was `gettxpayload-fail`), `rpc_psbt.json` (changed) and
`quorum_proof.json` (new in v24); `invalid_txs.py` also differs but is not data and is not copied. The C++ tests that `transcribed/` is taken from differ more:
`dip14_tests.cpp`, `evo_islock_tests.cpp` (the real ISDLOCK), `quorum_proofs_tests.cpp` and the
extended-address parts of `evo_simplifiedmns_tests.cpp` do not exist in v23.1.8, and
`block_reward_reallocation_tests.cpp` and `dynamic_activation_thresholds_tests.cpp` changed. The
v24 values are the program's target; re-pinning means running the two scripts below on the new
commit and reviewing the diff, nothing else.

## 1. Verbatim copies

Copied by `contrib/node/sync-core-vectors.sh <dash checkout> 0364ed626ec2` with
`git show <commit>:<path>`, so a dirty working tree cannot leak in. The directory layout is Core's
(`src/test/data`, `src/bench/data`, `test/functional/data`, `test/util/data`). Python modules and
`test/util/data/non-backported.txt` (Core's backport bookkeeping) are not copied. `FILES.sha256`
lists the sha256 of each file and `dash-core-vectors` verifies it (`tests/core_files.rs`).

| File | Core consumer | Content | sha256 |
|---|---|---|---|
| `COPYING` | - | Core's MIT licence text | `f5681e3372ff` |
| `src/test/data/README.md` | - | Core's description of the directory | `036a6cc52cb0` |
| `src/bench/data/block813851.raw` | bench/checkblock.cpp `DeserializeBlockTest` | mainnet block 813851 (999 transactions, 749,213 bytes), added for CS0 | `8f0522041033` |
| `src/test/data/asmap.raw` | addrman_tests.cpp | 59-byte asmap | `55dcec00c72b` |
| `src/test/data/base58_encode_decode.json` | base58_tests.cpp | 14 raw base58 pairs | `73006fdb9d83` |
| `src/test/data/bip39_vectors.json` | wallet/test/bip39_tests.cpp | 24 Trezor vectors, passphrase `TREZOR` | `99a8bd380ec1` |
| `src/test/data/blockfilters.json` | blockfilter_tests.cpp `blockfilters_json_test` | 7 BIP158 vectors with Dash X11 block hashes | `59b744f4e70c` |
| `src/test/data/key_io_invalid.json` | key_io_tests.cpp | 50 strings that must not decode | `f6a1bab254be` |
| `src/test/data/key_io_valid.json` | key_io_tests.cpp | 50 addresses/WIF keys, main/test/regtest | `6ed17a451cec` |
| `src/test/data/proposals_invalid.json` | governance_validators_tests.cpp | 19 invalid proposals | `b05024cbaaca` |
| `src/test/data/proposals_valid.json` | governance_validators_tests.cpp | 6 proposals (valid but expired) | `bd7bb10e8e71` |
| `src/test/data/script_tests.json` | script_tests.cpp `script_json_test` | 1,282 vectors (+52 comment rows); DIP0020 opcodes always enabled | `c9f2b64db5f2` |
| `src/test/data/sighash.json` | sighash_tests.cpp `sighash_from_data` | 500 legacy sighash vectors | `97795a01bc3f` |
| `src/test/data/trivially_invalid.json` | evo_trivialvalidation.cpp | 12 payloads with reject reason | `704e0231200a` |
| `src/test/data/trivially_valid.json` | evo_trivialvalidation.cpp | 25 special-tx payloads, `basic`/`legacy` BLS column | `6532a2daac48` |
| `src/test/data/tx_invalid.json` | transaction_tests.cpp `tx_invalid` | 76 vectors; 10 are `BADTX` (`CheckTransaction` must fail) | `f2c4a5ed2439` |
| `src/test/data/tx_valid.json` | transaction_tests.cpp `tx_valid` | 83 vectors; third column is the set of *excluded* flags | `095b517738e7` |
| `test/functional/data/blockheader_testnet3.hex` | functional `p2p_dos_header_tree.py` | real testnet headers | `229424d4c71c` |
| `test/functional/data/quorum_proof.json` | functional `feature_quorum_proof_chain.py` | testnet quorum proof chain 1548500 -> 1549547 | `10a400e0bf44` |
| `test/functional/data/rpc_bip67.json` | functional `rpc_createmultisig.py` | BIP67 sorted multisig | `9087f6bee68d` |
| `test/functional/data/rpc_decodescript.json` | functional `rpc_decodescript.py` | decodescript cases | `9b14d245b463` |
| `test/functional/data/rpc_getblockstats.json` | functional `rpc_getblockstats.py` | getblockstats golden values | `e6e920a83ba3` |
| `test/functional/data/rpc_psbt.json` | functional `rpc_psbt.py` | PSBT cases | `9869586a7563` |
| `test/util/data/bitcoin-util-test.json` | `test/util/test_runner.py` (`dash-tx`) | golden cases for the `dash-tx` utility; references the other files in the directory | `6b636569a9dd` |
| `test/util/data/blanktxv1.hex` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `7fe79f9a5700` |
| `test/util/data/blanktxv1.json` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `976059495a57` |
| `test/util/data/blanktxv2.hex` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `7dadae435682` |
| `test/util/data/blanktxv2.json` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `f4106fda1b70` |
| `test/util/data/tt-delin1-out.hex` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `a335c4001582` |
| `test/util/data/tt-delin1-out.json` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `415c3f068a62` |
| `test/util/data/tt-delout1-out.hex` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `c6cf0f00b326` |
| `test/util/data/tt-delout1-out.json` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `64f589f3de0c` |
| `test/util/data/tt-locktime317000-out.hex` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `a45243355010` |
| `test/util/data/tt-locktime317000-out.json` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `ba95b2829fa2` |
| `test/util/data/tx394b54bb.hex` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `717f0d4b5a5b` |
| `test/util/data/txcreate1.hex` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `bfcb19c14436` |
| `test/util/data/txcreate1.json` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `9acf7d045d27` |
| `test/util/data/txcreate2.hex` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `60df0e8d3383` |
| `test/util/data/txcreate2.json` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `4a9d5294aeff` |
| `test/util/data/txcreatedata1.hex` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `e8d46d90aa6c` |
| `test/util/data/txcreatedata1.json` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `4339b05f3481` |
| `test/util/data/txcreatedata2.hex` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `8abf8414f2e6` |
| `test/util/data/txcreatedata2.json` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `a0d773463d95` |
| `test/util/data/txcreatedata_seq0.hex` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `81f76dc2afe6` |
| `test/util/data/txcreatedata_seq0.json` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `b079e8bfa5b2` |
| `test/util/data/txcreatedata_seq1.hex` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `806c169c32a8` |
| `test/util/data/txcreatedata_seq1.json` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `f287bb90dec6` |
| `test/util/data/txcreatemultisig1.hex` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `31ac8c0b0e92` |
| `test/util/data/txcreatemultisig1.json` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `95b5157ac609` |
| `test/util/data/txcreatemultisig2.hex` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `5e7c4ca24924` |
| `test/util/data/txcreatemultisig2.json` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `6db4a4dc32c8` |
| `test/util/data/txcreateoutpubkey1.hex` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `6c5b9d3b4156` |
| `test/util/data/txcreateoutpubkey1.json` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `3d63a413b025` |
| `test/util/data/txcreatescript1.hex` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `0d13f84b55f6` |
| `test/util/data/txcreatescript1.json` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `63f38aacfd59` |
| `test/util/data/txcreatescript2.hex` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `40fc7942453d` |
| `test/util/data/txcreatescript2.json` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `f046ada82fd9` |
| `test/util/data/txcreatescript5.hex` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `3689793a2d8b` |
| `test/util/data/txcreatescript6.hex` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `b4cb053f3e21` |
| `test/util/data/txcreatesignv1.hex` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `7db872640d54` |
| `test/util/data/txcreatesignv1.json` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `63c6a001f180` |
| `test/util/data/txcreatesignv2.hex` | `dash-tx` golden file (`bitcoin-util-test.json`) | input/output of one `dash-tx` case | `c8de55845d4b` |

## 2. Generated by Core: `transcribed/`

Vectors that Core's C++ unit tests carry inside the source (not as JSON), and results Core computes
inside those tests. They are recorded **by Core's own tests** while they run, never copied by hand:
the commits on the local Dash Core branch `node/o4-core-vectors` (`8378e042d0`, `c89375bdbf`)
and, for `evo_snapshot_tests.json`, `1f590c8cfb` on `node/o2a-ev-core-vectors` (node program
task O2a-EV; that branch carries all three, and the same changes are in `instrumentation/o4-core.patch`, which applies to the commit above) add
`src/test/util/o4_dump.h` and a recording call next to the existing checks. With `O4_VECTOR_OUT` set
every instrumented test also writes what Core computed at that check; without it nothing changes,
and every original assertion still runs (the suites pass: 66 test cases with the audit, plus `evo_snapshot_tests`).

Regenerate with `contrib/node/regen-core-vectors.sh transcribed <test_dash>`; the output is
deterministic (checked by running it twice). Each file is
`{"source": "src/test/<name>.cpp", "records": [one JSON object per line]}`; `SHA256SUMS` pins them.

| File | Core test | Records |
|---|---|---|
| `pow_tests.json` | `pow_tests.cpp` `get_next_work` | the 24 real mainnet `(nTime, nBits)` headers ending at height 123456 and 6 DGW results (mainnet, devnet slow-block rules): expected `nBits` from the test, `core_bits` computed by `GetNextWorkRequired` |
| `subsidy_tests.json` | `subsidy_tests.cpp` | 12 `(prev_bits, prev_height, v20_active) -> subsidy` points (the research notes said 11) |
| `evo_simplifiedmns_tests.json` | `evo_simplifiedmns_tests.cpp` `simplifiedmns_merkleroots` | the 15 synthetic SML entries with their `SER_NETWORK` wire form, `CalcHash` and the merkle root `0bae2176…4022` (legacy BLS scheme) |
| `evo_islock_tests.json` | `evo_islock_tests.cpp` `deserialize_instantlock_from_realdata2` | a real mainnet ISDLOCK: wire hex, parsed fields, request id and `llmq::SignHash` computed by Core |
| `dip14_tests.json` | `dip14_tests.cpp` | 4 DIP-14 256-bit derivation paths from the published seed, with the derived key and chain code |
| `quorum_proofs_tests.json` | `quorum_proofs_tests.cpp` `real_testnet_wire_and_crypto` | the testnet archive proof (1548500 -> 1549547) and the height/hash Core verifies it to |
| `dynamic_activation_thresholds_tests.json` | `dynamic_activation_thresholds_tests.cpp` | the versionbits threshold Core reports for each of 13 periods of `testdummy:0:999999999999:0:100:80:60:5:0` |
| `block_reward_reallocation_tests.json` | `block_reward_reallocation_tests.cpp` | 71 checkpoints from the BRR/MN_RR regtest chain: subsidy, superblock subsidy, the gross masternode payment (`GetMasternodePayment`; in era 2 the template splits it into a Core and a platform output) and the block template's coinbase value and masternode outputs, recomputed from Core's functions (era 0/1/2 = `Classic`/`CreditPool`/`EvoReward`) |
| `dip0020opcodes_tests.json` | `dip0020opcodes_tests.cpp` | 4,512 `EvalScript` runs (script, stack, flags, result, error name, final stack) |
| `checkdatasig_tests.json` | `checkdatasig_tests.cpp` | 214 `EvalScript` runs of `OP_CHECKDATASIG(VERIFY)`, including the positive signatures that `script_tests.json` lacks |
| `harness_audit.json` | `o4_harness_audit_tests.cpp` | the flag-perturbation audit, section 4 |
| `evo_snapshot_tests.json` | `evo_snapshot_tests.cpp` | the masternode lists the tests build (`list`: evodb bytes, `SerializeCanonicalMNList` bytes, `CanonicalMNListHash` in internal byte order), the 9 byte strings they give `UnserializeCanonicalMNList` (`read`: Core's verdict and its `ios_base::failure` message, or re-encoding and hash), one `BuildDiff` in the canonical diff encoding (`diff`) and one `CDeterministicMNStateDiff` (`state_diff`). The tests run on mainnet parameters, whose default port the reader's netinfo check uses |

**From a functional test (no build):** `bip39_functional.json` is not recorded by a C++ test. It
holds the literals of `test/functional/wallet_upgradetohd.py:234-238, 258-267`, read from
`git show 0364ed626ec2:test/functional/wallet_upgradetohd.py` by
`contrib/node/upgradetohd-vectors.py` (`regen-core-vectors.sh functional <dash checkout>`), which
fails unless every pattern matches exactly one line. Each record gives the phrase, the passphrase
and the address the wallet owns after `upgradetohd` (the test pays it first and asserts the
balance), or, for the NUL-terminated passphrase, the two addresses it owns neither of (balance 0).
`lines` are the source lines of the phrase, the address and the `upgradetohd` call.

| File | Core test | Records |
|---|---|---|
| `bip39_functional.json` | `wallet_upgradetohd.py` | 3: phrase without and with `custom-passphrase` and the address each owns; `custom-passphrase\0` owns neither |

**Selection rules (they only drop records, never change one):**

- `dip0020opcodes_tests.json`: the test sweeps every operand width from 1 to 521 bytes (95k
  evaluations, 130 MB). Kept: deduplicated runs whose script and operands are at most 33 bytes
  (covers the 4-byte `CScriptNum` limit and the 8- and 32-byte widths), plus, at the element-size
  limit (520 and 521 bytes), six per (final opcode, outcome). See `KeepDip0020` in `o4_dump.h`.
- `checkdatasig_tests.json`: the test draws 4,096 random flag sets. A record is kept once per
  `(script, stack, flags & (STRICTENC | DERSIG | LOW_S | NULLFAIL))`, and the instrumented test
  *fails* if two flag sets that are equal under that mask ever produced different outcomes, so
  the mask provably loses nothing the test exercises.
- `block_reward_reallocation_tests.json` and the others record every checkpoint, unfiltered.

**Dash-specific facts the transcription surfaced:**

- `SCRIPT_ERR_IMPOSSIBLE_ENCODING` (`OP_NUM2BIN`/`OP_BIN2NUM`) occurs in the DIP0020 runs, but the
  error-name table of `script_tests.cpp` has no name for it, so `script_tests.json` cannot express
  it; only these records cover it.
- The Core unit tests have 12 subsidy points, not 11.
- The `state_diff` record diffs two in-memory ProTx version 3 states that differ in
  `scriptPayout`, which version 3 states do not serialize: Core's diff carries that field, a diff
  of the two serialized states cannot (`dash-oracle-sys/tests/mnlist.rs` checks both).

## 3. Loader semantics (crate `dash-core-vectors`)

The loaders reproduce the row rules of the Core test that consumes each file (`src/cases.rs`):

- `ParseScript` (`core_read.cpp`): whitespace is space, tab and newline only; decimal numbers
  in -0xFFFFFFFF..0xFFFFFFFF pushed like `CScript << int64_t`; `0x` raw bytes (not pushed);
  single-quoted strings pushed as data; opcode names with or without `OP_`, for `OP_RESERVED` and
  `OP_NOP..OP_CHECKDATASIGVERIFY` only (`OP_0`, `OP_1NEGATE`, `NOP2`/`NOP3` are not names).
- `script_tests.json`: a one-element row is a comment, other rows with fewer than four elements
  are bad tests, extra elements are ignored; flag and error names must be known.
- `tx_valid.json`/`tx_invalid.json`: a row whose first element is not an array is a comment; the
  prevout map is last-writer-wins; an unknown prevout is a bad test; the third column of
  `tx_valid` lists *excluded* flags; `BADTX` is a `CheckTransaction` failure; the transaction
  need not consume the whole stream.
- `sighash.json`: one-element rows are comments; hash types are full signed 32-bit integers;
  `SignatureHash` returns `uint256::ONE` for an input index out of range.
- `trivially_*.json`: row 0 is the header; the scheme column selects the chain tip, the harness
  runs each row with both global BLS-scheme settings.
- `hex`/`uint256` helpers follow `util/strencodings` and `uint256::SetHex` (whitespace skipping,
  empty vector on bad input, truncation and zero padding).
- A small `UniValue` reproduces number-text preservation and key order, which the governance
  proposal harness depends on (it hex-encodes `write()` output).

### Harness quirks that are *not* reproduced

Core's flag-perturbation loops (a valid vector must stay valid when flags are removed, an invalid
one invalid when flags are added) have defects (`flags.rs`):

1. They perturb `1 << i` for `i < 13`: bits 0..=12. The defined flags are bits 0..=10, 14
   (`NULLFAIL`) and 16 (`CONST_SCRIPTCODE`), so those two are never perturbed, and bits 11 and 12
   (undefined) are. `script_tests.cpp` draws 16 random bits, which reaches the retired bit 15
   but not bit 16.
2. `ExcludeIndividualFlags` tests `find(..) != end()` where `== end()` is meant, so it returns
   nothing and the "flags are maximal / minimal" checks never run. Its intent also mixes up flag
   spaces for `tx_valid`, where the argument is the excluded set but `CLEANSTACK` needing `P2SH`
   is a rule about enabled flags; `restore_one_excluded` works in enabled-flag space.
3. `tx_valid` passes `~verify_flags`, all 32 bits including undefined ones, to the verifier.

The loaders perturb every defined flag (`removal_sets`, `addition_sets`), mask to defined flags,
and provide the intended `exclude_individual_flags`.

## 4. Evidence: the harness audit

`o4_harness_audit_tests.cpp` runs the corrected checks with Core's own interpreter
(`transcribed/harness_audit.json`; 13 single-flag perturbations and 128 fixed pseudo-random flag
sets per vector, plus `exclude_individual_flags`):

| File | Vectors | Checks | Violations |
|---|---|---|---|
| `script_tests.json` | 1,282 | 180,762 | **0** (including `NULLFAIL` and `CONST_SCRIPTCODE`) |
| `tx_valid.json` | 83 | 11,874 | **0** (the repaired maximality check holds) |
| `tx_invalid.json` | 66 (+10 `BADTX`) | 9,493 | 64, all of one kind |

The 64 violations are all of the repaired *minimality* check ("dropping a listed flag makes the
transaction pass"), on 61 distinct vectors: 9 vectors list one flag and fail with no flags at all;
for 52 more, the set left after dropping `P2SH` (listed redundantly next to the flag the
transaction really depends on) still fails; 3 are other flags. So repairing
`ExcludeIndividualFlags` as written would make Core's own `tx_invalid` test fail: minimality is not
an invariant of that file. The loaders promise the monotonicity checks for `tx_invalid` and not
minimality; `tx_valid` gets both (`restore_one_excluded`). `load_tx_valid` also applies Core's
"Bad test flags" rule (a test may not exclude `P2SH` while leaving `CLEANSTACK` on).

## 5. Checks that tie the data together

- `tests/core_files.rs`: checksums, counts and loading of every verbatim file.
- `tests/transcribed.rs`: independent cross-checks of the generated files: threshold formula, SML
  entry hashes and merkle root recomputed from the wire form with Core's merkle rule, ISDLOCK
  request id and sign hash from their definitions, the C++ quorum proof equal to the functional-test
  copy, the audit numbers above.
- `dash/tests/core_vectors.rs` runs `sighash.json`, the base58 decode vectors and every
  transaction of the tx and trivially-valid files through `dashcore`. Known gaps are pinned there:
  169 of the 500 sighash vectors differ because `legacy_signature_hash` documents that it does not
  support `OP_CODESEPARATOR` (BL-08; the other 331 are exact, and the pin is exactly 169). The
  test's script-code helper is a port of Core's `SerializeScriptCode`, including its behaviour for
  a truncated trailing push (Core's length prefix counts bytes it does not write); it is test
  scaffolding, and a fix of `legacy_signature_hash` has to follow Core, not the helper's shape.
  Every row of `trivially_valid.json` and `trivially_invalid.json` decodes, with a typed payload
  exactly where Core extracts one (all but the `bad-protx-payload` row), including the two Evo
  ProRegTx rows of `trivially_invalid.json` (rows 1 and 2): Core reads the platform node id and
  ports for every payload with `nType == Evo` and rejects version 1 later
  (`bad-protx-evo-version`). Before `node/v24a-dashcore-wire`, `dashcore` read them only for
  `version >= 2` and failed to decode those rows.
- `dash/tests/core_vectors.rs` also decodes the 15 transcribed SML entries and the ISDLOCK, and
  checks `calculate_entry_hash`, `request_id` and `sign_id` against Core's values.

Not covered by O4, so that nobody assumes it is: the dashbls Chia/Legacy vectors (BS12), the
`feature_utxo_set_hash` golden hashes (functional tests, IX1) and `invalid_txs.py` (not data).
