# SB7 Sol review, round 4 (F1 only)

Reviewed `node/sb7-sml-core` at `692f91c291a5bbaaa1f26b606f99821858c32d43` in the detached worktree `/work/scratch/sb7-r4-sol/wt`. The delta from round 3 (`14fdebf2c`) changes only `dash-evo/tests/cbtx_root_history.rs`. The implementer's worktree and branch were not edited.

## Finding disposition

**F1 is closed.** `Chain::block` now requires the first proof height to equal START, and `check` calls `Chain::finish(end)` before strict success. `finish` asks the `Lines<BufReader<MultiGzDecoder<_>>>` reader for the next line. An extra decoded line fails; reaching `None` requires the decoder to consume the remaining gzip member data and validate its trailer. Read or decoder errors also fail. There is no strict success path around this call after the final range checks.

I independently ran the round 3 real-data cases against the pinned binary. Both six-block controls passed with zero mismatches (testnet 1,069,870..=1,069,875 and mainnet 2,089,995..=2,090,000). All 28 prior malformed proof cases rejected, including the eight F1 cases that round 3 unexpectedly accepted: duplicate END, garbage and higher-height tails, a partial tail line, missing trailer, bad CRC, bad ISIZE, and an incomplete concatenated member. The remaining five M2 adversarial cases rejected as before. Thus all 35 prior cases met expectations.

Ten fresh cases also met expectations:

| Construction | Result |
| --- | --- |
| Complete second gzip member with a genuine higher-height proof | Rejected as a line after END |
| Valid empty second gzip member | Accepted |
| Newline-only tail; whitespace tail with and without newline | All rejected as lines after END |
| Lower-height line before START | Rejected before the first record |
| END proof with no final newline | Accepted |
| END line missing its coinbase field | Rejected as a bad proof line |
| Empty second member with a bad CRC | Rejected as a gzip read error |
| Raw garbage after the completed member | Rejected as a read error |

The empty second member adds no decoded proof text, and both gzip members validate. This is consistent with the documented line-oriented format. The no-final-newline file also has exactly the required six lines. Python's independent gzip decoder accepted the valid second-member constructions and rejected the altered empty-member CRC and raw trailing garbage.

## M2 binding check

The X11 header-hash comparison, predecessor link, checkpoint/oracle manifest anchor comparison, exact coinbase decoding, partial merkle proof, and CbTx checks remain unchanged in the production acceptance path. The changed synthetic fixture helper selects the requested height range but retains the same header, coinbase and merkle proof construction. The existing tests for off-chain and unlinked headers, anchors, and a coinbase different from the one proven by the block all passed. The real wrong-network prefix and four cross-file skipped/reordered header constructions all rejected. The two captured real-data controls each proved all six coinbases and matched the terminal anchor; the mainnet range did not exceed 2,090,000. I did not re-query the oracle or repeat the full million-block acceptance runs.

## Validation and evidence

- `cargo test --locked -p dash-evo --release --test cbtx_root_history`: 31 passed, 0 failed, 1 ignored. This includes all four new F1 regression tests.
- Independent fixture runner: 45 cases, 45 expected outcomes (41 rejects, 4 accepts). Script, generated cases, per-case logs and JSON results are under `/work/scratch/sb7-r4-sol/`.
- `cargo clippy --locked -p dash-evo --release --test cbtx_root_history -- -D warnings`, `cargo fmt --all --check`, and `git diff --check 14fdebf2c..692f91c29`: passed.

All Rust builds and test-binary runs used `RUSTC_WRAPPER=`, nice 10, the private target at `/work/scratch/sb7-r4-sol/target`, and disk-backed `TMPDIR=/work/scratch/sb7-r4-sol/tmp`. The review target was deleted after validation. No commit, push or GitHub action was made; this report is uncommitted.

**Verdict:** APPROVE
