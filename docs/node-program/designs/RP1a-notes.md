# RP1a: tier-1 RPCs, implementation notes and D-rows

RP1a serves the tier-1 RPCs (blockchain reads, control, util and the hidden test RPCs) on
rdashd. RP1a-1a (branch `node/rp1a-1`) serves what CH3b's parked rows need: getblockhash,
getblock 0/1, getblockheader, getchaintips, submitheader, invalidateblock, reconsiderblock,
preciousblock, gettxoutsetinfo, setmocktime, mockscheduler and syncwithvalidationinterfacequeue.
RP1a-1b serves generate* over MN0. Core cites are `v24.0.0-rc.4`. Review: Sol, then Opus-h
(DEC-92, class STATE).

## D-rows

- D1: generate* = thin RPC wrappers over MN0's `dashd-mining` (DEC-594 #1), through the `Miner` seam dashd-rpc defines (RP1a-1b).
- D2: `coins_snapshot` is written by RP1a in dashd-chainstate; CH2 owner idle, so the Opus-h review covers it explicitly (DEC-594 #2).
- D3 (superseded by D14): getblock reuses dashd-node's P2P block reader; no new V API (CH3 written OK requested, DEC-594 #3).
- D4: `sync_mempools` adaptation for rdashd until MP1a (func-compat branch change, O11 reviews).
- D5: b4b's Core FT (feature_dbcrash) stays blocked on MP1a/RP3a; RP1a-1 unblocks the rdashd-level
  reorg loop only.
- D6: setmocktime runs inside a V actor turn (Core n:271 cs_main); template time max(MTP+1, now);
  scheduler waits on wall time, mockscheduler shifts tasks (DEC-594 #7).
- D7: adaptations (recorded, not stubs): `getrawmempool` empty for rdashd until MP1a (BL-464);
  rpc_getchaintips expected failure until CH5; `NotReady` generate error until CH7a/MN0 full.
- D8: BG `rpc-mainnet` getblock counters move to RP1a's last slice or CH5 import (DEC-594 #6).
- D9: submitheader's V-stopped reply (AbortNode during the turn) is `state.IsError()` with Core's
  text "Error: A fatal internal error occurred" (`blockchain::ABORTED`).
- D10: gettxoutsetinfo refuses `hash_or_height` before the flush ("Querying specific block heights
  requires coinstatsindex"): rdashd has no coinstatsindex, so Core's index branch is never reached.
- D11: the coins scan (`gettxoutsetinfo`, `start_coins_stats`) is one class M turn: a copy-point
  ALWAYS flush (`FlushControl::copy_point_flush`, E-b1.8: W runs the engine's hook, the memtable
  flush, before the release, so the walk pins none of the memtables Bk filled), then V waits only
  for the scan thread's snapshot ack, which carries the snapshot's epoch; V asserts it is the
  epoch W released (DEC-618 M-1). The thread never waits on V (spawn_thread + ActorReply, LO-9);
  the walk runs off V. The thread drops its snapshot and its coins-engine handle before it sends
  the result, so an answered RPC holds neither through shutdown's flushes (DEC-618 m-3, BL-271 /
  DEC-613). An interrupted walk answers Core's `RpcInterruptionPoint` -9 "Shutting down"
  (DEC-618 m-1). `disk_size` is RocksDB's live estimate and DIFF-RPC masks it. On the AbortNode
  path (a fatal ALWAYS flush) Core logs "failed to flush state" and still scans the database;
  V stops on the `Fatal`, so the reply is dropped and the RPC answers -32603 "Unable to read
  UTXO set" (DEC-618 n-4; the same path as D9).
- D12: func-compat edits live on `node/func-compat-v24.0-rc4-rp1a` (the rc4 branch is checked out by
  another thread); runs use a scratch clone whose pin branch points at it. rpc_getchaintips /
  invalidateblock / preciousblock are M3 tests, so RP1a-1 changes only M2's rpc_uptime entry.
- D13: the M2 run at RP1a-1 (func-compat be523dbbd23) also passlists `rpc_bind.py --nonloopback`
  (P2a-net's entry): it passes since getnetworkinfo (BL-391), and the ratchet refuses a passing
  expected failure. Flagged to the manager.
- D14 (DEC-606): getblock 0/1 reads the RPC slot's own NodeBlockFiles (a third reader step 7
  opens, like P4b's serve_files), never NodeChain's connected-block cache: lookup + header facts
  + IsBlockPruned + the position in one V query turn, the file read outside it. Verbosity 2/3 and
  `cbTx` (a coinbase with an extra payload; the signature text needs the tip's BLS scheme and
  validity) are refused with -32603 "getblock: ... is not served until RP1a-2": no block rdashd
  holds below DIP3 has a payload. Owner RP1a-2. Closed by RP1a-2c (D39–D42).
- D15: RP1a-1b is cut from RP1a-1a's READY head c0eb7ac3c and merges MN0-1's final head ca76c146f
  (DEC-611), then RP1a-1a's r1 fixes 569991d6f. dashd-node takes `dashd-mining` as a normal
  dependency without `test-support`; dashd-rpc does not depend on it. dashd-rpc defines the
  `Miner` seam (template, grind, TestBlockValidity) and dashd-node's RPC slot implements it over
  one `ChainTemplateSource` per node, built at attach with `-blockversion` (DEC-607 D3). Unit tests
  use a stub `Miner`; DIFF-RPC `generate_commands_match_dashd_v24` runs the real one.
- D16: descriptors are a port of Dash rc.4's whole grammar (`pk`, `pkh`, `combo`, `multi`,
  `sortedmulti`, `sh`, `addr`, `raw`; hex, WIF and BIP32 keys with paths, `*`, origins; the
  checksum), not a subset: rpc_generate.py asserts the xpub-path errors, and a subset would answer
  "not a valid descriptor function" where Core names the key. The fixtures are Core's
  descriptor_tests.cpp and bip32_tests.cpp vectors (`contrib/node/descriptor-vectors.py`,
  `contrib/node/bip32-vectors.py`).
- D17: a `generateblock` block whose parent is not V's tip gets `NotBestPrev` from
  TestBlockValidity (`BlockCheck::TipMoved`); `generateblock` rebuilds the template with the same
  transactions and checks again. Only V's `NotBestPrev` is that verdict; any other "not tested" answer is an internal bug
  (-1 "Internal bug detected"), and a shutdown request ends the rebuilds with `GenerateBlock`'s
  -1 "Failed to make block." (DEC-632 m-1). A single-turn assemble-plus-TBV command is DEC-607's
  follow-up. A TBV refusal answers -25 "TestBlockValidity failed: <reason>"; DIFF-RPC covers it
  (the dashd-node unit harness's connector accepts every block), and
  `the_generate_commands_mine_on_vs_tip` covers the tip-moved verdict on a real V.
- D18: at a G1 height the template is refused (MN0's `TemplateError::NotReady`), and generate*
  answer -32603 "Couldn't create new block: block <height> needs <rule>, which is not served until
  <owner>", where Core mines a CbTx block (DEC-607 D4; listed below). The allowed-diffs seed stays
  in this file next to 1a's; `compat/ALLOWED-DIFFS.md` is the manager's to create from it (O14).
- D19: func-compat at RP1a-1b (node/func-compat-v24.0-rc4-rp1a f245f88804e, 61bed2769b9): the M3
  tests the generate* commands reach run as a partial M3 run. rpc_preciousblock.py passes and is
  passlisted by hand: `ratchet --update` checks the pin branch of the fc worktree's repo (the
  shared Core checkout, another thread's), which it must not move. rpc_generate.py and
  rpc_invalidateblock.py stop at the dashd-built chain cache and `-checkblocks` (CH5), and
  mining_basic.py at MiniWallet's validateaddress (RP1a): expected failures. The full M2 run and
  the partial M3 run at 61bed2769b9 ratchet ok (7 passlisted, 12 expected failures).
- D20 (RP1a-2a): the waits read Core's `latestblock` and `cond_blockchange`, a `BlockChange` in
  dashd-rpc. The node's `uiInterface` sink feeds it from V's `notify_block_tip` (every sync
  state, ahead of `-blocknotify`, as Core's slots connect), but only between `StartRPC` and
  `StopRPC`: `RpcServer` grows Core's `g_rpcSignals` (`on_started` / `on_stopped`), whose slots
  connect and disconnect it, the stop slot waking every wait. The slots follow a one-way
  lifecycle: `on_started` is refused once `start` has begun (an `interrupt` does not reopen
  it), `on_stopped` connects at any time until `stop`, as Core's does, and `stop` asserts in
  release builds that `interrupt` came first, as Core's `StopRPC` does. Init sets the loaded tip
  (`RPCNotifyBlockChange(ActiveTip())`) just before the warmup ends; DIFF-RPC's restart case
  (`waitforblockheight_restarted`) checks it on a datadir whose tip is c6. A tip notified
  between init's read of the tip and its write can be overwritten by the older one until the
  next block; Core has the same window, as it notifies outside `cs_main`. `InvalidateBlock`'s
  notification moves it back, as Core's does (DIFF-RPC `waitforblockheight_a3`). The waits
  check `IsRPCRunning` only when woken, as Core's predicates do. The `test-hooks`
  `waitfornewblock` stand-in is gone; `hooks::stop_waits_for_a_running_call` now runs the real
  command and gets genesis's `{hash, height}`.
- D21 (RP1a-2a): `V`'s `TipWatch` (Core's `g_best_block`, for `getblocktemplate`'s long poll) is
  not what the waits read: Core's waits read `latestblock`, which `NotifyBlockTip` sets per
  `ActivateBestChain` step and on `InvalidateBlock`, and which init sets once more. Core's
  `OnRPCStopped` also wakes `g_best_block_cv`: when the long poll lands, the stop slot must wake
  `TipWatch` too, or a long poll hangs at shutdown. BL-486 tracks this; the slice that adds
  the long poll owns it.
- D22 (RP1a-2a): func-compat lists on `node/func-compat-v24.0-rc4-rp1a-2a` (639550b12a7), cut
  from RP1a-1a's list head be523dbbd23, not RP1a-1b's 61bed2769b9: rpc_preciousblock.py needs
  1b's generate*. feature_shutdown.py and interface_rpc.py pass and move to the passlist; the
  M2 run at 639550b12a7 ratchets ok (8 passlisted, 7 expected failures). Merged with 1b's lists,
  the two branches' changes are disjoint lines.
- D23 (RP1a-3a): scope. 3a serves `validateaddress`, `createmultisig`, `getdescriptorinfo`,
  `deriveaddresses` (rpc/output_script.cpp) and `verifymessage`, `signmessagewithprivkey`
  (rpc/signmessage.cpp), in a new `dashd_rpc::util` over the chain's `KeyIoParams`.
  `getmemoryinfo` (LockedPool stats and glibc `malloc_info`) moves to RP1a-3b; `getindexinfo`
  waits for IX1 (BL-424: rdashd has no txindex though its `DEFAULT_TXINDEX` is true). `echo` and
  `echojson` were already served. 3a is cut from RP1a-1b's d90512244 and does not follow-merge
  2a, which it does not use.
- D24 (RP1a-3a): `dashd_rpc::bech32` ports bech32.cpp (Decode, Encode, LocateErrors with its
  error locations) for validateaddress's errors and DIP-18. `decode_destination` keeps its
  `Option` signature for its callers; `decode_destination_with_error` returns Core's text and
  locations, and validateaddress then tries `DecodePlatformDestination`, describing a Platform
  address by its credit output script.
- D25 (RP1a-3a): descriptor printing closes the deferred items 1, 2 and 4 below. Origins are an
  `Origin` provider (fingerprint, path, inner key). One apostrophe flag per key is shared by the
  origin's path, the `*'`/`*h` suffix and the key's path, written in that order, so the last
  hardened marker decides how every hardened step prints, as in Core. `ToString` prints private
  keys as their public keys with the checksum; `IsSolvable` is false for `addr()` and `raw()`
  (an `sh()` recurses as Core's does, though nothing nests them); `Parse` takes
  `require_checksum` ("Missing checksum"). Inference serves createmultisig: a P2SH whose redeem
  script, a multisig of at most 520 bytes, is the only thing known. Only `sh(multi(...))` is
  reached, or `addr()` for a hybrid key (`InferPubkey` refuses it, so the multisig inference
  fails). The P2PK arm, the `raw()` fallback and the 17..20 key counts stay as Core's generic
  `InferScript` shape, unreachable from this caller (r1 n-3). Evidence: every `Check` vector of Core's
  descriptor_tests.cpp prints as its public form (with the `'`-to-`h` variant), and DIFF-RPC
  `util_commands_match_dashd_v24` byte-compares 217 replies with dashd (125, the 89 edge
  cases the validator probed, and r1's tie and checksum-precedence rows).
- D26 (RP1a-3a): message signing is util/message.cpp over secp256k1's recovery module (dashd-rpc
  enables the `recovery` feature of the pinned 0.33.1). The header byte is read as Core reads it,
  with no range check: `(header - 27) & 3` is the recovery id and bit 2 the compression.
  `MessageSign` cannot fail for a valid key, so signmessagewithprivkey's "Sign failed" is
  unreachable. Evidence: Core's util_tests vectors, and SP0's dashd transcript (8 signatures,
  2,576 verdicts on altered ones) reused from dashd-masternode's fixtures.
- D27 (RP1a-3a): the descriptor, key_io, bech32 and message ports stay in dashd-rpc for now. WL2
  decides whether they move to a crate the wallet can use without depending on the RPC server.
- D28 (RP1a-3a): func-compat lists on `node/func-compat-v24.0-rc4-rp1a-3a` (c9129568e94), cut
  from RP1a-1b's list head 61bed2769b9. The util FTs run as a partial M5 run:
  rpc_signmessagewithprivkey.py passes and is passlisted by hand (D19's reason).
  rpc_createmultisig.py, and mining_basic.py (M3, which validateaddress no longer stops), stop at
  MiniWallet's `scantxoutset` (RP1b); rpc_deriveaddresses.py (both runs) and
  rpc_getdescriptorinfo.py start on the dashd-built chain cache (CH5). Expected failures, so
  DIFF-RPC carries those FTs' cases. Both partial runs ratchet ok (8 passlisted, 16 expected
  failures). With 3a carrying 2a (DEC-669), b5dd15b564c merges 2a's list branch
  (639550b12a7) by hand: the passlist takes both sides (10 tests), and the expected failures keep
  3a's rows without interface_rpc.py, which 2a serves (14 rows). core-clone's pin branch points
  there; at 12cc1cc59 the full M2 run and the partial M5 and M3 runs ratchet ok (10 passlisted,
  14 expected failures).
- D29 (RP1a-3a): the DIFF-RPC harness retries a read that a signal interrupts (`EINTR`). std
  does not retry it, and tier 1 at d90512244 and at b7fc96ee9 each failed
  generate_commands_match_dashd_v24 on "watching for the close: Interrupted system call". The
  retry restarts the one-second close watch, so a close is still seen and nothing compared
  changes.
- D30 (RP1a-2b): gettxout's `include_mempool` answers as Core's over an empty mempool until
  MP1a: the coins tip's coin, the same for `true` and `false` (as BL-464 does for
  getrawmempool). Core's `EnsureMemPool` cannot fail on a node, so nothing else differs.
- D31 (RP1a-2b): getblockchaininfo's `pruned` is always false and the prune fields never print:
  init accepts `-prune` only as 0 (CH8).
- D32 (RP1a-2b): getbestchainlock prints the signature as V stores it, the serialized
  `CBLSSignature` as received. Core's `ToString()` re-serializes it in the scheme its global flag
  holds now, so the two differ only for a lock received under the legacy scheme that is still
  the best one after V19 activates. The `hex` field is built the same way (`nHeight`,
  `blockHash`, then the signature's bytes).
- D33 (RP1a-2b): getblockhashes always answers Core's -1 "Timestamp index is not enabled":
  init accepts `-timestampindex` only off until IX1 (IX2 owns the flag).
- D34 (RP1a-2b): getdeploymentinfo, and getblockchaininfo's `softforks` under
  `-deprecatedrpc=softforks`, report CS4's `DeploymentInfo` over V's deployments. The EHF
  signals come from `NoEhfSignals` until CH7a, so no `ehf_height` prints and an EHF deployment
  reads as unsignalled. On the regtest chains DIFF-RPC compares, no block carries an MnHF
  signal, so the replies match.
- D35 (RP1a-2b): the eight commands reach the chainstate through `NodeChainState`, a seam of
  their own beside `NodeChain`, which `ChainSlot` implements over the same attachment. Each
  read is one V query turn on V's live index and private active chain (Core's `cs_main`
  scope), except two: `size_on_disk` under the block files' own lock, and the best ChainLock
  from the published signatures (Core reads its handler outside `cs_main`). The IBD latch is
  read in the same turn for V's own tip; inside the turn the published view's latch would
  answer the same, since V publishes its tip at every command's end (D65 in V's
  `debug_invariants`, checked after every command in debug builds), so no test tells the two
  apart (Opus r1 n-2 (b), DEC-704); `verificationprogress` reads the system clock, which a
  testnet test pins. getblockchaininfo's `softforks` are read for the tip it reported, by
  hash. gettxout's coin read runs in the V turn, because the coins cache is
  V's leaf (CH3a D50). Every read a stopped V cannot answer, and a coins read that fails,
  answers as an `AbortNode` does (D9). The coins cell is now kept in every build, so in a
  release build the coins cache lives until the chainstate drops, as in test-support builds.
  getbestchainlock checks the chainstate (`EnsureChainman`) before the ChainLock, where Core
  reads the ChainLock first and ensures the chainman only for `known_block`: with no
  chainstate rdashd answers -32603 "Node chainman not found" where Core answers -32603
  "Unable to find any ChainLock" (or, with one known, still throws). The slot is empty only
  before init step 7, where warmup answers -28 first, and after shutdown's `detach`, so the
  difference shows only in the shutdown window; accepted (Opus r1 n-1, DEC-704).
- D36 (RP1a-2b): DIFF-RPC `chain_state_commands_match_dashd_v24` checks
  `-deprecatedrpc=softforks` on a second, freshly started pair of nodes, not after a restart:
  rdashd does not keep generate-mined blocks across a clean restart (BL-504, ND1; it reproduces
  at 47a82ae72, before 2b). When BL-504 merges, the session gets its restart case back
  (getblockchaininfo with `softforks`, and gettxout on the restarted chain).
  - Restored in RP1a-4 (BL-504 fixed by BL-271): the first pair is restarted under
    `-deprecatedrpc=softforks`. getblockchaininfo with `softforks` (at 104, with the
    invalidated tip's fork), getdeploymentinfo, getchaintxstats, gettxout on the spend and on
    block 102's coinbase, and the invalidated block's header all match dashd's after the
    restart, and the testdummy windows are mined on from there.
- D37 (RP1a-2b): func-compat lists on `node/func-compat-v24.0-rc4-rp1a-2b` (6ccc476ac96), cut
  from b5dd15b564c (D28's merged lists). M2 runs in full; M3 runs partially, over the FTs that
  call the 2b commands (rpc_blockchain in both transports, rpc_getchaintips, feature_cltv,
  feature_dersig, feature_bip68_sequence, feature_minchainwork, feature_maxtipage,
  feature_loadblock, feature_block) and over mining_basic and rpc_preciousblock. None of them
  reaches a 2b command yet. The blockers are MiniWallet's `scantxoutset` (RP1b), the
  dashd-built chain cache and `-loadblock` (CH5; the exported tree also has no
  `contrib/linearize`), `-acceptnonstdtxn` (MP1a), and `-maxtipage` and `-minimumchainwork`
  (CH1-node). They are listed as expected failures, so DIFF-RPC carries the 2b evidence. At
  0ebff40d7 both runs ratchet ok (10 passlisted, 24 expected failures).
- D38 (RP1a-2b): `script_to_asm_str` ports `ScriptToAsmStr(script, false)` only, the mode
  `ScriptToUniv` prints scriptPubKeys with. The `fAttemptSighashDecode = true` mode (a strictly
  encoded signature push printed as its body and `[ALL]`-style sighash name) is RP1a-2c's,
  for `TxToUniv`'s `scriptSig.asm`; until then the helper stays false-only and crate-private, so
  2c adds the mode rather than reusing it for scriptSigs (Sol r1 m-1, DEC-704). RP1a-2c added
  it, with Core's `script_GetScriptAsm` vectors.
- D39 (RP1a-2c): a block's undo is read through `Chainman::read_undo`, beside `read_block` on
  the same seam and under the same block-files lock, not through a new trait. One V query turn
  reads `IsBlockPruned`, the index's undo position and the parent hash (Core's `cs_main`
  scope); then `UndoReadFromDisk` runs under the files lock. As in Core's `blockToJSON`, every
  failure (pruned, no undo position, an open, read or checksum error) only makes `have_undo`
  false: the block still prints, without `prevout` and `fee`, and the read error is logged
  (Core's `error()`), not answered (manager ruling on 2c's plan).
- D40 (RP1a-2c): getblock verbosity 2 and 3 print each transaction's `instantlock` as the
  block's `chainlock` and `instantlock_internal` as false: rdashd has no InstantSend manager yet,
  so `IsLocked` is false for every transaction, as on a Core node that has seen no lock. Core's
  `EnsureInstantSendManager` cannot fail on a running dashd, and rdashd does not refuse in its
  place. When an InstantSend manager exists (LK1b, which brings `CInstantSendManager`), the RPC
  reads its `IsLocked`; getrawtransaction shares the case (r1 m-3, DEC-719). Until then a
  transaction with an islock in a block not yet chainlocked prints `false`/`false` where Core
  prints `true`/`true`: an allowed difference (table below), and D42's list has the case.
- D41 (RP1a-2c): `TxToUniv` and the payload objects are dashd-rpc's own port (`core_write`,
  `core_write/evo`; Q3), over base-sdk's decoded payload, and print what Core prints:
  - A payload object is printed only when `GetTxPayload` reads it whole (`is_complete`). A
    ProTx of a version Core does not know, and an unknown type, print none. For a payload that is
    exactly the version bytes, Core prints a default object (ProUpRegTx, ProUpRevTx) or fails
    `CHECK_NONFATAL(obj.netInfo)` (ProRegTx, ProUpServTx; `-1 "Internal bug detected: …"`).
    No connected block carries one, but getblock also serves a stored block that failed
    `ConnectBlock` (r1 m-1): below DIP0003, `CheckBlock` and `ContextualCheckBlock` accept any
    special type and payload, so on regtest a submitted block can carry one and both nodes print
    it. That edge is recorded, not ported: an allowed difference for getblock 2 and 3 on such a
    block (table below), closed by RP1b, whose decoderawtransaction reaches the same `TxToUniv`
    on any bytes. The same holds for an Evo ProTx whose `ExtNetInfo` has an unknown version under
    `-deprecatedrpc=service` (the `GetPlatformPort` bullet below).
  - The BLS fields Core prints under its process-wide flag (a CbTx's `bestCLSignature`, an MnHF
    signal's `sig`, an asset unlock's `quorumSig`) print their stored bytes (manager ruling, as
    D32). On a node whose tip is past V19 Core prints the same: the stored bytes are canonical
    under the basic scheme or zeros, since every such payload is post-V20. A lazy operator key
    prints as Core's `Get()` does under the scheme it was read with (zeros if invalid), and a
    commitment's keys and signatures under its version's scheme, so neither depends on the
    flag.
  - `GetPlatformPort` on a non-empty extended network info without the platform purpose fails
    Core's `CHECK_NONFATAL`; `Validate` refuses such an Evo ProTx at connect, so it prints −1
    here. A block stored without connecting can carry one (see the first bullet; same row).
    `CHECK_NONFATAL(MoneyRange(fee))` is not repeated (a connected block's undo keeps the fee in
    range), and the spent index is not read (no `-spentindex` until IX). The fee's sums wrap
    where Core's `CAmount` arithmetic would overflow, which no connected block reaches.
  - Undo data with fewer entries than the block has spending transactions fails getblock 2 and 3
    with `vtxundo.at(i - 1)`'s `std::out_of_range`, as `ExecuteCommand` answers it
    (RPC_MISC_ERROR, libstdc++'s `vector::_M_range_check` text). An entry with fewer coins than
    its transaction has inputs is undefined behaviour in Core (`vprevout[i]`); rdashd fails the
    same way, naming the first missing coin. Neither happens with undo data the node wrote.
  - `-deprecatedrpc=service` now reaches getblock (`DeprecatedRpc::service`), for a ProTx's
    `service` and an Evo node's platform ports.
- D42 (RP1a-2c): rdashd builds and connects nothing past G1 (DIP0003, regtest 432) until
  MN0-2/CH7a, so DIFF-RPC compares connected blocks only below G1 (coinbases, fee-paying spends,
  multi-input spends, a signed P2PKH spend whose `prevout` carries an address, genesis without
  undo data). The payload objects are proven two ways:
  - **Fixtures.** `dashd-rpc/tests/data/txtouniv` holds dashd v24.0.0-rc.4's
    `decoderawtransaction` for 221 special and plain transactions from Core's regtest
    functional-test chains and the testnet and mainnet oracles, bucketed so a domain entry and a
    share with its own reward script are kept, and for 5 crafted rows (an invalid network entry,
    an empty ProUpShareTx reward, `nOperatorReward = 7`; PROVENANCE.md). The unit tests check
    every row byte for byte; Core's dash-tx JSON goldens check the plain shapes.
  - **A stored block, live (r1 m-1).** DIFF-RPC submits to both nodes a block below G1 that
    carries the fixture special transactions verbatim, crafted ones included. Both store it and
    refuse it at connect (`bad-tx-type-dip3-inactive`), and getblock 1, 2 and 3 compare equal,
    with and without `-deprecatedrpc=service`, without undo data: a non-genesis no-undo block
    (n-3). This brings back, without fees or prevouts: ProRegTx (legacy, basic, Evo, extended
    network info, own-reward share), ProUpServTx (with a domain and an invalid entry),
    ProUpRegTx, ProUpRevTx, `qcTx`, MnHF, asset lock and unlock, ProDisTx, ProUpShareTx and
    ProUpSharedRegTx.
  - Two limits of that block, both allowed differences below. A type-5 coinbase stops rdashd
    (the ALLOWED-DIFFS D18 refusal of a special-version coinbase; BL-516, CH7a), so the block's
    coinbase is plain. A restart loses the stored block on rdashd while dashd keeps it (BL-517,
    CH1-node), so the `service` cases run on a second pair of nodes.
  When MN0-2/CH7a serve G1, DIFF-RPC gets these live cases back, owned by them:
  1. getblock 1, 2 and 3 on a block past DIP0003: the block-level `cbTx` and the coinbase's
     type-5 object, CbTx versions 2 to 4 as each activates;
  2. fees and `prevout`s on special transactions: a quorum commitment block, ProTx blocks, an
     MnHF signal, and asset lock and unlock blocks with the fee of a platform transfer;
  3. the v24 types connected, once EVO_SHARES and V24 are served;
  4. `instantlock`/`instantlock_internal` from InstantSend locks, once LK1b serves them (D40).
  This closes the DEC-611 allowed difference (D14's refusal).
- D43 (RP1a-2c-2): getblockstats is dashd-rpc's own port (`blockchain/stats.rs`) over the
  getblock seams:
  - `ParseHashOrHeight` reads a height on the published active chain, as getblockhash does, and
    a hash through `LookupBlockIndex`, both before the `stats` selection is read.
  - `GetUndoChecked`'s two errors need `read_undo` to tell pruned from unreadable
    (`UndoReadError`); getblock still prints no fee for either.
  - `GetBlockSubsidy` is a new `Chainman::block_subsidy`: connect's own
    `dashd_chainstate::connect::block_subsidy_after` (the parent's header through V's reader and
    `DeploymentActiveAfter(prev, V20)`, then `dash_consensus::subsidy::block_subsidy`) in one V
    query turn, under the miner's `SubsidyParams` (the node's one set;
    `ChainTemplateSource::subsidy_params`), rather than a new `ChainParams` argument at every
    `ChainSlot::attach`. Regtest's chains carry only the pow limit's bits and stop below V20, so
    two unit tests pin the inputs no DIFF varies (r1 m-1): mainnet's block 1,000,000 under a
    real difficulty is 334,559,821 (1,672,799,096 under the pow limit), and with V20 lowered to 6
    the blocks at 5 and 6 take the pre-V20 and V20 subsidies. Without those parameters (test wiring only) the
    command answers "Internal bug detected: subsidy"; Core's never fails. The live chains stop
    below G1, where regtest's V20 also activates (432), so the V20 switch of `subsidy` joins
    D42's list of live cases for MN0-2/CH7a: getblockstats at V20Height - 1 and V20Height.
  - There is no transaction index (IX1), so there is no `BlockUntilSyncedToCurrentChain` to wait
    for.
  - `CHECK_NONFATAL(payload)` and `CHECK_NONFATAL(MoneyRange(txfee))` answer RPC_MISC_ERROR
    "Internal bug detected: <condition>", the short form gettxoutsetinfo already uses, not Core's
    multi-line text with file and line. A connected block reaches neither. A unit test takes
    a Platform transfer's fee from an asset unlock of RP1a-2c's dashd fixtures, and its payload
    with one byte more is the `payload` check.
  - Undo data short of a transaction fails as `vtxundo.at(i - 1)` does (D41's text), and only
    for a selection that reads the inputs, as in Core.
  - Proven against Core's own `test/functional/data/rpc_getblockstats.json` chain in DIFF-RPC
    (`getblockstats_match_dashd_v24`; dashd's replies there equal the file's stats), and the
    FT `rpc_getblockstats.py` passes (M4, passlisted on node/func-compat-v24.0-rc4-rp1a-2c2;
    run log `/work/scratch/rp1a-2c2/fc2.log`, rc 0 in `fc2.rc`, runner log
    `fc2-m4/runner-v24.0-M4.log`).
- D44 (RP1a-2c-2): pruneblockchain answers Core's RPC_MISC_ERROR "Cannot prune blocks because
  node is not in prune mode." after `EnsureAnyChainman`, always: init accepts `-prune` only as 0
  (D31). The rest of Core's handler (the height and timestamp forms, `PruneBlockFilesManual`)
  is CH8's, with `feature_pruning.py` (M5).
  The subsidy seam (D43) adds no crate edge: its caller is dashd-node's `Writer`, which already
  depends on dashd-mining (`dashd-node/Cargo.toml`, the node's one `ChainTemplateSource`), and
  dashd-rpc sees only `Chainman::block_subsidy`; `check-layers.py` reports the same layers.

- D45 (RP1a-2c-3): getmerkleblocks is dashd-rpc's own handler (`blockchain/merkle.rs`) over a
  new `Chainman::merkle_blocks`; the filter and the merkle blocks are dashd-p2p's
  (`processing::bloom::BloomFilter`, `processing::merkle::merkle_block`), the ones `filterload`
  and `getdata(MSG_FILTERED_BLOCK)` use, so the RPC and P2P can't drift apart.
  - Three new crate edges: dashd-node → dash-p2p-core and dashd-node → dash-types (normal; the
    filter type and the `merkleblock` codec), and rdashd → dashd-p2p (dev-only, the DIFF's
    filters); `contrib/node/check-layers.py` passes with them.
  - Order as Core (`rpc/blockchain.cpp:827-924`): `EnsureAnyChainman`, `ParseHex` (lenient:
    whitespace before each pair, anything else the empty vector), the filter read, then
    `IsWithinSizeConstraints`, `ParseHashV("blockhash")`, `LookupBlockIndex`, and `count` last.
  - The filter read: Core prechecks the `vData` count before `LIMITED_VECTOR`; a canonical count
    above 36,000 answers "Filter is not within size constraints" when the whole `vData` and the
    9 fixed bytes follow, else end of data. Stream failures answer RPC_MISC_ERROR with libstdc++'s
    `what()` (Core's text + ": iostream error"). Bytes after the filter are ignored. dashd
    v24.0.0-rc.4's replies for each edge case are in `/work/scratch/rp1a-2c3/probe/p1.jsonl`
    (`probe.py`, rc 0); the unit tests use the same texts.
  - The walk: one V query turn reads the block's entry, `IsBlockPruned`, and the active chain
    from it (`ActiveChain().Next`; a block off the active chain is the only one read). Then the
    blocks are read from the slot's files, with one filter for the whole walk, updated as
    `CMerkleBlock` updates it, and blocks with no match skipped. As in Core, pruning is checked
    only for the first block (`GetBlockChecked`), and a later read failure is "Block not found
    on disk". The RPC's mapping of the pruned error is unit-tested through the `Chainman` seam;
    the node's own check (`Writer::merkle_blocks`, `if pruned`) can't be reached until CH8
    enables `-prune` (D31). BL-522 holds the real pruned-node test and DIFF (r1 n-1).
  - Proven in DIFF-RPC `getmerkleblocks_match_dashd_v24` against dashd byte for byte:
    - a chain whose coinbases pay pkh(G) and pk(G_full);
    - a spend of both, signed by dashd's `signrawtransactionwithkey` with G's compressed and
      uncompressed WIFs;
    - filters on G's key hash, full key, txid and outpoint under `BLOOM_UPDATE_NONE`, `_ALL` and
      `_P2PUBKEY_ONLY` (one match updating the filter for a later block);
    - all-ones, all-zero and empty filters;
    - count 1 and 2,000 and the walk's end at the tip;
    - each error;
    - an invalidated tip off the active chain while a branch from its parent stands above it (the
      walk reads that block alone), a headers-only block, and a stored block refused at connect.
  - Mutants: 13/13 die in the unit tests or the DIFF (`/work/scratch/rp1a-2c3/mut-apply.py`,
    `mut-r3.log`), with r1's `IsSpace` cut to space, tab and newline; the unit test and the DIFF
    put each of `\v`, `\f` and `\r` between filter-byte pairs (r1 m-1).
  - The FT `rpc_getmerkleblocks.py` passes (M5, Core `node/func-compat-v24.0-rc4` at 75297b8b03;
    run log `/work/scratch/rp1a-2c3/fc1.log`, rc 0 in `fc1.rc`, matrix `fc1-m5/`).
- D46 (RP1a-2c-4, DEC-766): verifychain is dashd-rpc's handler over a new `Chainman::verify_db`:
  the arguments read as `getInt<int>` (defaults 3 and 6) before `EnsureChainman`, as Core's
  (`rpc/blockchain.cpp:1413-1443`), passed unclamped; true only for `SUCCESS`. The node sends V
  the class M command `ChainCommand::VerifyDb`, which runs the same VerifyDB point
  (`V::verify_db`, `VerifyDbArgs` clamping) as the load path's `VerifyLoadedChainstate`. A
  stopped V answers RPC_DATABASE_ERROR, as the block commands do.
  - `V::verify_db` is Core's `CVerifyDB::VerifyDB` (`validation.cpp:5036-5180`) for levels 0–2:
    the walk back `depth` blocks from the tip with Core's start, progress and summary lines; the
    stop, with Core's line, at the first block without data on a pruned node; level 0's
    `ReadBlockFromDisk`, level 1's `CheckBlock`, and level 2's `UndoReadFromDisk` (checksum
    included), each failing as `CORRUPTED_BLOCK_DB` with the read's own line and Core's
    "Verification error" line; and `INTERRUPTED` on shutdown. The summary counts no
    transactions: Core counts them at level 3.
  - Levels 3 (disconnect onto a coins cache) and 4 (reconnect) are CH5's and pass with no check
    (BL-524, allowed difference below). Core's level-3 summary count and its level-4 reconnect
    loop's progress lines differ with them; the first loop's lines, on level 4's scale of 50,
    are Core's. The `IsPruneMode` test reads `m_have_pruned`, which nothing sets while
    `-prune` is refused (D31); `ShowProgress` (the GUI's) is not sent.
  - A block read that fails in the decoder now gives Core's exception text where Core refuses the
    count the same way (`ReadCompactSize(): size too large`, `non-canonical ReadCompactSize()`;
    `DecodeError::read_error_kind`), in both V's and recovery's `ReadBlockFromDisk` line.
  - Proven in DIFF-RPC `verifychain_matches_dashd_v24`: levels -1 to 5 by depths -1, 0, 1, 6,
    the height, past it and `INT_MAX`, on genesis alone, on 110 blocks and with the tip
    invalidated (dashd answers true to each), plus the defaults, type and range errors, extra
    and named arguments, and `help`. Neither node logs a VerifyDB line at a genesis tip (r1 n-2).
  - Proven in DIFF-RPC `verifychain_on_corrupted_datadirs_matches_dashd_v24`, on copies of each
    node's own 20-block datadir: a transaction byte flipped (merkle root), the transaction count
    garbled (above `MAX_SIZE`, and non-canonical `fd 01 00`, r1 m-2), the next block's undo
    checksum broken, the rev file cut before that checksum or removed (r1 n-6), and the tip's
    block cut short in its file. With the block and checksum damage at 10 (undo 11), verifychain
    at levels 0–4 by depths 0, 6, 9, 10, 11 and 21 gives the same replies and the same VerifyDB
    log lines, and dashd's are asserted: false from the catching level (0, 1 and 2) when the
    depth reaches the damage, true otherwise; at levels 3 and 4, rdashd's progress lines are a
    prefix of dashd's (r1 n-3). With the damage at 17 (undo 18) and the tip cut short, the start
    at the default level and depth exits 1 with Core's ": Corrupted block database detected" and
    the same lines ("Unable to open file" compared from `blocks/` on).
  - The walk's shutdown check is unit-tested: a shutdown requested before the walk reads the tip
    alone and answers `INTERRUPTED` (r1 m-1). A null position prints as Core's `GetBlockPos()`,
    `nPos=0` (`have_data_pos`, r1 n-1).
  - Mutants: 13/13 die in the unit tests or the DIFF (`/work/scratch/rp1a-2c4/mut-apply.py`,
    `mut.log`): each level's check skipped, the depth one deeper, an unreadable block passing,
    the progress, read-error and summary lines, the RPC defaults and order, the decode text, the
    readiness guard's old failure (D47) and the node's verdict. r1's five more die too
    (`mut2-apply.py`, `mut2.log`): the shutdown check, the non-canonical text, the genesis early
    return, the level-4 scale and D47's replayed-block failure.
- D47 (RP1a-2c-4): the startup readiness guard (CH3a D70) judges a block whose data does not read
  on its deployments alone, as it judges one without data, instead of failing the load as "Error
  opening block database". Core reads nothing at that point; its `VerifyDB`, next, reads the tip
  (rdashd refuses `-checkblocks`, so the depth is 6) and fails the load as a corrupted block
  database, which the corrupted-tip DIFF case now matches. Recovery read the transition's blocks
  just before the guard, so one of those (any block before the tip) that does not read still
  fails the load as `LoadFailure::Store` (r1 n-4); only the tip, which VerifyDB always reads,
  passes. A tip read that fails at the guard and then succeeds in VerifyDB would let it skip its
  coinbase test; a transient read fault is the only way there.
  - CH3 (the D70 guard's owner) reviewed this (10-29): recovery's replay has read every block of
    the transition, and `verify_loaded` (`dashd-node/src/steps.rs:421`) runs after
    `check_startup_readiness` (`:375`) and always reads at least the tip (a depth ≤ 0 reads
    every block); an unreadable block with `HAVE_DATA` gives `CorruptedBlockDb` even when
    pruned. So any block the guard passes on its deployments alone still fails the load at
    VerifyDB, as Core's does.
  - Superseded by CH7 D57 (CH7a-2b-2): the guard reads no block, so neither the tip nor a
    replayed block is read there and the r1 n-4 failure is gone.
- D48 (RP1a-3b, DEC-787 Q3): Core's `LockedPool` (`support/lockedpool.{h,cpp}`) is ported in a
  new layer-N crate, `dashd-lockedpool`, which dashd-rpc alone depends on (dashd-rpc stays
  `forbid(unsafe_code)`); it is in neither the SDK nor the mobile closure. It holds:
  - `Arena`, ported 1:1 over addresses: best fit, the chunk cut from the end of the free one,
    equal sizes taken in the order they were freed (the multimap's insertion order, a sequence
    number in the key), both neighbours coalesced, and Core's error texts;
  - the page allocators: Posix (mmap, mlock, `MADV_DONTDUMP`; cleanse, munlock and munmap on
    free) and, off Unix, the D50 stub;
  - `LockedPool` with `ARENA_SIZE` 256 KiB, `ARENA_ALIGN` 16, the first arena capped at
    `RLIMIT_MEMLOCK`, and `locked` counting only pages that locked; the process's manager is
    created on first use, as Core's `call_once`. Its locking-failed callback answers true and
    logs nothing, as rc.4's does (`lockedpool.cpp:384-388`, a TODO there), not "Cannot lock
    memory";
  - `Secure<N>`, the handle `secure_allocator` and `secure_unique_ptr` give Core: zeroed when
    made, zeroed (`memory_cleanse`, here `zeroize`) before its chunk returns to the pool, a new
    chunk per clone, and a `Debug` that prints no bytes;
  - `malloc_info` (glibc only; elsewhere getmemoryinfo answers -8, as Core without
    `HAVE_MALLOC_INFO`).
  The `unsafe` blocks are the mmap/mlock/madvise/munmap calls, the chunk pointer reads and
  writes in `Secure`, `Send`/`Sync` for `Secure`, and the `malloc_info` stream; each has a SAFETY
  comment. Core's arena, mock and live tests are transcribed (`lockedpool_tests.cpp` LFSR walk
  included), plus tie order, coalescing, the first-arena cap, an unlocked arena (the
  manager's own callback keeps it, r1 Sol n-1), a whole-arena request, a bad address (an
  arena's end included, r1 m-2) and `Secure`'s zeroing (over a chunk the test dirtied, r1 n-4)
  and freeing.
- D49 (RP1a-3b, DEC-787 Q1/Q2): dashd-rpc's `Key` and `ExtKey` keep their 32 secret bytes in a
  `Secure<32>` (`CKey`'s `keydata`, `key.h:56`), so they lose `Copy`. Each use builds a
  transient `secp256k1::SecretKey` from them and erases it when done, also on unwind
  (`non_secure_erase`: a volatile write and a `SeqCst` compiler fence, so it is not elided; its
  "non-secure" is about the copies moves leave, below), with the bytes copy zeroized. Only
  dashd-rpc reaches the transient key (`SecretBytes::with` is `pub(crate)`, r1 Sol M-1); a secret is validated before it takes a chunk, as `CKey::Set` checks first. Core's
  libsecp reads the pool bytes in place, so these stack copies are rdashd's own, and copies a
  move leaves behind (the `Result`s carrying a key, BIP32's tweak scalar and the tweaked child
  key's temporaries) are not erased. BIP32 derivation's 64-byte HMAC output is a `Secure<64>`
  (`key.cpp:294/320/391`). `DecodeSecret`'s final buffer is cleansed (`key_io.cpp:117-119`),
  and so is `DecodeExtKey`'s, which Core's vector keeps; `EncodeExtKey`'s is (`:180-182`). The
  base58 decoder's own scratch buffer, and a buffer whose checksum fails, are freed unerased,
  as Core's `DecodeBase58`/`DecodeBase58Check` do (parity). Copies Core leaves too, unerased
  here as there (r1 n-1): BIP32's HMAC-SHA512 engine state, which buffers the parent secret on a
  hardened step, and its finalized output before it is moved into the `Secure<64>`
  (`CHMAC_SHA512`/`CSHA512` have no cleansing destructor); base58's checksum hash over the
  payload; and the RPC request itself (the HTTP body, the parsed params and the WIF or xprv
  text), as `support/allocators/secure.h:58`'s TODO concedes. A pool that cannot give a chunk ends the process through
  `handle_alloc_error`, as `secure_allocator::allocate` throws `std::bad_alloc`
  (`support/allocators/secure.h:28-35`); no heap fallback.
- D50 (RP1a-3b, DEC-787 Q4): rdashd has no Windows page-locking port: off Unix, the pool's pages
  are zeroed heap pages that report locking as failed, so `locked` stays 0 and the arenas are
  kept (the callback answers true), where Core's `Win32LockedPageAllocator` (`VirtualAlloc`,
  `VirtualLock`) locks them. The backlog line to open (manager's number): "BL: port Core's
  Win32LockedPageAllocator (lockedpool.cpp:159-208) to dashd-lockedpool if rdashd ever ships on
  Windows; until then getmemoryinfo's `locked` is 0 there (RP1a D50)" is BL-530. BLS's
  `mt_pooled_secure_allocator` (`bls.cpp:396-430`) is out of scope (no BLS secrets in the
  node's RPCs yet). The `-sporkkey` secret (`dashd-masternode/src/sporks.rs`, `Signer.key`)
  stays in ordinary memory for the node's life, where Core's `sporkPrivKey` (`spork.h:206`) is
  a `CKey` in the pool; moving it into a `SecretBytes` is BL-531 (r1 m-3). At idle rdashd's pool is empty where dashd's holds `RNGState`'s 240-byte
  chunk (`random.cpp:497`; BL-502).
  - Proven in DIFF-RPC `getmemoryinfo_matches_dashd_v24`: dashd at idle has used 240, one chunk
    used and one free, total 262144; rdashd answers zeros; after signmessagewithprivkey,
    getdescriptorinfo and deriveaddresses over WIFs and xprvs, rdashd's stats equal dashd's less
    the RNG chunk (nothing left allocated); the mode errors and `help` match byte for byte;
    mallocinfo is XML on both. A second pair runs under a 64 KiB `RLIMIT_MEMLOCK`: dashd's
    stats are `[240, 65296, 65536, 65536, 1, 1]` (the first arena capped at the limit, locked),
    and rdashd's after a signature are those less the RNG chunk (r1 m-1).
- D51 (RP1a-4, DEC-831 Q1): getnetworkhashps and getmininginfo (`rpc/mining.cpp:66-136`,
  `:451-495`) are dashd-rpc's `mining::info`, over one new `ChainState` read,
  `active_window(height, back)`: the active height, and the active block at `height` (the tip)
  with up to `back` ancestors, in one V turn, as Core walks `pprev` under one `cs_main`.
  `GetNetworkHashPS` itself (the argument checks, -1's `nHeight % DifficultyAdjustmentInterval()
  + 1`, the cap at the block's height, the window's min and max time, `getdouble()` of the work
  delta) runs in dashd-rpc on that answer; -1 asks for the whole interval
  (`nPowTargetTimespan / nPowTargetSpacing`, 576 on every v24 network). getmininginfo reads the
  tip, the difficulty and the default estimate from one `active_window(None, 120)`, which is
  what Core's `getnetworkhashps().HandleRequest(request)` reads with getmininginfo's empty
  params. `currentblocksize`/`currentblocktx` come from dashd-mining's `last_block_stats`
  (a new `Miner::last_block_stats`); `pooledtx` is 0 over the empty mempool until MP1a, as D30;
  `chain` is `NetworkIDString()` (`devnet` on a devnet, unlike getblockchaininfo's devnet name).
  The miner's stats are read after the window's turn, where Core reads both under `cs_main`;
  over the empty mempool every assembly records the same stats, so no reply can tell (MP1a
  makes the pair one read). At G1 heights the readiness guard refuses the template and leaves
  `currentblocksize`/`currentblocktx` as they were, where Core updates them: DEC-607 D4's
  allowed diff (§ Allowed diffs), whose scope BL-476 widens now that getmininginfo is served;
  the DIFF below keeps its chain under every G1 height.
  - Proven in DIFF-RPC `mining_reads_match_dashd_v24`: a pair with DIP0003, V20 and MN_RR at
    5000 mines 600 blocks under one mock time (so windows of one time answer Core's `int` 0).
    Help for both, the argument errors (0, -2, heights -2 and past the tip, strings, a
    fraction, out-of-`int` values, a bool, three args, named args), getmininginfo before any
    assembly (no `currentblock*`) and after, a 10 × 13 grid of nblocks × height (with -1 at
    575, 576 and 577 across the interval boundary), and the invalidated tip all match
    byte for byte. Units in `mining::info::tests` pin the rules one by one.
- D52 (RP1a-4, BL-270, DEC-831 Q3): getdeploymentinfo on a block the node never connected.
  Core's `GetSignalsStage` reads `GetForBlock(block)` once V20 is active after the block; it
  walks `pprev` until `GetFromCache` answers (a block below V20's height, or one whose map
  `ConnectBlock` stored) and rebuilds each block above that from disk, throwing
  `failed-getehfforblock-read` (-1, `ExecuteCommand`'s catch) where a block has no data
  (`evo/mnhftx.cpp:285-362`). The node runs that walk before CS4's report (`ehf_signals_read`,
  `ehf_row`), so a header-only block from V20 on answers Core's error and never reaches the
  EHF source, whose contract is to panic on a missing row. Until CH7a keeps the maps,
  `BLOCK_VALID_SCRIPTS` (reached only by `ConnectBlock`, and kept by an invalidated block)
  stands for a stored map, and a block with data is taken to rebuild: the rebuild itself
  (`ProcessBlock` over the block read back, with its `failed-getehfforblock-construct` and
  `EvoDbInconsistencyError` failures) comes with CH7a's EHF store, which must keep this
  walk's answer. It must also keep what Core's rebuild stores: `ProcessBlock(..., false)`
  writes the rebuilt map (`AddToCache`, `DB_SIGNALS_v2`), so once Core has answered for a
  block it no longer needs that block's data, where this proxy still does. No reply can tell
  until rdashd prunes (CH8): a pruned, never-connected block from V20 on that Core answered
  for once would answer here "failed-getehfforblock-read" (validator m-1). On regtest no
  side-branch block with data reaches V20 before MN0-2 assembles
  CbTx blocks (`bad-cb-type` from DIP0003 on), so the rebuild path has unit cover only.
  - Proven in DIFF-RPC `deployment_info_on_header_only_blocks_matches_dashd_v24` (V20, MN_RR and
    DIP0003 at 5, tip 3, headers 4–6): block 4 answers its deployments, blocks 5 and 6 answer
    -1 "failed-getehfforblock-read", both as dashd. The unit
    `a_missing_ehf_row_is_rebuilt_from_data_or_fails_the_read` injects the rows (stored,
    rebuilt, unreadable) in every order the walk can meet them.
- D53 (RP1a-4, DEC-831 Q4): the corrupted-datadir DIFF (`verifychain_on_corrupted_datadirs_
  matches_dashd_v24`, D46) no longer flushes with `gettxoutsetinfo` before the copies: BL-271
  writes V's state at a clean stop, and the copies still load at 20 and match dashd.
- D54 (RP1b-1b, DEC-862 R1): dumptxoutset writes Core v24's file byte for byte. RP1a-4's D51-D53
  and 2c-4/3b's D46-D50 are on their own branches, so this branch starts at D54.
  - The file: `SnapshotMetadata` (the base hash, then `coins_count` as a `u64`), then each coin's
    `COutPoint` and `Coin` in the coins database's key order (`rpc/blockchain.cpp:3188-3205`,
    `node/utxo_snapshot.h:38`). No evo payload (NG-6; Core has no `loadtxoutset`).
  - One snapshot: Core flushes, runs `GetUTXOStats(HASH_SERIALIZED)` and opens the cursor under
    one `cs_main`. Here gettxoutsetinfo's V turn (RP1a D2: an ALWAYS flush, then the engine
    snapshot) starts the scan with a `CoinsDump`, and the scan's thread walks the snapshot
    twice, once for the statistics and once for the file
    (`dashd-chainstate/src/coins/stats_job.rs`). So the file holds exactly the coins the
    statistics count, whatever V connects meanwhile (DEC-862 R3). The snapshot pins the engine's
    memory for both walks, and each running dump holds its own, as each gettxoutsetinfo does.
  - `coins_count` is the statistics' `txouts`: without an index Core counts both once per coin
    (`kernel/coinstats.cpp:106`, `:137`).
  - Interruption: at each coin of the statistics (as gettxoutsetinfo's) and at every 5000th coin
    of the file from the first (`:3196-3197`); "Shutting down" -28 either way.
  - The base's height for the `LOG_TIME_SECONDS` line is read from the index reader on the
    scan's thread, which never waits on V (LO-9). The reply's `nChainTx` is read from V
    afterwards, by the RPC worker.
  - Proven in DIFF-RPC `dumptxoutset_matches_dashd_v24`: every reply and every file byte for
    byte against dashd (D54's snapshots and D55's paths and errors), and the debug logs'
    snapshot lines.
  - Mutants: 14/14 die (`/work/scratch/rp1b-1b/mut-apply.py`, `mut-manifest.txt`, `mut.log`).
    Key order, a fresh snapshot for the file, and the 5000 cadence die only in the unit tests,
    since no regtest chain reaches index 16512, a racing write, or 5000 coins. The worker's
    thread name and `nchaintx` die only in the DIFF.
  - The FT `rpc_dumptxoutset.py` passes, and is passlisted on Core
    `node/func-compat-v24.0-rc4-rp1b-1b` 9d9a113917d (M4 `/work/scratch/rp1b-1b/fc2.log`; M2 in
    full `fc3.log`: 12 pass, 24 expected failures, ratchet ok).
- D55 (RP1b-1b): dumptxoutset's path handling and errors are Core's.
  - Paths: `AbsPathJoin(GetDataDirNet(), path)` (dash-args `abs_path_join`; the empty name is
    the directory itself). The temporary file is `<name>.incomplete` under the same directory.
  - Order: `fs::exists` (dash-args' throwing port), then the temporary file opened with
    truncation, then the snapshot, then `fs::rename` (a new dash-args `rename`, libstdc++'s
    `filesystem error: cannot rename: <strerror> [<from>] [<to>]`). `ExecuteCommand` reports
    both `filesystem_error`s as RPC_MISC_ERROR (`rpc/server.cpp:540-541`).
  - A failed snapshot leaves the temporary file and no file at the path, as Core's does.
  - A NUL in the name: libstdc++ hands the file system `c_str()`, so every call sees the name up
    to its first NUL, `.incomplete` cut off too. `a\u0000b` writes `a`, renamed onto itself,
    and the reply and log print the whole name. rdashd cuts the name the same way (validator
    r0 minor 2); the DIFF's `nul_name` case shows dashd doing it.
  - `command_table` takes the network's data directory, read once at init as Core's cached
    `GetDataDirNet()`.
  - A failed write is RPC_MISC_ERROR "AutoFile::write: write failed: iostream error" (libstdc++'s
    `what()` of `streams.cpp:49`'s `ios_base::failure`, checked with g++ 15).
  - Two permanent divergences on a failing disk, both kept because rdashd's answer is the
    better one and neither can be made exact (r1 Sol n-1, Opus n-2):
    - The final buffered write: Core ignores `fclose`'s result (`:3207`), so a failure in its
      last stdio buffer (st_blksize) goes unreported and the truncated file is renamed into
      place; rdashd's explicit flush of its 8 KiB `BufWriter` answers the error above and leaves
      `.incomplete`. The two buffers differ in size, so the unreported window can't match.
      `a_dump_stops_when_interrupted_or_unwritable` pins it with a two-coin set whose bytes
      fail only at the final flush.
    - An engine read error on the file's walk (the snapshot's second walk): Core's loop
      (`:3196-3205`) never checks the iterator's status, so `Valid()` turns false, the loop ends
      and Core renames a truncated file whose metadata claims the full count, answering success.
      rdashd answers -32603 "Unable to read UTXO set", leaves `.incomplete`, and the read trips
      the fail-stop latch. A decode failure (Core's skipped `GetValue`, `:3199`) can't differ:
      the statistics decoded every record of the same snapshot first. No DIFF reaches either.
  - A missing chainman: Core dereferences `node.chainman`, which no served RPC sees null. rdashd
    answers -32603 "Node chainman not found" after creating the temporary file.
  - The log lines: no functional test asserts "writing UTXO snapshot", so
    `asserted_log_lines.txt` has no row to flip. rdashd logs Core's `LOG_TIME_SECONDS` pair
    unconditionally ("CreateUTXOSnapshot: writing UTXO snapshot at height N (hash) to file P
    (via T) started", then "... completed (X.XXs)", also when interrupted, as the timer's
    destructor logs). The "started" line is written on the scan's thread, which takes the RPC
    worker's thread name first, so `-logthreadnames` shows `httpworker.N` as Core does (validator
    r0 minor 1). The DIFF runs both nodes with `-logthreadnames=1` and compares every
    `CreateUTXOSnapshot` line, timestamps cut and durations masked: same lines, same order.
  - The paths that don't succeed (r1 Opus n-1): statistics that fail (interrupted, or no best
    block) write no byte and never start the timer, and a started walk logs "completed" whatever
    its result (`finish_dump_timer`). The refusals open no temporary file. Mutants rebuilt from
    the r1 descriptions all die in the unit tests (`/work/scratch/rp1b-1b/mut2-apply.py`,
    `mut2.log`): `on_start` before the scan (RVMUT=1), the temporary file opened before `exists`
    (2), "completed" only after `Ok` (3), and the final flush's error dropped (4).

- D56 (RP1b-1c): getspecialtxes is dashd-rpc's `blockchain/specialtx.rs`, over
  `Chainman::read_block` (getblock's lookup and `GetBlockChecked`, with the same errors).
  - Order as Core (`rpc/blockchain.cpp:2486-2556`): `EnsureChainman`, then `blockhash`, `type`,
    `count` ("Negative count"), `skip` ("Negative skip"), `verbosity` ("Verbosity must be in range
    0..2"), then the read. Core also ensures the mempool and the InstantSend manager, which exist
    whenever its chainman does; rdashd has neither yet, so only the chainman is checked.
  - The filter is `HasExtraPayloadField` (special version and type != 0) and `nType == type`
    unless the type is -1. Then come `skip` and `count`. Core sums `nSkip + nCount` as `int`.
    dashd v24.0.0-rc.4's build wraps that sum past `INT_MAX`: `count` 2147483647 with `skip` 1
    answers `[]` (the DIFF found this). rdashd wraps the same way (`i32::wrapping_add`).
  - Verbosity 2 is `TxToJSON`, ported as `core_write::tx_to_json` for RP1b-2's
    getrawtransaction too. It prints `TxToUniv` with the hex, then `blockhash`, then either
    `height`/`confirmations`/`time`/`blocktime` (on the active chain) or height -1 and
    confirmations 0 (off it). Then `instantlock` = the chainlock and `instantlock_internal`
    false (no IS manager, D40), and `chainlock`. There is no spent index (IX1).
  - The block's facts are read once, with the block. Core's `TxToJSON` looks the block up under
    its own `cs_main` for each transaction, so a tip that moves mid-reply can make Core's
    transactions disagree. rdashd's single read is one of Core's interleavings.
  - The on-chain verbosity 2 fields have no DIFF. Regtest connects no special transaction below
    DIP0003, and rdashd refuses G1 templates (DEC-607). The unit test covers them, and the DIFF
    covers the off-chain form on a stored block.
- D57 (RP1b-1c): gettxoutproof and verifytxoutproof without a transaction index
  (`rpc/txoutproof.cpp`) live in dashd-rpc's `blockchain/txoutproof.rs`, over a new
  `Chainman::txout_proof`.
  - gettxoutproof checks the txids first: an empty array, then `ParseHashV` for each, and a
    duplicate is -8 with the string as given. The set is kept in `std::set<uint256>` order (the
    internal bytes), which `Writer::txout_proof`'s binary search needs. The order can't show in
    a reply: a coin's height is its own block's, so whichever txid finds the block, that block
    holds every txid or answers "Not all transactions found". Only the unit test's call log
    checks it (r0 validator N4). Then `EnsureAnyChainman` and `blockhash`.
  - The node (`Writer::txout_proof`) finds the block in one V query turn: the given block's
    entry, or `AccessByTxid` over V's coins cache (outputs 0 to `MAX_OUTPUTS_PER_BLOCK` =
    2,000,000 / 9, per txid in set order; the first unspent coin's height on the active
    chain). A txid with no unspent output costs up to 222,222 point reads in V's turn, as it
    costs Core under `cs_main`.
  - A failed coin read stops the walk and answers RP1a D9's -20 "Error: A fatal internal error
    occurred", as does a V that has stopped before replying. Core's `CCoinsViewErrorCatcher`
    aborts the node on a read error, and V's latch has tripped (r0 validator N5, N6).
  - With no index, `GetTransaction` finds nothing else: "Transaction not yet in block". The
    `-txindex` path is IX1's.
  - The read has no `IsBlockPruned` check, as Core's has none. A header without its block, or a
    pruned block, fails `ReadBlockFromDisk` with its log line: -32603 "Can't read block from
    disk". The count check comes next, then `CMerkleBlock(block, txids)`. That is dashd-p2p's
    `partial_tree` (the `merkleblock` builder), and a txid match takes no type filter.
  - verifytxoutproof: `ParseHexV`, then the `CMerkleBlock` read with libstdc++'s stream errors
    (merkle.rs's `stream_error`, now shared). `ExtractMatches` is ported with all of Core's
    checks, including `MaxBlockSize() / 60`. The flag bits stay packed, eight to a byte, as
    Core's `std::vector<bool>` holds them. Unpacked, a 32 MiB request body (`MAX_BODY_SIZE`)
    of flag bytes would cost eight times Core's memory, though a proof with zero transactions
    returns before reading a bit (r0 validator S1). `a_proofs_flag_bits_are_stored_packed_as_cores`
    pins the stored bytes against a 1 MiB flag vector.
  - A failed extraction is the null root, compared to the header's root as Core compares it.
    Only after a match is the chainman ensured. Then `LookupBlockIndex(X11 hash)`: missing, off
    the active chain, or `nTx == 0` answers -5 "Block not found in chain". Otherwise the matches
    print when `nTx` equals the proof's count, and `[]` prints when it doesn't.
  - Proven in DIFF-RPC `specialtxes_and_txoutproofs_match_dashd_v24` against dashd (both nodes
    at `-txindex=0`), 135 calls byte for byte:
    - proofs by coin: an unspent coinbase, a transaction found by its second output, spent
      transactions, a set whose first txid is spent, and a set spanning two blocks (given in
      both orders; the reply is the same either way);
    - proofs by block: all, a subset, the genesis coinbase, a wrong block, an invalidated block, a
      headers-only block, and a stored block refused at connect;
    - every argument error, help, and named arguments;
    - verification of dashd's proofs and of altered ones (a hash, the count, the header, the
      flags, cut short, extended, bad hex, a huge count), and proofs of blocks off the active
      chain;
    - getspecialtxes over the stored block's 22 txtouniv special transactions, by every type,
      with `count`/`skip` windows past `int`, at each verbosity, and its errors.
  - `rpc_txoutproof.py` starts on the dashd-built chain cache, so it is listed as an expected
    failure owned by CH5 on Core `node/func-compat-v24.0-rc4-rp1b-1c` 2bc97397a37. Past that,
    MiniWallet (RP1b) and `-txindex` (IX1) block it.
  - No new crate edge: dashd-node already depends on dashd-p2p and dash-p2p-core (D45).
- D58 (RP1b-1a): scantxoutset (`rpc/blockchain.cpp:2564-2836`) lives in dashd-rpc's
  `blockchain/scan.rs`, over a new `Chainman::scan_txout_set`, with the walk in dashd-chainstate's
  `coins/scan_job.rs` beside the statistics scan.
  - Descriptors: `EvalDescriptorStringOrObject` (`rpc/util.cpp:1184-1215`) and
    `ParseDescriptorRange`, now shared with deriveaddresses. `Expand` fills a `SigningProvider`
    (Core's `FlatSigningProvider` without the keys): every expanded key's origin (a constant
    key's is its own id's first four bytes with no path), `pkh()`'s and `combo()`'s public keys,
    and `sh()`'s and `combo()`'s scripts. The first entry under an id is kept. `InferDescriptor`
    reads it, so a found coin's `desc` is `pkh([fp/0/3]key)`, `sh(multi([fp]k1,[fp]k2))` and so on.
    Each scan object has its own provider (Core's, `:2752`), and a script keeps its first
    inference: `addr(A)` before `pkh(K)` of the same key prints `addr(A)`. Core's own vectors
    test this: the extractor now takes each `Check`'s key paths and the five
    `CheckInferDescriptor` calls.
  - `ParseRange(-1)` is `[0, -1]`, so a negative `end` answers "End of range is too high", as
    Core's does.
  - The reserver: Core's `g_scan_progress`, `g_scan_in_progress` and `g_should_abort_scan` are
    process globals. Here they are the command's state, one per server (one per process). The
    slot is taken before anything else, and released, with the progress reset, on every exit,
    errors included. `start` clears the abort flag after evaluating the scan objects, as Core's
    does, so an `abort` sent while the needles are built is lost in both.
  - The walk: V's class M turn runs the copy-point ALWAYS flush (`ForceFlushStateToDisk`), and
    the walk's own thread takes the engine snapshot at that epoch before the turn ends (as
    `gettxoutsetinfo`'s, RP1a D11). The tip is the snapshot's meta tip, and its height is read
    from the index. A block connected after the start is not seen, as with Core's LevelDB cursor.
    The snapshot pins the engine's memtables and SST files for the walk's length: on a mainnet
    set, the time a full walk takes, the same pin `gettxoutsetinfo` and `dumptxoutset` already
    hold. Core's cursor pins its LevelDB snapshot the same way.
  - The cadence is `FindScriptPubKey`'s. Progress resets to 0 and updates every 256th coin to
    `(int)((0x100 * h[0] + h[1]) * 100.0 / 65536.0 + 0.5)` from the key's first two stored txid
    bytes, and ends at 100. Every 8192nd coin runs the interruption point (once the server stops
    running: -9 "Shutting down") and then the abort flag. An abort is unsuccessful, and that coin
    is counted but not matched. The column family's order is Core's serialized-key order
    (`txid ‖ VARINT(vout)`), so the samples, the boundary and an aborted walk's partial results
    are Core's. The found coins come in `std::map<COutPoint, Coin>` order (txid bytes, then vout
    numerically), which differs from the cursor's past `VARINT(16383)`.
  - A record that does not decode ends the walk unsuccessfully with what it found (Core's
    `GetValue` returning false). The cursor reports the corruption (RP1a D7). A set with no tip, a
    meta read error or a lost walk thread has no answer in Core: it answers `gettxoutsetinfo`'s
    -32603 "Unable to read UTXO set".
  - Proven by unit tests (cadence, formula, abort boundary, interruption, snapshot against live,
    result order, a bad record, the reserver across a running scan and its release on each
    error), and in DIFF-RPC `scantxoutset_matches_dashd_v24` against dashd, 91 calls byte for
    byte. The scan objects array is `get_array()`'s: `null` answers -3 "JSON value of type null
    is not of expected type array" (the DIFF found this). The
    DIFF covers each descriptor shape, the ranges, mixed and duplicate objects in both orders,
    the spent and OP_RETURN outputs, an immature coinbase, idle status/abort, every argument
    error, named arguments, arity and help. Mid-scan progress and an abort that lands are
    unit-proven only.
  - `rpc_scantxoutset.py` still can't pass: it funds each address with MiniWallet's
    `send_to` (`sendrawtransaction` into the mempool, MP1a). The five FTs whose MiniWallet
    needed scantxoutset (feature_dersig, mining_basic, rpc_blockchain ×2, rpc_createmultisig)
    are re-owned to their next blockers on this slice's Core list branch.

## RP0 D-rows (`RP0-D<n>`)

RP0, the `dashd-rpc` server core (HTTP, auth, dispatch, `help`), was reviewed and accepted on 10-14 (Sol r2 ACCEPT, Opus-h r1 APPROVE-WITH-NITS) and merged in train 11. Its D-rows were reported in the slice report but never written into a doc; they are recorded here because shipped code cites them as `RP0-D<n>` (BL-571). Numbers RP0-D2, RP0-D4 and RP0-D6 were not carried into the report and nothing cites them.

- RP0-D1: `help` describes Core v24's whole command table in its wallet-disabled view (dashd `-disablewallet`), whichever commands rdashd serves yet. Its DIFF compares against dashd run with `-disablewallet` (dashd-rpc/src/commands/mod.rs, rdashd/tests/rpc_diff.rs).
- RP0-D3: `/wallet/` is always registered, as a dashd built with wallet support registers it (dashd-rpc/src/httprpc.rs).
- RP0-D5: a failed `CHECK_NONFATAL` (`echo`'s `trigger_internal_bug`, and every internal-bug reply) reports rdashd's own identity where Core's text names its own package and source (dashd-rpc/src/commands/mod.rs).
- RP0-D7: one HTTP event thread waits on every connection at once with non-blocking I/O (mio, default features off), as libevent does in Core, in place of a thread per connection; idle connections hold no threads (dashd-rpc/src/http/mod.rs, dashd-rpc/Cargo.toml, rdashd/tests/rpc.rs).
- RP0-D8: the asserted-log ratchet leaves out the files whose literals are Core's verbatim RPC help text (dash-rpc-spec's generated table), which quotes log-like phrases no code logs (dashd-node/tests/asserted_log_lines.rs).
- RP0-D9: `rpc_bind`'s non-loopback cases (binding a public interface, and a client outside `-rpcallowip`) are not run by rdashd's tests. The loopback cases run end to end, and the allow list's matching is unit-tested (rdashd/tests/rpc.rs).
- RP0-D10: the `rdash-cli` end-to-end cases run only when the rdash-cli binary is built. `cargo test --workspace` (the gate) builds it; a per-crate `cargo test -p rdashd` (a CI group) does not, and skips them (rdashd/tests/rpc.rs).
- RP0-D11: no BG `rpc-regtest` PF0 row: node-bench's rpc workload needs getblockhash/getblock, and node rows are integration-only.
- RP0-D12: a handler that panics is answered "Unhandled request" as it unwinds, and its worker carries on. Core's worker dies with the process on an uncaught exception (dashd-rpc/src/http/mod.rs).
- RP0-D13: a chunk-size line longer than a header block fails the request; libevent buffers one without limit (dashd-rpc/src/http/parse.rs).
- RP0-D14 (manager ruling, 10-14): RP0's compat-ledger rows 318 and 322–328 are re-homed to WL6, P2a-net and P2a-core.
- RP0-D15: `uptime` counts from step 4a of startup.
- RP0-D16: the request URI is decoded lossily as UTF-8; a note for the `/wallet/<name>` implementer.

## Deferred to RP1a-3a

Closed by RP1a-3a (D24, D25, D27). From the RP1a-1b Opus r2 review (n-3, DEC-632). The descriptor and key_io ports fit RP1a-1b; the
util RPCs (getdescriptorinfo, deriveaddresses, validateaddress, createmultisig, verifymessage)
need these changes, which RP1a-3a's plan takes up:

1. **Key origins.** `descriptor.rs` checks `[fingerprint/path]` and drops it; getdescriptorinfo's
   `descriptor` (`desc->ToString()`) prints it, so `PubkeyProvider` needs an origin variant
   (fingerprint, path, inner provider).
2. **A required checksum.** `check_checksum` always runs as `require_checksum = false`;
   deriveaddresses parses with `require_checksum = true` and needs "Missing checksum".
3. **`decode_destination`'s error text.** It answers `Option` and drops Core's `error_str` and
   `error_locations`, which validateaddress returns (with the Platform bech32 decode and
   `bech32::LocateErrors`). This changes the signature at its three call sites.
4. **Encoders.** `EncodeDestination`, `ExtractDestination` and `InferDescriptor` (createmultisig's
   `descriptor`) are not ported yet; they are additive.

Also from that review: WL2's wallet will want the descriptor and key_io code, so a home outside
dashd-rpc avoids a wallet → rpc dependency. n-4 (separator-dense descriptors allocate the split
before any check) matches Core's `Split` and stays as it is.

## Allowed diffs

These seed `compat/ALLOWED-DIFFS.md` (O14):

| Difference | Ruling | Closer |
|---|---|---|
| ~~getblock verbosity ≥ 2, and the `cbTx` object a coinbase with a payload carries, refused with -32603 "getblock: ... is not served until RP1a-2"~~ (closed by RP1a-2c, D42) | DEC-611 | RP1a-2 |
| at `setmocktime 9223372036` (the maximum), Core's `NodeClock::now() + 2h` overflows int64 nanoseconds, so dashd refuses every header as `time-too-new`; rdashd's seconds arithmetic does not overflow and accepts them | DEC-618 n-3 | none (a Core overflow artefact at the range bound; not copied) |
| getblock and getrawtransaction `instantlock`/`instantlock_internal`: a transaction with an islock in a block not yet chainlocked prints `false`/`false`, where Core prints `true`/`true` (`IsLocked`) | D40 (r1 m-3, DEC-719) | LK1b |
| getblock 2 and 3 on a block stored but never connected (regtest, below DIP0003) whose ProTx payload is exactly its version bytes: Core prints a default ProUpRegTx/ProUpRevTx object, or answers -1 "Internal bug detected: obj.netInfo …" for a ProRegTx/ProUpServTx; rdashd prints no object. The same for an Evo ProTx with an unknown `ExtNetInfo` version under `-deprecatedrpc=service` (Core's `GetPlatformPort` `CHECK_NONFATAL`; rdashd −1) | D41 (r1 m-1) | RP1b |
| a block that failed `ConnectBlock` is lost across a restart: rdashd answers getblock -5 "Block not found" where dashd still serves it (its index keeps `BLOCK_FAILED_VALID \| HAVE_DATA`). Whether a valid side-branch block (stored, never connected) is lost too is untested (r2 n-1); the owner narrows or widens this row | D42 (found by r1 m-1's DIFF), BL-517, DEC-729 | CH1-node |
| a block below G1 whose coinbase is a special-version (type-5) transaction stops rdashd at connect ("evo validation (CH7a) for a special-version coinbase is not built", the ALLOWED-DIFFS D18 refusal), where dashd stores it and marks it invalid; CH7a first triages whether P2P reaches it | D42 (found by r1 m-1's DIFF), BL-516, DEC-729 | CH7a |
| getmemoryinfo "stats": at idle rdashd answers zeros where dashd has one arena holding `RNGState`'s 240 bytes (used 240, free 261904, total 262144, chunks 1/1); after any key RPC rdashd's stats are dashd's less that chunk (used −240, free +240, chunks_used −1) | D50, BL-502 | BL-502 |
| getmemoryinfo "stats" on a node started with `-sporkkey` (and a masternode's BLS key): dashd's pool holds the key's chunk for the node's life (used +32, chunks_used +1 for the spork key); rdashd's does not, and keeps the spork key in ordinary memory | D50, BL-531 (r1 m-3) | BL-531 |
| getmemoryinfo "mallocinfo": the XML's numbers describe each process's own glibc heap | D48 | none (two heaps) |
| VerifyDB's levels 3 and 4 check nothing: verifychain and the load's VerifyDB pass where dashd's disconnect or reconnect of the last blocks reports a coins inconsistency, an unconnectable block or a failed reconnect; at level 4 Core's progress lines differ, and at levels 3–4 its summary counts the transactions it disconnected where rdashd prints 0. Levels 0–2 match dashd (D46) | D46, BL-524 | CH5 |
| templates at G1 heights (DIP0003, V20, MN_RR, V24, EVO_SHARES) refused: generate* answer -32603 "Couldn't create new block: block <height> needs <rule>, which is not served until <owner>", where Core assembles a CbTx block; once served, getblocktemplate refuses the same way and getmininginfo's `currentblocksize`/`currentblocktx` stay unchanged, where Core updates them | DEC-607 D4 | MN0-2, MN0-3 |
