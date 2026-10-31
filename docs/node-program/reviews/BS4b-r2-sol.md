# BS4b r2 — Sol confirm

**Verdict: APPROVE.** M1, M2a, m3 and m4 are closed. The M2b campaign has the correct per-target CPU budgets and eligible sources; completion remains a separate merge gate.

Reviewed `a723c851 → 6d509894` on `node/bs4b-decode-evidence`, read-only at `/work/home/agent/workspace/rdc-wt/bs4b`. The range contains **five commits**, including `12f353e2`, rather than the four stated in the brief. All five were included. Contract: `REVIEW-COMMON.md`; prior report: `BS4b-r1-sol.md`; requirements: PLAN's BS4b acceptance and A1 §1.4. PLAN §7 handling observed: codenames only; private receipt inspection limited to pass/fail and count fields. No branch changes, commits, pushes or GitHub comments.

## Findings by severity

- **Blocker:** none in this confirm. The unfinished campaign remains a merge condition, as the brief explicitly separates it from the Sol gate.
- **Major:** none remaining in the reviewed fixes.
- **Minor:** none remaining from r1.
- **Nit:** none requiring a fix.

## M1 — closed

`node-l1/tests/support/bsdk_rows.rs:89` gives rows explicit coverage metadata. `BLS_FIELDS` specifies **45 type/version/field triples**, each with zero, valid-basic, valid-legacy and neither-scheme classes: **180 cells**. `floor_problems` at `:195` requires the field-kind outcome under **both flags** for every cell; a missing row, wrong agreement or divergence fails the floor. The live census and oracle-free replay both use this check.

The version floor agrees with the Core v24 serializers inspected at `v24.0.0-rc.3` and the pinned shim revision `a9a483e51efdadeee37cf40ff8697da62b3cd012`:

| Types / fields | Versions required |
|---|---|
| 1–4, operator key/signature | 1–3; Core reads only the version outside that range |
| 5, best ChainLock signature | 3–5; the serializer reads it for version ≥3 |
| 6, quorum key and both signatures | commitment versions 0–5; index inclusion is separately version-dependent |
| 7, MnHF signature | 0–3; signature uses the fixed basic scheme |
| 9, asset-unlock signature | 0–3; field is read independently of payload version |
| 12, operator key | 0–3; field is read independently of payload version |

The synthetic witnesses preserve Core-derived non-null points and fail construction rather than silently omit a field. This closes the null-commitment and missing-version holes identified in r1. Lazy/global/eager expectations are enforced separately.

I ran all committed oracle-free negative controls: retaining one DDE#10 row per type, dropping a class, dropping each required triple, and changing outcomes/divergence results. I also compiled a scratch-only extension of the replay test against the reviewed code and recorded verdicts. It independently deleted **each of the 180 individual cell rows**, then flipped **each of the 360 individual flag outcomes**. Every alteration was caught. This includes deleting one row of any of the nine BLS-bearing types, and expands outcome-flip coverage to all four classes and both flags. Probe: `/work/scratch/bs4b-r2-sol/floor_probe.rs`; result: `floor-probe.log`.

The 416 rows reproduce **320 accept / 96 reject**, including DDE#10 **145 accept / 35 reject**, with **0 divergences**. Their recorded inputs remain digest-bound, and the live oracle reproduces the recorded verdicts.

## M2a — closed

The full-history leg is genuinely base-sdk judged against Core's payload deserializer. `conformance/src/bsdk_judges.rs:75` calls `node_l1::core_payload`, which calls `Oracle::payload_decode` once with `false` and once with `true` (`node-l1/src/lib.rs:227`). The conformance leg then calls the shared base-sdk `judge_payload` for each corresponding flag (`conformance/src/bsdk_judges.rs:82`). No verbose-RPC sampling policy limits this loop.

I inspected `oracle_payload_decode` at the pinned Core shim's `src/test/oracle/oracle.cpp:2230`: it executes `ds >> obj` and `out << obj` on Core's actual payload classes, reports `ds.empty()`, and holds `ProcessStateMutex` plus `ScopedBlsScheme` throughout decoding and re-serialization. Thus the reference fields and consumption verdict come from Core, independently of base-sdk. The wrapper faithfully distinguishes rejection from an oracle error; errors produce harness mismatches.

For headers, blocks and transactions, the reference is Core's serialized best-chain bytes (`chain_side`), rather than a fresh call to the object-deserializer oracle. This is sound for the accepted full-history corpus and is stated in the evidence. Arbitrary-byte acceptance/rejection remains exercised by the live BS4b tests and campaign targets.

I audited every progress entry in `/work/scratch/bs4b/full/{mainnet,testnet}/`, alongside the run logs, run identities, summaries and empty mismatch files. The entries cover contiguous heights from zero, have linking boundary hashes and one execution identity, and have per-unit `bsdk_blocks == blocks`, `bsdk_txs == txs`, and `bsdk_payloads == sum(special_txs)`. Totals recompute exactly:

| Chain | Heights | Blocks / headers | Transactions | Payloads | Payload judgments |
|---|---|---:|---:|---:|---:|
| mainnet | 0–2,094,670 | 2,094,671 | 53,395,175 | 1,332,050 | 2,664,100 |
| testnet | 0–1,569,315 | 1,569,316 | 8,585,124 | 2,248,740 | 4,497,480 |
| **Total** | | **3,663,987** | **61,980,299** | **3,580,790** | **7,161,580** |

Both runs have **0 mismatches**, **0 uncovered/incomplete units**, **0 unverified units**, and no suppressed records. The `bsdk_*` counters establish that the feature actually ran. The execution revision is `b368bf46`; the conformance sources, judges, oracle sources and pins, and dependency pins are unchanged through `6d509894`. Types 10–12 occur in neither history; mainnet also has no types 7–9. Their coverage comes from the rows/census. This conclusion uses the supplied full-history executions with independent code and coverage-log verification.

## m3 and m4 — closed

- **m3:** the DDE#3 rows include the 9-byte MAX_SIZE, 2^32 and u64::MAX forms. `extra_payload_at_max_size` (`node-l1/tests/l1_bsdk_decode.rs:121`) constructs actual 33,554,431-, 33,554,432- and 33,554,433-byte payloads and requires accept, accept and reject respectively. The live test passed.
- **m4:** the large-seed skip has been removed (`node-l1/tests/l1_bsdk_decode.rs:361`). Every one of the 277 seeds receives the requested mutation attempts, including the large block seed. The previously accepted exclusion of negative transaction versions remains. My 400-mutation run reproduced the documented target counts over **111,311 judged inputs**, with **0 divergences**.

## M2b / R1-B — configuration confirmed; completion pending

The active user unit `node-fuzz-farm-bs4b-r1` runs from `/work/home/agent/workspace/rdc-wt/bs4b-campaign` at `8250ad6a`, with `--night bs4b-r1 --hours 24 --cores 4 --jobs 2 --slice-minutes 30 --min-slice-minutes 5`, nice 19, and a four-core CPU quota. Its registry assigns weight 1 to each of these four v24-only legs and zero nightly weight to the other targets. The farm creates a **96 CPU-hour** plan (`hours × cores`), yielding **24 CPU-hours / 86,400 CPU-seconds per target**. The receipt budgets confirm those shares.

Campaign eligibility is established by an empty diff between the campaign checkout and `6d509894` for `node-l1/src`, the public BS4b seeds, fuzz target sources, runner/farm sources, oracle sources/pins, and root dependency manifests/lockfile. The test/doc differences do not change the running fuzz comparisons or seeds. The runner measures user plus system CPU time, repeats segments until the slice budget is reached, and the farm credits clean pass/interrupted slices since the last finding; failed/lost slices earn no credit and a finding resets credit.

The supplied persisted receipts still reported zero slices. I ran the supplied public ledger reader `/work/scratch/bs4b/campaign-receipt.py` with output redirected to scratch, retained only count and pass/fail fields, and left the original receipts and campaign state untouched. Snapshot: **2026-10-09 21:49:05 UTC**. Only accounted completed slices are credited:

| Target | Credited CPU-s | Credited CPU-h / 24 | Passed slices | Executions |
|---|---:|---:|---:|---:|
| `diff_bsdk_tx_decode` | 10,918.4 | 3.033 | 3 | 211,925,746 |
| `diff_bsdk_block_decode` | 7,318.1 | 2.033 | 2 | 83,857,782 |
| `diff_bsdk_header_decode` | 7,320.6 | 2.034 | 2 | 166,288,224 |
| `diff_payload_decode` | 10,914.1 | 3.032 | 3 | 48,025,666 |
| **Total** | **36,471.2** | **10.131 / 96** | **10** | **510,097,418** |

All four have **0 divergences, 0 crashes and 0 sink failures**; **0/4 targets complete**. Count-only copies are in `/work/scratch/bs4b-r2-sol/campaign-counts/`.

The budget configuration satisfies R1-B, but the current launch also has a **24-hour wall deadline** (`node-fuzz/fuzzctl.py:1419`). Build time, contention and reserved-window pauses can leave credit below the 96-hour plan. If that happens, resume the same night and unchanged eligible sources until every target has ≥86,400 clean CPU-seconds. Refresh the authoritative receipts before merging. An active unit or its total systemd CPU usage is insufficient to close this separate gate.

## Independent validation

Cargo commands used `RUSTC_WRAPPER=`, `CARGO_BUILD_JOBS=12`, nice 10, the reviewed worktree's own `target/`, and `TMPDIR=/dev/shm/bs4b-r2-sol-tmp` in mode **700**. The private-data environment and unpinned-oracle override were unset for tests. Logs and the independent probe are under `/work/scratch/bs4b-r2-sol/`.

| Check | Result |
|---|---|
| `cargo test --locked -p node-l1 --features oracle --test l1_bsdk_decode --test bsdk_recorded -- --nocapture` | 9 passed; includes live boundary, census, oracle recording and floor controls |
| `cargo test --locked -p node-l1 --test bsdk_recorded -- --nocapture` | 5 passed without oracle |
| Independent exhaustive floor probe | 180 individual deletions and 360 individual flag flips caught |
| `BS4B_CENSUS_CASES=400 cargo test --locked --release -p node-l1 --features oracle --test l1_bsdk_decode census -- --nocapture` | 111,311 inputs; 0 divergences |
| `cargo test --locked -p dash-conformance --features bsdk-judges` | 77 passed |
| `python3 -I node-l1/scripts/gen-bsdk-seeds.py <reviewed worktree> --check` | 277 seeds match |
| `python3 -I node-fuzz/fuzzctl.py check` / `selftest` | Registry passes; 51 self-tests pass |
| Full-history progress/summary audit | Both chains contiguous and linked; all BS4b counts match; 0 mismatches |
| Campaign source/seed eligibility and count-only receipt refresh | Pass; 10.131/96 CPU-h credited |
| `git diff --check a723c851 6d509894` | Pass; reviewed worktree remains clean |

**Verdict: APPROVE. Campaign completion remains the separate merge gate.**

Sol gate satisfied: yes
