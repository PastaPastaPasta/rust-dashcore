# ND1 submitblock (DEC-279), Opus-h review round 1

**Head reviewed:** 9c9b4ddd6 on `node/nd1-submitblock`, two commits on ND1 persistence 4a041b71c
**Reference:** Dash Core v24.0.0-rc.3 (`git show v24.0.0-rc.3:…`)
**Scope:** `git diff 4a041b71c 9c9b4ddd6` (13 files, +822/−28); read-only.

**Verdict: ACCEPT WITH NITS.** No blocker or major. Two minors, both in the abort tail, and four nits.

## Method

I read the full diff, and these sources at the reviewed head:

- **dashd-rpc:** `src/mining.rs`, `src/mining/tests.rs`, `src/blockchain.rs` (the `NodeChain`/`Chainman` seam).
- **dashd-node:** `src/rpc.rs` (`ChainSlot`, `Writer`, `Catcher`), `src/node.rs` (shutdown order), `src/sut.rs` (the new test).
- **dashd-chainstate:**
  - `chain/accept.rs`: `process_block_accept`, `process_block_tail`, `accept_block`.
  - `chain/abc.rs`: `connect_tip`.
  - `chain/actor.rs`: `process_block`, the `ProcessBlock` dispatch, `flush_now`.
  - `chain/signals.rs`: `register`, `unregister`, `each`.
- **dash-chain:** `reader.rs`, `shared.rs` (`lookup`, `entry`), `status.rs` (`is_valid`, `FAILED_MASK`).
- **dash-consensus:** `primitives/decode.rs` (`decode_block`), `primitives/canonical.rs` (`is_coinbase`).
- **Core:**
  - `rpc/mining.cpp`: 531-548 (`BIP22ValidationResult`), 991-1066 (`submitblock_StateCatcher`, `submitblock`).
  - `validation.cpp`:
    - 3395-3450 (`ConnectTip`, `BlockChecked` at :3432);
    - 3724-3845 (ABC; the `ShutdownRequested` break at :3841 comes after one step);
    - 2985-3070 (`FlushStateToDisk`);
    - 4759-4865 (`AcceptBlock`);
    - 4867-4906 (`ProcessNewBlock`).
  - `core_read.cpp:144-159` (`DecodeHexBlk`), `chain.h:114-118, 300-308` (`BLOCK_FAILED_MASK`, `IsValid`), `consensus/validation.h:105-116` (`ToString`).

I ran these in a detached worktree (`/work/scratch/nd1-submitblock-review/wt`, its target reflink-seeded from the implementer's, both since removed):

    cargo test --locked -p dashd-rpc -p dashd-node --features test-support
    #   dashd-node lib 139 passed (1 ignored), asserted_log_lines 11, index_scan_cache 0 (2 ignored); dashd-rpc 68 passed
    eval $(python3 contrib/setup-dashd.py); cargo test --locked -p rdashd --test rpc_diff
    #   rpc_replies_match_dashd_v24 ok (133.9 s; dashd v24.0.0-rc.3 from the setup-dashd cache)
    # mutant 1: mining.rs checks `failed` before `valid_scripts`             -> all pass. EQUIVALENT (see §4)
    # mutant 2: rpc.rs:162 unregister removed                                -> sut.rs:785 FAILED. Killed.
    # mutant 3: rpc.rs:186 BlockState::Error(msg) -> Error(String::new())    -> all pass. Survives (m1)
    # mutant 4: rpc.rs:165 dropped reply -> accepted: true, new_block: false -> all pass. Survives (m1)
    # mutant 5: rpc.rs:138 status read at the published snapshot (status_at) -> all pass. Survives, but
    #           answer-equivalent (see §3)

## Answers to the review questions

### 1. Branch parity with Core's `submitblock`

**The order is Core's** (`mining.cpp:1033-1063`):

1. `DecodeHexBlk` gives -22 "Block decode failed" (`mining.rs:97-98`). `IsHex` rejects an empty string, an odd length, any non-hex character and an embedded NUL. Trailing bytes are ignored, as `CDataStream >>` ignores them.
2. The coinbase check gives -22 "Block does not start with a coinbase" (`:99-104`, with `is_coinbase` = Core's `vin.size()==1 && prevout.IsNull()`).
3. `EnsureAnyChainman` gives -32603 "Node chainman not found" (`:106`).
4. The index lookup (`:108-115`).
5. `ProcessNewBlock` with the catcher (`:117-125`).

The codes match: -22, -25 and -32603 (`protocol.rs:22,31,32`). `BIP22ValidationResult` maps Valid to `null`, Error to -25 with `ToString()`, an empty reason to "rejected", and any other reason to that reason. The "valid?" arm can't happen, as in Core.

**Help text, argument count, argument types and named parameters** come from the spec-driven server. `rpc_diff` pins them byte for byte (`help`, none, three, number, null, a number as dummy, an unknown name, `hexdata`).

**Where each answer comes from:**

| Case | Answer and its source (both nodes, `rpc_diff` unless noted) |
|---|---|
| connected block resubmitted | "duplicate" from the lookup (`b1_again`) |
| genesis | "duplicate" from PNB: Core never raises `BLOCK_VALID_SCRIPTS` on genesis (`validation.cpp:2536`) |
| valid, stored, not on the best chain | first "inconclusive" (no `ConnectTip`, no `BlockChecked`), then "duplicate" from PNB (`side`, `side_again`) |
| header known, data missing / header-only | lookup is `(false, false)`, so PNB runs and the block is stored and connected as new. Same in both nodes. No differential case: the RPC set can't make a header-only entry until `submitheader` lands (n2) |
| fails `CheckBlock` before it's stored | PNB's `BlockChecked` gives the reason (`high-hash`, `bad-txnmrklroot`); resubmitting gives the same reason, not "duplicate-invalid", because the header is never stored (`bad_merkle_again`) |
| fails `AcceptBlockHeader` | reason from PNB (`prev-blk-not-found`; `bad-prevblk` for a child of a failed block) |
| fails `ContextualCheckBlock` | reason (`bad-cb-height`), then "duplicate-invalid" from the lookup |
| fails at connect | reason from `ConnectTip`'s `BlockChecked` (`bad-cb-amount`), then "duplicate-invalid" |
| block-file position failure | -25 "AcceptBlock: Failed to find position to write new block to disk" (`accept.rs:282-288`, Core :4836-4839). Read, not run |
| block write throws | Core -25 "System error: …", ours "inconclusive" (m2) |

### 2. The catcher

- **It matches on the block hash, as Core does.** `StateCatcher::block_checked` compares `block.hash()` (X11) with the hash of the submitted block (`mining.rs:40-45`; Core :999-1004).
- **V signals `BlockChecked` in exactly Core's two places:** PNB's failure path (`accept.rs:133`, Core :4891) and `ConnectTip` (`abc.rs:652`, Core :3432). Both are synchronous on V. The catcher therefore does see `ConnectTip`'s signal for its own block when an ABC connects it, which is what Core intends. A check of any other block is filtered out (`another_blocks_check_is_not_caught`).
- **A second `submitblock` of the same block, running at the same time, sees this call's state.** Core behaves the same way: every listener sees every `BlockChecked`. Each call still answers from its own `ProcessNewBlock` outcome first, so the second one answers "duplicate" in both nodes. A different block is filtered by hash.
- **Unregister runs on every path that returns** (`rpc.rs:160-162`), including V's abort (a dropped reply makes `recv` return `Err`). Nothing between register and unregister can panic today (n1).
- **After unregister,** V may still call the catcher from a listener snapshot it has already loaded. That is the same as Core's `UnregisterSharedValidationInterface` (`signals.rs:363-364`). It is harmless here, because the state has already been read.
- **The race window is wider than Core's.** V can process the next command before the RPC worker wakes and unregisters, where Core's window is only the time `ActivateBestChain` takes to release `cs_main`. Both are nondeterministic, so this is not a parity finding.

### 3. The live-index read

**It can't block V or deadlock.** `lookup_block_index` (`rpc.rs:138-145`) does two things:

- the arena index lookup, `Shared::lookup`;
- one seqlocked read of the entry's mutable group (`shared.rs:447-468`).

Neither takes a lock that V holds. Other locks:

- The catcher's own mutex is held only to read or write one `Option`.
- `register` and `unregister` serialize on `edit`, which V never takes. V reads listeners through an `ArcSwap` load.

**A read can land in the middle of V's scope.** Core's read under `cs_main` can't. For example, it can see a header just inserted but not yet marked `FAILED_VALID`. Every such mid-scope status reads as "not valid-scripts, not failed", or as the scope's final value, and so it routes the same way as a read just before or just after V's scope. Each answer is one Core gives under some interleaving.

**Mutant 5 survives for that reason.** It reads the status at the published snapshot, and that only differs from the live read inside V's scope. V publishes at `scope_end`, before every reply.

**Mutant 1 is equivalent.** Both `IsValid` (`chain.h:305`) and `Status::is_valid` (`status.rs:55-61`) return false when any FAILED bit is set. `valid_scripts && failed` can't happen, so the order of the two checks makes no difference. `CONFLICT_CHAINLOCK` is outside the mask in both.

### 4. The differential

**`rpc_diff` reaches every answer reachable from RPC alone:** each -22 shape, "duplicate" both from the lookup and from PNB, "duplicate-invalid", "inconclusive", `null`, and reasons from `CheckBlock`, `AcceptBlockHeader`, `ContextualCheckBlock` and `ConnectBlock`.

**These paths are pinned only by unit tests with fakes:** -32603 (`rdashd` is loaded before RPC opens), -25 (it needs a write or system fault), and "rejected".

**Mutants:** 2 is killed. 3 and 4 survive (m1). 1 and 5 survive but are equivalent.

### 5. Accept, store or reject differently

In normal operation I found none. The block reaches V exactly as a P2P block does (`force`, `from: None`, `activated: true`). V's `CheckBlock`, `AcceptBlock` and ABC are the ones already reviewed.

**The decoder is the P2P decoder.** It reads special payloads as raw bytes, as Core does, so a malformed payload is a rule failure rather than a decode failure. Any remaining difference in its decode domain now surfaces as "Block decode failed" (n3).

**The differences are confined to the abort tail** (m2).

## Findings

### Blocker

None.

### Major

None.

### Minor

**m1. The seam's error and abort mapping is unpinned** (`dashd-node/src/rpc.rs:165-174`, `:186`). Two mutants survive every test, `rpc_diff` included:

- Catching `BlockState::Error(msg)` as `Error("")` (mutant 3) would answer -25 with an empty message.
- Reading V's dropped reply as `accepted: true, new_block: false` (mutant 4) would turn an abort into "duplicate". Core gives -25 or the caught state (`mining.cpp:1056-1063`).

The sut test checks `Error` only after unregister, and it never drops a reply. Suggested tests:

- a `Catcher` mapping test for each `BlockState`;
- a sut case where V stops before replying. A fault-injected `save_block` `Ok(None)` gives Core's -25 "AcceptBlock: Failed to find position…" (Core :4836-4839, with `new_block` already true); an `Err` exercises the dropped reply.

**m2. The abort tail differs from Core in two places. The implementer's analysis of the first is confirmed.** Both are V's code (`accept.rs`), not this branch's, and both reach `submitblock` as "inconclusive".

1. **A block write that throws** (`accept.rs:290`):
   - **Core:** `AbortNode` sets `state.Error("System error: " + e.what())`. `AcceptBlock` returns false, and PNB sends `BlockChecked(state)` (`validation.cpp:4840-4846`, `:4889-4891`). `new_block` is already true, so `submitblock` answers -25 "System error: <what>".
   - **Ours:** V returns `Fatal` without `BlockChecked`, so the answer is "inconclusive".
   - **For the fix:** it must also reproduce Core's `e.what()` text, not only send the signal.
2. **`FlushStateToDisk(NONE)` failing inside `AcceptBlock`** (`accept.rs:295`):
   - **Core:** it ignores the return value (`:4854-4856`). After `AbortNode`, PNB still runs its ABC, which connects at least one step before checking `ShutdownRequested` (`:3841`). The submitted block is connected, its `BlockChecked(Valid)` is caught, and the answer is `null`.
   - **Ours:** V stops on `flush_now(None)?` with no reply and no connect, so the answer is "inconclusive", and the block is stored but not connected.
   - **Reach:** `NONE` writes only when pruning flushes (`fFlushForPrune`, `:2989-3046`), so this is prune mode only.

   Backlog it with the first.

### Nit

**n1.** Unregister isn't structural (`rpc.rs:160-162`). A drop guard would keep the catcher from leaking if a later edit adds a panicking or `?` path between register and unregister. Nothing there can panic today.

**n2.** A header-only entry, or a header whose data is missing, has no differential case. The RPC set can't create one, so add the case when RP1a's `submitheader` lands. Both nodes route it to PNB today, and the fake test covers that `(false, false)` routing (`mining/tests.rs:200`).

**n3.** `submitblock` exposes `decode_block`'s domain directly as an RPC answer. Block-decode certification is still open (BL-216), so a remaining decode-domain divergence would show as -22 "Block decode failed" where Core gives a reject reason. No action on this branch beyond tracking it under BL-216.

**n4.** After V stops on a `Fatal`, the node's shutdown flag is set but RPC keeps serving until `Node::shutdown` stops it (`node.rs:363-370`). In that window, every block not already in the index answers "inconclusive", because V is gone and the send is dropped. Core keeps running PNB until `Interrupt`. This is a shutdown-only window, so there's nothing to do beyond noting it with m2.
