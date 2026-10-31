# ND1 independent review, round 3 — Sol

Range: `54a53108a..0031148f4` on `node/nd1-rdashd` (8 commits, all `G`). I reviewed detached `0031148f4` in `/work/scratch/nd1-sol-r3/wt`; the implementer branch and worktree were untouched. Pinned Core `v24.0.0-rc.3` peels to `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`. This review is limited to round-2 C10 fixes.

## Closure table

| Item | Result | Evidence |
| --- | --- | --- |
| C10-1 | **Closed at the pin.** | All ten previously absent strings are concrete rows: two in `feature_asmap.py`, three in `p2p_blockfilters.py`, two in `p2p_invalid_messages.py`, and three in `p2p_v2_misbehaving.py`. The five `expr` rows are gone; the list has 311 rows. `selftest` and pinned `check` pass. The generator records unresolved direct arguments and fails generation at `contrib/node/asserted-logs.py:659-673,762-765`. A synthetic string-addition assertion was rejected as unresolved. |
| C10-2 | **Open.** | The header and ratchet state the one-directional contract, and an unlisted `ND1_NEW_EMITTER_CANARY` is allowed as ruled. Pending unexpected `Misbehaving` is caught. The ratchet still treats any compiled-source string literal as an active emitter; a dormant literal and removal of the daemon's sole `args.log_args()` call both survived. Finding F1. |
| C10-3 | **Closed for the tested Linux target.** | `cargo tree -p rdashd -e normal` supplies crate/features and `Cfg` evaluates the target. A pending fragment in `#[cfg(windows)]` passed on Linux; `#[cfg(nd1_unknown_cfg)]` failed with `unknown cfg`. Both branches of `cfg!()` and host-only Cargo resolution remain declared limits. |
| C10-4 | **Closed.** | `.github/workflows/rust.yml:1-7,87-100` runs `asserted-logs` on every PR without path filters or the `plan` skip. Its shallow fetch of the named tag peeled to the expected commit in an independent scratch fetch. `selftest` and `check` run after the equality check; a list-drift mutant failed `check`, and a pin mismatch failed generation. |
| C10-2 amendment | **Open as an enforced condition.** | The five `feature_config_args.py:175` rows are `implemented` and `args_log_masks_sensitive_values` does use the same argument values, call `log_args()`, and assert all five absent from the captured output (`dash-args/tests/functional_behaviours.rs:281-318`). That proves the argument logger's output for this fixture. The rows cite only owner `ND1`, and no ratchet or CI check ties them to this proof. Removing the five absence assertions left both the proof test and ratchet green. The unit test also clears earlier startup output before calling `log_args()`, so it does not establish absence across Core's full startup scenario. Finding F2. |

## New findings

### F1 — P2: expected rows can be marked implemented without an active log path

Location: `dashd-node/tests/asserted_log_lines.rs:567-580,648-655,687-705`; live call path `dashd-node/src/logging/options.rs:148`.

Probe: I changed `feature_abortnode.py:37` from pending to implemented and added only `let _dormant = "Failed to disconnect block";` in `Node::new`. `the_ratchet_holds` exited 0. Flipping the row alone failed, so the dormant literal supplied the entire apparent evidence. Separately, I removed `args.log_args()` from `route_args_log`, its only call in `dashd-node/src` or `rdashd/src`; the ratchet and `args_log_masks_sensitive_values` both exited 0. The daemon would no longer emit the implemented `Command-line arg:` rows on startup, while the scanner still sees the formatter literals in `dash-args`.

Fix: connect implemented expected rows to the logging statements and exercise the current startup call path for the args rows. A string literal elsewhere in a compiled file must not satisfy an emitter claim. For rows composed from several literals, require the pieces to belong to the same emitting statement rather than arbitrary files.

### F2 — P2: implemented unexpected rows have no cited, required silence proof

Location: `dashd-node/tests/asserted_log_lines.txt:88-94`, `dashd-node/tests/asserted_log_lines.rs:669-719`, `.github/workflows/rust.yml:87-100`; proof test `dash-args/tests/functional_behaviours.rs:281-318`.

Probe: I replaced the negative assertion in `args_log_masks_sensitive_values` with `let _ = unexpected`. The named dash-args test exited 0 and `the_ratchet_holds` exited 0. The list has no proof-test field, and the asserted-logs CI job runs only the Python generator checks. The existing test genuinely checks the five strings in `log_args()` output when intact, but the row status has no mechanical dependence on that test or its absence assertions. Its `clear()` at line 296 excludes earlier startup messages.

Fix: cite `dash-args/tests/functional_behaviours.rs::args_log_masks_sensitive_values` on each of the five rows (or in a checked mapping), validate that the cited test is required by the gate, and add a process-level startup assertion for the full Core scenario. A reviewer still needs to inspect the proof's assertions when a status changes; a passing test name alone cannot establish its semantics.

### Nit: a future keyword spread can be silently skipped

`contrib/node/asserted-logs.py:685-695` ignores a direct `assert_debug_log(**{"expected_msgs": ["SPREAD_CANARY"]})` call: the synthetic probe returned no row and no unresolved entry. My AST survey found no spread at the pinned Core tree, so this is a future fail-closed gap, not a pin omission. Reject unrecognized call argument forms.

## Mutation results

Each worktree mutant was applied alone, restored byte-for-byte, touched, and checked with `git diff --exit-code`. The generator mutations used scratch inputs and did not alter the pinned Core tree.

| Probe | Result |
| --- | --- |
| Flip pending expected row to implemented, no source change | **Killed**, ratchet exit 101. |
| Add unlisted production `ND1_NEW_EMITTER_CANARY` | **Survived**, exit 0; allowed by the stated one-directional contract. |
| Add production `Misbehaving` log line while its unexpected rows are pending | **Killed**, exit 101, including the unexpected-row diagnostic. |
| Put a pending fragment in a Windows-only function on Linux | **Survived**, exit 0; correct target handling. |
| Add an unknown cfg predicate | **Killed**, exit 101, `unknown cfg`. |
| Add a dormant literal and flip its expected row to implemented | **Survived**, exit 0; F1. |
| Remove the daemon's `args.log_args()` startup call | **Survived** by ratchet and masking test, both exit 0; F1. |
| Remove the five absence assertions from the masking test | **Survived** by the named test and ratchet, both exit 0; F2. |
| Add a string-addition assertion | **Killed** as unresolved by the generator. |
| Change the Core commit pin to zeros | **Killed** before generation. |
| Add a bogus row to a scratch copy of the list | **Killed** by `check`, exit 1. |
| Direct `assert_debug_log(**kwargs)` synthetic call | **Survived** without a row or error; nit, absent at the pin. |

## Validation and limits

- `python3 contrib/node/asserted-logs.py selftest` and `check --core ~/workspace/dash` passed. An AST survey of pinned functional tests found 214 direct `assert_debug_log` calls and two `busy_wait_for_debug_log` calls, no direct-call spreads or aliases, and no positional `reject_reason` calls to the relevant helpers. The sole direct `debug_log_path` read in test files remains the explicit governance case. No silently dropped pinned assertion was found.
- `cargo test -p dashd-node -p rdashd` passed: 90 node unit tests passed and one was ignored; all eight asserted-log tests, four rdashd unit tests, and the 3/6/7 integration groups passed. `cargo test -p dash-args` passed every group, including `args_log_masks_sensitive_values`. `cargo clippy -p dashd-node -p rdashd -p dash-args --all-targets -- -D warnings` and `cargo fmt --all -- --check` passed. The restored asserted-log suite and named masking test passed again after the mutations.
- Instrumenting the ratchet's module walk found 176 scanned files among 211 `.rs` files under the nine in-repo production-closure crates. The 35 omitted files are test-only modules, modules gated by disabled `model`/`test-util`/`test-hooks` features, or the aarch64 backend on this x86_64 host. No active `#[path]`, `include!`, `cfg_if!` module, or build-script-generated Rust module was found at this pin. Future such module forms are outside the scanner's current coverage; `#[path]` in a kept file deliberately errors.
- The remote tag fetch reproduced the CI command and peeled to `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`. The review checkout is clean. No commit, push, or GitHub post was made.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
