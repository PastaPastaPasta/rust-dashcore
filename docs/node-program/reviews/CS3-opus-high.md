# CS3 consensus review: sequential Opus-high pass

**Scope.** I reviewed `node/cs3-checks` at `abdfc2f88867e07d32e8231e27283a4171da4063`, the diff `ce224dd4..abdfc2f8` (15 commits), after the Sol gate's three rounds (`CS3-r{1,2,3}-sol.md`; APPROVE at r3). I followed `REVIEW-COMMON.md`, PLAN §4's CS3 row and the `cs3-checks-facts` note. The reference was Dash Core `v24.0.0-rc.3` (DEC-82: v24 only), read from `~/workspace/dash`.

I read every production line of `dash-consensus/src/{tx,header,block}.rs` and the CS3 hunks in `lib.rs`, `payments.rs` and `primitives/tx.rs`. I compared them line by line with Core's `CheckTransaction` (`consensus/tx_check.cpp:14`), `ContextualCheckTransaction` (`validation.cpp:351`), `IsFinalTx` (`consensus/tx_verify.cpp:19`), `CheckBlockHeader` (`:4307`), `CheckMerkleRoot` (`:4324`), `CheckBlock` (`:4366`), `ContextualCheckBlockHeader` (`:4469`), `ContextualCheckBlock` (`:4530`), `GetTxPayload` (`evo/specialtx.h`), `CCbTx` serialization, and `CBLSWrapper::Unserialize`/`SetBytes` (`bls/bls.h`). I also read the test support (`checks.rs`, `checks_rows.rs`, `checks_gen.rs`), the node-l1 harness (`src/checks.rs`, `l1_checks.rs`, `l1_check_rows.rs`) and the fuzz registration.

**PLAN §7.** As a Claude reviewer with read access, I checked CS3 against EMBARGO-1 §2. CS3 respects it. The checks go through the canonical module's predicates and compare the in-memory fields as Core does; hashes and sizes come from Core's serialization. The private-corpus loader keeps its marker, and the five targets route `private`. I also ran a scratch differential privately: 480 cases, 0 divergences. It was deleted afterwards. Nothing about it appears in this file.

## Verdict

**APPROVE-WITH-NITS.** I found no blocker or major issue. Every check matches Core's order, limits, strings, result classes and debug messages at v24.0.0-rc.3. My own oracle probes agreed with Core on every edge I could construct. What remains is one minor coverage gap and four nits.

## Findings

### CS3-OH1 — minor: no row pins the DIP0001 size edge of ContextualCheckBlock; the census reaches `bad-blk-length` only through empty blocks

**Location:** `dash-consensus/tests/support/checks_rows.rs` (no row), `node-l1/tests/l1_checks.rs:186` (`contextual_check_block` requires `bad-blk-length`).

Before DIP0001, `ContextualCheckBlock` limits a block to `MaxBlockSize(false)` = 1,000,000 bytes, and to 2,000,000 from DIP0001 (`validation.cpp:4551`–4553; `block.rs:159`). The rows cover CheckBlock's 2,000,000/2,000,001 edge. They also cover ContextualCheckBlock's DIP0001 sigop edge (20,000/20,001 before and at DIP0001). They do not cover this size edge. The census's 155 `bad-blk-length` verdicts all come from `block()`'s `txs.clear()` branch. Census blocks hold at most five small transactions, so none comes near 1 MB.

A regression in which `contextual_check_block` called `max_block_size(true)`, or ignored `dip0001`, would therefore pass every committed test, every census and every recorded row.

**Evidence that the code is right today.** A scratch probe built mainnet blocks of exactly 1,000,000 and 1,000,001 bytes (eleven transactions under quirk 3's 100,000-byte limit) at heights 782207 and 782208 (DIP0001 = 782208). Core and the node agreed on all four. At 782207, 1,000,000 bytes is accepted and 1,000,001 rejected with `bad-blk-length` / "size limits failed". At 782208, both are accepted.

**Fix:** add these four rows (`contextual(Network::Main, dip1 - 1 | dip1, true, block)`) and re-record.

### CS3-OH2 — nit: the header edges rest on the census alone, which does not require the signed-version reasons

**Locations:** `checks_rows.rs` (no `bad-version` or checkpoint row); `l1_checks.rs:201`–209.

`bad-fork-prior-to-checkpoint` and `bad-version(…)` have no boundary rows. The census reaches both (99 and 186 verdicts). Its required list names only `bad-version(0x00000001|2|3)`. The signed comparison of `nVersion` (Core's `int32_t`) is the part a refactor is most likely to break, and its renderings `bad-version(0x00000000)`, `(0xffffffff)` and `(0x80000000)` are reached (36, 38 and 4 times) but not required.

My probes confirmed agreement at:
- versions 0, −1, `i32::MIN`, 3, 4, `i32::MAX` and `0x20000000` at height 100000;
- each buried version deployment's h−1/h (BIP34 951, DERSIG 245817, CLTV 619382);
- checkpoints 207992, 312645 and 407452 at cp−1, cp and cp+1, with the best header at cp and at cp−1.

**Fix:** add `bad-version(0xffffffff)` and `bad-version(0x00000000)` to the required list, or add a few header rows. Either is enough.

### CS3-OH3 — nit: the genesis path of `contextual_check_block` has no test

**Location:** `block.rs:131`–140 (the doc says "`None` for genesis").

Dash Core does run `ContextualCheckBlock` on genesis: `LoadGenesisBlock` → `AcceptBlock` (`validation.cpp:5415`) → `ContextualCheckBlock(…, pindex->pprev = nullptr)` (`:4820`). The oracle context cannot express a null `pprev`, and no CS3 test passes `prev: None`.

I checked it in scratch. Each network's `ChainParams::genesis` (mainnet, testnet, regtest and devnet `-devnet` default) passes `check_block` (PoW and merkle on) and `contextual_check_block` with `prev: None` and height 0. **Fix:** commit that as a four-line unit test, so a later change to the deployment or finality handling cannot break block 0 unnoticed.

### CS3-OH4 — nit: a stale Core anchor in the test support

**Location:** `dash-consensus/tests/support/checks.rs:371`.

The comment `// Core: chain.h:280 CBlockIndex::GetMedianTimePast` is stale: the function is at `chain.h:283` in v24.0.0-rc.3 (`:270` in v23.1.8). Every anchor in `src/` resolves correctly: `tx_check.cpp:14`, `validation.cpp:351/4307/4324/4366/4469/4530`, `tx_verify.cpp:19/118`, `consensus.h:10/11/12/17/22`, `policy.h:27`, `amount.h:26/27`, `chain.h:24`, `script.h:398` and `cbtx.cpp:158`.

### CS3-OH5 — nit: the reject-string count in the claim

The claim says "30 literal reject reasons". A source census gives **28** distinct literals in `tx.rs`, `block.rs` and `header.rs`, the same 28 in Core's `tx_check.cpp` and the two `validation.cpp` ranges, plus the formatted `bad-version(0x%08x)`: 29 templates on each side, as Sol r1 counted. The sets are equal, so this is a bookkeeping difference only. Use 29 in the merge note.

## Judgements on the focus items

1. **Check order and short-circuiting: matches Core.**
   - **CheckBlock:** header, PoW, then devnet, then merkle; root mismatch before mutation, before the CbTx asset-unlock root; size before coinbase position before `CheckTransaction` per transaction before sigops.
   - **ContextualCheckBlockHeader:** diffbits, checkpoint, time-too-old, time-too-new, version.
   - **ContextualCheckBlock:** size, then per transaction `IsFinalTx` before `ContextualCheckTransaction`, then sigops, height prefix and CbTx type.
   - **Probes that agreed:** high-hash with a bad root (high-hash first); an empty block with a bad root (bad-txnmrklroot first); a second coinbase that also fails `CheckTransaction` (bad-cb-multiple); a sigop overflow plus a bad transaction (transaction first); non-final plus bad type in one transaction (non-final first); bad type before a later non-final transaction; sigops before `bad-cb-type`; `[cb,a,b,b]` mutated; `[cb,a,b,a]` and `[cb,a,b]` accepted.
   - The wrapped debug message keeps Core's trailing space and uses the v24 `GetHash()` (stable txid) via `tx.txid(rules)`.

2. **Limits and overflow: correct.**
   - **Value sum:** every addend is ≤ MAX_MONEY, and the running sum is range-checked before the next addition, so `i64` cannot overflow.
   - **Sigops:** the sums are `u32` `wrapping_add`, as Core's `unsigned int` is. They run after the size check, which bounds them far below `u32::MAX`.
   - **DIP0001:** CheckBlock uses Core's default `MaxBlockSize()`/`MaxBlockSigOps()` (true); ContextualCheckBlock uses the DIP0001 state. All comparisons are strict `>`, as in Core.
   - **Sizes:** `Tx::size`/`Block::size` are Core-serialization lengths.
   - **BIP34:** `script_for_height` is `push_int64` exactly (unit test plus quirk 10). The prefix test is `starts_with`, which matches Core's size check plus `std::equal`.

3. **Activation heights per network: covered.** The census anchors positions at every buried deployment's height on main, test, regtest and devnet, with deltas −3..3. Core's own chainparams are compared against base-sdk's through the oracle, so this checks the heights themselves and not just their use. Mainnet's 68589 tolerance applies to mainnet only (`tolerance_applies`), and the census also anchors the other networks at 68590.

4. **`IsFinalTx` and the CSV/MTP cutoff: matches Core.** The comparison is strict, in `i64`. A lock below 500,000,000 is a height. The all-final-sequence exception holds even with zero inputs: a probe with a zero-input asset unlock locked at `u32::MAX` was accepted by both sides. The cutoff is the previous block's MTP from CSV, and the block time before it. The rows pin h−1/h, the final-sequence exception and 499999999/500000000, both before and at CSV.

5. **Asset-unlock and special-transaction handling: correct.**
   - Type 9 allows empty inputs whatever the version (quirk 2).
   - The CbTx root check reproduces `GetTxPayload<CCbTx>(…, assert_type=false)`. The type alone selects the payload, a version below 3 has no payload, and the payload must decode completely.
   - base-sdk's `read_global` rejects a BLS signature that is canonical in neither scheme, as Core's `Unserialize` throws. A wrong root next to such a signature is therefore accepted on both sides; my probe confirmed it.
   - CbTx versions 5 and `0xffff` still carry the root (Core's `>=`). Probes agreed on wrong and right roots, on a trailing payload byte, and on a version-2 coinbase with type 5.
   - Leaves are the instance hashes of `IsAssetUnlockWithStableTxid` transactions in block order (`cbtx.cpp:158`).

6. **Coverage.** The census does not "only reach" the reasons: the rows bind the edges for every transaction-level reason, for CheckBlock's size and sigops, and for ContextualCheckBlock's sigops, finality and BIP34. The exceptions are OH1 (a minor gap: ContextualCheckBlock size) and OH2 (a nit: header version and checkpoint edges, which census-only coverage and my probes leave correct today).

7. **Panic paths.**
   - **Reachable from untrusted bytes: none.** `inputs()[0]` is used only under `is_coinbase()`. `txs[0]` and `txs[1..]` are used only after the empty check. The height rule tolerates an empty coinbase `vin` (`bad-cb-height`; Core would read out of bounds there, but `CheckBlock` runs first). `extra_payload[0]` is guarded by the length check. The height is converted with `unwrap_or`.
   - **The `contextual_check_block_header` panic on `prev = None` is acceptable as declared.** It mirrors Core's `assert(pindexPrev != nullptr)`. Core never reaches it, because `AcceptBlockHeader` skips the contextual check when `hash == hashGenesisBlock` and otherwise rejects an unknown `hashPrevBlock` first.
   - **A condition for CH1-node and SB4's `dash-chain`:** they must keep that same guard: genesis by hash, and the previous block resolved before building the `ConsensusCtx`. A peer-supplied header cannot reach the panic. A caller bug can, so a one-line note in the CH1-node brief is worth adding.

8. **API fit: adequate for CS4, CH1-node and CS5b.**
   - **CS5b** (`node/cs5b-inputs`, built on this branch) adds `tx::{inputs, locks}` beside these functions and reuses `MAX_MONEY`/`money_range` from the crate root without reshaping CS3's API.
   - **CH1 class handling:** `BlockValidationResult::{TimeFuture, Checkpoint, Mutated}` are distinct, which is what CH1 needs to avoid caching `time-too-new` and mutated blocks as invalid.
   - **Two minor costs, both acceptable:**
     - `check_block` has no `known_hash`/`fChecked` cache. Its doc tells callers to cache, and `Header` already caches its hash.
     - `HeaderParams` is passed separately from `ctx.params`, so a caller could mix networks. Sol accepted this; a `debug_assert` comparing the genesis hashes would catch a mix-up cheaply.

## What I ran

- Scratch: a detached worktree at `/work/scratch/cs3-opus-high/wt`, with `RUSTC_WRAPPER=`, `CARGO_BUILD_JOBS=12`, a separate target dir, `TMPDIR=/dev/shm/cs3-opus-high-tmp` (mode 700) and `DASH_EMBARGO_DIR` unset.
- `cargo test -p dash-consensus -p node-l1 --features node-l1/oracle --locked --no-fail-fast`: **276 passed, 0 failed.** This covers the lib tests, `checks_vectors` (L0 `tx_valid`/`tx_invalid`), `checks_recorded`, `checks_header_windows`, quirks 2/3/9/10, `l1_check_rows` (both tests, live oracle), `l1_header_windows`, `l1_checks`, the twins and `private_seed_replay`'s synthetic tests. The real-private entry points reported SKIPPED, as expected without `DASH_EMBARGO_DIR`.
- **Census, reproduced exactly** (checked / decode / out of domain, 0 divergences):

  | Target | Checked | Decode | Out of domain |
  |---|---:|---:|---:|
  | transaction | 2001 | 999 | 0 |
  | block | 2542 | 458 | 0 |
  | contextual tx | 2641 | 354 | 5 |
  | contextual block | 1478 | 283 | 239 |
  | contextual header | 738 | 0 | 62 |

- **Scratch oracle probes (v24 pin): 54 comparisons, 0 divergences.** They covered the ContextualCheckBlock DIP0001 size edge, header versions and checkpoint edges, CheckBlock and ContextualCheckBlock ordering, CbTx payload edges, BIP34 at 950/951 and a truncated height push, and vacuous finality. Separately, the genesis unit check ran on four networks, plus the private PLAN §7 differential above. The probe sources are kept in the scratch dir as `.txt` (public probes only).
- `cargo fmt --all --check` and `cargo clippy -p dash-consensus -p node-l1 --all-features --all-targets --locked -- -D warnings`: clean.
- **Not repeated:** the full workspace gate, fuzz smoke, the complete oracle-diff (104/0) and the real-private receipts. The implementer's and Sol's runs stand for those.
- The branch under review was not modified. The scratch worktree, its 3.5 GB target and the TMPDIR were removed. No dashd was started. This file is uncommitted, and nothing was committed, pushed or posted.
