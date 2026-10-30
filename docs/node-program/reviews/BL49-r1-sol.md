# BL-49 code review, round 1 (Sol)

Reviewed `node/bl49-private-tests` at `af516a546bba2f75ea4b6e3a21efa4aa124698ef`, parent `b1445f98512186bb9381cdb7cc3f5acdbf96be94`, in a detached scratch worktree. Reviewed tracked code only; did not access anything under `~/private` or run the optional real-corpus check.

## Findings

### Major — pruning mistakes an idle live run for an abandoned run

**Location:** `contrib/node/check-private-tests.sh:138`.

The pruning predicate uses directory mtime without checking whether its gate is still alive. There is no maximum run duration or heartbeat. A gate can create its directory and then spend more than a day waiting for Cargo's locks/build, or be suspended after writing its first receipt. A second gate removes that live directory. The resumed first gate can recreate the directory while writing its second receipt, but its first receipt is gone: it reports one `receipt PASS`, one `receipt FAIL no receipt`, and exits nonzero. This reintroduces the concurrency failure BL-49 is supposed to remove for long-idle runs.

**Evidence:** ran the unchanged `run_private_tests`/`check_receipts` functions against a fresh synthetic tree with a controlled Cargo stand-in. Run A wrote its first receipt, its directory was backdated by 48 hours to model an idle live run, and A waited at a barrier. Run B finished successfully and removed A's directory through the real pruning code. After release, A returned 1 with `quirk_01_embargo_1: receipt FAIL no receipt` and `private_seed_replay: receipt PASS 1 seeds (1 required)`.

**Suggested fix:** protect each live run with a held lock and prune only directories whose corresponding lock can be acquired without waiting, with the lock lifetime covering receipt validation and cleanup. Alternatively omit opportunistic pruning until safe abandoned-run detection is available. Add a synthetic test that holds a run live with an old directory timestamp while another gate starts.

### Minor — the public id validation differs from the claimed contract and its unit test

**Locations:** `node-sink/src/lib.rs:152`, `node-sink/src/lib.rs:164`, `node-sink/src/lib.rs:232`.

`write_receipt` filters an explicitly empty environment value into `None`, bypassing the validation that its helper's unit test exercises. It successfully writes `.receipts/<test>` with `run -` instead of rejecting the set-but-empty id. Also, `Path::components()` normalizes away trailing separators and trailing dot components: `run-a/`, `run-a//`, and `run-a/.` all pass the supposedly single-component validation and name the same directory as `run-a`.

**Evidence:** a scratch executable linked to the reviewed node-sink library called the public `write_receipt` with those environment values; all four returned `Ok(())`. The tested traversal values (`..`, `.`, `a/b`, `/abs`, `../x`) returned `InvalidInput`.

The gate's generated ids do not contain these forms, so this does not create a gate false PASS. **Suggested fix:** distinguish an unset id from a present empty/invalid id and require the raw string to equal its sole normal component; exercise the public environment-reading path, preferably in a subprocess to avoid global environment races.

## Integrity and concurrency assessment

- `mktemp -d` atomically reserves a fresh directory. An existing directory or symlink at a candidate name causes a retry rather than reuse; ordinary concurrent gates therefore have separate receipt paths.
- The embedded run-id check remains in place, independently of the directory name. The positive/exact quirk count, executed/applicable equality, required-manifest count, minimum floor, and applicable range checks are unchanged. Neither DEC-30/DEC-32 required-seed handling nor CS0 §9.4's count assertions are weakened.
- Receipt reading and the existing private-file helpers still follow symlinks. They are not hardened against an actor who can replace paths inside the owner-private tree, but that is pre-existing behavior; such an actor can already rewrite receipts. An ordinary stale/foreign receipt with a different id is rejected. Pruning fails closed rather than accepting stale evidence in the reproduced case.
- A deterministic public synthetic barrier probe paused run A after its first receipt, let run B complete, and resumed A. The reviewed code produced two receipts and exit 0 for both runs; the parent script produced a stale quirk receipt and exit 1 for A under the same schedule. This confirms the new namespace fixes the original ordinary-concurrency race.
- The concurrent selftest launches both runs on the same synthetic tree and checks both exit statuses, both pairs of PASS lines, and cleanup. A second successful run under `strace` confirmed actual overlap: the two replay binaries ran during 12:32:01.534906–12:32:01.851927 UTC and 12:32:01.839806–12:32:02.141817 UTC. It is a scheduling-dependent smoke test, not a deterministic barrier test; it does not cover the active-pruning scenario above.

## Validation

- `TMPDIR=/dev/shm/bl49-r1-sol-tmp contrib/node/check-private-tests.sh --selftest`: PASS, both the requested run and the traced repeat.
- `TMPDIR=/dev/shm/bl49-r1-sol-tmp cargo test --locked -p node-sink`: PASS, 14 unit tests and doc-tests.
- `TMPDIR=/dev/shm/bl49-r1-sol-tmp cargo clippy --locked -p node-sink --all-targets -- -D warnings`: PASS.
- `cargo fmt -p node-sink -- --check` and `bash -n contrib/node/check-private-tests.sh`: PASS.
- `TMPDIR=/dev/shm/bl49-r1-sol-tmp contrib/node/selftest-checks.sh`: PASS, exit 0, `all selftests passed`.
- Scratch public-id, deterministic overlap, and active-pruning probes: reproduced the findings above. No reviewed tracked files were modified. The detached worktree, scratch probes, and dedicated `/dev/shm` temporary tree were removed after validation; only this requested review file remains.

Verdict: **CHANGES-REQUIRED**.
