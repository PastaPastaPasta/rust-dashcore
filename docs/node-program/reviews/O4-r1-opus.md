# O4 review, round 1 (Opus)

- **Branch:** `node/o4-core-vectors` at `ac11ac2f` (worktree `~/workspace/rdc-wt/o4-core-vectors`), 5 commits `89bd2708..ac11ac2f`, `git diff node/integration...HEAD`.
- **Core side:** `~/workspace/dash-wt/o4-core-vectors`, commits `8378e042d0` and `c89375bdbf` on `v24.0.0-rc.3` (`0364ed626ec2`).
- **Scope:** I wrote nothing to either branch or worktree. Scratch code is in `/tmp/o4-review-scratch` (a separate cargo project with its own target dir), and Core outputs are in `/tmp/o4-review-tr` and `/tmp/o4-review-g`.

**Summary:**
- Provenance is as strong as it can be without rebuilding Core.
  - All 61 verbatim files match `git show 0364ed626ec2:<path>` byte for byte.
  - The shipped `o4-core.patch` rebuilds exactly the tree of the Core branch.
  - Re-running the existing instrumented `test_dash` reproduces **all 11 transcribed files and both golden files byte for byte**.
  - The recorder cannot change test behaviour.
- The loaders reproduce Core's harness correctly, including the `ExcludeIndividualFlags` inversion, the 13-bit loops and `ParseScript`/`SetHex`/`ParseHex` edge cases.
- "Monotonicity only" for `tx_invalid` is the right call. Core's data violates minimality, and Core never checked it.
- The F12 replacement is correct. I additionally verified that **all 32 incremental `mnlistdiff` captures** produce a list whose root equals their own coinbase `merkleRootMNList` (the branch only checks 3).
- The qrinfo tip splice is encoding-consistent and clearly marked.
- Both dashcore-bug claims hold. The ProRegTx one has an unstated root cause that is not in BACKLOG.
- No blockers or majors. A few minors are about making the ground truth harder to misuse.

**Verdict: APPROVE-WITH-NITS.**

## What I ran

| Check | Result |
|---|---|
| `cargo test -p dash-core-vectors` | 20 unit + 6 `core_files` + 10 `transcribed` pass |
| `cargo test -p dashcore --test core_vectors --test core_mnlistdiff_ground_truth` | 4 + 5 pass |
| `cargo test -p dashcore` (all targets) | 506 unit + integration pass |
| `cargo test -p dash-spv --test core_fixtures` / `--lib` | 2 (+1 ignored `regenerate`) / 625 pass |
| `cargo fmt --check`; `cargo clippy -p dashcore -p dash-spv --all-features --all-targets -D warnings`; `cargo clippy -p dash-core-vectors --all-targets -D warnings` | clean (a default-features clippy error in `hash_types.rs:357` is pre-existing, not O4) |
| FILES.sha256: each entry vs `git -C ~/workspace/dash show 0364ed626ec2:<path>` and vs the checked-in file | 61/61 identical. Of Core's files in `src/test/data`, `test/functional/data` and `test/util/data`, the only ones not copied are `__init__.py`, `invalid_txs.py` and `non-backported.txt` (as documented) |
| `git apply --cached o4-core.patch` onto a temp index of `0364ed626ec2` | tree `f5d20909…` = `HEAD^{tree}` of the Core branch |
| Existing `build/src/test/test_dash` with `O4_VECTOR_OUT` (the 11 suites) | output sha256 = `transcribed/SHA256SUMS`, all 11 identical |
| Same binary, `o4_vector_dump_tests` with the regen script's manifests | both golden JSONs `cmp`-identical |
| Same binary, without `O4_*`: the instrumented suites plus `o4_*`, `transaction_tests`, `script_tests`, `sighash_tests` | "No errors detected" |
| Scratch: each `mainnet-*` capture applied on top of its base list (`from_diff(0→2227096)` then a chain via `apply_diff`), root vs `cbtx_merkle_root_mn_list` | **32/32 match**. Only the 2518986 tip diff can't be checked, because its base list is not in the set |
| Scratch: transcribed SML entries and ISDLOCK through dashcore | 15/15 entry decode + re-encode + `CalcHash` exact. ISDLOCK round-trips, and `request_id`/`sign_id` match Core |
| Standalone 70240 `mnlistdiff` captures vs the 70235 `qrinfo_2240504.bin` | all 5 (`2227096→{2239192…2240344}`) are byte-identical substrings of the qrinfo, so the splice mixes no encodings |
| Old vs new fixture bytes | `mn_list_diff_0_2227096`: 5600 bytes `00→ff` (IPv4-compatible to IPv4-mapped). `2227096_2241332`: 32 bytes `ff→00` (16 null services). Testnet: 62 bytes `ff→00` (31 null services) |
| `sighash.json` rows that contain an `OP_CODESEPARATOR` *opcode* (independent Python parse) | 169 / 500, matching the claim |

## Check-by-check

### 1. Provenance

- **Verbatim data.** Every file is unmodified (table above).
  - `sync-core-vectors.sh` uses `git show <commit>:<path>`, so a dirty tree cannot leak in.
  - `tests/core_files.rs` pins the checksums and the count (61).
- **Recorder is inert and cannot alter behaviour.**
  - `O4_RECORD` evaluates its argument only when `O4_VECTOR_OUT` is set. With it set, the extra calls are pure: `GetNextWorkRequired` on a fresh header, `GetBlockSubsidyInner`, `GetSuperblockSubsidyInner`, `GetMasternodePayment`, `CalcHash`, serialization.
  - The two unconditional blocks (`evo_islock_tests.cpp`, `pow_tests.cpp`) only build a `UniValue` from values that are already computed.
  - The only behavioural addition is the `checkdatasig` `BOOST_CHECK_MESSAGE` that proves the flag mask is lossless. It runs only when enabled and can only add failures.
  - Every original assertion is untouched. The audit and dump suites `return` early without `O4_*`.
- **Values the patch duplicates by hand** (they are the test's own literals, and Core recomputes them next to each):
  - the `expected_bits` literals in `pow_tests.cpp` (each paired with a Core-computed `core_bits`; `pow_and_subsidy_records_are_complete` asserts equality);
  - the DIP-14 seed string (`dip14_tests.cpp:82`, equal to `MasterKey()`'s seed);
  - the `vbparams` string.
  
  See N1.
- **Captures.**
  - The bytes come from public peers, not the local oracle (documented; BL-09 tracks the re-check).
  - Authenticity rests on several independent facts:
    - 2–3 independent peers return identical bytes;
    - Core re-serializes each capture exactly, so no non-canonical encodings;
    - entry and commitment hashes come from Core;
    - list roots equal the coinbase `merkleRootMNList` for every diff I could chain (32/32 plus the testnet head);
    - block hashes were checked against Core's header index.
  - Together that is sufficient.

### 2. Loader semantics vs Core's harness

- **`ParseScript`** (`script.rs`) matches `core_read.cpp:63-103`:
  - separators ` \t\n`;
  - `ToIntegral<int64_t>` with the ±0xFFFFFFFF limit;
  - `0x`+`IsHex` inserted raw;
  - quoted strings use the smallest push;
  - the opcode map is built from `GetOpName` for `OP_RESERVED` and `OP_NOP..MAX_OPCODE=0xbb`. I diffed the name list against Dash's `GetOpName`: identical, and 0xb1/0xb2 are CLTV/CSV with no NOP2/NOP3.
- **`ParseHex`/`TryParseHex`/`uint256::SetHex`** (`hex.rs`) match `strencodings` and `uint256.cpp` (whitespace skipping, odd-digit tail, truncation, zero padding).
- **Flags** (`flags.rs`):
  - The bit values match `interpreter.h` (NULLFAIL 14, retired 15, CONST_SCRIPTCODE 16).
  - The `mapFlagNames` set matches `transaction_tests.cpp:39-53`.
  - `TrimFlags`/`FillFlags` are exact.
- **The documented defects** are all real (`transaction_tests.cpp`, at `0364ed626ec2`):
  - `ExcludeIndividualFlags` uses `find(...) != end()` on an empty vector, so it is a no-op.
  - Both perturbation loops use `1U << i, i < mapFlagNames.size()` (bits 0..12), and the random draws use 13 bits.
  - `script_tests.cpp:DoTest` draws `InsecureRandBits(16)`.
  - `tx_valid` passes all 32 bits of `~verify_flags`.
  
  The loaders don't copy any of them, and the module docs say so.
- **Row rules** match Core:
  - comment rows;
  - `size() < 4`;
  - `test[0].isArray()`;
  - `size() != 3`;
  - last-writer-wins prevouts;
  - `uint32_t(getInt<int>())`;
  - `BADTX`;
  - the "Bad test flags" rule on `tx_valid`;
  - stream not fully consumed;
  - sighash `size()==1` comments and signed hash types;
  - `trivially_*` header row and scheme selection;
  - `key_io` `tryCaseFlip` default.
  
  Where Core reports `BOOST_ERROR` and continues, the loaders fail the whole file. That is stricter, and fine.
- **"Monotonicity only" for `tx_invalid`: correct.** Core never ran the minimality check, because it is a no-op. I spot-checked `harness_audit.json` against the file:
  - 9 rows list `P2SH` alone but fail with `NONE`;
  - 52 `P2SH,X` rows still fail after dropping `P2SH`;
  - 3 `P2SH,X` rows fail with `P2SH` alone (rows 59, 84, 89).
  
  So minimality is not an invariant of Dash's file (Bitcoin cleaned its copy; Dash did not backport that data change). Promising it would make the loader assert something Core's own data contradicts.
  
  For `tx_valid`, the maximality check Core never ran was audited: 0 violations over 11,874 checks. Promising it is backed by evidence.
  
  See m4 for a way to keep the minimality *verdicts* as data.

### 3. F12

- The old fixtures were Rust re-encodings, and the byte diff confirms exactly the documented field changes.
- The new captures are verified (above).
- The `.dat` files are now `bincode(decode(core wire))`, and `core_fixtures.rs` enforces that.
- **The synthetic splice** (`qrinfo_*.dat`: Core `qrinfo` with `mnListDiffTip` replaced by a Core `mnlistdiff`) is clearly marked in three places (`test_DML_diffs/PROVENANCE.md`, `core_fixtures.rs` module docs, `capture_fixtures.py`) and is technically sound:
  - The tip claim is correct. `snapshot.cpp:127` uses `ActiveChain().Tip()`.
  - The replacement bases match Core's construction:
    - legacy: `baseBlockIndexes.back()` = 2227096;
    - efficient: `GetLastBaseBlockHash(.., tip)` = H's work block `…1970a8…`, which is the live capture's own tip-diff base in the golden.
  - The 70240-captured diff is byte-identical to the diffs inside the 70235 qrinfo.
  - Note that there are **two** spliced files, not one. `qrinfo_2518986.dat` is a splice too (byte-identical to the previous file). It is acceptable and documented.
- `entry_hash_matches_core_calc_hash` now uses Core-computed hashes and pins the fixture sha256. The old constants were hashes of the corrupted encoding.

### 4. The dashcore-bug claims

- **OP_CODESEPARATOR: confirmed.**
  - `SighashCache::legacy_signature_hash` documents (`dash/src/crypto/sighash.rs:748`) that it does not strip `OP_CODESEPARATOR`, while Core's `SerializeScriptCode` does.
  - 169 rows contain the opcode, and with Core-style stripping all 500 match.
  - BL-08 tracks it.
- **Two ProRegTx rows undecodable: confirmed, root cause unstated** (m1).
  - Rows 1/2 of `trivially_invalid.json` are the same tx: ProRegTx payload **version 1, nType 1 (Evo)**, 298 bytes.
  - Core reads `platformNodeID` (and the ports for version < ExtAddr) whenever `nType == Evo` (`providertx.h`, `SERIALIZE_METHODS(CProRegTx)`), then rejects it in `IsTriviallyValid` with `bad-protx-evo-version`.
  - dashcore reads the platform fields only `if version >= 2 && HighPerformance` (`provider_registration.rs:271`). It mis-parses the platform node id as the signature length (0x49), leaves 16 bytes, and fails the decode.

### 5. Test quality and tracking of known failures

- Both gaps flip a test when they are fixed:
  - sighash: `assert!(unstripped_mismatches > 0)`;
  - ProRegTx: exact `[1, 2]`.
- V24a already gates on type alone (`node/v24a-dashcore-wire:provider_registration.rs:545`). Whichever branch merges second must update `UNDECODABLE_INVALID_ROWS` to `[]`. That is the intended signal.
- Cross-checks in `transcribed.rs` are independent of the producing run:
  - threshold formula;
  - SML hashes and root from the wire form;
  - ISLOCK ids from their definitions;
  - quorum proof equal to the functional-test copy;
  - audit counts.

### 6. Licensing

- `test-vectors/core/<commit>/COPYING` is Core's MIT text with the Bitcoin/Dash notices.
- `dash-core-vectors` is `license = "MIT"`, overriding the workspace's CC0, and `publish = false`. The README states the licence.
- The Core patch files carry Core's MIT header.
- The fixtures under `dash/tests/data` are chain/wire data, not ported code.
- Consistent with DEC-07.

## Findings

### m1 — MINOR: the ProRegTx gap is described by its symptom, and is missing from BACKLOG

- **Where:**
  - `dash/tests/core_vectors.rs:121-126`;
  - `test-vectors/core/0364ed626ec2/PROVENANCE.md:213`;
  - `BACKLOG.md` (only BL-08 exists).
- **Scenario:**
  - The comment says the rows have "a payload version that does not fit their BLS scheme" and that dashcore "rejects them while decoding".
  - The actual defect is a decode-domain divergence: `provider_registration.rs:271` gates the Evo platform fields on `version >= 2`, while Core gates them on `nType == Evo` only.
  - Any v1 Evo ProRegTx (which Core decodes, then rejects with `bad-protx-evo-version`) is undecodable in dashcore. A node would produce a different reject path and a different DoS/ban decision for such a tx.
  - This is the class the program tracks in "base-sdk decode-domain divergences".
- **Fix:**
  - State the root cause in both places.
  - Add a BACKLOG entry (or note that V24a fixes it, at `provider_registration.rs:545` on `node/v24a-dashcore-wire`).
  - The manager should expect the `[1, 2]` pin to flip when V24a and O4 are both on `node/integration`.

### m2 — MINOR: `strip_code_separators` is not Core's `SerializeScriptCode` for malformed scripts

- **Where:** `dash/tests/core_vectors.rs:23-38` (`Err(_) => break` then copy to the end).
- **Scenario:**
  - Core's `SerializeScriptCode` writes the length `size - nCodeSeparators` but only the bytes up to where `GetOp` failed.
  - `GetScriptOp` has advanced past the opcode and size bytes, but not past the data, so a truncated trailing push is dropped from the pre-image while the length prefix still counts it.
  - The helper copies the whole tail.
  - It is fine for `sighash.json`, whose generator never emits malformed pushes (all 500 match). But the doc comment says it *is* Core's function, and the test tells the BL-08 fixer to drop the stripping once dashcore handles it.
  - A fix modelled on this helper would diverge from Core in a sighash differential and fuzz (O1/L1), and in which error a `CHECKSIGVERIFY`-before-bad-push script reports.
- **Fix:**
  - Mirror Core exactly (stop at the failed op and keep Core's length prefix), or say in the comment that it is not exact for malformed scripts.
  - Note the quirk in BL-08.

### m3 — MINOR: the golden producer root-checks only 3 of the 35 diffs, and covers no qrinfo snapshot fields

- **Where:**
  - `contrib/node/regen-core-vectors.sh:67` (`"checkRoot": False` for all 32 `core_wire` diffs);
  - `o4_vector_dump_tests.cpp` `o4_dump_qrinfo_ground_truth` (records no `CQuorumSnapshot` data);
  - `dash/tests/core_mnlistdiff_ground_truth.rs:106`, whose doc says the test checks "the three snapshots Core parsed" though it compares none.
- **Scenario:**
  - The root check against the coinbase is the strongest Core-side authenticity proof of a capture. 32 of 33 incremental diffs can be chained from `0→2227096` (I did it in Rust: 32/32 match), but the ground truth does not record it.
  - Snapshot semantics (`skipListMode`, `activeQuorumMembers` bit order, `mnSkipList`) are covered only by byte round-trip, not by Core's field-level reading. A dashcore decoder that consumed the right byte count with a wrong bit order would pass.
- **Fix:**
  - Give the 31 chainable captures a `group` per base chain in the manifest (base 2227096, and the 2239480→… chain) so Core records `rootsMatch`.
  - Dump each `CQuorumSnapshot` (mode, members as a bit string, skip list) and compare in Rust.
  - Fix the doc comment in the meantime.

### m4 — MINOR (enhancement): keep the `tx_invalid` minimality *verdicts* as Core-produced data

- **Where:** `test-vectors/src/flags.rs` (`exclude_individual_flags`), `transcribed/harness_audit.json`.
- **Scenario:**
  - Dropping minimality as an invariant is right. But the audit already holds Core's verdict for every `exclude_individual_flags(listed)` combination: pass, except the 64 recorded `(row, flags)` failures.
  - Those are exact Core outcomes for ~150 extra flag combinations, which a Rust interpreter could be required to reproduce. Right now nothing exposes them.
- **Fix:**
  - Add a helper that yields `(row, flags, core_passes)` for those combinations from the audit.
  - Have the future interpreter test assert equality with Core rather than an invariant.

### N1 — NIT: values duplicated by hand inside the recorder

- **Where:**
  - `dip14_tests.cpp:82` (seed literal, duplicating `MasterKey()`);
  - `pow_tests.cpp` `o4_record(..., 0x1b1441deU)` and the others (duplicating the `BOOST_CHECK_EQUAL` literals);
  - the `vbparams` string in `dynamic_activation_thresholds_tests.cpp`.
- **Fix:** read the seed and params from the test's own constants. For pow, the equality check in `transcribed.rs` already guards against a copy error, so this is cosmetic.

### N2 — NIT: the sighash gap is pinned as "> 0", not as 169

- **Where:** `dash/tests/core_vectors.rs:84-87`.
- **Fix:** `assert_eq!(unstripped_mismatches, 169)`, so that a partial change in `legacy_signature_hash` is noticed too.

### N3 — NIT: the audit samples flag space instead of covering it

- **Where:** `o4_harness_audit_tests.cpp:132` (`RANDOM_SETS = 128`).
- **Fix:** with 13 defined bits there are 8,192 subsets per vector, so an exhaustive audit is about 12M `VerifyScript` calls (minutes). That would make the loaders' monotonicity promise proven rather than sampled.

### N4 — NIT: the `tx_invalid` loader does not mirror Core's `CLEANSTACK`-needs-`P2SH` assert

- **Where:** `test-vectors/src/cases.rs:250-262`.
- **Scenario:** Core verifies `tx_invalid` with the listed flags as-is ("to detect invalid verifyFlags combination"). `VerifyScript` asserts if `CLEANSTACK` is set without `P2SH`. No current row does this (I checked), so the loader should report such a row as a bad test, just as it does for `tx_valid`.

### N5 — NIT: documentation accuracy

- `PROVENANCE.md:14`: `trivially_invalid.json` changed **one** line between v23.1.8 and the pin. It is a reject string, `gettxpayload-fail` → `bad-protx-payload`, and that is worth naming because the mainnet pin is v23.1.8. `invalid_txs.py` also differs, though it is not copied.
- `test_DML_diffs/PROVENANCE.md:18`: the null-service fix also applies to the testnet artifact (31 entries), not only the "second file".
- "One synthetic splice": there are two (`qrinfo_2240504.dat`, `qrinfo_2518986.dat`). The PROVENANCE says so correctly; only the task summary undercounts.
- Name what R4 V1 lists but O4 leaves to others, so nobody assumes coverage:
  - dashbls Chia/Legacy vectors → BS12;
  - `feature_utxo_set_hash` golden hashes → IX1 functional tests;
  - `invalid_txs.py` → not data.

### N6 — NIT: cheap extra consumers

- dashcore already passes the transcribed SML entries (15/15) and the ISDLOCK (`request_id`, `sign_id`, round trip). I checked both in scratch.
- Adding them to `dash/tests/core_vectors.rs` turns two more transcribed files into live L0 gates now, instead of waiting for later tasks.

## Verdict

**APPROVE-WITH-NITS.**

- No blockers or majors.
- The data is provably Core's: verbatim checksums, a patch that rebuilds the Core tree, and byte-identical regeneration of every transcribed and golden file.
- The loaders reproduce Core's harness without its three defects.
- The tx_invalid decision is correct and evidenced.
- The F12 replacement is sound.
- **Should be addressed before consensus tasks start consuming these vectors:**
  - m1 (state the ProRegTx root cause and track it);
  - m2 (don't let the sighash helper become the template for BL-08).
- **Can follow:** m3, m4 and the nits.
