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
- D17: `generateblock`'s TestBlockValidity needs the template's parent to be V's tip. Core asserts
  that under `cs_main`; rdashd's template and its check are two V turns, so a tip that moved in
  between (`BlockCheck::TipMoved`) rebuilds the template with the same transactions and checks
  again. Only V's `NotBestPrev` is that verdict; any other "not tested" answer is an internal bug
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
| templates at G1 heights (DIP0003, V20, MN_RR, V24, EVO_SHARES) refused: generate* answer -32603 "Couldn't create new block: block <height> needs <rule>, which is not served until <owner>", where Core assembles a CbTx block; once served, getblocktemplate refuses the same way and getmininginfo's `currentblocksize`/`currentblocktx` stay unchanged, where Core updates them | DEC-607 D4 | MN0-2, MN0-3 |
