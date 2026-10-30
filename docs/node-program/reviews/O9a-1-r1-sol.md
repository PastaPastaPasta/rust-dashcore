# O9a-1 independent Sol review — 385e4fd54

Scope: O9a's 15 non-merge commits relative to `4abf10870` and excluding O8 head `07e7af7f3`, plus the merge commits. The three O8 merges have no combined conflict-resolution hunks. O8's implementation was outside this review.

## Findings

### High

1. **A shared linear hole is reported clean.** `dash-statedigest/src/compare.rs:505` pairs only recorded heights; `dash-statedigest/src/compare.rs:439` decides clean from those pairs and never considers `Projection::holes`. `conformance/src/digest/compare.rs:142` passes the projections' picked records without their hole information. Two inputs with the same missing heights 5–7, each starting a new run before height 8, produced `CLEAN (exit 0)` over 10 pairs while both projections reported one hole. A shared recording gap is uncovered state and must not pass.

2. **Stored-file verification accepts incomplete sets.** `conformance/src/digest/store.rs:274` skips a digest, decisions or golden file when its `extract.json` hash is missing; `conformance/src/digest/store.rs:283` skips `blocks.dat` when the stored file is missing. A copy of a stored node verified successfully with three files after deleting `blocks.dat.zst`, then with two checked files after removing the golden hash. The current four-file requirement must fail these cases.

### Medium

3. **The decoder conflicts with the manager ruling.** `conformance/src/digest/store.rs:27` calls `zstd::stream::read::Decoder`, and `conformance/Cargo.toml:35` adds `zstd`. The required decoder is pure-Rust `ruzstd`.

### Low

4. **An inverted height range is accepted as an incomplete comparison.** `conformance/src/main.rs:572` copies `--from` and `--to` without validating their order. `--from 10 --to 5` returned exit 3 with zero pairs; this is an invalid argument and should return exit 2.

possible security note: see manager

## Claims checked

| Claim | Result and evidence |
|---|---|
| Gate copy 2 | Stored `gate-last-green` names exact head; `gate.log` ends `tests passed 4537 failed 0`, `GATE EXIT 0`. Log checked, gate not rerun. |
| cargo-mutants 244 / 211 / 32 / 1 | Confirmed in `mutants4.log`; `missed.txt` contains only `compare.rs:508` `<`→`<=`. Equivalent: the preceding equality arm consumes `lh == rh`; with `rh == None`, both versions take the gap arm. |
| 66 planted `C.s` changes | Library's existing 66-case test passed. A temporary independent binary test checked first, middle and last positions for all 22 keys (66 calls): every exit was 1 and stdout named the expected height, block and key. Temporary test restored. |
| `fx` stream-only; `seq` ignored | Confirmed by class filter and passing `observations_compare_in_stream_mode_only_and_schedules_never` test. |
| Coverage and `--require` fixes | Required absences diverge and other absences remain incomplete. The shared-hole case above still passes falsely. |
| Linear/stream pairing | Existing tests cover 228 matching linear pairs plus the two expected independent chains, and 112 selected stream pairs. Rewind, reorg and floor tests pass; the shared-hole case is missing. |
| Field-table rescan | `Comparator::new` builds per-kind compared-key lists once (`compare.rs:293`); `both` uses them (`compare.rs:381`). |
| CLI mode validation | Linear `--org` and observation-only `--require` are refused; the inverted-range case remains. |
| `RecordedDecisions` / `final_connect` | Strict and keyed tests pass. `final_connect` updates from the last tip `C` over all runs, including a run without decisions. All 81 stored nodes pass the golden-to-attempt check. |
| Decoder and four-file verification | Both conflict with manager rulings, as above. |
| Private-output claim | possible security note: see manager |
| Purity | `dash-statedigest` has no dependencies, uses ordered collections, and `check-pure.sh` passed. |

## Independent mutations

Each mutation was applied alone to `compare.rs` or `decisions.rs` from saved exact-head copies. After each run, `cp -p` restored the file, `touch` forced rebuild, and `cmp` matched the copy. Final tracked tree was clean.

| Mutation | Test | Result |
|---|---|---|
| M1: ignore stream-origin filter in `next_kept` | `stream_origins_filter_the_records` | Caught: 0 passed, 1 failed. |
| M2: clear the whole chain at a run rewind | `a_run_may_rewind_at_its_first_tip_connect`; then full comparator suite | Focused test survived (1 passed); full suite caught it (23 passed, 1 failed at partial-rewind case). |
| M3: retain first `final_tip` entry instead of replacing it | `final_connect_counts_tip_connects_only` | Caught: 0 passed, 1 failed. |
| M4: restart the strict cursor at query zero | `strict_lookup_requires_the_recorded_order` | Caught: 0 passed, 1 failed. |

## Validation and limits

- `cargo test -p dash-statedigest -p dash-conformance`: exit 0; 237 passed, 0 failed, 22 ignored across 19 test summaries.
- `cargo clippy -p dash-statedigest -p dash-conformance --all-targets --all-features -- -D warnings`: exit 0, no warnings.
- `cargo fmt --all -- --check`: exit 0.
- `contrib/node/check-pure.sh`: exit 0, 8 pure crates passed.
- Additional temporary tests: 66 binary planted changes passed; missing-file/hash acceptance reproduced (1 passed); both restored to exact-head content.
- No live oracle nodes, `/work/chains`, `/work/bench`, GitHub, or implementer worktree were touched. The 4,537-test gate was checked from the stored exact-head log, not independently rerun.

**Verdict:** CHANGES-REQUIRED
Sol gate satisfied: yes
