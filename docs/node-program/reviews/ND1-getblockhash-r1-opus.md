# ND1 `getblockhash` (DEC-279), Opus review round 1

**Head reviewed:** 3852ef564 on `node/nd1-rpc-chain`, base 85345d971 (`node/nd1-2b2`)
**Reference:** Dash Core v24.0.0-rc.3 (`git show v24.0.0-rc.3:…`)
**Scope:** `git diff 85345d971 3852ef564` (10 files, +353/−7); read-only. Implementer notes: `/work/scratch/nd1/plan-2b.md` 2b-D26, 2b-D28, 2b-D30.

## Method

Read the full diff and these sources at the reviewed head:

- RP0: `dashd-rpc/src/server.rs` (`Command::handle`, `valid_num_args`, `type_mismatch`), `request.rs` (`param`, `get_int_i32`), `dash-args/src/univalue.rs` (`get_int64`), `http/mod.rs` (`stop`, the worker's `catch_unwind`).
- V: `dashd-chainstate/src/chain/view.rs` (`ChainView`, `ChainViewHandle`), `actor.rs` (`view()` = `ArcSwap::load_full`, `scope_end` → `publish_view` before the reply, and the `Stage` order Snapshot → Published → Spliced → Watched → Ui), `dash-chain/src/snapshot.rs` (`ActiveChainView::{height,at,base}`), `reader.rs` (`ChainReader::entry`), `active.rs` (anchoring/`set_base`).
- Node: `dashd-node/src/node.rs` (step 7 loop, `shutdown`), `steps.rs` (`load` ⇒ `Ok` only once V started and the coins attached), `chain.rs` (`wait_for_genesis` waits on `view().snap.has_tip()`).
- Core: `rpc/blockchain.cpp:578-605` (getblockhash), `rpc/server_util.cpp:87-98`, `rpc/util.cpp:525-551, 747-758` (`HandleRequest`, `MatchesType`), `univalue.h:137-147` (`getInt<Int>`), `rpc/server.cpp:480, 501`.

Ran in a `git archive` copy (`/work/scratch/nd1-gbh-review`, since deleted):

    cargo test --locked -p dashd-rpc --lib blockchain          # 2 passed
    # mutant: swap ensure_chain / get_int_i32 order (blockchain.rs:50-51) → 2 passed (survives, N1)
    cargo test --locked -p rdashd --test rpc_diff              # 1 passed, 70 s, dashd v24.0.0-rc.3
    #   with the branch's 15 getblockhash cases plus 10 extra cases (below): all byte-identical

The extra differential cases, all matching dashd: `[9223372036854775808]`, `[123456789012345678901234567890]`, `{"h":0}` (unknown named), `{"height":"0"}`, `{"height":0,"x":1}`, `{}`, `[true]`, `[[0]]`, `[-0.5]`, and a batch `[getblockhash 0, getblockhash 5]`.

## Answers to the review questions

### 1. Byte parity with Core

It holds for every class in the brief, both by reading the code and in the run above.

- **Order of checks.** Core runs `IsValidNumArgs` (which throws the help text), then `MatchesType` (`RPC_TYPE_ERROR`, "Wrong type passed"), then the body's `EnsureAnyChainman`, then `getInt<int>`, then the range check. Rust does the same: the first two are RP0's `Command::handle`, then `ensure_chain`, `get_int_i32` and the range check in `blockchain.rs:50-54`.
- **`getInt<int>`.** Core runs `from_chars<int>` over the whole number text. `get_int_i32` runs `from_chars_i64` over the whole text and then `i32::try_from`. Both accept the same inputs and both raise "JSON integer out of range" (-1) otherwise. That covers `-0`, `1.5`, `0e0`/`1e0`, int overflow and underflow, values beyond i64 and huge values; I checked the last two against dashd.
- **Range.** Core's `nHeight < 0 || nHeight > Height()` maps to -8 "Block height out of range", and `Height()` is −1 on an empty chain (`Published::height`). Above the tip (`[1]`), `INT_MAX`, `-1` and `INT_MIN` all match.
- **Result.** `BlockHash`'s `Display` is `GetHex`, with the bytes reversed. The unit test pins this (`ends_with("01")`) and so does the genesis differential.
- **Help, arity, named arguments.** These come from the dash-rpc-spec v24 entry through `Command::new`. `help getblockhash`, `[]`, `[0,1]`, `{"height":0}`, unknown or extra named arguments, and `{}` all match.

### 2. Races and ordering

No race found.

- **Publish before reply or notify.** V stores the `ChainView` in `scope_end` before it replies (actor.rs:1404-1412). In a hold release, the view is published before the signals splice, before `TipWatch` and before `NotifyBlockTip` (the `Stage` order). The RPC reads that same `ArcSwap`, so a call that follows a `process_block` reply or a tip notification sees that block. The SUT test checks this after each reply.
- **Startup.** `attach` runs at step 7 after `steps.load` returns `Ok`, which only happens once V has started and the coins are attached. That is before step 11, whose `wait_for_genesis` waits on the same published view, and before `set_warmup_finished` at step 13. Every command answers -28 with the init message until step 13, so no request can see an unattached slot. The `None` path ("Node chainman not found", -32603) is reachable only through the API, as in Core, where `node.chainman` exists before the RPC warmup ends. The step-7 loop runs at most once without a GUI, so the `attach` assert can't fire on a retry.
- **Shutdown.** `interrupt()` makes the table refuse new calls. `rpc.stop()` joins every HTTP worker (`http/mod.rs:414-433`), so no `Published` (which holds a `ChainReader` clone) is still alive when `detach()` runs. V stops after that, in `steps.shutdown()`. The handle is released before `self.lock = None`. Without `-server`, the slot is still attached and still detached, which is harmless.

### 3. Interface shape

- **Hot path.** The diff makes no change to V. The read side is an `ArcSwap::load_full`, one `ChainReader` clone (an `Arc`), and one `Box` per call, all on the RPC worker. The `ChainSlot` mutex is taken only by RPC threads, and only long enough to clone. Nothing lands on V's hot path.
- **For RP1a.** It is a sound base: one `Box<dyn ActiveChain>` per call holds one published view, so a command's reads are coherent, matching Core's single `cs_main` section. The layer rules would allow dashd-rpc to depend on dashd-chainstate directly, so the trait is a choice (fakes in unit tests, a light dashd-rpc build), not a constraint. That is reasonable, but RP1a will widen it a lot (lookup by hash, entries, MTP/IBD, ChainLocks, the `query` path for `getchaintips`). See N4.

### 4. Tests

- The differential compares the whole HTTP reply byte for byte, headers included, with only `Date` masked. Both nodes wait for "init message: Done loading", which is logged after the warmup ends in both, and `find_dashd` panics rather than skips when dashd is missing. So the cases really are compared, and the genesis case would catch a wrong byte order or a wrong height mapping. Heights above 0 are compared only in the SUT test, against the hashes of mined blocks, until `submitblock` lands (2b-D28 says so).
- Gaps: the chainman-before-`getInt` order is untested (N1), `Node::shutdown`'s `detach` is untested (N3), and there are no differential cases for the beyond-i64 branch or for named-argument errors (N2). They all pass today, so these are nits.

### 5. Correctness

No defects found. The `expect`s in `Published` (rpc.rs:127, :135-136) can't fire: the range is checked first; in node mode the active chain starts at 0 (anchoring and `set_base` are SPV-only); index entries are never evicted outside SPV; and the HTTP worker catches unwinds in any case (N5).

## Findings

### B (blocking)

None.

### N (nits)

**N1. The chainman-before-`getInt` order is untested.** `dashd-rpc/src/blockchain.rs:50-51`, `dashd-rpc/src/blockchain/tests.rs:93`.
- Scenario: swap the two lines, so a bad height is parsed before the missing-chain check. Both unit tests still pass (I ran this mutant). With no chain, `[1.5]` would then answer -1 "JSON integer out of range" where Core answers -32603 "Node chainman not found". The path can't be reached over HTTP today, but RP1a will copy this body's shape.
- Fix: in `getblockhash_without_a_chain_is_cores_missing_chainman`, also assert that `[1.5]` and `[-1]` with no chain answer `RPC_INTERNAL_ERROR` "Node chainman not found".

**N2. The differential has no beyond-i64 or named-argument error cases.** `rdashd/tests/rpc_diff.rs:126-146`.
- Scenario: `get_int_i32` has two failure branches, `get_int64` failing (beyond i64) and `i32::try_from` failing. Only the second is pinned against Core (`2147483648`, `-2147483649`). A change that mapped `get_int64`'s error straight through (`?`) instead of `.ok()` would still give the same text today, but nothing would catch a change to it. The named-argument errors for this command aren't pinned either.
- Fix: add `[9223372036854775808]`, `{"h":0}`, `{"height":"0"}` and a two-call batch, for example `[{…[0]…},{…[5]…}]`. I ran all four against dashd v24.0.0-rc.3 and they match.

**N3. `Node::shutdown`'s `detach` is untested.** `dashd-node/src/node.rs:362`.
- Scenario: deleting the line leaves the `ChainViewHandle`, and the `ChainReader` backend and stores behind it, alive in `Node::rpc_chain` after `self.lock = None` releases the datadir lock. The ChainSlot doc comment (rpc.rs) says this must not happen. The SUT test calls `detach` on its own slot, so it doesn't cover `Node`.
- Fix: in a dashd-node test that runs `Node` init and then `shutdown()` (any existing one with a loaded chainstate), assert `node.rpc_chain.active_chain().is_none()` afterwards, or count the handle's strong references.

**N4. Name and shape the seam for RP1a now.** `dashd-rpc/src/blockchain.rs:26-34`.
- Scenario: RP1a's `getblockheader`, `getblock`, `getblockchaininfo` and `getchaintips` read things that aren't on the active chain: a lookup by hash, entry fields and status, MTP/IBD, ChainLocks, and V queries. If they are added to a trait called `ActiveChain`, the name becomes wrong. Also, `block_hash`'s panic-on-precondition contract doesn't carry over to fallible reads by hash.
- Fix: rename it now, while it has one implementer, to something like `PublishedChain` or `ChainRead`, and record in 2b-D28 that RP1a widens this trait (and `Published`) rather than adding a second seam. For new methods, prefer `Option` returns, with the command mapping `None` to Core's error.

**N5. Write down the node-mode invariant behind `Published::block_hash`'s `expect`s.** `dashd-node/src/rpc.rs:133-137`.
- Scenario: `ActiveChainView::at` returns `None` below `base()` (anchored or SPV-windowed chains), and `ChainReader::entry` returns `None` for evicted entries. Neither happens in node mode, but the comment only says "an entry's hash is immutable".
- Fix: add one line saying that a node's active chain starts at 0 and its index never evicts (`ChainConfig::node`), so every height in `0..=height()` is answered.

**N6. Cite and note drift.**
- `dashd-rpc/src/blockchain.rs:48` cites `rpc/blockchain.cpp:580-607`. At rc.3 the function is 578-605 (607 is `getblockheader`).
- `plan-2b.md` 2b-D28 says the differential "gains 13 `getblockhash` cases", but the branch has 15 (`getblockhash_minus_zero` and `getblockhash_int_underflow` were added). Update the row, and 2b-D30 if needed.

## Verdict

**ACCEPT-WITH-NITS.** No blocking items. Parity with Core holds for every class in the brief and for 10 extra cases run against dashd v24.0.0-rc.3. The publish-before-reply/notify order, the startup and shutdown ordering, and V's hot path are all sound. N1–N3 are cheap test additions, and N4 is worth doing before RP1a builds on the seam.
