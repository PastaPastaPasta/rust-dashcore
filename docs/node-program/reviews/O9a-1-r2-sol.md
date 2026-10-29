# O9a-1 independent Sol review, round 2

Reviewed `385e4fd54..3894b5a60` in a detached worktree. O8 and the rest of O9a-1 were outside scope.

## Findings by severity

None. I found no new defect in the delta.

## Round 1 closure

| Finding | Status | Evidence and independent reproduction |
|---|---|---|
| F1 High, shared linear hole reported clean | Closed | `dash-statedigest/src/compare.rs:581` records skipped heights, and `:460` makes a comparison with any skipped height incomplete. I removed heights 5–7 and 12–13 from both copies of a stored golden and started new runs at heights 8 and 14. Comparing the copies returned exit 3 and listed both holes on both sides. Ranges wholly inside a hole (`5..7`, `6..6`) also returned 3; ranges outside the holes (`8..11`, `14..16`) returned 0. Mutating `:460` to ignore `skipped` failed the focused regression test. |
| F2 High, incomplete stored set verified | Closed | `conformance/src/digest/store.rs:276` requires all four hashes; `:282` rejects missing or non-string hashes; `:285` rejects missing files. The committed test removes each of the four files and hashes. My separate probe made each hash a JSON number and, for each stored file, appended invalid data or truncated its compressed stream; every case failed verification. Mutating either missing-hash or missing-file branch to skip the item failed the focused test. |
| F3 Medium, decoder choice | Withdrawn | The manager's ruling requires the `zstd` crate. `conformance/src/digest/store.rs:38` uses it. `Cargo.lock` and `conformance/Cargo.toml` are byte-identical to `3b50d8117`; `cargo tree` shows `zstd` 0.14.0 and no `ruzstd`. The round 1 finding came from the misquoted ruling. |
| F4 Low, inverted range returned exit 3 | Closed | `conformance/src/main.rs:574` rejects `--from > --to`. My CLI probes returned exit 2, empty stdout, and the range error for stream mode `16..3` and the `i64::MAX..i64::MIN` boundary. Mutating the check off failed my focused CLI test. |

## Decoder and verification checks

- `conformance/src/digest/store.rs:25` recognizes zstd and skippable frame magics, and `:38` feeds the stream to libzstd. For each of digest, decisions, golden and `blocks.dat`, my copied-node probe appended a valid empty zstd frame and a skippable frame: verification still passed because the decompressed SHA-256 was unchanged. Appending garbage, cutting a skippable frame, or truncating the stored frame caused verification to fail for each file.
- The committed reader tests also cover concatenated frames, a flipped checksum, cut frames, trailing garbage, and frames whose declared size is too small or too large. Mutating zstd detection off failed the focused decoder test.
- A valid set returned exactly `digest`, `decisions`, `golden`, `blocks.dat` from `Node::verify`. Missing `extract.json` and malformed JSON fail through `read_json` at `conformance/src/digest/store.rs:274`.

## Checks run

- `cargo test -p dash-statedigest -p dash-conformance`: exit 0, 247 passed, 0 failed, 22 ignored across the test summaries.
- Three temporary independent Rust probes: all passed (four-file stream ends, malformed hash types, inverted stream range). Seven direct CLI range and hole cases: all passed.
- Five temporary mutations were caught by focused tests: F1 skipped verdict; F2 missing hash; F2 missing file; F4 range validation; zstd detection. Tracked files were restored byte-for-byte; focused exact-source tests and CLI probes passed again afterward.
- `cargo fmt --all -- --check` and `git diff --check 385e4fd54..3894b5a60`: exit 0.
- `cargo clippy -p dash-statedigest -p dash-conformance --all-targets --all-features -- -D warnings`: exit 0.
- The review worktree and target were removed after checking the tracked tree was clean. No commit or push was made.

**Verdict:** APPROVE
Sol gate satisfied: yes
