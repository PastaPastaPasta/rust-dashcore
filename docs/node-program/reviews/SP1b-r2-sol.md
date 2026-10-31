ACCEPT

## Findings

None.

## Review

DEC-583's downgrade stands. In `contrib/node/sp1b-timing/timing.py:603-666`, `pick()` still selects by completed plus in-flight counts and assigns IDs at dispatch. The independent audit of `accept1/samples.tsv` found exactly 10,000 sessions for each node/kind, fixed ID residues (dashd f6=0, dashd s30=1, rdashd f6=2, rdashd s30=3), and dispatch-position KS D=0.0001 for both kinds. The first-to-last-quarter medians match the manager's evidence and show no consistent node-specific drift. The remaining load limitation is narrower and is addressed by S1's running pause and per-session metadata.

S1 is correct:

- `timing.py:637-652` serializes the load gate, pauses dispatch while the one-minute load is at or above `--max-load`, and lets all workers queue behind the pause. The `short()` condition ends a pause when no session remains, including when the last in-flight sessions finish under a high load.
- `timing.py:654-666` records the session's Unix dispatch time and one-minute load at dispatch. `Ledger.record()` writes both to every successful sample row at `timing.py:519-527`.
- `timing.py:552-571` accepts both the new seven-column rows and r1's five-column rows, while ignoring an unterminated killed-run row. `drop_partial_line()` removes that row before resume.
- `timing.py:744-792` consumes either row shape without changing the KS inputs. Re-running `ks` on the acceptance copy produced a byte-identical `ks.tsv`, including all D values, p-values, quantiles, and verdicts.

No statistical regression was found. The acceptance run was not rerun, as required by the brief; its existing data remains valid for the compatibility and KS reproduction check.

## Commands and results

- `TMPDIR=/work/tmp CARGO_TARGET_DIR=/work/scratch/sp1b-r1/target-sol python3 /work/scratch/sp1b-r1/wt/contrib/node/sp1b-timing/timing.py selftest` — passed (`selftest ok`).
- `TMPDIR=/work/tmp CARGO_TARGET_DIR=/work/scratch/sp1b-r1/target-sol python3 -m py_compile /work/scratch/sp1b-r1/wt/contrib/node/sp1b-timing/timing.py` — passed.
- `bash -n /work/scratch/sp1b-r1/wt/contrib/node/sp1b-timing/nightly.sh` — passed.
- `CARGO_TARGET_DIR=/work/scratch/sp1b-r1/target-sol cargo test -p dashd-masternode --test sync_transcripts -- --test-threads=1` — 10 passed, 0 failed.
- Independent audit of `/work/scratch/sp1b/accept1/samples.tsv` — 40,000 sessions, 10,000 per node/kind; residues and dispatch-position D as above; quarter medians agree with DEC-583.
- `timing.py ks` on a reflinked acceptance copy — output `ks.tsv` is byte-identical to `/work/scratch/sp1b/accept1/ks.tsv` (SHA-256 `648e3f89ff84267302f63b873af0af55ff2cf15b5ae207adb64e484fb64e868f`).
- `git -C /work/scratch/sp1b-r1/wt diff --check f8c61e079..b7dadce03 -- contrib/node/sp1b-timing/timing.py contrib/node/sp1b-timing/nightly.sh` — passed.
- Full acceptance run — not rerun, per the brief.
