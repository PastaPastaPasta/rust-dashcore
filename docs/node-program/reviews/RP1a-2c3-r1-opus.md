# RP1a-2c-3 review, round 1 (Opus)

- Branch `node/rp1a-2c3`, head 119526c2d; base 49196c094. Reviewed `git diff 49196c094 119526c2d` (3 commits, 11 files, +826/−21).
- Core reference: `v24.0.0-rc.4:src/rpc/blockchain.cpp` (written as `bc.cpp` below), `merkleblock.cpp`, `common/bloom.{h,cpp}`, `serialize.h`, `util/strencodings.{h,cpp}`, `rpc/util.cpp`.

## Verdict: ACCEPT-WITH-NITS

The handler is a faithful port of Core's `getmerkleblocks` (`bc.cpp:827-924`), and so is the node's walk. I found no reply rdashd can give that dashd v24.0.0-rc.4 can't. The reviewed checks are:

- the argument order and limits;
- every error text and code;
- the walk (start, `Next`, the count cap, the stop at the tip, off-chain starts);
- one filter shared and updated across the walk;
- the skip of blocks with no match;
- the hex array reply.

My own run of the DIFF passes byte for byte. The one gap that matters is in a test, not the code: a cheap `ParseHex` whitespace mutant survives both the unit tests and the DIFF (m-1).

Counts: M 0, m 1, n 2.

---

## Findings

### m-1: `ParseHex`'s `\v`, `\f` and `\r` are untested; dropping them survives

`dashd-rpc/src/blockchain/merkle.rs:35`. Core's `IsSpace` is `' ' '\f' '\n' '\r' '\t' '\v'` (`util/strencodings.h:168-170`), and the port lists all six. The tests reach only three of them:

- the unit test `merkle/tests.rs:33` uses `' '`, `\t` and `\n`;
- the DIFF case `"spaced"` (`rdashd/tests/rpc_diff.rs:2665`) uses only spaces.

**Mutant (run in my copy):** `matches!(c, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r')` → `matches!(c, b' ' | b'\t' | b'\n')`. Results: `cargo test -p dashd-rpc --lib merkle` passes 3/3, and `getmerkleblocks_match_dashd_v24` passes. The mutant survives.

**Failing input with the mutant:** the params `["03\u000b000000010000000000000000", "<genesis>", 1]`. Here dashd skips the `\v` and reads the filter, so it answers `[]` or the merkle block. The mutant reads the empty vector instead, so it answers `DataStream::read(): end of data: iostream error`. `\f` and `\r` fail the same way.

**Fix:** add a DIFF filter case with each of `\t \n \v \f \r` between pairs. As JSON escapes these are `\t \n \u000b \f \r`, so dashd itself confirms the set. Also add a unit assertion such as `parse_hex_lenient("\r0a\x0b0b\x0c")` equal to `[0x0a, 0x0b]`.

### n-1: the node's start-block pruned check has no test; D45 overstates it

`dashd-node/src/rpc.rs:615` (`if pruned { return Err(BlockReadError::Pruned) }` in `Writer::merkle_blocks`).

D45 (`RP1a-notes.md:355`) says "the pruned error is unit-test only". The unit test (`merkle/tests.rs`, the `BlockReadError::Pruned` arm) fakes `Chainman::merkle_blocks`, so it tests only dashd-rpc's mapping to "Block not available (pruned data)". Nothing runs the node's check:

- the node can't prune (D31);
- no dashd-node test calls `merkle_blocks` with `have_pruned`.

So deleting the branch survives. It is unreachable today, and `read_block` (`:570`) and `read_undo` (`:661`) have the same gap.

**Fix:** reword D45 to "the mapping is unit-tested; the node's check is unreachable until CH8 (D31)". When CH8 lands `-prune`, add the pruned start case to the DIFF.

### n-2: redundant dev-dependencies in dashd-node; D45 records no layer result

`dashd-node/Cargo.toml:60-61` makes `dash-p2p-core` (with `std`) and `dash-types` normal dependencies. The dev-dependency lines `:77` (`dash-types`) and `:80` (`dash-p2p-core`, `std`) are now redundant: they add no features. Their comments ("Regtest blocks mined…", "P3's (A − B) node leg…") also now explain an edge that the normal section already gives.

D44 records for D43 that the seam "adds no crate edge" and that `check-layers.py` reports the same layers. D45 adds three edges and says nothing about layers:

- dashd-node → dash-p2p-core (normal);
- dashd-node → dash-types (normal);
- rdashd → dashd-p2p (dev).

None breaks a rule:

- dashd-node already reached dash-p2p-core through dashd-p2p;
- dev edges never reach a dependent;
- dashd-rpc gains no edge, because `BloomFilterData` keeps it off dashd-p2p.

My run of `contrib/node/check-layers.py` at the head gives `layers ok: L1 L2 L3 L8 over B 9, P 7, Pn 3, S 7, V 1, N 11, W 11, A 5, T 11` (rc 0).

**Fix:** drop the two dev-dependency lines, or keep them with a comment, and add a one-line layer statement to D45 like D44's.

---

## What I checked

**Parity with Core rc.4 (`bc.cpp:827-924`):**

- **Order.** `EnsureAnyChainman`, then `ParseHex`, the precheck, `>> filter`, `IsWithinSizeConstraints`, `ParseHashV("blockhash")`, `LookupBlockIndex` ("Block not found", -5), then `count` (default and maximum `MAX_HEADERS_UNCOMPRESSED_RESULT` 2000; `<= 0 || > 2000` gives "Count is out of range", -8), then `GetBlockChecked`. This matches `merkle.rs:117-141`.
- **Argument types.** RPCHelpMan's type check (`rpc/util.cpp:525-547`) comes from the generated spec table (`dash-rpc-spec`). The DIFF reaches it with `filter_number`, `filter_null`, `hash_number`, `count_string` and `count_bool`.
- **`TryParseHex` (`util/strencodings.cpp:80-97`).** Whitespace is skipped only before a pair's first digit, and the second digit is read unconditionally. The port matches. Non-ASCII bytes are not hex digits in either.
- **The filter read.**
  - The precheck (`bc.cpp:850-878`): a canonical count over 36,000 answers the size error when the whole `vData` and the 9-byte trailer follow, and end of data otherwise. A count that fails to read falls through.
  - `LIMITED_VECTOR`: the branch at `merkle.rs:83-87` is unreachable after the precheck, as its comment says, and it mirrors `serialize.h:1018-1031`.
  - Trailing bytes are ignored.
  - The `": iostream error"` suffix and the `ReadCompactSize` texts are confirmed by the DIFF.
- **The walk.**
  - Core starts at the start block: `for (; pblockindex; pblockindex = ActiveChain().Next(pblockindex))` with `if (--nCount < 0) break`.
  - The port in `dashd-node/src/rpc.rs:601-612` reads the start block always. Its successors are read only when `active_contains_private`, at heights `e.height+1 ..= min(tip, e.height+count-1)`. That equals `CChain::Next`, including the stop at the tip.
  - Pruning is checked for the start block only (`GetBlockChecked`, `bc.cpp:786-803`). Any read failure is "Block not found on disk": a headless start has a null `block_pos`, so the read fails, which the DIFF `headless` case covers.
- **Matching.**
  - Core calls `CMerkleBlock(block, filter)` (`merkleblock.cpp:30-72`) with one `CBloomFilter` through the whole loop. The port passes `merkle_block(&block, &mut filter)` one `BloomFilter` for the whole walk.
  - `allowedTxTypes` equals `!special || !matches!(ty, 6 | 7 | 13..)` against rc.4's enum (`primitives/transaction.h:29-41`).
  - The P4b `BloomFilter` against `bloom.cpp` (`contains`, `insert`, empty "match-all", `IsRelevantAndUpdate`'s order, `ProcessTxOut`'s mask, special-tx arms) shows no difference that this RPC reaches.
  - Blocks with no matched transaction are skipped.
- **The reply.** An array of hex `merkleblock`s in chain order; `[]` when nothing matches.

**Read order.**

- The start entry, its `IsBlockPruned`, and the whole active-chain slice are taken in one V query turn. This is the `cs_main` snapshot Core holds. The reads afterwards come from the slot's block files.
- Block files are append-only, and rdashd can't prune (D31). So a block disconnected (or reorged out) after that turn still reads, and the reply is exactly Core's at the snapshot instant.
- The earlier `lookup_block_index` turn can't go stale, because index entries are never removed.
- Unlike Core, the walk doesn't hold V while it reads up to 2,000 blocks. That is an improvement, with no visible difference.

**DIFF test (`rpc_diff.rs:2432-2787`).**

- `diff_batch` compares the raw HTTP replies with only `Date` masked, and reports per call where the replies parse. That is byte for byte.
- It reaches:
  - the off-chain start (`off_chain_5`, an invalidated tip with a branch above its parent);
  - the unknown hash;
  - count 0, −1, 2001, 1.5 and 2^32+1;
  - the help and arity errors;
  - headless and stored-not-connected starts;
  - the count boundaries 102/103 around the spend.
- The final asserts check that dashd's own replies have the expected match counts, so the cases are not vacuous.
- The fixture is one Core accepts:
  - dashd signs with `signrawtransactionwithkey` (`"complete":true` asserted);
  - dashd mines the spend itself with `generateblock`, which runs `TestBlockValidity`;
  - `91avARGdfge8E4tZfYLoxeJ5sGBdNJQH4kvjJoQFacbgwmaKkrx` is the standard testnet/regtest uncompressed WIF of secret 1;
  - Core's P2PK signing looks up the key by `CKeyID(CPubKey(uncompressed))`, so the uncompressed WIF is really required.

**Mutants.**

- I read the implementer's 12 (`/work/scratch/rp1a-2c3/mut-manifest.txt`, `mut-r2.log`); all die.
- I ran 1 of my own (m-1); it survives.
- Two more I can name survive by construction: the node's pruned branch (n-1), and the unreachable `LimitedVector` branch.

**Test-file edits.** These touch only what the new trait method needs:

- `blockchain/tests.rs`: the import, `Live::merkle` (`Live` derives `Default`) and the fake's `merkle_blocks`;
- `mining/tests.rs`: the import and an `unreachable!` `merkle_blocks`.

**Ran** (in the reflink copy `/work/scratch/rp1a-2c3-r1-opus`, `CARGO_BUILD_JOBS=8`, nice 19, `TMPDIR` inside the copy):

- `python3 -I contrib/node/check-layers.py`: ok, rc 0.
- `cargo test -p dashd-rpc --lib merkle`: 3/3.
- `cargo test -p rdashd --test rpc_diff getmerkleblocks`: passes against dashd v24.0.0-rc.4 (28 s).
- The m-1 mutant: lib 3/3 and the DIFF pass; the file was restored afterwards (`cmp` against the worktree).
- I also read `bi.log` (bench_inputs 13/13 SAME, rc 0) and `fc1.rc`/`fc1.log` (`rpc_getmerkleblocks.py` pass, rc 0).
- I did not rerun fmt, clippy or the full dashd-rpc/dashd-node suites.
