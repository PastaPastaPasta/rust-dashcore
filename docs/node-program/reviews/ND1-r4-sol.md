# ND1 independent review, round 4 — Sol

Range: `0031148f4..6f2723aa0` on `node/nd1-rdashd`. Both commits, `a50b8d94d` and `6f2723aa0`, have good (`G`) signatures. I reviewed detached `6f2723aa0` in `/work/scratch/nd1-sol-r4/wt`; the implementer's checkout was untouched. Scope is C10's asserted-log ledger under DEC-204. The list has **311 rows**, **19 implemented**, each with a proof citation.

## Round-3 closure

| Item | Result |
| --- | --- |
| C10-1, C10-3, C10-4 | Remain closed: 311 concrete rows, Linux-target cfg handling, and the pinned Core/list check in CI. `selftest` and `check` pass. |
| F1 | **Closed under DEC-204.** Its requested static emitter-statement proof was superseded by cited runtime output. The three cited process tests read the real `debug.log`; removing the daemon's `args.log_args()` call fails the args proof. The literal scan remains a tripwire. |
| F2 | **Closed.** Every implemented row cites a gate-run test. The five unexpected rows cite a process test that checks absence across the complete captured log, and its positive control rejects each injected forbidden text. Disabling that check fails the test. |
| Spread nit | **Closed.** The generator refuses `*`/`**` spreads, positional `reject_reason`, keyword names as string values, and an assertion helper named without a call. The selftest and independent synthetic probes cover all four. |

## Per-row proof judgement

Proof keys: **C** = `rdashd::features::config_file_cases_are_logged`; **A** = `rdashd::features::the_args_log_masks_sensitive_values`; **I** = `rdashd::features::init_logs_its_start_lines`. All three are nonignored `#[test]` functions in the `features` integration target, which the workspace gate runs on x86_64 Linux. Each checks real process `debug.log` output. Core source references below are `test/functional/` paths at the pinned v24 tag.

| List line | Core assertion | Proof | Judgement |
| --- | --- | --- | --- |
| 84 | `feature_config_args.py:58`, expected unknown `dash_conf` warning | C | **Proved:** included config sets `dash_conf=1`; the new run's log contains the same warning. |
| 85 | `:65`, expected `reindex=1` warning | C | **Proved:** included config sets `reindex=1`; the new run's log contains the full warning before CH5 refusal. |
| 86 | `:144`, expected `Config file: {conf_path}` | C | **Proved:** default-location config names the datadir; the log names that actual config path, as Core checks. |
| 87 | `:159`, expected confusing `-connect=0` warning | C | **Proved:** `-noconnect=0` produces the identical warning before P2a-net refusal. |
| 88 | `:166`, expected `addnode="some.node"` | A | **Proved:** Core's exact argument is supplied and its log line is required. |
| 89 | `:166`, expected masked `rpcauth` | A | **Proved:** the same credential is supplied; `rpcauth=****` is required. |
| 90 | `:166`, expected masked empty `rpcpassword` | A | **Proved:** `-rpcpassword=` is supplied; `rpcpassword=****` is required. |
| 91 | `:166`, expected masked `rpcuser` | A | **Proved:** same secret is supplied; `rpcuser=****` is required. |
| 92 | `:166`, expected masked `torpassword` | A | **Proved:** same secret is supplied; `torpassword=****` is required. |
| 93 | `:166`, expected `regtest="1"` config arg | A | **Proved:** the test writes `regtest=1` and requires its log line. |
| 94 | `:166`, expected `[regtest] server="1"` config arg | A | **Proved:** the test writes that section/value and requires its log line. |
| 95 | `:175`, unexpected raw `rpcauth` credential | A | **Proved:** the full captured log must omit Core's exact credential. |
| 96 | `:175`, unexpected `secret-rpcuser` | A | **Proved:** the full captured log must omit Core's exact value. |
| 97 | `:175`, unexpected `secret-torpassword` | A | **Proved:** the full captured log must omit Core's exact value. |
| 98 | `:175`, unexpected masked `rpcbind` | A | **Proved:** the full captured log must omit this exact, incorrectly masked line. |
| 99 | `:175`, unexpected masked `rpcallowip` | A | **Proved:** the full captured log must omit this exact, incorrectly masked line. |
| 132 | `feature_init.py:88`, expected scheduler start | I | **Proved:** a successful start logs `scheduler thread start`. |
| 136 | `:88`, expected block-index loading | I | **Proved:** a successful start logs `init message: Loading block index…`, containing Core's asserted fragment. |
| 139 | `:88`, expected verifying-blocks init message | I | **Proved:** a successful start logs `init message: Verifying blocks…`, containing Core's asserted fragment. |

Core's args case reaches a full start; rdashd currently refuses `-addnode`, `-torpassword`, then `-rpcallowip` after writing the args log. This limits the negative proof to the startup paths reached before those refusals, but it directly proves the current masking behavior at the actual process boundary. The test pins each successive refusal. Lifting `-rpcallowip` changed the third refusal to `-rpcauth` and failed the test, requiring review as RP0 progresses. The same exact-argument first run supplies all seven positive and five negative assertions. The config and init rows concern lines written before refusal or during ordinary startup, respectively; they do not claim Core's additional restart behavior.

## Citation resolution and generator

`implemented_rows_cite_a_proof_the_gate_runs` checks workspace membership, a runnable `lib` or integration target, a unique nonignored and non-`should_panic` test in that target's module tree, gate-target cfg, unified workspace features, and a row-text string literal in the test's file. The active `#![cfg(unix)]` file is included for the Linux gate. An inactive `#[cfg(windows)]` proof function is rejected. A same-named function in another integration target does not make this target ambiguous. Comment-only text is rejected. The resolver checks a file's literals rather than the cited function's assertions by DEC-204; a citation to an unrelated test in the same file resolves, and my semantic review above rejects that citation as proof.

Independent `fragments()` probes refused direct `*xs`, direct `**xs`, positional `reject_reason`, a string-valued `expected_msgs` key, and an uncalled `node.assert_debug_log` reference. The pinned Core generation check remained byte-for-byte equal.

## New findings

None in this round.

## Mutations

Each mutation was applied alone in the detached checkout, then restored from a saved byte copy, touched, and compared with `cmp`. Exit 101 is the intended test failure. The final checkout diff is empty.

| Mutant | Focused result | Judgement |
| --- | --- | --- |
| Replace scheduler literal with another string, leave original only in a comment | Citation test 101 | Comment-only mention rejected. |
| Gate cited `init_logs_its_start_lines` with `#[cfg(windows)]` | Citation test 101 | Inactive function rejected on Linux. |
| Add same function name to a different integration target | Citation test 0 | Correct: citation includes its target. |
| Cite an unrelated test in the same file for one row | Citation test 0 | Expected DEC-204 limit; semantic review rejects the citation. |
| Disable the unexpected-text branch of the args checker | Process test 101 | Positive control catches it. |
| Disable the missing-expected-line branch | Process test 101 | Positive control catches it. |
| Remove `args.log_args()` from the daemon route | Process test 101 | Real process proof catches loss of the logger. |
| Promote `-rpcallowip` to implemented support | Process test 101 | Pinned refusal forces a revisit. |

## Validation

- `cargo test -p dashd-node -p rdashd --no-fail-fast`: exit 0, including 90 node unit tests (one ignored), 10 asserted-log tests, four rdashd unit tests, the pinned v24 differential tests, and all 9 feature process tests. `DASHD_V24_PATH` named the pinned binary and `SKIP_DASHD_TESTS` was unset.
- `python3 contrib/node/asserted-logs.py selftest` and `check --core ~/workspace/dash`: exit 0.
- `cargo clippy -p dashd-node -p rdashd --all-targets -- -D warnings` and `cargo fmt --all -- --check`: exit 0.
- Restored focused suites: 10/10 asserted-log and 9/9 feature tests passed. Both commits are `G`. No commit, push, or GitHub post was made.

**Verdict:** APPROVE
Sol gate satisfied: yes
