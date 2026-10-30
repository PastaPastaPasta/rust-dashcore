# BS6b — Sol review, round 2

2026-10-09. Narrow confirmation of the r1 findings under `REVIEW-COMMON.md`.
PLAN §7 handling: codenames only; DEC-141 applies.

Reviewed base-sdk `738c1c6..c156bc6` in `bsdk-wt/bs6b` and rust-dashcore
`0fd303b0..cb8071cf` in `rdc-wt/bs6b`. Also compared campaign checkout
`acb06e88` with the reviewed head. The code and committed seed repairs pass this
confirmation. The required campaign remains incomplete, and its old executable's
eligibility does not establish that its runs used the corrected seeds.

## Findings by severity

### R1-B1 — blocker, remains open: incomplete campaign and unestablished corrected-seed credit

**Locations:** `docs/node-program/PLAN.md:517`; rust-dashcore
`node-fuzz/targets.toml:83`, `node-fuzz/run.sh:397`,
`node-l1/seeds/gen.py:153`.

The receipt snapshot read during r2 reports **running**, **3.0396 / 288 credited
CPU-hours**, **0 / 12 complete legs**, and **0 divergences, 0 crashes, 0 sink
failures**. Its completed slices are type 7: 1.0047 CPU-h / 17,224,262 executions;
type 8: 1.0173 CPU-h / 82,794,116 executions; type 9: 1.0176 CPU-h / 15,735,490
executions. The other nine legs report zero completed-slice CPU-hours and
executions. These counters do not include an assurance about any in-flight slice.
The required 24 clean CPU-hours per target have not been supplied.

**Revision eligibility: the implementer's executable-reachability argument is
correct.** The target registration at `node-l1/src/lib.rs:215` sends each of the
12 targets to `payload::compare_payload`. At `node-l1/src/payload.rs:118`, that
function calls the pinned Core payload oracle and `SpecialPayload::decode`, then
checks full consumption and `core_bytes`. The latter, at
`node-l1/src/payload.rs:59`, calls `BaseCodec::encode` and the existing BLS
serialization adapters. It does not call any provider signing API. For example,
base-sdk `pkgs/primitives/src/payload/proregtx.rs:192` encodes through the
unchanged `encode_unsigned`; its signing entry point is separate at line 296.
The same separation holds for the other three provider types. The base-sdk range
changes those signing entry points, their tests and documentation, without
changing the decoder, encoder, BLS implementation or AU txid implementation.
The rdc payload comparison's only source change is a comment; the remaining
changes are seed generation/data, test assertions and dependency pins.
Consequently, using executable revisions `acb06e88` / `738c1c6` is **not by
itself** a reason to discard payload-campaign receipts.

**Seed eligibility is a separate requirement.** The campaign checkout's public
seeds are still its clean, old set. The reviewed range adds **481 binary seeds**:
types 1, 2, 3, 4, 5, 6, 7, 9 and 12 gain respectively 24, 51, 24, 51, 70, 168,
17, 68 and 8 files. Types 8, 10 and 11 retain byte-identical public seed sets.
The farm starts slices through `node-fuzz/fuzzctl.py:1368`; the runner copies the
selected public directories at `node-fuzz/run.sh:397` and passes the persistent
public-seed directory to libFuzzer at line 447. No updated seed-directory
override is supplied by the farm's slice call. Another run could have populated
that persistent directory, so the checkout alone does not prove the private
working corpus is deficient. However, the permitted receipt status/counts do
not establish when the corrected inputs were present. A successful final-head
smoke or seed replay cannot retroactively establish that earlier farm slices
were seeded with every repaired field/outcome pair.

**Merge decision and fix:** retain eligible clean credit for types **8, 10 and
11**; their code paths and public inputs are unchanged. For types **1–7, 9 and
12**, count old-executable receipts only when there is auditable evidence that
the corrected full seed set was loaded before the credited slices began.
Otherwise, supply 24 clean CPU-hours per affected target starting with the
corrected seeds, while retaining the existing mutated working corpus. The old
executable may be reused for that run; rebuilding solely for these unreachable
signing changes is unnecessary. Require complete per-leg receipts and zero
unresolved findings before merge. Neither a revision substitution alone nor
the currently running receipt closes R1-B1. This review grants no campaign
waiver.

### Major

None remain from r1; no new major finding in this narrow range.

### Minor / nit

No remaining r1 minor finding or new nit in the reviewed range.

## Confirmation of the repairs

- **R1-M1 resolved.** `node-l1/seeds/gen.py:113` now selects samples by
  `(label, legacy outcome, basic outcome)`. Independent comparison against
  base-sdk's actual `bls_wrapper.json5` confirms every manifest source key and
  sample byte: **50/50 public-key pairs** and **74/74 signature pairs**.
  There are **34 field/flag rows**: 11 key rows with 50 pairs each and 23
  signature rows with 74 pairs each, totaling **2,252/2,252 cells**. Every row
  includes `(valid, valid)`. The floor at `node-l1/tests/l1_payload.rs:156`
  independently enumerates the required layouts, fields and flags, checks file
  digests, locates each field, and compares the entire seed with the base plus
  its expected sample. Regeneration into scratch reproduced all **2,267 binary
  seeds and `COVERAGE.json` byte for byte**.
- **Floor negative controls passed.** With scratch seed directories mounted
  over the seed path in a read-only process namespace, the actual compiled
  floor test passed its baseline and failed all five controls: a missing field
  row, a missing dual-valid signature cell, a missing seed file, altered payload
  bytes with a newly correct SHA-1 filename and manifest reference, and a cell
  redirected to its base seed. Each negative returned an assertion failure,
  not an infrastructure failure. The reviewed checkout was never edited.
- **R1-m1 resolved:** unsupported versions are refused (DEC-141).
  Locations: base-sdk `pkgs/primitives/src/payload/mod.rs:99`, the four provider
  signing APIs, ProRegTx's dependent methods, and
  `pkgs/primitives/tests/payload_sighash.rs:243`. The new regression test passed.
  The Errors documentation is present.
- **R1-m2 resolved.** `node-l1/seeds/core_cbtx_v1.json:1` records two mainnet
  coinbases with heights, block hashes, txids and payload bytes. Read-only RPC
  through `contrib/oracle-cli` independently confirmed both coinbases, at
  heights **1028160** and **1088639**, against the mainnet oracle. The generator
  uses the first as the CbTx v1 base and commits it under both flags; the second
  remains an additional provenance-backed fixture. The coverage floor and
  differential replay confirm the previously missing layout.
- **Sighashes and AU v2 remain green.** Primitives nextest reran the
  237-transaction Core-signature corpus and four txid/instance-hash fixtures.
  Node-l1 reran four AU fixtures and their eight boundary edits each against
  the pinned v24 oracle. The unchanged implementation also matches Core tag
  `v24.0.0-rc.3`, `src/primitives/transaction.h:392` and
  `src/primitives/transaction.cpp:82`. R1's D1 remains limited to verification
  of Core-made signatures; direct oracle sighash vectors remain the O2a-EV
  follow-up, rather than a completed direct-preimage comparison.

## Independent validation

All test and generator invocations used `TMPDIR=/dev/shm/bs6b-r2-sol-tmp`
(verified mode **700**), `RUSTC_WRAPPER=` and `EIO_BACKEND=posix`. Cargo used
12 build jobs and an unset `CARGO_TARGET_DIR`; each worktree kept its own
`target/bs6b-r2-sol`. Node-l1 ran without the private-seed environment or an
unpinned-oracle allowance. Logs and scratch controls are under
`/work/scratch/bs6b-r2-sol/`. The temporary directory was deleted after testing.

- base-sdk: `cargo nextest run --config-file maint/nextest.toml --profile=ci
  -p dash-primitives --features full --target-dir target/bs6b-r2-sol`:
  **292 passed, 0 skipped**; nextest marked six passing tests as leaky.
- rust-dashcore: `cargo test --locked -p node-l1 --features oracle --test
  l1_payload --target-dir target/bs6b-r2-sol -- --nocapture`: **4 passed,
  0 failed**. This includes all public-seed replays, the floor, the 1,024-case
  property test and AU boundary comparisons.
- Both repositories: `cargo fmt --all --check` and reviewed-range
  `git diff --check`: **passed**.
- Exact generator replay, independent source/manifest equality, five negative
  floor controls, and the two mainnet provenance checks: **passed**.

The implementer's workspace nextest total, CodeQL result, conformance 77/0 and
smoke 12/12 were not independently rerun in r2. They are not substituted for
the campaign requirement. Only the receipt's status/pass-fail and aggregate or
per-leg counts were exposed; no private inputs or finding details were inspected.
No reviewed branch was modified, committed or pushed, and no GitHub action was
taken. This report is the requested uncommitted output on `node/integration`.

**Verdict: CHANGES-REQUIRED.** The implementation repairs are confirmed; R1-B1
remains the merge blocker under the credit rules above.

**Sol gate satisfied: no**
