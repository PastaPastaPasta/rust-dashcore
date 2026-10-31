# CS5a r1 review (Opus): coin and undo codecs, `CoinsView`

- **Branch:** `node/cs5a-coins` at `17014d4b`, 9 commits on `30a7ccd0`. Reviewed in a detached worktree at `/work/scratch/cs5a-opus/wt`.
- **Read:** REVIEW-COMMON; PLAN row CS5a; DEC-30, DEC-32, DEC-59 Q6, DEC-77 and DEC-78; CH2 §2.1–2.4, §2.10, §2.11 (row CS5a), §3.1 and §3.5. I did not read the GPT review.
- **Core:** `~/workspace/dash`, tags `v23.1.8` and `v24.0.0-rc.3`, plus the local bench branch `node/cs5a-bench` (`bc2c15278f`).

## What I ran

| Check | Result |
|---|---|
| `cargo test -p dash-consensus --all-features` | pass |
| `cargo clippy -p dash-consensus --all-features --all-targets -D warnings` | clean |
| `contrib/node/check-pure.sh` | `dash-consensus` and `dash-sha256` ok |
| `cargo test -p node-l1 --features oracle --test l1_coin_boundaries --test l1_coin_records --test l1_twins` | pass (4 + 1 + 6). Live oracle on `Pin::ALL`. |
| Scratch panic and idempotence probe (untracked file, deleted after the run; release with `overflow-checks` and `debug-assertions`) | See below. |
| Implementer's fuzz verdicts (`/work/scratch/cs5a-coins/fuzz-sink`) | Every leg 0 divergences and 0 crashes, about 0.05 CPU-h each. See G1. |

The probe ran 3 M biased random inputs. It decoded 965 k coins, 614 k undo entries and 98.7 k block records:
- no panic;
- `write`/`write_undo` sizes equal `serialized_size`/`undo_size`;
- `write ∘ decode` is a fixed point on the money range;
- `to_coin().serialize() == CoinRef::write`;
- undo transcoding keeps height and coinbase;
- every `decode_block_undo` → `core_bytes` result re-decodes.

## Findings

No blockers and no majors.

### Minor

**m1. No borrowed path for whole undo records, in either direction.**
- **Where:** `dash-consensus/src/coins/undo.rs:123-128` and `:143-150`, plus `CoreUndoBytes` (`:175`).
- **Building.** `CoreUndoBytes` can be made only from an owned `BlockUndo` or from `Decoded<BlockUndo>`. CH3a's connect holds the spent coins as Core bytes (CH2 `PackedCoin`). To write undo it must, for every spent input:
  - call `to_coin()`, which expands the script and allocates above 35 B;
  - for codes 4 and 5, pay libsecp256k1's square root (about 45 k instructions, REPORT row `coin_decode/p2pk_uncompressed`);
  - re-compress the script in `core_bytes()`;
  - build a `Vec<TxUndo>` of `Vec<Coin>` per block.
- **Reading.** `BlockUndo::decode` is owned too, so CH2's blind recovery leg and CH3a's disconnect pay the same costs to put coins back as bytes.
- **The harness already needs this.** It hand-rolls the borrowed walk, but outside the crate: `node-l1/src/coins.rs:223-230` (`rewrite_tx_undo`) and `:262-269`.
- **Failure scenario:** no wrong result, but a cost on V's critical path. A1/CH2 §0 says the cache "transcodes coin ↔ undo through `write`/`write_undo`", but the undo-record API forces an owned round trip around exactly that path.
- **Fix:**
  1. Add a sealed builder, for example `BlockUndoWriter { fn tx(&mut self, inputs: u64); fn coin(&mut self, c: CoinRef<'_>); fn finish(self) -> CoreUndoBytes }`. Every entry goes through `write_undo`, so the type's invariant still holds by construction.
  2. Add a borrowed reader, for example `BlockUndoRef<'a>` or `for_each_entry(&mut &'a [u8], |tx, CoinRef<'a>|)`, with `read_vector`'s count rules.
  3. Doing this as a CS5a follow-up before CH3a starts is acceptable.

**m2. No `CoinRef::from_output`.**
- **Where:** `coin.rs:39-47`. `coin_bytes` builds exactly this `CoinRef` internally, then discards it.
- **Who needs it:**
  - MP1's mempool view must return `CoinRef<'_>` for Core's `Coin(tx.vout[n], MEMPOOL_HEIGHT, false)` (`CCoinsViewMemPool::GetCoin`). `MEMPOOL_HEIGHT` is `0x7FFFFFFF`, which is `MAX_COIN_HEIGHT`.
  - CH2's overlay, for a `CoinSrc::Output` read back before commit.
- **Today:** both must encode the output to bytes, or keep an owned `Coin` alive inside the view, just to hand out a borrowed coin.
- **Fix:** add `pub fn from_output(out: &'a TxOut, height: u32, coinbase: bool) -> CoinRef<'a>`, masking the height as `Coin::new` does. `coin_bytes` then becomes `CoinRef::from_output(..).write(w)`.

**m3. `MemoryCoins` cannot hold a raw value, so its own documented `HaveCoin` split is unreachable.**
- **Where:** `view.rs:136-146` (`write`) and `:193-197` (`have_coin`).
- **The gap:** `MemoryCoins::have_coin` documents Core's `Exists(key)` semantics ("true for a stored value that `coin` cannot decode"). But the only way to store a value is `write(Put(Coin))`, which stores `coin.serialize()`, and that always decodes. So:
  - the documented branch is dead;
  - CH2's `RefCoinsCache` twin cannot model DEC-78's corrupt-coin deviation test against Core's semantics (Core: missing; CH2: abort).
- **Fix:** add a model-only `MemoryCoins::put_raw(outpoint, Vec<u8>)`, or have `CoinWrite::Put` carry bytes. Bytes are what CH2's flush writes anyway. `CoinWrite::Put(Coin)` forces an owned decode on any byte-level store, so today nothing but `MemoryCoins` can use `CoinsBatch`.

**m4. CH2's neighbour ask, "`CoreWrite` over a fixed `&mut [u8]`", was not delivered.**
- **Where:** CH2 §2.11, row CS5a. `CoreWrite` (`primitives/serialize.rs:18`) is open and infallible.
- **Problem:** a fixed 34-byte inline writer must either size first, which costs a second encode (and a second square root for codes 4 and 5), or handle overflow out of band.
- **Fix, either:**
  - ship an `ArrayWriter<const N>` with an overflow flag next to `SizeWriter`; or
  - record in CH2's brief that CH2a owns it.

### Nits

- **n1** (`view.rs:20-32`, the `CoinsView` doc). The type system enforces DEC-77 rule 2 only for views CH2 writes, so the trait doc should state the implementer contract: an `Error` is returned only from a storage read failure, after the node-wide latch has tripped, and is never built for a missing or undecodable coin. See also "Item 3" under Scope below.
- **n2** (argument order). `coin_bytes(height, coinbase, out, w)` uses a different order from `Coin::from_output(out, height, coinbase)` and CH2's `PackedCoin::from_output(out, height, coinbase)`. Pick one.
  - Related: `coin_bytes` panics on value −1 (Core's assert), and CH2 §2.2 wants `Err`. CH2 must check before calling. A one-line `# Errors` note pointing at it, or a `try_` variant, would help.
- **n3** (`coin.rs:85`, "Decoding never produces one"). This is true, and I checked it:
  - for `e ≥ 1`, `DecompressAmount` returns a multiple of 10, so it is even and never `2^64−1`;
  - for `e = 0` it is at most `⌊(2^64−2)/90⌋·10 + 9 < 2^61`.

  `CoinsView::coin`'s "never `is_spent`" contract and `write`'s assert rely on it. A two-line comment with that argument, or a `debug_assert!` in `read_out`, would document it in place.
- **n4** (`CoinRef` and `ScriptRef` have no `PartialEq`). CH2's `-consensusxcheck` compares fetch results. Comparing `write` bytes works, but a byte-wise `eq` (or documenting that idiom) saves every caller from rediscovering it.
- **n5** (`benches/REPORT.md`):
  - The file is titled "CS0 benchmark report" but now carries CS5a. Retitle it, or give each task its own section heading at the top level.
  - "Mainnet-shaped" (14/4/1/1 per 20) is an asserted mix, not a measured one. Call it synthetic, or cite where the mix came from.
  - There are no encode or transcode rows (`coin_bytes`, `CoinRef::write_undo`), although those are CH2/CH3a's per-coin hot path. Core has direct counterparts (`Coin::Serialize`, `TxInUndoFormatter::Ser`). They are not required by the BG ("crit coin decode"), but cheap to add with the same patch.
- **n6** (fuzz-leg provenance). The `diff_coin_decode` v23.1 smoke ran on `fcf2db99` with a dirty tree. The head commit `17014d4b` touches only `view.rs` and docs, so this is immaterial, but G1's long runs should be at the merged head on both pins.

### Merge gates (not defects)

- **G1. Merge fuzz budget is still outstanding.** NFR-CEQ-04, PLAN §L1 and DEC-59 Q6 require ≥ 24 CPU-h clean per new target (16 h on v24.0, 8 h on v23.1) for `diff_codec_primitives`, `diff_coin_decode` and `diff_undo_decode`. Only about 0.05 CPU-h smokes per leg exist so far. This is the O3 farm's or the merge checklist's job; I mention it so it is not missed.
- **G2. Expected merge conflict.** CS1 adds the same exact `secp256k1 =0.33.1` / `secp256k1-sys =0.14.1` pins to `dash-consensus/Cargo.toml`, so expect a trivial conflict. Keep one copy with both comments' rationale, and check that `Cargo.lock` resolves to the single 0.14.1 `-sys`.
  - `secp256k1-sys 0.10.1` also appears in the graph (from other crates). That is harmless because libsecp's symbols are version-prefixed, but only the 0.14.1 copy may be reachable from `dash-consensus`. It is today.

## Scope items

### 1. API fitness for CH2, CH3a and CH6a

**What CH2 needs is all there:**
- `coin_bytes`;
- `CoinRef::decode` (borrowed, never allocates, trailing bytes left to the caller as `CDBWrapper::Read` does);
- `CoinRef::write`/`write_undo`, which re-encode from the coin Core holds rather than splicing bytes. That is the right call, and it is pinned by `an_off_curve_key_is_written_as_core_read_it`;
- `serialized_size`/`undo_size`;
- `ScriptRef::is_unspendable` without decompressing;
- `MAX_OUTPUTS_PER_BLOCK`, `access_by_txid`;
- the fallible trait with exactly DEC-77's shape.

**CH6a's legacy normalisation works with the current API:**
1. `decode_block_undo` gives the record, with `wire_is_canonical` decided exactly.
2. `Coin::new(c.value(), c.script_pubkey(), height, coinbase)` rebuilds each zero-height entry.
3. `BlockUndo::core_bytes()` gives the record Core writes.

The gaps are m1 (performance of the undo paths), m2 (mempool and overlay views), m3 (the model twin) and m4 (CH2's writer).

**A CH3a obligation the docs could repeat.** On disconnect, a restored undo coin whose script `is_unspendable` (for example a raw script over `MAX_SCRIPT_SIZE`, which reads as `OP_RETURN`) must be skipped, because Core's `AddCoin` returns early (`coins.cpp:72`). `ScriptRef::is_unspendable` covers it, and a sentence in `decode_undo`'s doc would carry the obligation forward.

### 2. Soundness

- `#![forbid(unsafe_code)]` is set at the crate root. The new code has no `unsafe`.
- **`CoinRef<'a>`/`ScriptRef<'a>`** borrow only from the decoder's input slice, or from the `Coin`/`TxOut` they were made from.
- **`expand`** ties its output to the shorter of `&self` and the buffer. That is sound, and slightly stricter than needed for raw scripts.
- **Panics reachable from untrusted bytes: none.** The arithmetic is wrapping where Core's is unsigned (`compress_amount`, `decompress_amount`). `code()` cannot overflow, because every `CoinRef` height is ≤ 2^31−1 by construction.
  - `decompress_script_into`'s documented panic needs a short `data` slice, which only the crate-private `special` constructor could supply, and the decoder always passes exactly `special_script_size` bytes.
  - `write`'s spent-coin assert is unreachable from decoded coins (n3).
  - The probe above found no panic.
- **Allocation is bounded.**
  - `read_vector` reserves at most `min(count, remaining/min_size, 5 MB / size_of::<T>)`. After that, growth is linear in the input consumed: about 19× for `Vec<Coin>` per 3 input bytes, and 24× for empty `TxUndo`s.
  - A raw script over `MAX_SCRIPT_SIZE` is skipped with `read_bytes`, without allocating, up to a 4 GiB declared length. It still needs the bytes present, as Core's `ignore` does.
  - The forged-count test covers the CompactSize range check.

### 3. Fallible `CoinsView` (DEC-77, DEC-78)

**No path makes a read error look like a missing coin:**
- the default `have_coin` is `Ok(self.coin(op)?.is_some())`;
- `access_by_txid` uses `?` on every probe, so it stops at the first error rather than skipping to the next index;
- the `&T` impl forwards all three methods;
- nothing in the crate calls `.ok()`, `unwrap_or` or `is_ok` on a view result;
- `MemoryCoins::coin`'s `.ok()` is on a *decode* error. That is Core's `CDBWrapper::Read` semantics (`dbwrapper.h:324-326`, v23.1.8 `:314-316`), not a read failure, and DEC-78 records CH2's declared deviation from it.

**No concrete `CoinsReadError` in the pure crate: I agree** (as DEC-78 already ratified):
- The pure crate cannot know about the latch, so it should not be able to construct the error.
- `Infallible` makes in-memory callers statically error-free.
- CH2's concrete type, built only by `GuardedEngine`, is where rule 2 can actually be enforced.
- A sealed marker bound (`type Error: ReadFailure`) would add machinery with no benefit while CH2 is the only storage view.

The remaining risk is an ad-hoc test or adapter view choosing a freely constructible `Error`; n1's doc contract covers it. `Error: Debug` is enough for CS5b's `ViewOr<V::Error, _>`. `best_block` staying infallible matches CH2 (loaded at open).

### 4. Core equivalence (both pins)

Spot-checked line by line:

| Core | Location (v24.0.0-rc.3; v23.1.8 where it differs) | Result |
|---|---|---|
| `compressor.cpp` | `IsToKeyID`/`IsToScriptID`/`IsToPubKey`, `CompressScript`, `GetSpecialScriptSize`, `DecompressScript`, `CompressAmount`, `DecompressAmount` (identical at both pins) | identical logic and order. The compressed-key form is unvalidated, as `CPubKey::Set` leaves it; the uncompressed form is validated by `IsFullyValid` (`pubkey.cpp:219`); codes 4 and 5 use `Decompress` (`:226`). |
| `compressor.h` `ScriptCompression::Ser/Unser`, `AmountCompression` | (identical) | `VARINT(size+6)` with `unsigned int` wrapping. A failed `DecompressScript` leaves the fresh script empty. An oversize raw script becomes `OP_RETURN` and its bytes are ignored. The amount is read as a `uint64_t` VARINT then converted to `CAmount`. |
| `coins.h` `Coin::Serialize`/`Unserialize` | `:61`, `:69` (both pins) | `uint32_t` code, 31-bit height, `!IsSpent()` assert |
| `undo.h` `TxInUndoFormatter` | `:26`, `:36` (identical) | a `0x00` dummy after a non-zero height; on read, a `VARINT(unsigned)` version, dropped |
| `undo.h` `CTxUndo`/`CBlockUndo` | (identical) | `VectorFormatter`/vector read semantics; no `MAX_INPUTS_PER_BLOCK` check in Dash, and none in the port |
| `serialize.h` `ReadVarInt` | `:405` / `:401` | check order matches: read the byte, then `n > max>>7`, then `n == max` before the increment |
| `serialize.h` `ReadCompactSize` | `:292` / `:288` | matches |
| `serialize.h` `WriteVarInt` | (10-byte buffer) | matches |
| `serialize.h` `MAX_VECTOR_ALLOCATE` | `:45` / `:41` | matches. The serialize.h pin diff (`BasicByte` concepts, `ReadFixedBitSet` bound) does not touch these. |
| `coins.cpp` `MAX_OUTPUTS_PER_BLOCK`, `AccessByTxid` | `:342-353` (both pins) | `MaxBlockSize()` = 2,000,000 / 9 = 222,222 |
| `CCoinsViewErrorCatcher` | `:355-370` / `:356-379` | as documented |
| `IsUnspendable` | | matches |

**The `write_compressed` argument for codes 4 and 5 is right.** An on-curve x decompresses with parity from `code−2`, and re-compresses to `0x04 | y&1`, which is the same code. An off-curve x reads as an empty script, which writes as `0x06`.

### 5. Bench methodology

**The local patch is sound:**
- `bc2c15278f` adds Core rows on DEC-16's local-branch model.
- Inputs are built from identical fields on both sides; I compared the coin fields and the 500 × 4 record generator line by line.
- Each side decodes into a fresh object, with a rewound stream on Core's side and one padding byte, as in `DeserializeBlockTest`.

**Instruction counts are the right metric on this box,** and slopes across 3 counts agreeing within 0.3% is adequate.

**Two caveats:**
- The Rust slope includes the `Box<dyn Fn>` call and loop. If anything that biases against Rust (a few instructions out of 222–444), so the conclusions are conservative.
- The `p2pk_uncompressed` row compares two different vendored libsecp256k1 versions (secp256k1-sys 0.14.1 against Core v24's). That is fine as a "both are dominated by the square root" statement, not a claim about our code.

See n5 for labelling and the missing encode/transcode rows.

### 6. Conventions

- **Pure crate.** `check-pure.sh` passes. Neither secp256k1 nor its `-sys` crate pulls in threads, clocks or a randomized global context: only `context_no_precomp` parsing and serialization are used. The exact pins are documented as consensus-critical.
- **Sealing.** `CoreUndoBytes` is sealed: a private tuple field, two constructors, a `compile_fail` doctest, length-only `Debug`, and a type distinct from `CoreBytes`. `Decoded::new` is crate-private, with the exactness rule documented. `ScriptRef::special` is crate-private.
- **Fuzz (DEC-30 N5).** The three targets are thin `l1_target!` wrappers, run only through `run.sh` with the private sink, and registered in `node_l1::TARGETS`. Each has proptest twins and L1 boundary rows on `Pin::ALL`.
- **Private seeds.** None are scheduled for CS5a under DEC-30 N2 (that list is CS1/CS3/CS5b), so there is no DEC-32 manifest flip at this merge.
- **Vectors.** The Core-written vectors have provenance (binary hashes, a reproducible script, byte-identical records at both pins). The mainnet replay of `rev00000` checks every undo coin against the output it spends and ends at Core's 1,269 UTXOs.

## Verdict

**APPROVE-WITH-NITS.** The codecs are Core-exact at both pins, panic-free on untrusted bytes, allocation-bounded and well evidenced. The fallible `CoinsView` meets DEC-77 and DEC-78 with no path that turns a read error into a missing coin.

- m1–m4 are API-fitness gaps for CH2, CH3a and MP1. None affects correctness, and each can land before merge or as a CS5a follow-up ahead of CH2a/CH3a; m2 and m3 are small enough to do now.
- G1 (the ≥ 24 CPU-h fuzz budget per new target) must be satisfied before merge.
