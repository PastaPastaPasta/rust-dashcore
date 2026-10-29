# CS5b — Sol r2 confirmation

Reviewer: GPT-6.1-Sol, xhigh. Date: 2026-10-09.

**APPROVE. All findings from both r1 reports are closed.** No new consensus divergence or unexpected panic was found within the documented Core input domain.

Reviewed `node/cs5b-inputs` at **061b2e137a9bacd0c1c7841f4bb4ca3116655087**. Read `REVIEW-COMMON.md`, both r1 reports, PLAN's CS5b row and §7 handling, and the relevant integration contracts. Applied the code-review-validator workflow. The implementer worktree remained read-only; this report is uncommitted.

Used `git range-diff ea501ed7..3e9a0660 abdfc2f8..061b2e13` to distinguish the rebase from the fixes. The old r1 final commit maps unchanged to `555aaa16`; reviewed `555aaa16..061b2e13` for the fix round. The earlier commits' rebase adjustments adapt the public header-window test support. Inherited CS3 changes are outside this confirmation.

All Core citations below refer to **Dash Core v24.0.0-rc.3**, commit **da567d7dba945143eee4d2ce1621c21b32089989**, read with `git show` from `/work/home/agent/workspace/dash`.

## Findings by severity

- **Blocker:** none.
- **Major:** none outstanding.
- **Minor:** none outstanding.
- **Nit:** none outstanding.

## R1 closure

### Major — Opus MF1: closed

`dash-consensus/src/tx/inputs.rs:188` and `:217` now return `ViewOr::Reject(MissingCoin { input })` when a spent coin is absent. Storage failures remain `ViewOr::View`. The old `spent_coin` expectation is removed. The module and `ARCHITECTURE.md:111` correctly distinguish block and mempool ordering.

Core asserts on missing coins in `consensus/tx_verify.cpp:140–141`; its block ordering runs `ProcessSpecialTxsInBlock` at `validation.cpp:2663`, and `evo/assetlocktx.cpp:188–189` rejects an asset unlock with inputs. Mempool input existence is checked at `validation.cpp:970–979`, before its special-transaction check at `:1103`. Returning a typed error outside that valid call domain is an intentional reliability improvement.

Retried the original malformed-unlock counterexample independently: input fee check accepts with fee 1,000, and both sigop functions return `MissingCoin` at input 0 without panicking. Both also report input 1 when only the later coin is missing. An unlock without inputs counts zero; counting without P2SH reads no coins. Permanent tests at `tests/inputs_and_locks.rs:260`, `:268` and `:312` pass, including storage-error separation.

### Major — Opus MF2: closed

`tests/inputs_and_locks.rs:475` tests time-cutoff equality directly and through calculated locks. The Core-regtest fixture now contains a lock ending at **1,700,081,000**, exactly the parent MTP: Core rejects it with `non-BIP68-final` in the mempool and `bad-txns-nonfinal` in the block path. Its accepted companion ends at **1,700,080,999**. Independently recalculated these times from the regenerated headers, coin heights and transaction sequences.

This matches Core `consensus/tx_verify.cpp:103–108`. The replay additionally asserts the calculated boundary values. Changing the time comparison from `>=` to `>` now fails both the unit suite and the Core-recorded replay.

### Minor — Sol M1: closed

`dash-consensus/src/coins/utxo_hash.rs:124` checks txid order before flushing the previous group or writing the new coin. Retried `[2; 32]` followed by `[1; 32]`, and returning to an already flushed txid: both trigger the documented guard. After catching each panic in scratch, finalization equals the hash of the accepted prefix, proving the rejected push did not alter the hash state. Descending output indices within one txid still sort correctly.

Core's grouping and sorted output serialization are `kernel/coinstats.cpp:72–86` and `:123–146`. The guard enforces its cursor-order precondition. The permanent ordering and output-index regression tests pass.

### Minor — Sol M2: closed

`dash-consensus/ARCHITECTURE.md:106` now documents input-check preconditions and fees, special-transaction ordering, fatal view failures versus rejections, sigop errors, mutated sequence heights, complete ancestry and parent MTP, both UTXO hash APIs, cursor order and quirk 33. Checked those contracts against Core `consensus/tx_verify.cpp:41–115`, `:132–207`, `kernel/coinstats.cpp:51–98`, and `crypto/muhash.cpp:314–322`.

### Minor — Opus N1 and N2: closed

- **N1:** `tests/muhash_vectors.rs:58` permanently compares **144 products and 16 quotients** over 12 boundary values against Core's recorded arithmetic. It checks final hashes, numerator bytes and denominator reset. This reaches Core `crypto/muhash.cpp:126–142`, `:181–215`, `:261–275` and `:314–322`. Both reported non-equivalent MuHash mutants are now caught. The pinned generator reproduced the fixture byte for byte.
- **N2:** the regenerated UTXO snapshot contains compression codes **0–5**, plus ordinary scripts, and `tests/utxo_hash_vectors.rs:111` explicitly asserts that coverage. All six sampled heights match Core for both hashes; incremental MuHash removal/insertion also passes. The generator creates P2PKH, P2SH, compressed and valid uncompressed P2PK of both parities, and an off-curve uncompressed P2PK stored as a script. Core judges the resulting set through `dumptxoutset` and both `gettxoutsetinfo` modes.

### Nit — Opus N3, N4 and N5: closed

- **N3:** `inputs.rs:68–85` retains the first-pass `CoinRef`s; each input coin is read once during `check_tx_inputs`. Missing-input precedence is preserved, and the second-pass expectation is gone.
- **N4:** the reported anchors now match the pinned source: input checks `tx_verify.cpp:163`, lock calculation `:41`, granularity `transaction.h:151`, multiplication `muhash.cpp:181`, finalization `:314`, and index insertion/removal `coinstatsindex.cpp:167/:188`.
- **N5:** `inputs.rs:62` uses the existing `Tx::is_platform_transfer()` predicate.

The r1 recommendation to retain public asset-unlock probes is also implemented at `node-l1/tests/l1_inputs.rs:167`. Four original records agree with Core, followed by 96 signature-byte mutations each: **388 comparisons**, with **50 both-accept and 334 both-reject** among the mutations, no divergences or out-of-domain skips.

## Independent validation

Ran in the CS5b worktree with `nice -n 10`, `RUSTC_WRAPPER=`, `CARGO_BUILD_JOBS=12`, and a mode-0700 `TMPDIR` under `/dev/shm`:

```text
cargo test -p dash-consensus -p node-l1 --features node-l1/oracle --no-fail-fast
310 passed, 0 failed; exit 0
```

This includes 18 input/lock tests, the new MuHash boundary replay, BIP68/112/113 replay, UTXO-hash replay and L1 differential suites. The public asset-unlock test was also rerun with its outcome counts visible. Four separate scratch counterexample tests passed. `cargo fmt --all --check` and `git diff --check 555aaa16 061b2e13` passed.

Public mutation probes compiled altered source copies under `/work/scratch/cs5b-r2-sol/`, using the required test build's dependency artifacts; the branch's sources were untouched:

| Mutation | Independently caught by |
|---|---|
| Time cutoff `>=` → `>` | Input/lock unit tests and Core locktime replay |
| Remove height lock's subtraction of one | Input/lock unit tests |
| MuHash `is_overflow` `>` → `>=` | Core MuHash boundary replay |
| Delete MuHash's carry-triggered second `full_reduce` | Core MuHash boundary replay |
| Disable txid-order assertion | Independent r1 counterexample test |
| Coinbase maturity `<` → `<=` | Input/lock unit tests |

Regenerated all three fixtures into scratch and verified exact equality with their committed bytes:

| Fixture | SHA256 |
|---|---|
| 34 Core locktime cases, 141 headers | `4e8bec1e375fbdc0954c0d2411aaafd5d94ec23879338ed5446159ddbae4ee08` |
| Six Core UTXO snapshots and hash pairs | `6d17c687510cbde2e94c2e4633012be9fde533ec14ea284d2a6e70a1e721c7b0` |
| 144 Core MuHash products, 16 quotients | `de92576dab267b271f3198175107e378b37a1bb00196883ea67e7bd878a63d29` |

The regtest generators used a fresh archive of Core's functional framework at the pinned tag and the release `dashd`, whose SHA256 is `e204e3163185b1bffcd3bf3532c0957d8cc11019eaf8f3dbc3b2fc82a7ab5bb4`. Each ran in its own fresh datadir and ports; both nodes stopped. MuHash used the unchanged Core crypto/serialization sources in the built shim tree `a9a483e51efdadeee37cf40ff8697da62b3cd012`; the generator's tag/source-cleanliness checks passed. Its C++ program calls Core's actual `MuHash3072` implementation, rather than calculating expected results with Rust. This confirms the provenance and reproducibility claims.

## Private receipts and limits

Ran `check-private-tests.sh` through a filter exposing only pass/fail and counts. No private inputs, corpus contents or private coverage mechanics were inspected:

```text
quirk_01_embargo_1: receipt PASS 522 cases
private_seed_replay: receipt PASS 4324 seeds (279 required)
check-private-tests.sh: exit 0; PASS receipts 2; FAIL receipts 0
```

The integration gate's three-receipt floor remains a merge requirement after the prerequisite rebase; it is not a CS5b finding. This confirmation does not claim that the full integration gate is green. The full-workspace **3169/0** and fuzz-smoke **21/21** claims remain implementer gate evidence, not independent reruns in this narrow review.

No new panic was found for callers satisfying the Core contracts. The txid-order assertion intentionally rejects a malformed cursor; input-check coinbase/output preconditions and sequence-lock height/ancestry preconditions remain as documented. BIP112 replay still uses the disclosed test-local checker and must be rerun with CS1-b's production checker when integrated. The optional oracle extension observing mutated heights remains future coverage work; r1 had already independently verified that side effect against Core, and the unchanged implementation and height-mutation unit tests remain consistent with it.

Evidence is retained under `/work/scratch/cs5b-r2-sol/`. The temporary `/dev/shm` directory was removed after the checks.

## Verdict

**APPROVE.** Both r1 must-fix findings, both Sol findings, and all five Opus non-blocking findings are closed. The new boundary evidence is Core-judged and reproducible. Proceed to the next sequential review and the manager's remaining merge gates.

**Sol gate satisfied: yes**
