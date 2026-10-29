# CH2b b4 fix: `write_script` inline (740569dba). Narrow Opus review, round 1

- **Target:** `node/ch2b-b4` at `740569dba`, parent `dca5a930e`. I reviewed it in the detached scratch worktree `/work/scratch/ch2b-b4-fix-r1/wt`, which is now deleted.
- **Files:** `dashd-chainstate/src/coins/packed.rs` (+45/−4), and the D83 row in `docs/node-program/designs/CH2b-b4-notes.md`.
- **Core reference:** `~/workspace/dash` at `v24.0.0-rc.3`: `compressor.cpp` and `compressor.h`.
- **Oracle:** the v24.0 pin `77dbec824` (`v24.0.0-rc.3-150`). Its `compressor.*`, `pubkey.*`, `serialize.h` and `script/script.h` are identical to rc.3. `coins.h` differs only by an oracle-hook accessor.

**Verdict: ACCEPT WITH NITS.**

`write_script` is byte-for-byte identical to `ScriptRef::write_compressed` and to Core v24's `ScriptCompression::Ser`. I checked this two ways: by reading the code, and by a differential test against the Core v24 oracle over every form. The commit changes no on-disk format.

There is one Minor finding. The existing test does not pin every form, although the commit message and D83 say it does. Three hand mutants that change the output bytes survive the whole default-feature dashd-chainstate suite. A four-line test addition, verified below, kills all three.

## 1. Equivalence by reading

### Local code against upstream

`ScriptRef::write_compressed` is at `dash-consensus/src/coins/script.rs:223`. `write_compressed_script` is at `compress.rs:183`.

| Input | Upstream writes | `write_script` (`packed.rs:213-238`) writes |
|---|---|---|
| `Form::Script(s)`, `compress_script(s) = Some((c, d))` | `[c]`, then `d` | the same `(c, d)`, through the single write site at `:228-229` |
| `Form::Script(s)`, `compress_script(s) = None` | `VARINT(len as u32 wrapping+ 6)`, then `s` | `write_raw_script(s)` at `:235-238`: the same expression |
| `Form::Special{c, d}` with `c < 4`, or decompress succeeds | `[c]`, then `d` | the same (`:219-223`) |
| `Form::Special{c, d}` off the curve (codes 4 and 5) | `write_compressed_script(&[])`, which is `compress_script(&[]) = None`, so `VARINT(6)` = `0x06` | `write_raw_script(&[])`, also `0x06` (`:225-227`) |

The `else` at `:224` would also catch "neither `as_script` nor `as_special`". That cannot happen, because `Form` has exactly two variants. The calls to the writer are the same in both versions: a 1-byte code then the data, or a varint then the script. So the `ArrayWriter` overflow path, followed by the `Vec` rewrite in `from_coin` (`:78-90`), behaves the same way.

### Against Core v24

**`CompressScript`** (`compressor.cpp:55`) and `IsToKeyID`, `IsToScriptID` and `IsToPubKey` (`:19-53`) match `compress_script` exactly:

- **P2PKH (code 0):** 25 bytes `76 a9 14 … 88 ac`. Core and `compress_script` check the same positions.
- **P2SH (code 1):** 23 bytes `a9 14 … 87`. Same checks.
- **Compressed P2PK (codes 2 and 3):** 35 bytes, a `0x02` or `0x03` key prefix, then `0xac`. Neither side checks the key: Core's `CPubKey::Set` checks only the length against the header. The code is the prefix byte itself.
- **Uncompressed P2PK (codes 4 and 5):** 67 bytes, a `0x04` prefix, then `0xac`, and `IsFullyValid`. The Rust uses `PublicKey::from_slice`, which is `secp256k1_ec_pubkey_parse`. The code is `0x04 | (key[64] & 1)`, and `key[64]` is `s[65]`. A `0x04` key off the curve returns false in Core and `None` in Rust, so both write it raw. Hybrid `0x06`/`0x07` prefixes fail `script[1] == 0x04`, so both write them raw.

**`ScriptCompression::Ser`** (`compressor.h:65`) is:

```
unsigned int nSize = script.size() + nSpecialScripts; VARINT(nSize); Span{script}
```

with `nSpecialScripts = 6` (`:62`). `(len as u32).wrapping_add(SPECIAL_SCRIPTS)` matches that `unsigned int` arithmetic.

**Oversize scripts.** `Ser` has no branch for scripts over `MAX_SCRIPT_SIZE`. Core writes an oversize script whole, as a raw script. Only `Unser` (`:77-95`) replaces a stored script over 10,000 bytes with `OP_RETURN` and skips its bytes. The upstream `read_script_ref` does the same. On the write side, `write_script` writes the whole script, as Core does. A raw form read back as oversize becomes `ScriptRef::from_script(&[0x6a])`, which re-encodes as `07 6a`. Core produces the same bytes: the oracle check below asserts `== [7, 0x6a]`.

**Special forms read back.** For codes 4 and 5, `DecompressScript` returns false off the curve and leaves the freshly constructed script empty, so `Ser` writes `0x06`. The local code does the same. Codes 0–3 always decompress, so `CompressScript` returns the same code and bytes, and the local code writes them straight through. That shortcut is valid: when I mutated `code < 4` to `code < 2` (M7b), the results did not change.

The citations `// Core: compressor.h:65 ScriptCompression::Ser` and `compressor.h:62` are correct at rc.3.

## 2. Equivalence against the Core oracle (scratch test, now deleted)

I added a scratch module `review_oracle` to `packed.rs`, plus a dev-dependency on `dash-oracle-sys` with the `oracle` feature. It contained two tests.

**`write_script_is_cores_script_compression`.** For every input it compared `write_script`, the upstream `write_compressed`, and the oracle's `CoinCodec::ScriptCompress`. It covered three groups of input.

Raw scripts from outputs, about 20.6k:

- P2PKH and P2SH.
- Compressed P2PK with `0x02` and `0x03`, on and off the curve.
- Uncompressed P2PK from 16 real keys, with both y parities.
- The same keys with the y coordinate flipped, so off the curve.
- `0x04` keys of all `ff` and all `00`.
- Hybrid `0x06`/`0x07` prefixes.
- Every fixed byte of every special form XOR-ed with 1, and also set to 0.
- Every special form one byte short, one byte long, and with a byte prepended.
- The empty script.
- Lengths 1, 2, 20, 21, 32, 33, 65, 66, 120–123, 127, 128, 520, 9,999, 10,000, 10,001, 16,499, 16,500, 16,505 and 20,000, both as `0x51` repeated and as random bytes. These cover the varint 1→2-byte boundary at lengths 121/122, and the oversize sizes.
- `OP_RETURN` scripts.
- 20k random scripts, biased toward the special lengths and their fixed bytes.

Each script was also checked after Core reads it back: `ScriptDecompress`, then `ScriptCompress`, against `write_script(read_script_ref(..))`. The coverage counts for codes 0–5 and raw were 786 / 765 / 414 / 382 / 12 / 4 / 18,249. Every form was hit.

Special forms as the decoder makes them, about 3.1k: codes 0–5, each with 500 random payloads, all-`ff`, all-`00`, and 16 real x coordinates. Of the code 4 and 5 forms, 533 were on the curve and 503 off it. Each was compared against Core's `ScriptCompress(ScriptDecompress(..))`.

Oversize raw forms of 10,001 and 20,000 bytes, read back. Core gives `07 6a`, and `write_script` matches.

**`from_coin_is_cores_coin_encode`.** It compared `PackedCoin::from_coin(..).as_bytes()` with the oracle's `CoinCodec::CoinEncode` on 11,780 coins. These are every 7th script in the corpus, each at 4 height, coinbase and value combinations, including height 2²⁷−1 and `MAX_MONEY`.

**Result:** every assertion passed. `write_script` agrees with both upstream and Core v24 on every input tested.

## 3. Mutants

Each mutant was applied alone to `write_script` or `write_raw_script` and reverted before the next. "Existing" means the commit's `coins::packed::tests`; "oracle" means my scratch tests above.

| # | Mutant | Existing tests | Oracle tests |
|---|---|---|---|
| M1 | Swap codes 4 and 5 at the single write site | **caught** (`write_coin_is_coin_ref_write`) | caught |
| M2 | Drop the on-curve check (`… \|\| true`) | **caught** | caught |
| M3a | Raw varint offset `SPECIAL_SCRIPTS + 1` | **caught** (3 tests) | caught |
| M3b | Raw varint offset `SPECIAL_SCRIPTS - 1` | **caught** (3 tests) | caught |
| M4a | Compressed P2PK prefix 3 → 2, both paths | **missed** | caught |
| M4b | Compressed P2PK prefix 2 → 3, both paths | **caught** | caught |
| M4c | Compressed P2PK prefix 3 → 2, output path only | **missed** | caught |
| M5 | "Oversize branch": apply `Unser`'s rule on write (write len > 10,000 as `OP_RETURN`) | **caught** | caught |
| M5b | Raw length capped at 10,000 | **caught** | caught |
| M6 | Off-curve fallback writes `[0x00]` instead of the empty script | **caught** | caught |
| M7 | `code < 4` → `code < 5` | **caught** | caught |
| M7b | `code < 4` → `code < 2` | missed | missed: **equivalent**, because codes 2 and 3 always decompress |
| M8 | Raw path skips `compress_script` | **caught** (4 tests) | caught |
| M9 | Swap codes 4 and 5 on the output path only | **missed** | caught |
| M10 | Swap codes 4 and 5 on the decoded-special path only | **caught** | caught |
| M11 | Drop the data write | **caught** (4 tests) | caught |
| M12 | Raw path drops the script bytes | **caught** (3 tests) | caught |

The brief suggested a "skip the oversize branch" mutant. It does not apply as written, because there is no oversize branch on the write side, in Core or here (see §1). M5 and M5b test the inverse instead: they add or approximate one. Both are caught.

M4a and M9 also pass the whole default-feature `cargo test -p dashd-chainstate`: 9 test binaries, zero failures, with `review_oracle` skipped.

## Findings

### m-1 (Minor): `write_coin_is_coin_ref_write` does not pin code 3, or codes 4 and 5 from an output

- **Where:** `dashd-chainstate/src/coins/packed.rs:363-371`. The claim appears in the doc comment at `:351-353`, in D83 ("pins the encoding for every script form (codes 4 and 5 on and off the curve)"), and in the commit message ("for every script form").
- **What is missing:** the `scripts` list has only a `0x02` compressed P2PK. It has no `0x03`, and no 67-byte uncompressed P2PK at all. Codes 4 and 5 are reached only through the decoded `[0x02, 0x00, code, x]` loop at `:392-397`. That loop exercises the `as_special` arm, not the `as_script` → `compress_script` arm.
- **Failure scenario:** a later edit to `write_script` writes code 3 as code 2 (M4a or M4c), or swaps codes 4 and 5 for scripts taken from an output (M9). The whole default-feature dashd-chainstate suite stays green. The coins DB then gets bytes Core would not write. For a coin with code 3, that is a different key prefix, so the coin decompresses to a different script, which is a consensus-relevant corruption of the UTXO set. The code under review is correct; the gap is that the test does not guard it.
- **Fix (verified):** add the following to `scripts`, with `gy` and `neg_gy` (the generator's y, and the negated point's y) as constants next to `gx`:

  ```rust
  [&[33, 0x03][..], &gx, &[0xac]].concat(),
  [&[65, 0x04][..], &gx, &gy, &[0xac]].concat(),      // code 4
  [&[65, 0x04][..], &gx, &neg_gy, &[0xac]].concat(),  // code 5
  [&[65, 0x04][..], &gx, &[0xff; 32], &[0xac]].concat(), // off the curve: raw
  ```

  - `gy` = `483ada77…fb10d4b8`, which is even.
  - `neg_gy` = `b7c52588…04ef2777`, which is odd.

  I applied this in scratch. Unmutated, the test passes. With the addition, `write_coin_is_coin_ref_write` fails under M4a, M4c and M9.
- **Also:** reword D83 and the test doc to match the coverage. Once the scripts are added, the claim becomes true.

### n-1 (Nit): the `else` arm's comment covers only one of its cases

`packed.rs:226` says "Core read an empty script for an x coordinate off the curve". That is the only case that can reach the arm, so the comment is accurate. A reader still has to work out that `!as_script && !as_special` cannot happen. An optional fix is to say so ("…; a `ScriptRef` is always one of the two forms").

## On-disk format

The commit changes no on-disk format:

- `git diff dca5a930e 740569dba` touches only `write_coin`'s script write, two new private functions, the imports and doc comments, and the D83 row.
- The `PackedCoin` representation, `INLINE_MAX`, `from_db`, `as_bytes`, and the coins DB key and value encodings are untouched.
- The value bytes are Core's `Coin` serialization. The oracle `CoinEncode` comparison above shows the bytes are unchanged for every tested coin, and the existing round-trip tests pass.

## Not checked

- The perf claim (the four PF0 rows at +0.000% against 4d85bfd4b). It was outside the brief, so I did not re-measure.
- Feature-gated suites (`alloc-tag`, `crash`, `planted-bugs`). I did not run them on the surviving mutants. They are not expected to exercise code 3 or uncompressed P2PK outputs.

## Verdict

**ACCEPT WITH NITS.** The code is byte-identical to upstream and to Core v24 for every form. m-1 is a test-coverage gap with a verified four-line fix. It is cheap enough to land before merge, but the merge does not depend on it.
