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

## Deferred to RP1a-3a

From the RP1a-1b Opus r2 review (n-3, DEC-632). The descriptor and key_io ports fit RP1a-1b; the
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
