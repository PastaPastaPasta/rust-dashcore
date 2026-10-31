# BS2 independent review — GPT round 1

**Verdict: CHANGES-REQUIRED**

Reviewed `node/bs2-conformance-harness` at `c66335fd325518df9d112f2846ee6c6019c8d426`, using `git diff node/integration...HEAD` (16 commits), in `/work/home/agent/workspace/rdc-wt/bs2-conformance-harness`. No branch files were changed and no commits were made. This review prioritizes false negatives because this harness is intended to gate consensus work.

## Findings

### 1. BLOCKER — a resumed failing gate returns success

**Location:** `conformance/src/main.rs:285`; `conformance/src/run.rs:250`, `:260`; `conformance/src/report.rs:315`.

**Scenario:** A unit containing a library/oracle mismatch is recorded as completed. Re-running the same command with `--resume --fail-on-mismatch` skips that unit and accumulates zero current-run mismatches. The exit decision consults only `result.stats`, despite the just-written summary retaining the failure. Thus CI can turn red into green simply by resuming, without fixing or rechecking anything. This affects both drivers; blk mode rechecks the index but still skips completed block units.

**Reproduced:** A local mock RPC served the real `testnet-7000` fixture with a one-second disagreement in `getblock 1.time`. The first invocation exited **1**; its resumed invocation exited **0**, made no additional RPC calls, and still printed `1 mismatches` / `header.field:time:both` in the summary. Evidence: `/tmp/bs2-r1-review/gate_probe.py`, `gate-first.log`, `gate-resume.log`.

**Fix:** Define a gate over the requested validated coverage, including reused checkpoints and their non-informational failures. Require complete coverage under the current check identity. Do not use only newly processed counters to decide a resumed gate. An incremental informational run may retain current-run counters, but it must not represent a successful conformance gate while retained failures remain.

### 2. HIGH / MAJOR — checkpoints do not identify the code, check policy, or canonical chain

**Location:** `conformance/src/report.rs:229`, `:315`; `conformance/src/run.rs:245`, `:299`.

**Scenario:** Metadata pins only network/source/chunk/blocks-directory. Neither harness/library revisions nor feature/build identity nor `verbose_every`/`verbose_types` are bound to completed units. A consensus implementation can change and `--resume` will reuse results from the previous implementation. Increasing oracle coverage also skips units that never received the added checks. RPC unit keys contain heights only: a reorg below the old tip leaves the replacement canonical blocks entirely unchecked. `done_units()` accepts any historical completion, rather than checking whether that completion remains valid. The blk selection signature likewise hashes offsets, lengths and two boolean flags, but not block identities, contents or the actual expected hash value; same-size content replacement is invisible.

**Reproduced policy gap:** With a deliberate `getblock 2.locktime` disagreement, the fixture run with `--verbose-every 0` exited 0. Resuming with `--verbose-every 1` also exited **0**. A fresh output directory with `--verbose-every 1` exited **1**. Evidence: `/tmp/bs2-r1-review/policy_probe.py`.

**Fix:** Give each checkpoint an explicit validation identity covering harness/library code and features, oracle network/genesis, and comparison policy. Invalidate incompatible checkpoints. Anchor RPC work to a consistent chain snapshot and store/revalidate checkpoint boundary hashes, rewinding through reorgs. Bind blk checkpoints to their actual selected block identities and reference values, and detect changed file contents or generations. Starting new output directories manually is an operational workaround, not a sound default for a consensus gate.

### 3. HIGH / MAJOR — block counts do not establish coverage; the summary erases real findings

**Location:** `conformance/src/report.rs:395`, `:409`, `:416`.

**Scenario:** The summary chooses the latest run with at least as many blocks as the previous run in a group. Equal-sized disjoint ranges inside one RPC chunk are not supersets. For example, checking heights 0–4 and then 5–9 with chunk size 10 replaces the first result and discards its mismatches, although none of those heights were rechecked. Conversely, a genuinely new failure discovered in a narrower recheck is ignored whenever an older wider run has more blocks. The same count-based assumption is unsafe for changed selections of a blk file. `unaccounted` remains zero because discarded mismatches and discarded counters disappear together.

**Reproduced against the public Reporter API:** A five-block 0–4 unit containing five findings followed by a clean five-block 5–9 unit changed summary mismatches **5 → 0**, with reported heights 5–9. A subsequent failing one-block recheck also left the summary at **0**. Evidence: `/tmp/bs2-r1-review/probe.rs`, `/tmp/bs2-r1-review/count-out/`.

**Fix:** Track actual coverage and identities, not just cardinality. Replace a result only when the replacement validates the same covered blocks or a proven superset under the same check identity. Ideally retain the latest result per block/check and union coverage. Newly discovered failures in partial rechecks must remain visible until those same blocks/checks are successfully revalidated.

### 4. HIGH / MAJOR — the advertised two-way oracle comparison has derivable-field blind spots

**Location:** `conformance/src/verbose.rs:398`, `:430`, `:497`, `:500`; `conformance/src/verbose.rs:169`, `:278`.

**Scenario:** `not_derivable()` exempts all addresses and every descendant of `.addresses`, `.payouts`, and `.shares`. These are not all non-derivable chain/index values: service addresses and ports, payout/share scripts, share amounts, and destination key IDs come from transaction bytes. The ProTx flatteners omit these oracle representations. Comparing consensus encodings between the two libraries does not establish semantic agreement with Core when both libraries share a reversible misinterpretation. In particular, the live v24 oracle exposes `shares[].amount`, `refundScript`, and `rewardScript`; these are unambiguous integers/hex, yet all are skipped.

The reverse walk also examines an oracle object's leaves only if the library emitted at least one path for that top-level object. An entirely unhandled oracle payload object therefore silently disappears, without an explicit unsupported-coverage finding. Unknown-type failures currently catch several such cases, but that does not make this generic omission rule safe as handlers evolve.

**Reproduced:** The clean pre-v24 Evo fixture `regtest-feature_protx_version-201` yielded zero findings after replacing its oracle Core/Platform address endpoints and owner/voting addresses with wrong values. Adding a new top-level oracle payload object with a derivable scalar also yielded zero findings. Evidence: `/tmp/bs2-r1-review/probe.rs`.

**Fix:** Normalize oracle endpoints and destinations to raw addresses/ports/key IDs/scripts using the known network, compare integer and script fields directly, and explicitly enumerate remaining unsupported coverage. Remove blanket subtree exemptions. Reverse comparison should flag unrecognized derivable objects; suppress duplicate failures based on a recorded decode failure for that side/object, rather than based on whether a flattener happened to emit a field. Until implemented, narrow the README/results claims of field-for-field agreement.

### 5. MEDIUM — malformed terminal blk data is silently treated as EOF

**Location:** `conformance/src/blkfile.rs:145`, `:161`, `:195`.

**Scenario:** When resynchronization finds no later magic, `scan_bytes` breaks without reporting the nonzero discarded suffix. A nonzero suffix shorter than eight bytes is also never examined by either scanner loop. A damaged final frame or partially written framing can consequently vanish without a `blk.scan` finding. If the anchored chain itself is still present—for example, the discarded record is stale—anchoring does not expose the omission even with `--include-stale`.

**Reproduced:** A correctly framed 80-byte header followed by twenty nonzero garbage bytes returned **one record and zero scan issues** from `blkfile::index`. Evidence: `/tmp/bs2-r1-review/probe.rs`, `/tmp/bs2-r1-review/scan/`.

**Fix:** Report every non-padding discarded byte range, including terminal garbage and short framing. Distinguish a retryable live-file tail from permanent corruption, and do not claim that an incomplete scan is complete coverage. Retain the existing stored-zero-tail handling for preallocation and xor files.

### 6. MEDIUM — blk-mode txid reference is wrong for the very v24 rule documented here

**Location:** `conformance/src/check.rs:356`, `:501`; `conformance/src/run.rs:443`.

**Scenario:** Without a per-block oracle, the txid reference is unconditionally `sha256d(raw transaction)`. Core v24 AssetUnlock v2 instead hashes the serialization with the trailing quorum signing information zeroed. Consequently blk mode blesses the current two libraries' wrong txid against a wrong reference, and will report a correct library's future stable txid as a divergence. The current fixture still has independent block-merkle/decode failures, so this is not a claim that its whole block presently passes; it is a broken individual check and an impediment to fixing the documented library bug.

**Verified:** `feature_asset_locks` h=969 tx1 has oracle txid `ed6d78c04f2e9ba2f1c81536cde6885725448ae7083b7580b21031cf565590b8`, whereas full-serialization sha256d is `9471462b6675e0f770c0844aeffc323b22a7b49a98a344def73026d2c55af7ec`. Independently zeroing the final 132 bytes reproduces the oracle txid. Core rc.3 `src/primitives/transaction.cpp:82` implements that rule.

**Fix:** Use an independently implemented Core-compatible reference hash rule, including its exact type/version/payload-length predicates, or fetch oracle txids for affected blocks. Add a regression that demonstrates agreement between RPC and blk modes and that accepts a correctly implemented stable txid.

### 7. MEDIUM — an inverted RPC height range succeeds after checking nothing

**Location:** `conformance/src/run.rs:247`; `conformance/src/main.rs:283`.

**Scenario:** `--from` greater than the effective `--to` produces zero units without an error or coverage finding. A mistaken CI range therefore returns success even with `--fail-on-mismatch`.

**Reproduced:** `--from 7001 --to 7000 --fail-on-mismatch` exited **0** with no new checks. Evidence: `/tmp/bs2-r1-review/gate_probe.py`.

**Fix:** Reject inverted ranges before opening/altering output. Distinguish a legitimately fully resumed range from an invalid/empty selection, and validate the number and identity of covered requested blocks.

### 8. MEDIUM — the output cap does not bound mismatch memory on a shared box

**Location:** `conformance/src/run.rs:433`, `:434`, `:449`, `:458`; `conformance/src/report.rs:334`, `:339`; `conformance/src/check.rs:127`.

**Scenario:** Every selected blk file is loaded wholesale, and all its mismatches are collected and concatenated before the Reporter applies its line cap. Each mismatch can hold another complete transaction hex, plus encoded values. On a dense corpus with widespread payload divergences, many parallel files can retain much more mismatch data than input data. A cap of 1,000 lines per group does not prevent this allocation. Default worker count is all cores. This is especially relevant to the intended use: a new consensus format can produce failures in nearly every block/transaction. Summary tail repair also reads entire accumulated logs on every reopen (`report.rs:240`).

**Fix:** Stream/batch per-block reporting, or cap retained examples while maintaining complete per-group counts and ranges during checking. Bound simultaneously loaded file bytes and choose a conservative shared-box worker/memory default. Locate the last complete NDJSON line from the end of the file rather than reading an entire historical log. This finding is based on allocation paths; I did not run an OOM stress test on the shared host.

## Validation and oracle spot-checks

- `CARGO_BUILD_JOBS=12 cargo test -p dash-conformance`: **PASS** — 9 library tests, 1 blk integration test, 5 fixture tests, 9 v24 finding tests; no failures (24 total).
- `CARGO_BUILD_JOBS=12 cargo clippy -p dash-conformance --all-targets -- -D warnings`: **PASS**.
- No full-chain conformance pass was run. Fault probes used individual committed fixtures and synthetic report/scanner inputs outside the worktree.
- Shared oracle height queries succeeded: mainnet 745,201 and testnet 768,379 at query time. These are status checks, not claims of validated coverage.
- For the three requested repro checks, copied the existing small regtest datadirs into `/tmp/bs2-r1-review/`, launched one isolated rc.3 dashd at a time with no P2P, one validation thread and 32 MiB cache, and queried it **through `contrib/oracle-cli mainnet` with explicit `-regtest`, `-conf`, `-datadir`, and `-rpcport` overrides**. The wrapper itself accepts only mainnet/testnet; overriding dash-cli flags avoids modifying it or touching the shared oracles. Each temporary daemon was stopped through RPC and its process exit awaited.

| Document repro | Live oracle check | Attribution assessment |
|---|---|---|
| RDC-5 / BSDK-3: `feature_masternode_shares`, h=273, tx1 | Raw block equals fixture. ProRegTx v3, `operatorReward=0`, `pubKeyOperator=83d7356e…`, `earlyPeriodBlocks=30`, `earlyPenalty=5000000000`; payload JSON equals fixture. | Supported. Existing snapshot confirms dashcore decodes/reencodes but gets reward/key wrong and omits the two fields; base-sdk reports payload decode failure. |
| RDC-6: `feature_masternode_payout_shares`, h=125, tx2 | Raw block equals fixture. ProUpServTx v3, txid `ae738cc6…`, Core P2P endpoint `127.0.0.2:12209`; payload JSON equals fixture. | Dashcore rejection and base-sdk success are supported. The broader claim that base-sdk matches **all** JSON fields is too strong: endpoint/operator payout fields are excluded from the oracle comparison (finding 4). |
| RDC-8 / BSDK-5: `feature_asset_locks`, h=969, tx1 | Raw block equals fixture. AssetUnlock v2, index 801, requested height 968, oracle txid `ed6d78c0…`; payload JSON equals fixture. Independent stable hash calculation agrees. | Supported: both libraries compute the full-serialization hash; base-sdk's merkle calculation consequently diverges. Blk-mode's individual txid reference remains wrong (finding 6). |

Evidence scripts and captured live JSON: `/tmp/bs2-r1-review/oracle_spotcheck.py` and `oracle-<chain>-<height>.json`. The assertions in the existing v24 tests agree with these observed library findings. I did not re-count all five historical regtest run reports or regenerate their functional-test chains.

## Other observations and limits

- The independent splitter follows Core's signed low-16-bit version, type-dependent payload presence and canonical CompactSize rules. Decode failures are reported, and fallback per-transaction decoding preserves checks for later transactions when a block decode fails. I found no additional clean-pass error-swallowing path in that logic.
- xor offsets, raw-zero preallocation, download-order linking and normal stale-fork selection have useful tests and look sound for ordinary inputs. Dual-library header hashes help preserve linkage if only one hash implementation diverges. Missing anchors/root blocks fail rather than silently truncate coverage.
- Live-file indexing and checking are separate reads. Re-reading a truncated/replaced file can panic at `run.rs:439` rather than produce a retryable failed unit; selection signatures do not detect same-length mutations. Use checked ranges and a file-generation/snapshot contract. Oracle anchoring protects ordinary best-chain reads, but unanchored mode cannot establish Core acceptance or greatest-chainwork selection: it selects maximum height. Validation failures in that mode should not automatically be described as bugs on a chain accepted by dashd.
- Sampling is intentional, but the default omits most CbTx/QcTx semantic oracle checks. Re-encoding, hashes, and library-to-library agreement do not prove every unsampled field matches Core. Gate fixtures should use `--verbose-every 1`; reports should expose per-type/version oracle coverage and explicitly state what sampling leaves unproven.
- The results document's three checked bug repros are reliable. Its statements that all harness false negatives are fixed (`BS2-conformance-results.md:39`, `:95`) and that the summary can mechanically certify full coverage (`:344`) need updating after the above fixes. Its “both libraries' stateless validation” wording should distinguish dashcore's header-PoW check from base-sdk's much broader `Block::check()`.
- Code is generally readable, the source separation is sensible, and targeted tests/lint pass. The missing adversarial tests are in checkpoint/gate semantics and oracle completeness, rather than basic codec happy paths.

**Approval status: CHANGES-REQUIRED.** Fix the false-success gate, checkpoint validity, coverage-aware accounting, and oracle-field omissions before relying on this harness as a consensus-work gate.
