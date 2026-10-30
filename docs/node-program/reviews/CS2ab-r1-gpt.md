# CS2 phases A+B — implementation review r1

Reviewed 2026-10-08 by GPT-6.1-Sol, following `briefs/REVIEW-COMMON.md` and the `dash-core-security-auditor` skill. Scope: the six commits `3807f318..d88308d8` on `node/cs2-pow-subsidy`, with base `a900c163`, in `/work/home/agent/workspace/rdc-wt/cs2-pow-subsidy`. HEAD remained `d88308d80835f83b6412971464ad981da2b9646d`; the worktree remained clean. No implementation edits, commits, pushes, GitHub comments, or running-node changes.

Read the charter, process, decisions, plan, A1's relevant architecture/FP sections, and `designs/CS2-pow-subsidy.md` rev1, including DEC-33/DEC-35. Compared the implementations and their surrounding call sites with Core **v23.1.8 (`728f5055836c6d29806412fc7223ac8fe05af991`)** and **v24.0.0-rc.3 (`0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`)**. This approves the provisional integer implementation and its explicit deferrals, not completed phase C/D consensus parity.

## Findings

### N1 — nit: payment overflow comments contradict the approved domain

**Locations:** `dash-consensus/src/payments.rs:7`, `:328`, and `:362`.

The module says that no product overflows on mainnet/testnet/regtest; the test comments say that no chain reaches the multiplication-overflow cases. Design rev1 §5 explicitly establishes otherwise: accepted devnet high-subsidy factors can reach the platform/operator overflow domains, and the ordinary-network fee bound can reach the operator/owner multiplication domain. For example, a masternode reward of `1_575_000_000_000_000` duffs, within the stated fee-derived bound, multiplied by 10,000 exceeds `i64::MAX`.

The implementation already uses the required wrapping multiplication and is correct here. The inaccurate comments could mislead a later maintainer into replacing that arithmetic with checked or widened multiplication, or excluding required cases from tests.

**Suggested fix:** align these comments with rev1 §5: identify the products that cannot overflow separately from the operator/owner/platform products that can; describe the latter tests as checking the agreed release-binary semantics, not unreachable inputs. No arithmetic change is needed.

**No blocker or major findings.**

## Consensus comparison

| Surface | Checked result |
|---|---|
| PoW dispatch, `pow/mod.rs:29` | Preserves Core's order: devnet minimum-difficulty window, BTC, post-BTC no-retargeting, two min-difficulty branches, KGW deferral, DGW. Both network parameter sets match the recorded Core values. Testnet switches at next height 4002; devnet at 4001; neither reaches KGW. |
| BTC, `pow/btc.rs:9` | Retarget modulo and ancestor height, strict `> 2*spacing` min-difficulty test, raw-word walk-back to a retarget boundary/non-limit word, no-retargeting behavior, sequential clamps, 32-bit multiplier conversion, division, numeric powLimit cap, and final compaction match both pins. Regtest retains BTC's own min-difficulty behavior before reaching the post-BTC no-retargeting branch. |
| DGW, `pow/dgw.rs:13` | Requires last height at least 24. Uses last through last−23, `avg=(avg*count+target)/(count+1)` after the first target, wrapping multiply/add, and truncation at every step. The measured span is **23 intervals**, the target **24 spacings**. Clamps, multiplier truncation to `u32`, unsigned division, cap, and compact encoding match. The only relevant PoW change between the pins is explanatory DGW commentary. |
| Post-BTC min difficulty, `pow/mod.rs:50` | Strict `>7200` returns powLimit before strict `>4*spacing` decodes the previous target, wraps its multiplication by ten, caps, and compacts. Does not canonicalize stored bits. Timestamp operands widen to `i64`, matching `GetBlockTime()`. |
| `CheckProofOfWork`, `pow/mod.rs:76` | Rejects decoded negative, zero, overflow, and numerically above-powLimit targets before comparing the little-endian hash with the target. Equality passes. Non-canonical target encodings are accepted/rejected by their decoded value and flags, not raw-word ordering or re-encoding. Also inspected base-sdk's compact decoder, shift guards, byte order, and arithmetic at `33fcc59`. |
| Subsidy, `subsidy.rs:187` | Fixed base exactly when V20 or devnet past its high-subsidy window. All other formula inputs defer. Previous-height decline count, fixed-point termination, devnet factor after decline, strict treasury start, `/10` versus `/5`, and block/treasury projections match both pins. Genesis comes from the parameter view; the outer function forwards previous bits/height and the caller's `DeploymentActiveAt` flag. |
| Payments limit, `subsidy.rs:305` | Same valid-height predicate, artificial bits choice, previous height `h−1`, direct V20-height comparison, treasury projection, and wrapping multiplication by cycle as `CSuperblock::GetPaymentsLimit` at both pins. |
| MN schedule, `payments.rs:158` | Each increment truncates separately; strict increase boundaries; period 8 skipped; BRR start advances to the next superblock cycle even when activation is itself cycle-aligned; all 19 permille entries and final clamp match. V23's V20 bool maps to `era != Classic`; both non-Classic V24 eras use `block_value*3/4`. `MnRewardEra::after` checks V20 before MN_RR. |
| Platform/operator/owner amounts, `payments.rs:214`, `:226`, `:233` | Platform is specifically **`wrapping_mul(375)/1000`, then MoneyRange**, allowing a wrapped product whose quotient is in range. Operator uses wrapping multiplication then signed truncating `/10000`; owner payouts use the same operation for all but the last and give the remainder to the last. Script checks and filtering of non-positive outputs remain the payout assembler's responsibility, matching the agreed scope. V23 has one owner payout, not V24's payout lists. |
| Shared amounts, `payments.rs:254` | V24's `ProRataFloor` casts the three signed inputs to unsigned 64-bit values, multiplies/divides in `arith_uint256`, takes low 64 bits, and converts to CAmount. Two unsigned 64-bit factors fit exactly in `u128`; the Rust quotient and low-word conversion therefore match. The share-total/paid/remainder wrapping behavior follows the design. Empty and single-share lists avoid division; zero total with multiple shares returns the explicit error where Core throws. The Core unit-test assertions at `evo_sharedmn_tests.cpp:522` are faithfully transcribed. This API does not exist at the V23 pin. |

Core loop-counter overflow heights and configurations where Core divides by zero or overflows a signed timespan remain the documented excluded domain. The optimized decline agrees on the defined domain; its fixed-point result outside that domain is not presented as a Core comparison. Missing ancestry takes the design's documented broken-contract path; all actual-chain probes obey the ancestry contract.

## Proof of the replacement division

For **every** `a` in `[0,2^256−1]` and nonzero `d` in `[1,2^64−1]`, `pow/mod.rs:118` returns `floor(a/d)`, the same quotient as Core and dash-num. This is an arithmetic proof, not an inference from random tests.

Let `B=2^64`. Read the four limbs from most significant to least significant. Inductively let the already-read prefix be `P=dQ+r`, with `0≤r<d`. Appending limb `x`, `0≤x<B`, gives:

`PB+x = d(QB) + (rB+x)`.

Since `rB+x < dB ≤ (B−1)B < 2^128`, `cur=(rem<<64)|word` cannot overflow `u128`. Its quotient `q=floor(cur/d)` satisfies `0≤q<B`, so conversion to `u64` loses no bits; its remainder `r'=cur%d` again satisfies `0≤r'<d`. Appending `q` to the quotient establishes `PB+x=d(QB+q)+r'`. The zero prefix starts the induction; after four limbs, the assembled little-endian bytes are exactly the unique quotient of `a/d`.

Core's 32-bit-limb path and general division above `u32::MAX` compute that same unique floor quotient. Thus changing limb width does not change the result. For `d=0`, the helper returns zero like dash-num, whereas Core throws; this is **not** claimed as equivalent Core behavior. No ported call can pass zero: BTC's timespan is validated positive; DGW divides by counts 3..25 and `24*positive_spacing`. The constructor also prevents signed overflow in the timespan bounds.

The existing 20,000-numerator test, with ten divisor choices per numerator including random large divisors and `u64::MAX`, passed. Additional exact-tag C++ probes exercised the large-divisor DGW path described below.

## `FpPending` seam

There is no placeholder numeric subsidy or KGW answer. `block_subsidy_helper` returns `FpPending` for every non-fixed base, including cases that could already converge to the same integer; the projections and outer function propagate it. Off-devnet negative previous heights take `Unreachable`, and genesis remains independent of the formulas. The DGW/BTC/min-difficulty answers precede the KGW deferral only where Core's dispatch does too.

At mainnet heights through 68589, unequal raw header/expected bits defer. Equal words return `Ok(true)` deliberately: `ConvertBitsToDouble` produces the same non-negative value from each identical word. A finite value subtracts to zero; infinity subtracts to NaN; neither makes Core's literal `abs(n1−n2)>n1*0.5` rejection true. This also holds for zero/subnormal difficulty from unchecked artificial compacts. It is an exact shortcut, not a fabricated tolerance answer. After 68589 and off mainnet, raw equality is the implemented rule.

Checked 1,680 subsidy seam inputs over five network/override selections, formula/decline/treasury/high-subsidy boundaries, unusual raw compacts, and both V20 states. All integer answers matched both pins. Checked next-work at last heights 15198, 15199, 34138, and 34139: Core computed all four; Rust deferred exactly the two KGW cases. Checked 20,004 raw compact words for equal/unequal inputs on each side of the tolerance switch.

## Validation rerun

- `cargo build -p dash-consensus --all-features --locked` — passed.
- `cargo test -p dash-consensus --all-features --locked` — **40 tests passed**, including recorded oracle cases, Core vectors, both quirks, property suites, division, decline, raw bits, and FP confinement.
- `cargo clippy -p dash-consensus --all-features --all-targets --locked -- -D warnings` and `cargo fmt --all --check` — passed.
- `contrib/node/check-pure.sh`, `contrib/node/check-base-sdk-pins.sh`, and `python3 .github/scripts/ci_config.py verify-groups` — passed.
- Rebuilt a copy of `/work/scratch/cs2/diff` in `/work/scratch/cs2ab-r1-gpt/diff` against the reviewed source, using `cargo build --release --offline --locked`. Ran `cs2-diff diff 1000 3290525697`: **10,702 next-work, 11,000 PoW checks, 11,000 MN-payment, 10,000 platform-share, and 12,901 subsidy comparisons agreed with both pins**. Four KGW and 2,905 formula cases explicitly deferred. 294 short windows and 814 loop-overflow inputs were oracle refusals, not parity passes; 380 subsidy inputs were Core assertion cases.
- Large-spacing differential: compiled `pow.cpp`, `arith_uint256.cpp`, and `chain.cpp` from clean `git archive` snapshots of **each exact release tag**, with a small driver, and compared their 224 results with Rust. Covered spacings 86,401; 178,956,970/971 (Core's wide-divisor transition); 536,870,911/912 (timespan multiplier truncation); `2^31`; and `i64::MAX/72`, with minimum-difficulty blocks set to 4000. Included zero/overflowing/high targets and forward/backward/extreme timestamps. **Zero mismatches.** The pinned oracle's network-selection guard rejects all spacings above 86,400; that shim restriction is not Core's startup rule, so those cases required the direct source driver.
- Share arithmetic: 20,000 random signed totals/share lists compared with separate general `Arith256` multiplication/division and low-word conversion. Owner split compared with `i128` multiplication narrowed to machine width. Passed. These are independent arithmetic probes, not calls to a live payout oracle.
- Header sweeps: mainnet **0..2,551,766**, testnet **0..1,568,459**. PoW passed for every header whose hash is available from its successor, excluding each dump's tip. Exact-rule next-work results matched all corresponding headers. Mainnet's 49,649 non-KGW early next-work results matched both Core pins; all 18,940 KGW evaluations explicitly deferred. Post-V20 block/treasury subsidy comparisons passed against both pins at **563,991 mainnet** and **663,360 testnet** heights. All pre-V20 subsidy heights deferred.

Evidence and reproducible probe sources are under `/work/scratch/cs2ab-r1-gpt/`: `differential.log`, `edges.log`, `history-main.log`, `history-test.log`, `pure.log`, `pow_reference.cpp`, both `pow_reference-*.txt` result sets, and `diff/src/bin/review_edges.rs`.

The scratch harness's original `src/sweep.rs:69` incorrectly expects `FpPending` even for equal early-mainnet bits. The unmodified checker reproduced that failure. For the successful mainnet sweep I changed **only the reviewer-owned copy** to expect `Ok(true)` on equality and `FpPending` otherwise; neither the original harness nor the reviewed branch was changed. This checker assertion should be updated when the harness is brought into the repository.

## Remaining gates

The phase C floating-point implementation, exhaustive all-u32 sweeps, tolerance boundary differential, and KGW platform-invariance tests are still pending as designed. Live shared-payout differential awaits the O1b entry point; history of operator/multiple-owner/shared payouts and the phase D fuzz budgets are not established by this review. I did not rerun the complete workspace or perform a new Core-versus-Rust performance benchmark. These limits do not invalidate approval of the explicitly provisional A+B slice.

**Verdict: APPROVE-WITH-NITS.**
