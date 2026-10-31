# RP1a-2c-1 round 1 review (Opus-h, independent second reviewer)

Branch `node/rp1a-2c` at 602bb9bf7 (worktree `rdc-wt/rp1a-2c`, read only), base a7ae369d2.
Core reference: v24.0.0-rc.4 (`~/workspace/dash`). base-sdk pin f27bc4f5 (`Cargo.lock`).

**Verdict: ACCEPT-WITH-NITS** (0 M, 3 m, 4 n)

On the connected-block path I found no Core divergence: TxToUniv, the payload objects, the
sighash mode, the undo alignment and the block-level `cbTx` all match. The port is careful, and
the fixture approach is the right call. The three minor findings are as follows:

- m-1: one D41 premise is wrong. getblock serves stored blocks that failed `ConnectBlock`, and
  on regtest such a block can carry any payload below G1. So the version-only edge D41 calls
  unreachable is reachable on rdashd today. The same fact gives a cheap live DIFF for most of
  D42's deferred cases.
- m-2: four surviving payload mutants, on shapes valid on chain, that the trimmed fixtures miss.
- m-3: D40 has no owner and no ALLOWED-DIFFS row.

None of them is a wrong answer on a block a node would connect. m-1 and m-2 are cheap and
worth landing in 2c-1's r1 fixes.

## Findings

### m-1: getblock serves stored blocks that never connected, so D41's version-only edge is reachable below G1, and D42 can get live payload cases now

- **Where:** `dashd-rpc/src/core_write/evo.rs:57-62`: `UnknownProTxVersion` and `Unknown` print
  no object. D41 (`docs/node-program/designs/RP1a-notes.md:233-237`) says "no connected block
  carries one, and RP1b's decoderawtransaction owns that edge". D42 (`:257-259`) says DIFF can
  compare getblock 2 and 3 live "only below G1 (coinbases, fee-paying spends, …)".
- **Core:**
  - `validation.cpp:351-385` `ContextualCheckTransaction` checks transaction types only when
    `fDIP0003Active_context` holds. Below DIP3, `CheckBlock` and `ContextualCheckBlock` accept
    a version-3 transaction of any type with any payload, and `AcceptBlock` writes the block to
    disk.
  - Only `ConnectBlock` refuses it, through `evo/specialtxman.cpp:227-229`
    (`bad-tx-type-dip3-inactive`). The block keeps `BLOCK_HAVE_DATA`.
  - `getblock`/`GetBlockChecked` (`rpc/blockchain.cpp:786-804`) does not look at validity.
- rdashd ports this exactly: `dash-consensus/src/tx.rs:155-175` and
  `dashd-chainstate/src/connect/evo.rs:569`. The block is therefore stored on both nodes.
- **Failing input:** on fresh regtest, `submitblock` a block at height ≤ 431 that carries a
  version-3 type-1 transaction whose payload is `6300` (ProRegTx version 99), then
  `getblock <hash> 2`.
  - **Core:** `CProRegTx`'s reader bails after the version (`evo/providertx.h:167-171`).
    `GetTxPayload` sees an empty stream and returns the object with `netInfo == nullptr`.
    `GetNetInfoWithLegacyFields` then fails `CHECK_NONFATAL(obj.netInfo)`
    (`evo/core_write.cpp:40`). `NonFatalCheckError` is a `std::runtime_error`, so
    `ExecuteCommand` answers `-1 "Internal bug detected: obj.netInfo\nevo/core_write.cpp:40
    (GetNetInfoWithLegacyFields)\n…"` (`rpc/server.cpp:541`, `util/check.cpp:19-25`).
  - **rdashd:** a result whose transaction has no `proRegTx`.
  - **Same payload as type 3 or 4** (ProUpRegTx `providertx.h:338`, ProUpRevTx `:407-411`):
    Core prints a default object, for example
    `"proUpRevTx":{"version":99,"proTxHash":"00…00","reason":0,"inputsHash":"00…00"}`. rdashd
    prints none.
  - **Same edge, with `-deprecatedrpc=service`:** an Evo ProRegTx v3 whose `ExtNetInfo` has an
    unknown version (Core reads only the version byte and leaves `m_data` empty, so `IsEmpty()`
    is false) fails `CHECK_NONFATAL(HasEntries(...))` in `GetPlatformPort`
    (`evo/core_write.cpp:60-93`). rdashd prints −1. D41's "`Validate` refuses such an Evo
    ProTx" is true, but `Validate` runs at connect, after the block is stored.
- **Reach:** regtest only, on a crafted block. Mainnet and testnet cannot store a block below
  DIP3 any more (checkpoints and minimum chain work). Core's answer is an internal-bug report,
  so copying it is optional. What matters is that D41's stated reason is wrong for getblock.
  The owner it names, RP1b, owns decoderawtransaction, not getblock.
- **Fix:**
  1. Either port the edge or record it. Porting means a default object for a version-only
     ProUpReg/ProUpRev payload, and the `CHECK_NONFATAL` error for ProReg/ProUpServ. Recording
     means an ALLOWED-DIFFS row for getblock 2/3 on stored-but-unconnected blocks, with RP1b as
     closer.
  2. Reword D41's "no connected block carries one" either way.
- **Test opportunity (cheap, high value):** the same mechanism gives DIFF-RPC most of D42's
  cases 1–4 now, live against dashd, instead of after MN0-2/CH7a. `submitblock` to both nodes a
  block below 432 that has:
  - a type-5 coinbase with a v3 or v4 CbTx payload (the block-level `cbTx`, D42 case 1);
  - fixture ProRegTx (legacy, basic and Evo), ProUpServ, qcTx, MnHF and asset lock/unlock
    transactions, verbatim from `txtouniv`. Their inputs need not exist: the block is stored
    before any input is looked up.

  Then compare `getblock` 1/2/3 with and without `-deprecatedrpc=service`. Both nodes store the
  block and refuse it at connect, so it prints with no undo data: another no-undo case besides
  genesis (n-3). Fees and prevouts on special transactions still need G1.

### m-2: four payload mutants survive every dashd-rpc test; the trimmed fixtures lack shapes that are valid on chain

I ran env-gated mutants in a reflink copy against the whole `dashd-rpc --lib` suite (193 tests).
The unmutated build passes.

| # | Mutant (in `dashd-rpc/src/core_write/evo.rs`) | Core | Result |
|---|---|---|---|
| 1 | `share_list` (:113-116): `reward` is always `refund` | `providertx.h:80` `RewardScript()`; `providertx_util.cpp:66-85` | **survives** |
| 2 | `share_list`: an empty reward prints empty (no fallback to refund) | same | killed (fixtures) |
| 3 | `entry_addr_port` (:227-230): a domain entry prints without `:port` | `netinfo.h:188` `DomainPort::ToStringAddrPort` | **survives** |
| 4 | `pro_up_share_tx` (:448-450): pushes `"rewardAddress":""` when the reward script has no destination | `evo/core_write.cpp:271-282` | **survives** |
| 5 | `INVALID_ENTRY` prints `[invalid]` | `netinfo.cpp:264-275` | **survives** |

My scan of the 213 fixture rows (`/work/scratch/rp1a-2c1-rv-opus-tools/scan.py`) agrees with
the table:
- All four share entries have `rewardScript == refundScript`.
- There is no domain entry anywhere in `addresses`.
- Both ProUpShareTx rows carry `rewardAddress`.
- No row has an `[invalid entry]`.

Mutants 1, 3 and 4 are shapes valid on chain once EVO_SHARES and V24 are live. Domains are
allowed for `platform_https` (`netinfo.cpp` `ExtNetInfo::Validate`), and a share may set its own
reward script. Mutant 5 is reachable only through a stored-but-unconnected block (m-1) or
decoderawtransaction. Reading the code, I believe all four print what Core prints today. The gap
is that a regression would pass.

The likely cause is `trim.py`, which keeps the two shortest rows per bucket. The bucket key
(`t<type>/pv<ver>/mn<type>`) does not separate these shapes, so a domain row or a distinct-reward
share loses to a shorter sibling if the source chains had one.

- **Fix:**
  1. Add bucket dimensions to `extract.py` (domain present, share reward ≠ refund, empty
     ProUpShare reward) so that trim keeps one of each.
  2. Where the chains have none, add a second fixture of crafted transactions decoded by the
     same dashd. `decoderawtransaction` needs no validity, so hand-built payloads are fine, and
     PROVENANCE can list them as crafted.
  3. One crafted ProRegTx v3 with a domain `platform_https` entry and one invalid entry, plus
     one shared ProRegTx with a distinct reward script, kill 1, 3 and 5. A ProUpShareTx with an
     empty reward script kills 4.

### m-3: D40's deferral has no owner and no ALLOWED-DIFFS row

- **Where:** `dashd-rpc/src/blockchain.rs:465-466` and D40 (`RP1a-notes.md:226-230`, "owner to
  be named"). The ALLOWED-DIFFS seed table (`:300-306`) gained no row.
- **Core:** `rpc/blockchain.cpp:193-194` prints `instantlock` as `fLocked || chainlock` and
  `instantlock_internal` as `fLocked`, from `isman.IsLocked(txid)`.
- **Failing input:** past G1 with LLMQs, `getblock <h> 2` on a block that is not yet
  chainlocked and holds a transaction with an islock. Core prints `true`/`true`; rdashd prints
  `false`/`false`.
- **Reach:** none today. InstantSend needs DIP3 and LLMQs, which rdashd does not serve. Every
  other deferral in these notes names a closer and has a table row. This one would be lost when
  G1 lands, because D42's restore list does not mention it either.
- **Fix:** name the owner (the slice that brings `CInstantSendManager`), add an ALLOWED-DIFFS
  row, and add the case to D42's list of what DIFF regains past G1. getrawtransaction will
  share the same row.

### n-1: mutant 17 is an equivalent mutant; its survival is fine

- **Where:** `evo.rs:155-160` (`groups()`'s `version == EXT_NET_INFO_VERSION` guard).
- base-sdk's `NetInfoV2::decode` (pin f27bc4f5,
  `pkgs/primitives/src/types/netinfo.rs:428-436`) returns `entries: []` for an unknown version,
  as Core's `ExtNetInfo::Unserialize` does (`netinfo.h:413-418`). No decoded payload can tell
  the guard from its removal, so no fixture can kill this mutant.
- The `is_empty()` guard at :172 is the same in effect. For an extended info it feeds only
  `platform_port`, which answers −1 on both sides of the guard.
- **Fix (optional):** keep the guards, and say in a comment that they are defensive against a
  hand-built `NetInfoV2`. Alternatively, drop them. Neither changes output.

### n-2: no fixture has a fractional `operatorReward`

- **Where:** `evo.rs:370-371`. Core: `evo/core_write.cpp:223`, a `double` through
  `UniValue::setFloat`, which formats with `%.16g`.
- All fixture rewards are whole percentages (2, 3, 7, 8, 25). 1,237 of the 10,000 possible
  `nOperatorReward` values print with a long tail under `%.16g`: for example, 7 prints as
  `0.07000000000000001`. `dash-args`' `format_g16` has its own test
  (`float_formatting_matches_setprecision_16`), so this is covered indirectly.
- **Fix (optional):** a crafted row with `nOperatorReward = 7` (see m-2) pins it end to end.

### n-3: the no-undo DIFF case is only genesis

- **Where:** `rdashd/tests/rpc_diff.rs:2113-2256`.
- Genesis has no `HAVE_UNDO`, so it reaches the `undo_pos == None` arm. A block that was stored
  but never connected also reaches it, as a side-branch block or the m-1 block, and it is the
  realistic case. Such a block checks that rdashd never sets `HAVE_UNDO` on a block it has not
  connected.
- **Fix (optional):** the m-1 block covers this for free. Otherwise, `submitblock` a competing
  block at the tip's height (first seen stays active) and `getblock <it> 3`.

### n-4: prevout scriptPubKeys in the DIFF are all `raw(51)`

- **Where:** `rpc_diff.rs:2170-2199`. Every spend in the DIFF spends a `raw(51)` coinbase or a
  `51` output, so every live `prevout.scriptPubKey` is `nonstandard`.
- The unit test at `blockchain/tests.rs` (`getblock_verbosity_2_and_3_print_each_tx_with_its_undo`)
  covers a P2PKH prevout, but it builds its expected JSON with `script_to_univ` itself.
  `script_to_univ` is diffed through `vout` (P2PKH, P2SH, nulldata, and the coinbase descriptor
  shapes), and `prevout` calls it with the same flags. The risk is therefore small.
- **Fix (optional):** spend one P2PKH output with a real signature (the generator key signs) so
  that a `prevout` carries `address` and `desc` live.

## Things I checked and found to match Core

**TxToUniv** (`core_write.rs:352-452` vs `core_write.cpp:180-362`):
- **Header fields:** key order is `txid, version, type, size, locktime, vin, vout,
  [extraPayloadSize, extraPayload], [payload object], [instanceHash], [fee], [blockhash], [hex]`.
  `version` is `(uint16_t)nVersion` from base-sdk's `i16`, so −1 prints 65535. `type` is the raw
  16-bit type, also for version < 3 (Core keeps `nType`; `Tx::new` keeps `to_base()`).
- **Inputs:**
  - A coinbase input prints only `coinbase`. Other inputs print `txid`, `vout` and
    `scriptSig{asm,hex}`.
  - `prevout` comes between `scriptSig` and `sequence`.
  - No `txinwitness`, as Dash has none.
- **Undo-dependent fields:**
  - `fee` is present iff the transaction has undo data. A Platform transfer prints its
    payload's fee (`IsPlatformTransfer` = special version and type 9, `transaction.h:330-333`).
    `amt_total_out` adds up only when undo data exists in Core; the output is the same.
  - `prevout` prints `generated` (bool), `height` (`uint64_t`), `value` (`ValueFromAmount`, which
    `peers::amount` matches, sign included) and `scriptPubKey` (`ScriptToUniv` with hex and
    address).
- **Payload and hash fields:**
  - `extraPayloadSize` is a `size_t`, so unsigned.
  - A payload object prints only when `is_complete()` (Core's `ds.empty()`).
  - The type → key mapping covers types 1–12. `UnknownProTxVersion`/`Unknown` print none (see
    m-1 for the version-only edge).
  - `instanceHash` appears only under `IsAssetUnlockWithStableTxid` (rc.4
    `transaction.h:383-395`, identical to the rc.3 port), after `assetUnlockTx` and before
    `fee`.

**blockToJSON / getblock** (`blockchain.rs:395-501` vs `rpc/blockchain.cpp:168-207, 954-1067`):
- **Verbosity:** ≤ 0 prints hex (checked before any undo read). 1 prints txids. 2 means
  `SHOW_DETAILS`, and ≥ 3 means `SHOW_DETAILS_AND_PREVOUT`. A bool `true` means 1.
- **Per transaction:** `include_hex = true` and `block_hash = uint256()`, so there is no
  `blockhash`. `instantlock` and `instantlock_internal` come after the TxToUniv keys.
- **Block-level `cbTx`:** it follows `tx` at every verbosity ≥ 1. It is decoded from the raw
  coinbase payload whatever the coinbase's type, with `rest.is_empty()`, as Core's
  `GetTxPayload<CCbTx>(vExtraPayload)` (`specialtx.h:17-28`) does.
- **CbTx decoding:** base-sdk's `CoinbaseCommitment::decode` reads the same `>=`-versioned
  fields as `CCbTx`'s `SERIALIZE_METHODS` (`cbtx.h:52-66`). Versions 0 and 5+ read like Core's,
  and `bestCLHeightDiff` is a CompactSize.

**Undo:**
- **Alignment:** the coinbase is skipped by index 0, not by `IsCoinBase` (Core's `i > 0`).
  `vtxundo.at(i - 1)` maps to `undo.txs.get(i - 1)`, and the `_M_range_check` text matches
  libstdc++. `ExecuteCommand` maps `std::exception` to `RPC_MISC_ERROR` (`server.cpp:541`).
  The short-coins check comes before printing, which is fine because Core's case is UB.
  Extra undo entries are ignored, as in Core.
- **Read path:** `Chainman::read_undo` (`dashd-node/src/rpc.rs:582-605`) works like this:
  - It takes `IsBlockPruned`, `GetUndoPos` (`HAVE_UNDO`-gated, `dash-chain/src/reader.rs:361`)
    and `pprev` hash in one V turn.
  - A file of −1 counts as null (424ef341f).
  - The read uses the same `read_undo_record` that DisconnectBlock uses. That function checks
    Core's checksum over `prev_hash ‖ record` and treats the size field as a hint.
  - Every failure gives `None`, so the block prints without fee or prevout, as with Core's
    `have_undo = false`.
- **Lock and tip:** the block read and the undo read are two turns, as in Core
  (`GetBlockChecked` and then `blockToJSON`'s own `cs_main`). Neither a reorg nor an invalidation
  removes undo data in either implementation, and a prune between the two turns answers
  `have_undo = false` on both. The header facts (`confirmations`, `chainlock`) come from the
  block read's turn, as Core's `tip` does.

**ScriptToAsmStr** (`core_write.rs:209-246` vs `core_write.cpp:107-157`):
- A push of ≤ 4 bytes, including a zero-length push, prints as a number before any decode.
  Longer pushes are decoded only when the whole script is not `IsUnspendable` (`OP_RETURN`
  first, or more than 10,000 bytes).
- `check_signature_encoding(STRICTENC)` is the exact port: empty passes, then DER, with no
  LOW_S under STRICTENC alone, then a defined hashtype.
- A defined hashtype is exactly the six `mapSigHashTypes` entries, so `sighash_name` never
  misses after the check.
- Bad DER, hashtype 0x00/0x80 or an undefined byte print the full hex. Core's
  `script_GetScriptAsm` vectors are reproduced.

**Payload objects** (`evo.rs` vs `evo/core_write.cpp`, `llmq/core_write.cpp:15-40`,
`providertx_util.cpp:36-85`):
- **ProRegTx:** fields in order. `service` is gated by the flag. `ownerAddress` appears iff not
  shared. Then shares / payouts (≥ v3) / `payoutAddress` (`ExtractDestination`, P2PK included).
  `operatorReward` is a `double / 100` (see n-2). Evo adds `platformNodeID` and the ports gated
  by the flag. `inputsHash` comes last.
- **ProUpServTx, ProUpRegTx, ProUpRevTx, ProDisTx, ProUpShareTx, ProUpSharedRegTx:** each in
  Core's order.
- **Net info:**
  - `GetNetInfoWithLegacyFields` adds `platform_https` then `platform_p2p` for an Evo node with a
    non-empty legacy info.
  - `GetPlatformPort` returns the legacy stored port, −1 when empty, or the first entry's port.
  - `MnNetInfo` is empty when `!service.IsValid()`, so `service` prints `[::]:0` (1 fixture row).
  - `ExtNetInfo::GetPrimary` uses only a `Service` entry.
  - A domain is `%s:%d`. Domain bytes are validated, so `from_utf8_lossy` is lossless.
- **CbTx:** the v1–v4 field ladder.
- **qcTx:** `quorumIndex` is 0 when absent. The bits are LSB-first and the counts run over
  `num_bits`. The BLS fields use the version's scheme (legacy for v1/v2).
- **MnHF:** `signal{versionBit, quorumHash, sig}`.
- **Asset lock:** `scriptPubKey` without an address. v2 adds `address`; every network has an
  HRP, so the `platform_hrp.is_some()` guard never changes output.
- **Asset unlock:** `index` is `u64` and `fee` is `u32`.
- **Lazy operator key:** printed under `bufLegacyScheme` (`bls.h:571-574`), which is `get()`
  under `scheme()`.

**D38–D42:**
- D38's sighash mode is done.
- D39 matches Core's `have_undo` semantics.
- D41's BLS-flag ruling is sound for connected blocks; the stored bytes are canonical or zero.
  So is the MoneyRange omission (a connected block's undo data). m-1 covers the version-only
  premise.
- D42's five deferred cases are right to need G1 for fees and prevouts, but see m-1 for what can
  come back now.
- D40: see m-3.

## What I checked and ran

- I read `git log -p a7ae369d2..602bb9bf7` in full: all 8 commits and 20 files.
- I read these Core rc.4 sources:
  - `core_write.cpp`, `rpc/blockchain.cpp`, `rpc/server.cpp` and `util/check.cpp`;
  - `evo/core_write.cpp`, `evo/providertx.h`, `evo/providertx_util.cpp`, `evo/netinfo.{h,cpp}`,
    `evo/cbtx.h`, `evo/specialtx.h` and `evo/specialtxman.cpp`;
  - `llmq/core_write.cpp`, `bls/bls.h`, `key_io.cpp`, `primitives/transaction.h` and
    `validation.cpp` (`ContextualCheckTransaction`).
- On the rdashd side I read:
  - base-sdk at f27bc4f5: `CoinbaseCommitment::decode`, `NetInfoV2::decode`, `PayloadDecode`
    and `Transaction`;
  - `dash-consensus`'s `Tx` helpers, `check_signature_encoding`, `is_unspendable` and
    `contextual_check_transaction`;
  - `dashd-store`'s `read_undo_record` / `read_undo_stream`, `dash-chain`'s `undo_pos`, and
    `dash-args`' `from_f64`.
- I scanned the 213 fixture rows for conditional-field coverage, with
  `/work/scratch/rp1a-2c1-rv-opus-tools/scan.py` (run with `python3 -I`).
- **Reflink copy:** `/work/scratch/rp1a-2c1-rv-opus`, left for the manager. Its only change is
  the env-gated mutants in `dashd-rpc/src/core_write/evo.rs`.
  - I built it once (`cargo test -p dashd-rpc --lib`, jobs 8, nice 19, TMPDIR in the copy).
  - The baseline gave 193 passed.
  - Mutants 1–5 were run as `A6MUT=n` against the whole lib suite. 1, 3, 4 and 5 survive, and 2
    fails `dashd_decoderawtransaction_fixtures`.
- **Not rerun:** the getblock DIFF against dashd, gate-fast, and the implementer's 17 mutants.
  I had no reason to doubt them.
- I did not run the m-1 `submitblock` case live. Its reach follows from the code, which both
  sides store identically (`tx.rs:155-175` vs `validation.cpp:351-385`) and refuse at connect
  (`connect/evo.rs:569` vs `specialtxman.cpp:227-229`). Core's answer follows from
  `providertx.h:167-171` plus `evo/core_write.cpp:40`. The m-1 DIFF case would confirm it.

## Agreement with the Sol review (RP1a-2c1-r1-sol.md)

When I finished, the Sol file held only a stub: "Verdict: IN PROGRESS" and an empty Findings
section. There was nothing to agree or disagree with yet. The manager should compare the two
once Sol's findings are written.
