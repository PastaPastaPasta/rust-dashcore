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
  holds below DIP3 has a payload. Owner RP1a-2.
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
  2c adds the mode rather than reusing it for scriptSigs (Sol r1 m-1, DEC-704).

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
| getblock verbosity ≥ 2, and the `cbTx` object a coinbase with a payload carries, refused with -32603 "getblock: ... is not served until RP1a-2" | DEC-611 | RP1a-2 |
| at `setmocktime 9223372036` (the maximum), Core's `NodeClock::now() + 2h` overflows int64 nanoseconds, so dashd refuses every header as `time-too-new`; rdashd's seconds arithmetic does not overflow and accepts them | DEC-618 n-3 | none (a Core overflow artefact at the range bound; not copied) |
| templates at G1 heights (DIP0003, V20, MN_RR, V24, EVO_SHARES) refused: generate* answer -32603 "Couldn't create new block: block <height> needs <rule>, which is not served until <owner>", where Core assembles a CbTx block; once served, getblocktemplate refuses the same way and getmininginfo's `currentblocksize`/`currentblocktx` stay unchanged, where Core updates them | DEC-607 D4 | MN0-2, MN0-3 |
