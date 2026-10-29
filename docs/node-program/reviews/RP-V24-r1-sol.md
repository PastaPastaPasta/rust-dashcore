# RP-V24 — first Sol review

- Reviewed: `node/rp-v24` at `618428f0031a8f51aa1f68088d131e9e85f8df61`.
- Diff base: `9d7e6c119d0a0bdc086c91cc7b464381e1c57d36`, the merge base with `node/integration` at review start.
- Contract/context: REVIEW-COMMON, DEC-82, DEC-92, O17 STATUS, and `o17-pin-delta-method` read. This is the Sol pass; no Opus review was run or delegated.
- Work: detached head and base worktrees under `/work/scratch/rp-v24-review/`; `TMPDIR=/dev/shm/rp-v24-r1-tmp` (0700), `CARGO_BUILD_JOBS=8`. The implementer worktree was not modified.

## Findings (severity ordered)

No blocker or major found.

1. **Minor — active planning documents still prescribe the removed profile axis.** `docs/node-program/PLAN.md:560` still assigns P5 both the v23 ObjectDownloadState and v24 TxRequestTracker; `:574` still assigns ND1 `-releaseprofile`; `:693` requires updating ReleaseProfile and rerunning the old two-line O17 audit, and `:694` maintains the dropped v23 reference. `docs/node-program/designs/A1-architecture.md:1313` retains the mainnet v23/70240 identity, `:1040` retains BlsSchemeLeak, and `docs/node-program/compat/COVERAGE.md:75` still names the removed `txid(rules)` / `merkle(rules)` APIs. `docs/node-program/briefs/DESIGNER.md:3`, `:10`, and `:16` still mandate the historical profile spec, both pins, and per-profile behavior. A subsequent implementer following these task rows can reintroduce obsolete work or call removed APIs. DEC-82 and the new A1 preface override the stale text, so this does not establish a current runtime defect. **Suggested fix:** normalize the active consumer/task/coverage rows to v24, remove the obsolete effect and option from active pseudocode, and label any tables intentionally preserved as history. Leave the historical O17 audit intact.

2. **Minor — the new release guard accepts ordinary release tests it says it rejects.** `contrib/node/pin-delta/check-release-refs.sh:19` recognizes a comparison only when Release is on the right and recognizes match arms but not `matches!`. In a separate synthetic Git repository, `r == Release::V24_0` exits 1, while `Release::V24_0 == r` and `matches!(r, Release::V24_0)` both exit 0. These are branches on the release under the script's own stated contract. No current consumer using these forms was found. **Suggested fix:** handle comparisons on both sides and `matches!` (including normal multiline formatting), with rejected selftest cases for them.

## Consensus and coverage assessment

- **O17 classifications:** DEC-82 selects the v24 side of the former `profile` rows, rather than deleting their behavior. For the implemented paths, SIG-01 retains the former v24 stable asset-unlock txid and merkle leaves; CONS-04's context now describes NodeClock rather than adjusted time; CONS-07/MPIX-01 retain the v24 mandatory mask `0x615`. The block-file rollover retains Core v24's highest-undo-height-since-start rule. WAL-45's `v24-all` fastprune growth and CONS-28's startup cleanup are retained. Future O17 consumers that are not implemented yet are not proven by this review.
- **Historical validation:** `block_script_flags` still derives flags from buried deployments; collapsing the policy mask does not force all modern block flags onto history. Canonical serialization, decoding, stable-unlock shape/hash calculation, and coin/undo codecs were not removed or altered by the collapse. Checked the relevant v24 tag source in `primitives/transaction.{h,cpp}`, `script/standard.h`, and `node/blockstorage.cpp`.
- **Recorded cases:** all retained v24 oracle/consensus data, public conformance fixtures, and core vectors are unchanged. The removed data files are the v23 sets. Three obsolete release-module tests are deleted; other changed test names are renames with retained v24 assertions. This explains the implementer's reported workspace count of 3130 → 3127, independently corroborated by the targeted local suite's 401 → 398. The full workspace gate itself was not rerun here.
- **Coverage minimums:** type 1/type 5 minimums 28/2 → 14/1 and invalid-payload minimums 12/8/4 → 6/4/2 match the retained v24 case counts. The devnet test independently requires at least four fixture cases and checks that every retained case executed. `require_per_pin` multiplies by the one linked pin; no current v24 coverage dilution found.
- **`bad-txns-type`:** the unchanged v24 fixture contains 14 rejected blocks and one such rejection (type 13). Its type-10 case still executes and expects `bad-protx-payload`. The deleted v23 fixture contributes the other two `bad-txns-type` rejections (types 13 and 10). Thus 3 → 1 is correct for the sole reference, with both distinct v24 reject paths retained.
- **Fuzz registry:** six L1 targets retain their v24 legs and all per-leg smoke/nightly weights; exactly six v23 legs disappear. The two model targets were already v24-only. Total smoke legs are 14 → 8 and the CPU floor 600 → 360 seconds, rather than a blanket halving of model coverage. Synthetic/public-seed smoke runs are separate from the actual private corpus; only the authorized private-test gate was used for that corpus.
- **Leftovers:** the production Rust consumer scan and release-ref guard find no v23 release selection, UngatedRules, or ReleaseProfile. Oracle `Pin::V24_0` remains a useful archive identifier; its singleton ALL loop is not a profile selector. The base-sdk Release variant and cached v23 archives are the explicitly documented BSX/DEC-98 follow-up/reference boundary. The legacy SDK installer/CI still defaults to 23.1.0 (`contrib/setup-dashd.py:9`, `:26`), explicitly because its masternode DKG tests do not run on v24 yet; node oracle and ST2 checks select v24 independently. Thus this branch collapses the node release profile, not every legacy SDK test environment. Old source anchors and historical recorder/probe descriptions are not reachable v23 consensus branches.

### Per-target registry comparison

| Target | Baseline legs | Head legs | Smoke CPU seconds per leg (unchanged) | Nightly weight (unchanged) |
|---|---|---|---:|---:|
| `diff_tx_decode` | v23.1 + v24.0 | v24.0 | 60 | 4 |
| `diff_block_decode` | v23.1 + v24.0 | v24.0 | 60 | 4 |
| `diff_header_decode` | v23.1 + v24.0 | v24.0 | 30 | 1 |
| `diff_codec_primitives` | v23.1 + v24.0 | v24.0 | 30 | 1 |
| `diff_coin_decode` | v23.1 + v24.0 | v24.0 | 30 | 2 |
| `diff_undo_decode` | v23.1 + v24.0 | v24.0 | 30 | 2 |
| `kv_model` | v24.0 | v24.0 | 60 | 1 |
| `coreimport_external_scan` | v24.0 | v24.0 | 60 | 2 |

The runner-only `selftest_mismatch` still has no farm legs or budget. No v24 smoke leg, public vector directory, or target route was removed.

## Independent validation

All independent runs use the detached revisions and the review TMPDIR/jobs above. `DASH_EMBARGO_DIR` was unset except for the expressly authorized private gate and the isolated synthetic/public-seed smoke corpus.

| Check | Baseline | Reviewed head |
|---|---:|---:|
| `oracle-diff`: `cargo test --locked -p dash-oracle-sys --features oracle --no-fail-fast` | 62 passed, 0 failed | 62 passed, 0 failed; exit 0 |
| `oracle-diff`: `cargo test --locked -p node-l1 --features oracle --no-fail-fast` | 26 passed, 0 failed | 26 passed, 0 failed; exit 0 |
| Targeted local crates: `dash-consensus`, `dashd-store`, `dash-coreimport`, `node-sink`, `dash-conformance` | 401 passed, 0 failed, 3 ignored; exit 0 | 398 passed, 0 failed, 3 ignored; exit 0 |
| `node-fuzz/fuzzctl.py selftest` | 50 passed | 52 passed |
| Synthetic/public-seed `node-fuzz/fuzzctl.py smoke` | Initial 13/14 passed; 600 s CPU floor, 615 s consumed; one v23 block leg refused before execution; separate rerun passed, restoring 14/14 executed legs | 8/8 passed; 360 s CPU floor, 396 s consumed; exit 0 |
| Authorized `check-private-tests.sh`: quirk receipt | PASS, 522 cases | PASS, 522 cases |
| Authorized `check-private-tests.sh`: seed receipt | PASS, 279 seeds, 279 required | PASS, 279 seeds, 279 required |

The private command was exactly `DASH_EMBARGO_DIR=$HOME/private/embargo/corpus contrib/node/check-private-tests.sh`, with output restricted to the table's pass/fail and counts. Both revisions exit 0, with 2/2 receipts passing.

- **Live v24 scanner parity:** `cargo test --locked -p dash-coreimport --test scanner_parity -- --nocapture`: 2 passed, 0 failed, exit 0. The deterministic damaged-file cases matched v24 dashd's accepted chain and blk/rev bytes.
- **SPV v24 dashd sync:** The supplementary `cargo test --locked -p dash-spv --test dashd_sync --no-fail-fast` run finished at **29 passed / 8 failed** (exit 101): four wallet-load/setup failures and four sync timeouts. A direct serial rerun of `tests_basic::test_sync_empty_wallet` with the same compiled binary advanced through headers/filters but timed out (0/1, 196.66 s). A v23.1.8 reference control using that binary also failed at the same wallet-setup error (0/1, 24.60 s). The host load average was about 190 on 32 vCPUs during these checks. A later serial rerun of all eight failed cases, with load still about 90–145, finished at **4 passed / 4 failed** (exit 101, 921.32 s): empty-wallet sync, wallet sync, disconnect/reconnect, and random restarts passed; the multi-wallet case missed its 60 s midpoint deadline, restart consistency hit a mining RPC socket timeout, fresh-wallet restart failed in wallet setup, and same-client restart timed out awaiting SyncComplete. The multi-wallet trace showed filters advancing to height 13,000 before its deadline. These are **not a clean SPV pass**, and load is an attribution limit, not proof that every failure is a flake. The unchanged SDK setup/sync code and the v23 setup control do not establish a regression from the profile collapse; full SDK v24 integration coverage remains unproven. No masternode/DKG suite was run.
- **Static and build checks:** `check-base-sdk-pins.sh`, `check-r2.sh`, `check-pure.sh` (3 crates), CI `verify-groups` (28 crates), the registry check (9 targets, 8 smoke/farm legs), release guard/selftest, `cargo fmt --all --check`, and the targeted Clippy check all pass. Clippy used `--locked --all-features --all-targets -- -D warnings` for `dash-consensus`, `dashd-store`, `dash-coreimport`, `node-sink`, `dash-conformance`, `dash-oracle-sys`, `node-l1`, and `dashcore-rpc-json`. The private gate's synthetic `--selftest` also passes. The two release-guard bypass cases in finding 2 were reproduced separately. `git diff --check` passes.
- **Oracle/shim reproduction:** `contrib/node/check-oracle-repro.sh --work /work/scratch/rp-v24-review/repro-plain --jobs 4` passes, exit 0. Two fresh cache/build roots of different path lengths independently reproduce the v24 plain archive at commit `a9a483e51efdadeee37cf40ff8697da62b3cd012`, both matching `pins.txt`: **`af924b639ec4cac7cddeb5d076b7c906070a3470632932175b02901b8c95a1f3`**. The smoke legs use the pinned fuzz archive (`790dc62ec79c3e13ca1c52c98060e7d0197636d7c2ccd5354bd16dfc012f8d4e`); a fresh fuzz-flavor two-root reproduction was not run.
- **Baseline smoke refusal:** the one initial failed leg was `diff_block_decode`/v23.1, refused before creation of a run with `the private sink is not writable`. The shared target-directory bootstrap in baseline `node-fuzz/run.sh:385` uses a check-then-`mkdir` race when two pin workers first create the same directories. A separate `node-fuzz/run.sh diff_block_decode --pin v23.1 --cpu-hours 0.016667 --jobs 1` passed. All eight v24 legs passed on both revisions, and all fourteen baseline legs have now executed successfully across the initial run and rerun. This is not a clean initial 14/14 aggregate pass.
- **DML replay framing:** the single-pin script accepts a synthetic checker and produces exactly three output rows for three input samples, exit 0.

## Limits and handoff

The implementer's full gate and BL-20 rerun are supporting reports, not substituted for the independent results above. No full-chain replay, nightly fuzz budget, fresh fuzz-flavor archive reproduction, or manager PF0 counter gate was run. These are not a claim of merge approval; the sequential Opus high pass and manager PF0 remain next under DEC-92/PLAN. No private file was directly inspected; private evidence is restricted to the gate's pass/fail and counts.

**Verdict: APPROVE-WITH-NITS.**

**Sol gate satisfied: yes.** No blocker or major was found in the scoped v24 node-profile collapse; the sequential Opus high pass may proceed. This does not waive the manager's PF0/full gate or turn the inconclusive supplementary SDK/SPV runs into a pass.

Cleanup: both detached review worktrees, their build trees and scratch experiments, and the review TMPDIR are removed after recording this report. No own dashd process remained at cleanup; no source fixes, commits, pushes, or GitHub comments were made.
