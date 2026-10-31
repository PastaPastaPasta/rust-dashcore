# O1 and O1b: post-merge GPT review

**APPROVE-WITH-NITS. No blockers or majors remain.** B1 and M1–M4 from my round-1 review are closed. DEC-44's KGW restriction is safe for both pinned releases. One previously reported minor remains in the O1b script adapters; its concrete fix task is below.

Reviewed 2026-10-08 under DEC-42c/DEC-49 and `briefs/REVIEW-COMMON.md`, independently and read-only. The implementation is the O1/O1b merge `8c43bf92` plus the R2 entry `ca66c271`, inspected in `rdc-wt/integration`, initially at `f29edb27` and finally at `50ba9117` after concurrent manager updates. Both commits are ancestors of the final HEAD; the crate and oracle build/repro scripts have no changes since the O1 merge. Core pins: v23.1.8 plus shim at `350ce242a59c19782dc9eebc8e23dead0a2e16ee`, and v24.0.0-rc.3 plus shim at `23769258c239e69282569e9f9e219ce9e2c4ee2e`. The two shims are identical. References below are relative to those worktrees; Core line numbers use v24 unless specified.

## Round-1 closure

- **B1 — closed.** `dash-oracle-sys/build.rs:92` calls `build/archive.rs::snapshot`, which validates manifest identity, copies the archive to Cargo's private output directory, hashes the copy, and links only the authenticated copy. A mismatch fails by default. Both plain and fuzz digests are recorded. The eight archive-check tests passed, including substitution with the original manifest, a bit difference, wrong manifest identity, and flavor-specific verification. I independently hashed all four installed archives: every hash equals its corresponding pin column. The explicit development opt-in marks results unpinned; `Oracle::new` requires the opt-in at runtime too.
- **M1 — closed.** `contrib/node/build-oracle.sh:97` clears ccache settings and disables it, fixes locale/TZ, applies an absolute build-root prefix map, sets `SOURCE_DATE_EPOCH`, strips debug data, and refuses path leaks and mismatched fresh artifacts. I inspected the round-2 independent two-root log and independently rehashed both retained v23 plain archives under `/work/scratch/o1-review-r2/repro/{a,second-root}/`: both equal the current pin `22f00737694ab069e22d01d2cee3fc749b8c5c3814023a0a086d230c03829463`. This closes the original v23 diagnostic-path counterexample. To keep this review short, I did not repeat multi-minute clean builds or the prior fuzz reproducibility experiment.
- **M2 — closed.** Network selection no longer applies the BTC adjustment-interval restriction globally. `NextWorkPreconditions` applies it only when Core actually dispatches to BTC; a zero interval there would divide by zero. Devnet spacing 86401 is admitted and usable for DGW and other network operations. The stock-release devnet fixtures and `a_devnet_spacing_above_the_timespan_is_accepted` passed on both pins. The post-BTC minimum-difficulty and DGW overflow checks follow Core's evaluation order.
- **M3 — closed.** The successful suite now exercises standalone VARINT decoding against undo bytes, successful PlatformShare and all payment eras, post-V20 subsidy/superblock records, v24 asset-unlock txid versus instance hash, and default/overridden regtest/devnet contexts. I inspected fixture provenance and the minimum-coverage assertions and ran these tests. The independent Core reward/script records and stock-dashd captures close the specific missing branches from round 1; this is not a claim of exhaustive coverage of every argument.
- **M4 — closed.** The masternode-payment guard uses a safe Core call with value 1000 to identify the multiplication actually executed, then checks that multiplication. Classic height 1 with `INT64_MAX` now returns `1844674407370955161`. Boundary probes on both pins accepted `INT64_MAX/585`, `INT64_MIN/585`, and `INT64_MAX/3` on the applicable paths and refused the next overflowing value. KGW short ancestry is now `ORACLE_E_CONTEXT`, explicitly distinguished from Core verdicts and assert/UB errors; DEC-44 approves that coverage restriction. PlatformShare's multiplication/MoneyRange checks match its implementation.

## O1b calls and guard domain

I traced these entry points through Core at both pins, including the handwritten Rust declarations and wrappers.

- `eval_script`, `pubkey_verify`, `check_low_s`, `parse_der_lax`, `check_signature_encoding`, `find_and_delete`, `script_sigops`, and `script_sigops_with_sig` invoke the named Core implementation directly. The EvalScript call deliberately uses `BaseSignatureChecker` and BASE, as specified. No additional input-domain guard occurs in these thin wrappers beyond valid ABI pointers and output capacity.
- `verify_script` and `signature_hash` call `VerifyScript`/`SignatureHash`. Input-index checks prevent the latter's assert. The CLEANSTACK-without-P2SH guard reruns Core with only CLEANSTACK removed; it refuses exactly a path that would reach the final P2SH assert, while preserving earlier script failures. My probes verified passing-first, failing-first, later-failing, and P2SH+CLEANSTACK cases.
- `check_minimal_push` matches Core's push-opcode assert exactly. The four ScriptNum helpers call Core's constructor/serializer/minimality helpers. Decode refuses more than eight bytes only if Core's own size/minimality checks would let them reach the invalid shift. Constructor failures remain REJECT. Eight-byte negative decoding and `INT64_MIN` serialization passed; the ninth-byte UB boundary and constructor-check precedence passed.
- `transaction_sigops` calls Core's legacy, P2SH, and total counters; `check_input_scripts` calls Core's inline `CheckInputScripts`. The adapter supplies a real `CCoinsViewCache`, initializes the caches once, stores no verdicts, and holds `cs_main` for the script-cache access. Spent-marker and unspendable-output refusals prevent missing-coin asserts. There is one excess refusal: identical duplicate prevouts, detailed below. The requested “no guard refuses a defined Core input” property is therefore **not literally true for these two adapters**.
- `convert_bits_to_double` directly calls `ConvertBitsToDouble` without filtering compact encodings. Direct calls into scratch copies of the authenticated Core objects agreed with the shim for bits 0, `0x1d00ffff`, `0xff000001`, and 1 on both pins. In the pinned GCC/IEEE build, zero mantissa returns positive infinity and extreme exponent can underflow to zero; neither is silently censored. This is a statement about the pinned build, not unrestricted C++ floating-point portability.
- `superblock_payments_limit` calls `CSuperblock::GetPaymentsLimit`. Its chain argument is unused at both releases; V20 selection uses the configured height. It guards the subsidy halving-loop overflow/assert only on heights where Core actually calls the subsidy helper. Non-superblock and negative default-network heights retain Core's zero result. The stock-dashd budget comparisons passed, and the largest mainnet superblock height below `INT_MAX` was refused consistently with the corresponding subsidy call.
- `split_amount_by_shares` calls v24's `SplitAmountByShares`; v23 correctly returns UNSUPPORTED because that function is absent. It preserves defined signed/unsigned conversion and wide-arithmetic behavior in `ProRataFloor`. Checked weight accumulation, payout accumulation, and the final subtraction cover the signed overflows in Core's order. After a defined final subtraction, Core's last `paid += payout` equals the representable `total`, so it needs no additional guard. Empty and single-share inputs are accepted, including a zero single weight. Multi-share zero-sum weights reach Core's division exception and map to EXCEPTION rather than ARG. Probes covered these cases, weight underflow, `INT64_MAX` splitting, and a defined negative-weight wrap.

These conclusions use the ABI's existing distinction between per-call arguments and **trusted network configuration** (`oracle.h:94`). They do not extend the fuzz-safe promise to inconsistent/extreme configuration overrides expressly excluded there.

## DEC-44: KGW restriction is safe

At both pins KGW can run on mainnet/regtest for next heights 15200–34139. Those networks have immutable 150-second spacing and 86400-second timespan: `PastBlocksMax = 4032`, `PastBlocksMin = 14`. Testnet/devnet have no KGW interval, and only devnet allows spacing overrides.

Core's loop processes at most 4032 indices with positive heights, or stops earlier at the event horizon/genesis. The admitted window therefore supplies every field Core reads. A window beginning at height 1 may break after processing height 1 instead of advancing to genesis and exiting; all accumulated state and subsequent arithmetic are identical. With a full 4032-header window, the last processed header likewise supplies the same state whether the loop subsequently exits by null predecessor or the iteration bound. No accepted window can truncate a required iteration.

I reran a shortened version of the round-2 probe against the current authenticated archives: three synthetic timing patterns × seven heights × two pins. Full ancestry from height 1, 4032-header windows, and 5000-header windows agreed in all **42 comparison sets**. All 4031-header windows returned CONTEXT. The source argument establishes the general bound; these runs are supporting evidence.

The restriction can refuse a short query whose event horizon would have stopped early. That accepted coverage cost does not produce a wrong Core verdict. CS2 KGW fuzzing must supply the full ancestry and treat CONTEXT as a harness refusal. The hypothetical `PastBlocksMax == 0` concern is unreachable at these pins and should be reconsidered when changing pins or exposed configuration.

## Remaining finding and fix task

**PM-m1 — minor, existing round-2 m1 / BL-24: accept identical duplicate prevouts.**

Locations: Core `src/test/oracle/oracle.cpp:352`, `oracle.h:277`; called by `oracle_transaction_sigops` and `oracle_check_input_scripts`.

Concrete case: a transaction has two empty-script inputs referencing the same outpoint, each associated with the identical spent output `(value=5, script=OP_TRUE)`, with flags P2SH. Both shim calls return ARG at input 1. In fresh scratch-only copies of each current archive I exposed Core's existing internal symbols without changing their bodies, constructed the real coins view with that coin once, and called Core directly. On **both pins**, legacy/P2SH/total sigops were `0/0/0` and `CheckInputScripts` returned true. There is no assert or UB in this scenario.

This remains minor because `CheckTransaction` rejects duplicate inputs earlier in real block/mempool validation, and the adapter restriction is documented. It still violates the exact function-domain promise at `oracle.h:14`; DEC-44 accepted KGW, not this second restriction.

**Fix task PM-m1 (carry under BL-24):** when an outpoint is already present, accept an identical supplied `CTxOut` and reuse the existing coin; refuse conflicting supplied outputs as malformed context. Add regressions for identical and conflicting duplicates on both adapters and both pins. This changes shim semantics, so bump/rebuild/repin the ABI artifacts according to the existing ABI rule. No implementation changes were made in this review.

## Validation and limits

Evidence: `/work/scratch/o1-postmerge-gpt/`.

- Required `TMPDIR=/dev/shm/o1-gpt-tmp CARGO_BUILD_JOBS=12 cargo test -p dash-oracle-sys --features oracle`: **47 passed** (archive_check 8, contexts 9, script 12, smoke 18), zero failures; smoke execution 9.04 seconds.
- `contrib/node/check-r2.sh`: passed. `ca66c271` permits the crate's own test-only dashcore edge and gives the oracle its own restricted entry; it does not open an unrestricted transitive runtime path.
- All four installed plain/fuzz hashes match pins; both retained round-2 v23 plain reproduction artifacts also match.
- Shortened C boundary/KGW probes: zero failures on both pins. Direct Core duplicate/compact-difficulty probes: passed on both pins; only scratch object symbol visibility changed.

No clean rebuild, sanitizer campaign, live daemon launch, commits, pushes, or GitHub comments were performed. Temporary storage used the requested 0700 directory and was removed after the checks.

**Verdict: APPROVE-WITH-NITS. B1 and M1–M4 closed; DEC-44 safe; PM-m1 remains a minor fix task.**
