# AB-AR2-o calibration confirmation, round r2

**Verdict: APPROVE**

Reviewed only `node/ab-ar2-o` at `3a985146a9294d4f71351422366a7c2c59c12d68`, specifically the three fixes after the r1 head `47a95239`, against AB-REVIEW.md, REVIEW-COMMON.md, AB-AR2.md, PLAN §4 AR2 and DEC-84. Used a detached worktree at `/work/scratch/ab-review-ar2-o-r2/wt`, with `TMPDIR=/dev/shm/ab-review-ar2-o-r2-tmp` (0700). The implementer's branch/worktree was not modified. No other arm's implementation or review was inspected.

The reviewed commits are:

- `75d21bfb` — fix(dash-rpc-spec): portable generator that owns its dashd endpoint
- `f159e21b` — fix(dash-rpc-spec): live help test owns its dashd endpoint
- `3a985146` — docs(dash-rpc-spec): name v24.0.0-rc.3's commit, not its tag object

## Findings and r1 disposition

No remaining in-scope findings.

**r1 Major 1 — CLOSED.** `generator/build-core.sh:12` uses `mkdir`/`cd`/`pwd` instead of GNU `realpath -m`; `:16` honors `MAKE`; `:25` obtains the native depends host using `print-host`; and `:28` selects that host's config.site without a Linux glob. The shell constructs used are compatible with Bash 3.2. The deleted build-dump/regenerate shell helpers have moved into Python, removing `local -n`, GNU path options and `sha256sum`. `generator/generate.py:201` onward handles paths, shell quoting and hashes using Python, and executes Core's compile/link commands through the platform POSIX shell.

Independent Linux checks passed:

- `bash --posix -n generator/build-core.sh`.
- Execution of the unmodified build-core script under `bash --posix`, with controlled git/make/build fixtures reporting `x86_64-apple-darwin`, `MAKE=gmake`, and an output directory containing spaces. It selected the Darwin config.site and used gmake for all three make calls. This checks the script's host-selection and quoting logic; it is not a macOS build.
- The real pinned Core depends tree reports `host=x86_64-pc-linux-gnu` through gmake's print-host target.
- Python's parser accepted generate.py with Python 3.8 grammar.
- The complete real build-dump/regenerate workflow passed with `MAKE=gmake`; `/bin/sh` on this host resolves to dash. The regenerated table was byte-identical.

`generator/README.md` accurately distinguishes Linux validation from untested macOS. Native macOS, Windows/MSYS2, BSD and an actual Bash 3.2 executable were not available for execution. The original concrete GNU/Bash-4/Linux-host dependencies are removed; native platform QA remains a stated limitation rather than an unresolved instance of r1 Major 1.

**r1 Minor 2 — CLOSED.** `tests/dashd_help.rs:149` reads only its child's datadir cookie; `generator/generate.py:97` uses a fresh random password. Both require the child to be alive after successful readiness RPC, then canonicalize and check `getrpcinfo.logpath` against their own datadir before setting `owned` (`dashd_help.rs:176–184`, `generate.py:126–129`). Cleanup sends `stop` only for an owned endpoint with a live child; otherwise it kills/reaps the child by PID (`dashd_help.rs:237`, `generate.py:153`). Both automatic startup paths have five attempts; an occupied explicit port fails.

Five independent adversarial checks passed, using only review-owned datadirs and RPC ports 41464–41470:

1. Replayed the r1 attack with a separate v24 daemon on 41464 using the old `rpcspec:rpcspec` credentials. The unmodified Rust live test failed as expected (exit 101, child bind failure); the foreign daemon remained alive and answered `getblockcount`.
2. Replayed the same attack against the Python Node. Startup rejected the occupied explicit port; the foreign daemon survived and answered.
3. Forced Python's auto-port selection to choose occupied 41464 first and free 41466 second. It retried, established ownership on 41466 and stopped only its own child.
4. Tested Rust's ownership check with its real child alive on 41470, while a review-controlled proxy on 41468 forwarded requests to the foreign daemon and accepted the child's credentials. Thus even authenticated `getblockcount` succeeded against the foreign endpoint while the child lived. The foreign logpath caused the expected assertion failure; the proxy recorded `getblockcount` and `getrpcinfo`, and no `stop`. The foreign daemon survived.
5. Repeated that child-alive/proxy experiment for the Python Node. The foreign logpath was rejected, no `stop` was sent, and the foreign daemon survived.

The free-port selection still has a bind gap, but it no longer grants readiness or shutdown authority over an ordinary foreign daemon. Per-instance authentication prevents that substitution; the separate logpath check also rejected substitution when the proxy deliberately bypassed authentication. If the child dies between readiness and the ownership query, the unowned cleanup path kills/reaps only that child. No remaining ownership race was found within the harness's endpoint-isolation contract. Rust's automatic retry loop was inspected; the forced automatic collision was executed for Python.

## Independent acceptance evidence

- `cargo fmt --all --check`: PASS.
- `cargo clippy -p dash-rpc-spec --all-features --all-targets -- -D warnings`: PASS.
- `cargo test -p dash-rpc-spec --all-features -- --nocapture`: PASS, **9 passed, 0 failed** (7 table tests, base64 regression test, live help test). Used `CARGO_BUILD_JOBS=6`, the private TMPDIR, `DASHD_V24_PATH` pointing to the setup-dashd cache's v24.0.0-rc.3 release, and RPC port 41460. No dashd-test skip was set.
- Live comparison: **291 help outputs with a wallet + 291 under `-disablewallet`**, byte-equal, covering all 289 commands, both overviews and both unknown-command cases. Both argument maps and the filtered client/server conversion comparison passed. This includes more than ten concrete Core behavior checks: `getblock`, `getrawtransaction`, `getrawtransactionmulti`, `listtransactions`, `stop`, `echojson`, `deriveaddresses`, `importdescriptors`, `fundrawtransaction`, `scanblocks`, `protx register`, `protx info`, and `getcoinjoininfo`.
- All **347 conversion entries** passed the table comparison. The supplied Core build's client.cpp and the checked-in client.cpp fixture both hash to the pinned blob `260e3875ec50e7bcc4c8f15645f334aed86eeb07`.
- Ran unmodified `generate.py regenerate` against this arm's existing fresh archived-tag build at `/work/scratch/ab-ar2-o/r1-build/core`, using the cached release dashd, `MAKE=gmake`, a new dump/capture directory and port 41462. It emitted **289 commands / 347 conversion entries**, passed its metadata/help/conversion/PRD cross-checks, and `git diff --exit-code -- dash-rpc-spec/src/generated.rs` passed. This round independently rebuilt the dump and recaptured help; it did not rebuild all of Core's dependencies.
- CI groups: PASS, all 29 workspace crates assigned. R2 classification: PASS. Pure-crate checks: PASS, 3 crates.
- Provenance correction: PASS. `v24.0.0-rc.3` resolves to annotated tag object `da567d7dba945143eee4d2ce1621c21b32089989` and commit `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`, now correctly distinguished in PROVENANCE.md. The existing Core build's core.commit agrees.

**Full-gate claim inspected, not independently replayed:** this arm's `/work/scratch/ab-ar2-o/gate.log` records `tests passed 3138 failed 0`, `receipt PASS lines: 2`, and `GATE EXIT 0`; its gate-last-green names the reviewed head. This narrow confirmation independently reran the touched-crate gates and classification checks above, not the workspace-wide gate/private corpus.

## Limits and scope

A separate output-path probe found that Core's generated libtool link step loses whitespace quoting when `--work` contains spaces. The same failure was reproduced with the r1 build-dump.sh from `47a95239` against the same Core build; it is an inherited toolchain/workflow limitation, not a regression in these fixes, and is outside this narrow r1-finding confirmation. The successful full regeneration used a whitespace-free work directory. Native platform execution remains untested as documented.

No new brief deviation or RPC-data discrepancy was found. Review-owned daemons, datadirs, TMPDIR and the detached worktree were cleaned up; scratch evidence logs are retained under `/work/scratch/ab-review-ar2-o-r2/`. This report is uncommitted.

**Verdict: APPROVE**

blockers=0 majors=0 minors=0 nits=0
