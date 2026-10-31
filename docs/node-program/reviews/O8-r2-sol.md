# O8 StateDigest v1 — independent review, round 2

2026-10-12. Narrow review of rust-dashcore `8e6e8453d..07e7af7f3` and Dash Core `0a36fab32c..02118350e1`. Work was done in private detached copies under `/work/scratch/o8-sol-r2`; the implementer worktrees, branches, and GitHub were untouched.

## Findings

1. **Medium — possible security note: see manager.**
2. **Medium — possible security note: see manager.**

No other Medium or higher defect was found in the reviewed delta. The 02118350e1 superblock-payments move executes argument construction inside `QueryBuilt` (`src/governance/superblock.cpp:629-639`, `src/statedigest.h:220-228`). The new tap wrapper and post-body guard are at `src/statedigest.h:97-108,151-164`; the corresponding Core tests are at `src/test/statedigest_tests.cpp:84-152`.

## Round 1 closure

| R1 finding | Judgment and evidence |
|---|---|
| 1. Missing MN-list hash suppressed checks | **Closed.** Rust `conformance/src/digest/extract/checks.rs:53-64,139-176` derives the era independently and rejects a missing in-era value; Core's producer remains `src/validation.cpp:2716`. The output-equivalent Core mutant was refused at digest line 6. |
| 2. Header fields copied unchecked | **Closed.** Rust `conformance/src/digest/extract/checks.rs:217-250` checks raw header bytes before `conformance/src/digest/extract.rs:774-784` writes the golden; Core encodes the fields at `src/statedigest.cpp:183-188`. The output-equivalent Core mutant was refused at digest line 2. |
| 3. Incomplete storage accepted | **Closed.** Rust `contrib/node/statedigest/record-ft.py:150-160` writes a node manifest; `contrib/node/statedigest/store-vectors.sh:69-120` checks the manifest, exact file hashes, counts and `statedigest-check`; `dash-statedigest/src/recording.rs:147-216,244-329` checks joins and the tip suffix. Core writes the `H`, `C` and `A` material at `src/statedigest.cpp:71-105,161-189`. Both R1 storage fixtures were refused. |
| 4. Early effect taps | **Partially closed.** Core `src/evo/chainhelper.cpp:94-101`, `src/llmq/snapshot.cpp:321-335`, `src/instantsend/db.cpp:379-385`; Rust `dash-statedigest/src/recording.rs:61-93` validates the recorded file structure. **possible security note: see manager.** |
| 5. Post-connect construction outside guard | **Closed for the reported path.** Core `src/validation.cpp:2501-2511`, `src/statedigest.h:151-164`, with fault tests at `src/test/statedigest_tests.cpp:84-152`; Rust's recording join at `dash-statedigest/src/recording.rs:147-216` rejects incomplete attempts. |
| 6. Flags-off allocation claim | **Closed for the reported path.** Core `src/statedigest.cpp:109-114`, `src/validation.cpp:2283-2295,2486-2490`, and `src/test/statedigest_tests.cpp:65-82`. Rust's `dash-statedigest/src/recording.rs:19-27` remains a pure, byte-taking checker. |

## Mutation probes

| Probe | Result |
|---|---|
| R1 `TapNewList` removed (219 affected `C` lines) | **Caught:** `error: digest line 6, block 2: s.mnl is null at height 2, inside its era`; no golden written. |
| R1 `bits`/`nonce` swapped (221 affected `C` lines) | **Caught:** `error: digest line 2, block 0: s.bits is Some(Int(1096447)), but the block says Int(545259519)`; no golden written. |
| R1 node directory removed | **Caught:** `manifest.json records ['node0', 'node1'] but rec/ has ['node0']`; store exit 1. |
| R1 digest truncated to `H` | **Caught:** `a digest file other than the one extracted`; store exit 1. The unit fixture also confirms `statedigest-check` refuses it if the hash is adjusted. |
| Own source mutant: disable the blocks.dat hash check | **Caught:** `test_a_blocks_dat_other_than_the_one_extracted_is_refused` failed (`FAILED (failures=1)`). The unmutated script rejects a one-byte truncation with `a blocks.dat other than the one extracted`. |
| Own source mutant: disable BIP9 unchecked-height coverage | **Caught:** `test_bip9_heights_left_unchecked_are_refused` failed (`FAILED (failures=1)`). The unmutated script rejects the fixture with `BIP9 heights left unchecked`. |
| Own real-rig delta probe | **possible security note: see manager.** |

The first two R1 probes replayed the exact `C`-line changes made by the archived Core source mutants on a copied frozen `feature_governance/node0` regtest recording, using the new Rust extractor and a copied Core binary. This was an output-equivalent replay, not a new compilation of those two Core source mutants. The independent source mutants changed only the detached scratch copy and were restored after each test.

## Delta checks and limits

`contrib/node/statedigest/extract.py:159-191,237-280` checks alternate `-vbparams` runs before writing the extraction manifest; `conformance/src/digest/extract/checks.rs:253-385` checks deployment names, states, and parent `since`. **possible security note: see manager.** `extract.py:106-112,242-275` records and hashes `blocks.dat`; `store-vectors.sh:105-139` verifies and compresses it. The stored inventory contains all **18/18** listed FTs and **81/81** manifest-listed nodes, with zero missing `blocks.dat.zst`, digest, decisions, golden, or extraction files.

- `cargo test -p dash-statedigest -p dash-conformance --all-features`: **194 passed, 0 failed, 22 ignored**, across 16 result sections.
- `cargo clippy -p dash-statedigest -p dash-conformance --all-features --all-targets -- -D warnings`: exit 0.
- `cargo fmt --all -- --check`: exit 0.
- `contrib/node/check-pure.sh`: **8/8** pure crates passed.
- Core `test_dash --run_test=statedigest_tests`: **5/5** cases passed, zero errors; the copied binary identifies `02118350e1` in its `H` record.
- `python3 -I -m unittest discover -s contrib/node/statedigest/tests -v`: **27/27** passed.

Core execution used copied binaries from the supplied recording and final build trees; no new Core object was built. The R1 Core probes were output-equivalent replay, not newly compiled source mutants. I did not run the full 4,468-test gate again, the full Core FT suite, live oracle nodes, or a forced concurrency fault. All network work was local regtest.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
