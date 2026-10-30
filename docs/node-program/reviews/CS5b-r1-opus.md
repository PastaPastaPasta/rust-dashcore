# CS5b — Opus high review (r1, after Sol)

Reviewer: Claude Opus 5.5, high. Date: 2026-10-09. DEC-92 sequential pass after Sol r1 (`CS5b-r1-sol.md`, APPROVE-WITH-NITS).

Reviewed `node/cs5b-inputs` at **3e9a066037adab01818a4d7e9b06e954ce9fe08d**, diffed against **ea501ed7** (7 commits). The work ran in a detached worktree under `/work/scratch/cs5b-review-opus/`, with its own target dir and a mode-0700 `TMPDIR` in `/dev/shm`. The core reference is Dash Core **v24.0.0-rc.3** in `~/workspace/dash`: `consensus/tx_verify.cpp`, `crypto/muhash.cpp`, `kernel/coinstats.cpp`, `validation.cpp` ConnectBlock, and `evo/assetlocktx.cpp`. I also read SB0 rev6.1 §4.1.1 and the EV addendum rev6. The `code-review` skill ran at high effort. Sol's M1 (dead txid-order guard) and M2 (ARCHITECTURE.md contract) are not repeated here. I independently confirm M1.

## Must-fix

### MF1 (major): the sigop-count precondition is not enough for asset unlocks

**Location:** `dash-consensus/src/tx/inputs.rs:179-186` and `:203-208`. The `p2sh_sigop_count` and `transaction_sigop_count` docs say "Callers count after [`check_tx_inputs`] accepted."

`check_tx_inputs` returns `Ok(fee)` for an asset unlock before it reads any input (`inputs.rs:61`, as Core does). For an asset unlock with an input, the documented order therefore leads to `spent_coin`'s `expect` panicking.

**Reproduced** in a scratch test (since reverted): version 3, type 9, a valid payload with fee 1,000, and one input whose coin is not in the view. `check_tx_inputs` returned `Ok(1000)`, then `transaction_sigop_count(.., P2SH)` panicked with "AccessCoin: the spent coin is in the view". A node following the doc can be crashed remotely by a block carrying such a transaction.

Core never reaches this, for two reasons:
- ConnectBlock runs `ProcessSpecialTxsInBlock` (`validation.cpp:2663`) before the per-tx loop (`:2688`, sigops at `:2740`).
- `CheckAssetUnlockTx` rejects any input with `bad-assetunlocktx-have-input` (`evo/assetlocktx.cpp:188-189`).

This is the CH3a consumer's ordering obligation, and the doc currently misstates it.

**Fix:**
- State the real precondition on both sigop functions and on `check_tx_inputs`: an asset unlock must already have passed the special-tx checks (it has no inputs), as Core's ConnectBlock order guarantees.
- Pin it with a unit test: an asset unlock with no inputs counts 0 sigops.
- Include it in the ARCHITECTURE.md section Sol's M2 asks for, so CH3a inherits it.

A code change to tolerate the missing coin is not required, because Core's behaviour there is an assert.

### MF2 (major, test strength): a BIP68 boundary is not guarded by any test

I ran nine single-line mutations against the permanent suites: `inputs_and_locks`, `locktime_vectors`, `l1_inputs` (the 2,000-case sequence-lock census against Core), and `l1_twins diff_sequence_locks`.

| Mutation (`tx/locks.rs`) | Caught by |
|---|---|
| `SEQUENCE_LOCKTIME_GRANULARITY` 9 → 10 | unit tests and the L1 census |
| type flag tested as bit 21 instead of bit 22 | unit tests and the L1 census |
| drop the `− 1` from the time lock | unit tests and the L1 census |
| coin time = MTP of the coin's own block, not `h − 1` | unit tests and the L1 census |
| **`locks.min_time >= block_time` → `>`** (`:149`) | **nothing: every suite passes** |
| one further version-handling mutant (details under PLAN §7 handling) | covered privately, not by a tracked suite |

So a 512-second-unit or type-flag slip would be caught, but an off-by-one at the time cutoff would not. The regtest "time lock met exactly" cases use `units = Δ // 512` with Δ a multiple of the 600 s mock spacing, so `min_time` never equals the parent MTP. The unit test's TYPE|11 / TYPE|12 pair sits 369 s below and 143 s above the cutoff. The cutoff mutant would accept blocks Core rejects with `bad-txns-nonfinal`.

**Fix:**
- In `inputs_and_locks.rs`, add a window whose parent MTP minus coin MTP is ≡ 511 (mod 512). Assert that `min_time == median_time_past` is rejected and that one second earlier is accepted.
- Version-handling coverage: per PLAN §7 handling (private).
- In `locktime-vectors.py`, offset one mock block time so that one Core-judged case lands exactly on `min_time == MTP` (reject) and one on `MTP − 1` (accept). This keeps Core as the judge of the boundary.

## Non-blocking findings

- **N1 (minor): rare MuHash branches have no permanent coverage.** Two mutants of `coins/muhash.rs` survive both the MuHash unit tests and the UTXO-hash replay:
  - deleting the `if c[0] != 0 { full_reduce }` second reduction in `reduce_into` (`:149`);
  - changing `is_overflow`'s `>` to `>=` (`:128`), which corrupts a value of exactly p − 1.

  A third mutant, dropping `divide`'s pre-reduction, is probably equivalent, because `multiply` re-reduces. Random data never reaches these branches, but a state read back through `from_bytes` (the coinstats DB) can. Sol's 132 Core-compared boundary states exist only in scratch. Commit them as a fixed vector file generated from pinned `muhash.cpp`, covering 0, 1, p − 1, p, p + 1, 2^3072 − 1 and the carry cases.
- **N2 (minor): the UTXO-hash fixture has no P2PKH or P2PK coins.** `utxo-hash-vectors.py` mines only `raw(51)` and adds one P2SH output, so compressed-script types 0 and 2–5 never reach `tx_out_ser` or `HashSerialized2` against Core's hashes. Mainnet's set is dominated by those types. Coverage exists only indirectly, through CS5a's `compare_script_decompress`. Add P2PKH, compressed P2PK and uncompressed P2PK outputs to the generator.
- **N3 (nit): coins are read twice.** `check_tx_inputs` reads each coin in the HaveInputs pass, then again through `spent_coin` (`inputs.rs:67-82`), and `p2sh_sigop_count` reads it a third time. This matches Core against its cache, and CH2's `BlockOverlay::have_inputs` makes the later reads memory hits, so it is acceptable. Keeping the `CoinRef`s from the first pass would remove the second `expect` entirely.
- **N4 (nit): several `// Core:` line citations are off by 1–5 lines at v24.0.0-rc.3.**

  | Cited | Actual |
  |---|---|
  | CheckTxInputs :168 | :163 |
  | CalculateSequenceLocks :40 | :41 |
  | SEQUENCE_LOCKTIME_GRANULARITY :96 | `primitives/transaction.h:151` |
  | Num3072::Multiply :180 | :181 |
  | MuHash3072::Finalize :309 | :314 |
  | coinstatsindex Insert/Remove :153/:173 | :167/:188 |
- **N5 (nit): duplicated predicate.** `inputs.rs:61` re-implements `Tx::is_platform_transfer()`, which node-l1's `meets_preconditions` already uses.

## Focus areas

**MuHash3072 arithmetic.** I compared the port line by line with Core's limb code and found no difference:
- the helpers `extract3`, `mul`, `mulnadd3` (`c2 = t + d2·n` truncated), `muln2`, `muladd3`, `muldbladd3` and `addnextract2`;
- `Multiply` and `Square`, including the odd-`j` terms and `(j+1)/2 == j.div_ceil(2)`;
- the shared second reduction, with the overflow and carry `FullReduce` in Core's order;
- `GetInverse`'s 11-step repunit ladder and its 14 `square_n_mul` pairs;
- `Divide`'s pre-reduction and post-reduction.

Finalize divides, sets the denominator to one, and returns SHA256 of the 384 little-endian numerator bytes. Core's `HashWriter << Span` and `<< unsigned char[384]` add no length prefix, and the Core vectors confirm this.

ChaCha20 uses a zero nonce and counter over 6 blocks, so the 64-bit-counter and 96-bit-nonce variants agree. Serialization is numerator then denominator, as 64-bit little-endian limbs.

Constant-time behaviour is not needed. MuHash runs only over public UTXO data, and Core's implementation is itself variable-time (the `IsOverflow` short-circuit and the carry branches).

`u64` additions in `muladd3` and `muldbladd3` cannot overflow, because the high half of a 64×64 product is at most 2^64 − 2. The `debug_assert`s replace asserts that Core proves unreachable. The fixed vectors, the order-independence test and Sol's 132-state comparison agree. N1 is about keeping that last comparison.

**hash_serialized_2 on a large set.** The construction is independent of set size, and the risk lies in order and field encoding, all of which match Core:
- Core's cursor walks `'C' ‖ txid(32, internal) ‖ VARINT(n)` keys bytewise. Every output of a txid is therefore contiguous, and txids ascend by internal bytes, which is what `push` expects.
- Core's `std::map<uint32_t, Coin>` re-sorts by index, so the order of VARINT keys does not matter. The `BTreeMap<u32, _>` matches it, including replacement on a repeated index.
- Quirk 33 reads the lowest index's code.
- `VARINT(n + 1)` wraps at `u32::MAX` as Core's `uint32_t` does.
- The script is written as a CompactSize length plus the expanded bytes.
- Values use the nonnegative-signed VARINT.
- Memory is per transaction (`fragments` is cleared on each `apply`).

The 200-output transaction exercises two-byte indices, and fixture heights are irrelevant to the header because of the quirk. The large-set risks that remain are the caller's order, which is Sol's M1 (CH2 §1632 states its cursor yields Core's key order), and the script types in N2.

**API for CH3a.** The signatures follow Core's ConnectBlock loop (`validation.cpp:2688-2745`):
- `check_tx_inputs(tx, view, pindex.height)` returns the fee. `bad-txns-accumulated-fee-outofrange` remains CH3a's check.
- `prev_heights` come from `AccessCoin(..).nHeight` and are mutated in place, as Core does.
- `block_lock_time_flags(csv_active)` gives the lock-time flags.
- The ConsensusCtx is that of the block being connected, with `median_time_past` the parent's. The unit test asserts this.
- `transaction_sigop_count(tx, view, script_flags)` returns a `u32` that wraps like Core's `unsigned int`.
- `ViewOr` follows CH2 §2.4.4, and storage errors are never turned into rejections.

The one unsound piece of the contract is MF1. The incremental MuHash API (`insert_coin`, `remove_coin`, `finalize(&mut self)` with state change, `to_bytes`/`from_bytes`) matches coinstatsindex's accumulator, which calls `Finalize` and then persists the mutated state. It supports the per-block MuHash comparison in CH2 §6.3.

**SB0 rev6.1 and the EV addendum.** I found no conflict. `ancestor_mtp` (`locks.rs:135-140`) reads the MTP at `coin_height − 1`. That depth is unbounded: it is the only consensus read here that reaches below any window. On the node it is safe, because SB0 §4.1.1 keeps `ChainAncestry` infallible and complete (`window = None`, every entry resident). SPV never runs ConnectBlock, and `-coinstatsindex` with `-spv` is an InitError (SB0 :224). CH3a must keep calling these functions with the node's complete ancestry, never with a windowed `RuleContext`. Nothing here touches `VContext`, `OnV` or `LlmqChain`, and the crate stays pure.

**Panics on adversarial input.** Each panic mirrors a Core assert or throw that Core's call order keeps unreachable:
- `value_out` is protected by CheckTransaction.
- A coinbase reaching `check_tx_inputs` is excluded because ConnectBlock skips it.
- `spent_coin` is protected by the HaveInputs pass.
- `prev_heights` length matches by construction.
- The ancestry reads hold for coin heights ≤ the block height.

The exception is the documentation gap in MF1. Value-sum wrapping is sound: an overflowing addend is already out of range, and I agree with Sol's analysis. `format_money` is panic-free over all of `i64`, because `i64::MIN / COIN` is negatable.

## Verification

| Check | Result |
|---|---|
| `cargo test -p dash-consensus -p node-l1 --features node-l1/oracle --no-fail-fast` at 3e9a0660 | **298 passed, 0 failed** |
| Mutation probes (table in MF2, plus N1) | 4 of 6 lock mutants caught by tracked suites; both non-equivalent MuHash mutants survive |
| Asset-unlock sigop probe (MF1) | panic reproduced |
| Private tests, `check-private-tests.sh`: `quirk_01_embargo_1` | **PASS, 522 cases** |
| Private tests: `private_seed_replay` | **PASS, 4,324 seeds (279 required)**; exit 0 |

For the private tests, only the receipt lines were read, and none of the corpus contents.

## Verdict

**APPROVE-WITH-CHANGES.**

Must-fix:
- **MF1:** document the asset-unlock precondition for the sigop counts and `check_tx_inputs`, and pin it with a test.
- **MF2:** add tests at the BIP68 time-cutoff equality, with a Core-judged regtest vector at the boundary.

The consensus logic itself matches Core. Fix these together with Sol's M1 and M2. N1–N5 are recommended.
