# BS7 closure review r1 — Sol

Reviewed base-sdk **3229f62b9b1c107fe65bee14dfb9511b1ebcfba5**, 33 commits on **a1d4dd3**, and rust-dashcore **1aa86c7b5cdb88bf17ae11ce2b0e6bc4b9e4b25b**, 22 commits on **878ad553**. Read REVIEW-COMMON.md first, then the current PLAN/DECISIONS, BSX area D and its pre-merge instruction, DDE-SWEEP, and both earlier BS7 reviews. Reference source: **v24.0.0-rc.3**. Review worktrees were detached under `/work/scratch/bs7-closure-sol/`; tests used `TMPDIR=/dev/shm/bs7-closure-sol-tmp`, `RUSTC_WRAPPER=`, `EIO_BACKEND=posix`, and 12 Cargo build jobs.

## Findings

No blocker or major finding.

### F1 — minor: differential command exits can report success after failures

**Locations:** rust-dashcore `contrib/node/dde-probe/bs7_diff_framing.py:163`, `bs7_diff_headers2.py:448`, `bs7/src/fuzz.rs:277`.

The framing command writes mismatches/errors but never fails the process. The headers2 command fails on mismatches and missed controls but omits harness errors. The fuzz command fails on a failed classifier but does not fail on its own mismatch counter. A caller checking only the exit code can therefore accept a failed run. An independent negative control replaced the framing runner with one deliberately mismatching result: the JSON reported `mismatches: 1`, and `main()` returned normally (process exit 0).

**Fix:** after writing the report, return nonzero for mismatches, harness errors, or an unexpectedly empty run; preserve the classifier check. Add an exit-status negative control. This does not invalidate the zero-mismatch JSON evidence below: I checked the summaries, and the private receipt wrapper also checks fuzz summary fields. It should be fixed before these standalone commands become unattended O3/CI gates.

### F2 — nit: the count-at-count documentation is stronger than the implementation

**Locations:** base-sdk `pkgs/p2p_core/src/msg/headers.rs:34`, `pkgs/p2p_core/src/msg/headers2.rs:352`; rust-dashcore `docs/node-program/DDE-SWEEP.md:208`.

The handler uses `count.into_len(data.len())`, not the number of complete headers the payload can hold. Counts larger than the remaining **byte count** fail immediately. Other physically impossible header counts can still enter the loop. Independent API reproduction: count 4, a whole zero header (`0x38` plus 80 bytes), then one compressed zero header (`0x01` plus 38 bytes). The 121-byte payload consumed all 121 bytes, called the hash callback once, and returned EOF attempting header three; it did not fail immediately after the count.

**Fix:** qualify the documentation as “count larger than the remaining byte count.” The current code implements the catalogue's requested fail-fast byte bound, preserves reject verdicts, and bounds reservations; this is not an allocation or honest-traffic defect.

### F3 — nit: obsolete cursor fixture packaging remains

**Locations:** base-sdk `pkgs/p2p_core/tests/core_cursors.rs:19`, `pkgs/p2p_core/corpus/core_cursors.json5:1`; catalogue `designs/BSX-leniency-catalogue.md:523`.

The catalogue literally asks to drop the cursor corpus/test. They remain under their old names, although the test has correctly become a verdict comparison and checks consumption only on success. Failed-case cursor metadata is consequently unused. This is redundant packaging, not retained read-until-failure behavior.

**Fix:** remove the obsolete fixture, or rename/trim it into a v24 getblocktxn verdict fixture and record that substitution in the catalogue. Keep the useful overflow and valid-encoding regressions.

## Closure checks

- **D-17/D-18:** `git diff --exit-code a1d4dd3 HEAD -- pkgs/types pkgs/primitives` passes. Both directories are byte-identical to the base. No `BaseCodec::BYTE`, chunked-reader, or read-until-failure change survives this increment.
- **D-09:** byte-count precheck restored on the handler path; standalone codecs additionally use minimum-entry bounds. Oversized protocol counts retain the explicit 2000/8000 gate. F2 qualifies the claim precisely.
- **D-07/D-08:** first-header fields and version offsets are checked strictly. Unused flag bits remain byte-preserved; version-list ordering, duplicates, seven-entry eviction, unsigned time subtraction, modular reconstruction, and reply/announcement modes follow Core. `Compress` emits a whole first header and only offsets it found in the list, so these strict checks do not reject honest Core emissions. Core citations: `src/primitives/block.h:154`, `src/primitives/block.cpp:57`, `src/net_processing.cpp:4818`, `:4850`, `:5323`, and the announcement path at `:6498`.
- **D-13:** the count is bounded before reservation; differences retain checked addition/u16 conversion. Private indexes, the validated constructor, and serde `try_from` prevent duplicate/decreasing indexes from bypassing the encoder invariant. Core: `src/blockencodings.h:22–54`, `src/net_processing.cpp:4704`.
- **D-14:** merkleblock/cfilter/cfheaders/cfcheckpt are typed through dispatch and encode; `Unread` and its macro arm are gone. Real Core replies pass the corpus and fresh capture checks. Layouts agree with `src/merkleblock.h:93`, `src/blockfilter.h`, and `src/net_processing.cpp:3483`, `:3520`, `:3563`. Core's server ignore policy is documented for dashd-p2p before decoding.
- **D-11:** sendcmpct reads exactly 0/1 and writes those bytes. Core's sender serializes a bool; strictness only changes crafted noncanonical flags. Core: `src/net_processing.cpp:4274`, `src/serialize.h:238`.
- **D-04/Q-26 and Q-25…Q-28:** reserved receive slots fail with `UnknownShortId`; named slots, long-form validity and contents limits, INIT/baseline/platformban send bands, MIN_MASTERNODE, CURRENT removal, the owned V2 decoder, and always-known inventory type 34 agree with the rulings. Regenerated the short-ID table from Core source, and the exhaustive 256-slot/seven-version-band tests pass. Core: `src/net.cpp:935`, `:989`, `:1028`, `:1297`, `:1494`, `:1566`; `src/protocol.h:497`; `src/protocol.cpp:281`.
- **Allocation:** the hostile-count tests pass for all BS7 vector-bearing decoders, including the four reply types. Generic vectors use the unchanged payload bound and bounded reservation batches; header handler reservations additionally have fixed entry caps. GetBlockTxn reservation is bounded by actual payload bytes; its decoded indexes cannot overflow u16. No new reservation trusts an unbacked MAX_SIZE count.
- **Earlier findings:** the V2 owned-payload API gap and unchecked getblocktxn construction are fixed. Failed-cursor/error-text parity requests are superseded by DEC-98. Headers2 now checks declared count, successful consumption, header/hash observations, connection state, and negative controls. Tx/block comparison checks successful consumption and node/Core re-encoding, compares reject verdicts, and routes counted-apart cases through the opaque classifier contract.

## Boundaries and upstream readiness

BS7's own retained tolerances are the catalogue's kept wire forms: D-05 V2 long-form 0x7f/contents bounds, D-08 wire images and reconstruction arithmetic, and D-10 raw bloom flags. Trailing/empty-handler reads and other inherited behavior are classified separately in area A.

This is **approval of the BS7 increment into the fork**, not a finding that the entire a1d4dd3 fork has finished BSX convergence. Inherited (c) behavior remains, notably CoreBool fields (`msg/control.rs:19`, `:29`, version/SML fields), nonzero headers tx_count (`msg/headers.rs:139`), and tolerant version/pong/empty-payload handling. These are A-12/A-16/A-18/A-19/A-26 and are assigned to the separate per-unit convergence process in catalogue §7.5. They are neither newly introduced BS7 leniencies nor reclassified as “keep.” A whole-crate claim that every remaining leniency is required or kept would be false.

The additions follow no_std/alloc, typed errors, feature propagation, repository formatting/commit conventions, and explicit breaking-change changelog entries. The catalogue still requires upstream reshaping during the rebuilt unit series: BaseCodec for V1Header, a Result-returning GetBlockTxn constructor, Numeric/normal SDK construction for ShortTxId, root exports for limits, and moving peer policy helpers such as disconnects/request_interval out of the SDK. Those are declared BSX-PR actions; this branch is not ready for direct unsplit upstream publication.

DDE-SWEEP §5 records the node-side strict-headers2 reconstruction/penalty consequence, sendcmpct noncanonical-flag policy, and four-command ignore list as P2a backlog. None is claimed implemented here. The version-zero/regtest qualification is retained. GovSync remains the named BS9b exception. Instance-hash selection for v24 compact-block construction remains the caller/BS6b responsibility; BS7 exposes keys and raw-hash short-ID calculation (`blockencodings.cpp:29–50`).

## Independent validation and evidence limits

- Workspace nextest, all features: **1347 passed, 1 skipped** (nextest reported two leaky tests). No default features: **1115 passed, 1 skipped**.
- p2p_core clippy, all targets, both all-features and no-default-features: **passed**. Workspace no-default clippy: **passed**. Workspace all-features clippy fails on 10 dead-code diagnostics in unchanged dash-pow Groestl/AES code; `pkgs/pow` is identical to a1d4dd3. The clean-clippy claim is reproduced for the changed crate, not that broader feature combination.
- `uv run --frozen --extra dev python maint/lint_all.py`: **exit 0**. Semgrep: 24 rules, 245 files, zero findings. CodeQL and Nix checks skipped because their binaries are unavailable; I cannot independently confirm the reported 266-file CodeQL run. Explicit commit lint over a1d4dd3..HEAD: **33/33 pass**. Formatting and diff whitespace checks pass.
- Built bs7-tool with its committed locked 3229f62 pin. Built/tested its oracle feature: **four comparator negative-control tests passed**.
- Fresh isolated v24 pair: **87 handler cases and six framing cases exactly reproduce the committed verdict fields**, including read classification, connection outcome, successful consumption, exception text, version/mode, and replies. **708 fresh Core messages** round-trip without failure; capture timing differs from the committed **722 messages/108 compact pairs**. The committed corpus, 13 captured headers2 messages and four 300-header oracle samples all pass nextest.
- Independent live headers2 differential, seed 9321, 180 seconds: **318 cases, zero mismatches/errors**, including **87 strict refusals**; **868/868 negative controls rejected**.
- Independent live framing differential: seed 9531: **100 cases, zero mismatches/errors**.
- Root/nested pin check: **passed**, root pin still 481f8c3; BS7 helper pin 3229f62. Node-check selftest: **all selftests passed**. Opaque receipt-wrapper **synthetic selftest passed**; no files under `~/private` were searched or opened.
- **33 base-sdk and 22 rust-dashcore commits cryptographically verified** against the configured agent signing public key using a scratch allowed-signers file. Both reviewed trees are clean; base-sdk changes only p2p_core/Cargo.lock; the rust-dashcore root manifest and lockfile are unchanged. The remaining cursor fixture has a test consumer, rather than being an untracked stray.

The full final 1979-message/891-control headers2 run, 1000-message framing run, 112,788,665-transaction/3,366,095-block fuzz run, and private PASS receipts are **reported evidence** in the brief/DDE-SWEEP, not independently rerun at their full budgets. I verified `git diff bfb7c93 3229f62`: only SendCmpct's Hashable implementation and a test rewrite differ; decoder source is unchanged, so the documented runs apply to this decoder. The classifier stays opaque; the actual private PASS receipts are accepted as manager-provided attestations. This review does not close PLAN's later 24-CPU-hour-per-target milestone or full-chain fuzz/performance gates.

Review scratch, detached worktrees, own dashd processes, and RAM scratch were cleaned up. No branch edits, commits, pushes, or GitHub messages.

**Verdict: APPROVE-WITH-NITS. The Sol gate is satisfied for the sequential Opus-high pass under DEC-92.**
