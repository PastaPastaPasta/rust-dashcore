# RP1a-2b round 1 review (Opus-h, independent second reviewer)

Branch `node/rp1a-2b` at 40ee05b4a (worktree `rdc-wt/rp1a-2b`, frozen), base 47a82ae72.
Core reference: v24.0.0-rc.4 (`~/workspace/dash`).

**Verdict: ACCEPT-WITH-NITS** (0 M, 0 m, 4 n)

I found no Core divergence in the eight commands or the script port beyond the ones D30–D34
record, and I agree that each recorded divergence is justified. The nits are about test reach and
one error-order corner. None of them blocks the merge.

## Findings

### n-1: getbestchainlock checks the chainstate before the ChainLock; Core checks it after

- **Where:** `dashd-rpc/src/blockchain/state.rs:432`. The test that pins the order is
  `dashd-rpc/src/blockchain/state/tests.rs:305`.
- **Core:** `rpc/blockchain.cpp:271-286`. Core calls `EnsureAnyNodeContext`, then
  `GetBestChainLock()`, and throws `-32603 "Unable to find any ChainLock"` on a null sig. It calls
  `EnsureChainman` only afterwards, for `known_block`.
- **Failing input:** `getbestchainlock` while `NodeChainState::chain_state()` is `None` and no
  ChainLock is known. rdashd answers `-32603 "Node chainman not found"`; Core answers
  `-32603 "Unable to find any ChainLock"`. With a ChainLock known, Core still prints the object
  until its `EnsureChainman`, and then throws.
- **Reach:** the slot is empty only before step 7 (warmup answers -28 first) and after `detach`.
  So this is at most a shutdown-window difference. `every_command_needs_the_chainstate` asserts
  rdashd's order on purpose, so this looks like a choice rather than a slip. It does contradict
  the module doc's "Core's order".
- **Fix (optional):** read `best_chainlock` through a seam that does not need the chainstate (the
  published view outlives the slot only if the node keeps it). Alternatively, record the order in
  D35 as accepted and drop getbestchainlock from the test's list with a comment. Recording it is
  enough.

### n-2: two of the V-turn reads in `tip_info` have no test that would fail if they regressed

- **Where:** `dashd-node/src/rpc.rs:659` (`SystemClock.node_now()`) and `:679`
  (`handle.ibd_at(Some((e.chain_work, e.header.time)))`).
- **Core:** `validation.cpp:5917` (`time(nullptr)`, never the mock time); `validation.cpp:1976-1998`
  plus `rpc/blockchain.cpp:1608` (the latch read under `cs_main` for `m_chain.Tip()`).
- **Surviving mutants:**
  - (a) Replace `SystemClock.node_now().secs()` with the node's mock clock. On regtest
    `chain_tx_data` has `dTxRate = 0`, so `fTxTotal = nChainTx` and progress is 1.0 at any
    clock. Every DIFF case passes. The dashd-rpc fake fixes `verification_progress` at 1. The
    mutant survives everywhere.
  - (b) Replace `handle.ibd_at(...)` with `handle.ibd_service_cache()` (the published snapshot).
    `generate*` returns after publication, so `info_genesis`/`info_mined` still read true/false.
    The mutant survives.
- **Why it matters:** both are exactly the "same V turn / Core's clock" choices D35 argues for. A
  later refactor could silently move them back.
- **Fix:** for (a), add a dashd-node test on testnet or mainnet params with `setmocktime` far from
  now: progress must not move with the mock time. Or unit-test `guess_verification_progress`'s
  call site through a `now` parameter. For (b), a deterministic test is hard without a
  publication seam. A one-line comment in the closure citing D35 (b) is enough for this round.

### n-3: the dashd-node `ChainState` impl is covered only by the dashd-backed DIFF session

- **Where:** `dashd-node/src/rpc.rs:638-797` (`impl ChainState for Writer`) and `:325-338`
  (`coins_read`). Every in-crate `slot.attach(...)` passes `coins: None`
  (`dashd-node/src/chain/tests.rs:489`, `:688`, `:1076`, `:1228`, `:1311`; `sut.rs:894`, `:929`;
  `p2p/chain.rs:851`).
- **Evidence:** mutants 31–34 were killed only by `chain_state_commands_match_dashd_v24` (the
  manifest says so; 34 only by its header-only case). With dashd absent, or with the DIFF test
  skipped, mutants survive. Examples: `chain_tx_final` without `active_contains_private`
  (`getchaintxstats [1, <off-chain hash>]` → stats instead of `-8 "Block is not in main chain"`),
  or `coins_read` returning the spent coin.
- **Fix:** add one dashd-node test, beside `the_chain_commands_move_and_read_vs_tip`, that
  attaches a real coins cell (`steps.coins_cell()` / the rig's) and asserts four things:
  - `gettxout` on a spent and an unspent output;
  - `getchaintxstats` on an off-chain hash;
  - `getblockheaders` from an off-chain block (one entry, `confirmations: -1`);
  - `getdeploymentinfo` by an older hash.

  This also exercises the `debug_assert` in `coins_read` on V's thread in a non-DIFF build.

### n-4: the getbestchainlock `hex` test derives its expected bytes the same way the code does

- **Where:** `dashd-rpc/src/blockchain/state/tests.rs:584`
  (`hex(hash(5).as_bytes())`, the same expression as `state.rs:442`).
- **Core:** `chainlock/clsig.h` `SERIALIZE_METHODS(ChainLockSig, obj) { READWRITE(obj.nHeight,
  obj.blockHash, obj.sig); }`. `blockHash` goes in its storage (little-endian) order.
- **Surviving mutant:** none that's natural, but the hash's byte order is asserted only
  tautologically. If `BlockHash::as_bytes` ever returned display order, test and code would agree
  and both would be wrong against Core. The DIFF session never has a ChainLock (regtest without
  masternodes), so nothing catches it.
- **Fix:** spell the expected `hex` as a literal for a hash with distinct first and last bytes, so
  the test pins the order independently.

## Things I checked and found to match Core

**Arity, types and argument order.** `server.rs:61-80` already applies `IsValidNumArgs` and
`MatchesType` from the v24 spec before each handler, so the handlers' `get_bool`/`get_int` calls
only matter for range. Each handler runs Core's steps in Core's order:
- getblockheaders: `ParseHashV` before `EnsureChainman`, lookup before count, count before
  `verbose`.
- gettxout: `EnsureChainman` before `ParseHashV(txid)`, then `getInt<uint32_t>`, then `get_bool`.
- getchaintxstats: `blockhash` (param 1) before `nblocks` (param 0).
- getdeploymentinfo: `EnsureChainman` before `ParseHashV`.

The DIFF session covers these orders.

**getblockchaininfo** (`blockchain.cpp:1586-1631`):
- Field set and order match, including `headers` from the best header and `chain` as
  `devnet-<name>` (dash-params stores the prefixed form, matching `GetDevNetName`).
- `pruned` is false with no prune fields (D31). `softforks` is read by the reported tip's hash.
- `verificationprogress` uses the system clock, as `time(nullptr)` does. The IBD latch uses the
  node clock (mockable, as `NodeClock::now()` is) against V's private tip.

**getdeploymentinfo / `DeploymentInfo`** (`blockchain.cpp:1445-1544`, `:1664-1739`):
- buried: `type, active, height`.
- bip9: `type, [height], active, bip9{[bit], start_time, timeout, ehf, [ehf_height],
  [activation_height], min_activation_height, status, since, status_next, [statistics{period,
  elapsed, count, [threshold, possible]}], [signalling]}`.

These all match. DIFF checks testdummy across DEFINED→STARTED→LOCKED_IN. ACTIVE `height` and
locked-in stats are covered by the unit fake (mutants 13 and 14 were killed).

**getchaintxstats** (`blockchain.cpp:2074-2153`):
- The default is a month clamped by `max(0, min(·, h-1))`.
- The explicit range check is `< 0 || (> 0 && >= h)`.
- `nTxDiff` is `int` from `unsigned` subtraction (`wrapping_sub().cast_signed()`).
- `txrate` prints only for `nTimeDiff > 0`.
- The two V turns are by hash, so a moving tip cannot mix windows.

**gettxout** (`blockchain.cpp:1328-1411`):
- `fetch` filters spent entries (`cache.rs:270`), as `CCoinsViewCache::GetCoin` does.
- `bestblock`/`confirmations` come from the coins' best block in the same V turn.
- A read failure trips the latch and abort sink in `GuardedEngine::read` before the RPC sees
  `Aborted`. That is the same outcome as Core's `CCoinsViewErrorCatcher`.

**The coins lock:**
- Every other holder takes the cell on V's thread with `try_lock`: the connector's `lock`
  (Fatal on WouldBlock) and the flush site's `lock_cell`.
- The new `debug_assert` is consistent with that rule.
- In release builds a contended lock becomes D9's error. There is no unsafety: it is a `Mutex`,
  and nothing reads unlocked.

**getblockheaders** (`blockchain.cpp:680-784`):
- The walk follows `next` from `header_facts`, which is `tip->GetAncestor(h+1)` on V's private
  chain in one turn. That is equivalent to Core's `active_chain.Next`.
- An off-chain block gives exactly one entry.
- Non-verbose output is the 80-byte header serialization.

**getblockhashes:** Core throws before reading `high`/`low` when there is no index (D33), and the
server's spec checks still apply.

**The script port** (`core_write.rs`):
- `get_op_name` matches Core rc.4's `GetOpName`. I extracted all 114 cases from `script.cpp` and
  resolved their values from `script.h`: 0 differences, `OP_UNKNOWN` default included.
- `get_op` matches `GetScriptOp`'s bounds.
- `script_num` matches `CScriptNum(vch,false).getint()`, including negative zero
  (`00000080` → 0).
- `ScriptToAsmStr(script, false)` never decodes sighash, which is correct for `ScriptToUniv`'s
  callers.

**`Solver`'s order:**
- The order is P2SH, NULL_DATA (`IsPushOnly` from byte 1), P2PK (`ValidSize`, hybrid allowed),
  P2PKH, MULTISIG.
- Rc.4 has no Dash-specific templates.
- `infer_script` tests P2PK and multisig directly instead of through `Solver`. The leading and
  trailing bytes of those shapes cannot collide with P2SH or NULL_DATA, so the result is the
  same.

**`match_multisig` now reads keys with `GetOp`:**
- Keys pushed with `PUSHDATA1/2/4` are accepted, as in Core. Core's `MatchMultisig` checks only
  `ValidSize`, not minimal push.
- Stripping the trailing `OP_CHECKMULTISIG` first is equivalent to Core's `it + 1 == end`: any op
  that crosses into the last byte fails both.

**`ScriptToUniv`:**
- The order is `asm, desc, hex, [address], type`.
- `address` is omitted for P2PK.
- `desc` comes from `InferScript` with a dummy provider:
  - `pk()`/`multi()` only for non-hybrid keys (`IsValidNonHybrid` is a header check, so an
    off-curve key still infers `pk()`);
  - no `pkh()`, no `sh()`;
  - `addr()` when the destination's script round-trips, else `raw()`.

**Mutant 7's equivalence** is sound. `gettxout`'s `include_mempool` is spec type `BOOL`, optional
and not `skip_type_check`. So a non-null non-bool answers `-3` in `Command::handle` before the
handler runs, and the handler's `get_bool` cannot fail. DIFF case `txout_bool_mempool` pins the
`-3`.

**D30–D37:**
- D30: no mempool means `CCoinsViewMemPool` over an empty pool, so the coin is the tip's.
- D31: init refuses `-prune≠0`.
- D32: the stored-bytes vs re-serialized difference is reachable only for a legacy-scheme CLSIG
  that is still best after V19. `hex` and `signature` diverge consistently.
- D33: as above.
- D34: `NoEhfSignals` until CH7a; on the DIFF chains no MnHF signal exists. On a live network
  with an MnHF signal, `ehf_height` and the EHF deployments' status would differ, and D34's
  "reads as unsignalled" says so.
- D35: as reviewed above.
- D36: BL-504.
- D37: I checked 6ccc476ac96 against b5dd15b564c: +10 lines, all in
  `rust_expected_failures.txt`. I spot-checked the reasons against the FT sources:
  - feature_cltv and feature_block pass `-acceptnonstdtxn=1`;
  - feature_dersig and feature_bip68_sequence build MiniWallets;
  - feature_minchainwork passes `-minimumchainwork`;
  - feature_maxtipage passes `-maxtipage`.

  Each is justified, and none hides a 2b divergence.

## What I checked and ran

- I read `git log -p 47a82ae72..40ee05b4a` in full: `state.rs`, `core_write.rs`, the
  `descriptor.rs` changes, `request.rs`, `dashd-node/src/rpc.rs`, the chainstate/node plumbing,
  the DIFF session, the unit tests, and the D30–D37 notes.
- I compared against Core rc.4: `rpc/blockchain.cpp`, `core_write.cpp`, `script/standard.cpp`,
  `script/script.{h,cpp}`, `script/descriptor.cpp` (`InferScript`, `InferPubkey`),
  `rpc/util.cpp` (`HandleRequest`, `ParseHashV`), and `validation.cpp`
  (`GuessVerificationProgress`, `IsInitialBlockDownload`).
- I ran a mechanical opcode-name diff (Core `GetOpName` against `get_op_name`): 114/114
  identical.
- I traced the coins-cell lock holders (`connect/connector.rs:253-261`, `flush/site.rs` `lock_cell`
  sites, `sut.rs:431-436`) and the read-failure path (`store/guarded.rs:310-320`).
- I checked base-sdk at f27bc4f5: `TxHash` and `BlockHash` share `make_hash!`'s `FromStr`, so the
  generic `parse_hash_v` behaves the same for txids. `devnet_name` is stored as `devnet-<name>`.
- I ran the prebuilt dashd-rpc lib test binary from the frozen worktree
  (`target/debug/deps/dashd_rpc-b43f11a82c4b8a63`). It includes 4d911ac9e's
  `a_window_without_time_has_no_rate`. I ran it with TMPDIR in my scratch: **187 passed, 0
  failed.**
- I did not build or run mutants or the dashd-backed DIFF myself. `/work` had 231–234G free
  against the brief's 230G floor, so I made no reflink build copy. I relied on the implementer's
  DIFF / tier 1 / mutant evidence, which I had no reason to doubt after reading the session.
- Scratch: `/work/scratch/rp1a-2b-rv-opus-core` (Core reference extracts plus a tmp dir), left for
  the manager to clean.

## Agreement with the Sol review (RP1a-2b-r1-sol.md)

(I read it only after writing the findings above.)

We reach the same verdict, ACCEPT-WITH-NITS, and neither of us found an M.

**Sol m-1 (no sighash-decoding mode in `script_to_asm_str`): I agree with the fact, but I'd
grade it n.**
- No 2b command calls `ScriptToAsmStr(_, true)`. `ScriptToUniv` passes false.
- The function is documented as `ScriptToAsmStr(script, false)`, and `core_write.rs:2` says
  `TxToUniv` (the only true-mode caller, for scriptSigs) is RP1a-2c's.
- Nothing prints wrong today, so this is a scope boundary. It should be recorded as 2c's (a
  D-row or the 2c brief) so 2c doesn't reuse the false-only helper for `scriptSig.asm`.
- If the RP1a series grades "a Core helper ported partially" as m by convention, I don't object.

**Sol n-1 (no between-turn test for multi-read commands): I agree as n.**
- I reached the same conclusion: hash-pinning plus immutable ancestor facts keep both
  `getblockchaininfo` (softforks by the reported hash) and `getchaintxstats` (ancestor by the final
  hash) linearizable. Core itself releases `cs_main` before `GetAncestor` in getchaintxstats.
- Of the mutants Sol lists, "drops the active-chain check" is mine too (n-3), and it is caught by
  DIFF `txstats_off_chain`.
- "Drops the tip-hash pin" is mutant 12, killed by the dashd-rpc unit test that asserts the
  `deployment_info Some(hash(9))` read.
- So the residual gap is only the race itself.

**Sol n-2 (release-mode coins-lock behavior untested): I agree as n. It overlaps my n-3.**
- `coins_read` has no in-crate test at all; every dashd-node `attach` passes `coins: None`.
- One correction: the mapping is D9's `-20` (`RPC_DATABASE_ERROR`, "Error: A fatal internal error
  occurred", `dashd-rpc/src/protocol.rs:30`, `blockchain.rs:315`), not `-32500`.
- My n-3's suggested dashd-node test with a real coins cell would cover the normal path. A
  poisoned-cell case would cover Sol's release fallback.

**Mine that Sol doesn't have:**
- n-1: getbestchainlock checks the chainstate before the ChainLock.
- n-2: surviving mutants for the system clock and the private-tip IBD read.
- n-4: the tautological hash order in the `hex` test.
