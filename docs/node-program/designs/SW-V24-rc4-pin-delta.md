# SW-V24 PIN-DELTA: Dash Core v24.0.0-rc.3 ⇄ v24.0.0-rc.4

| | |
|---|---|
| Task | SW-V24 re-pin procedure, step 1 (DEC-456 (4), BL-393): audit of rc.3 → rc.4 and the re-pin plan |
| Date | 2026-10-26 |
| Pins | **old** v24.0.0-rc.3: tag object `da567d7dba`, commit `0364ed626e` · **new** v24.0.0-rc.4: tag object `49caf372bb`, commit `0e7382df80` (`git rev-parse v24.0.0-rc.4^{commit}`; it equals the head of upstream `v24.0.x` on 2026-10-26). rc.3 is an ancestor of rc.4 |
| Outputs | this report · `SW-V24-rc4-pin-delta/` (packets `ce.md`, `mn-overlay.md`, `wallet.md`; `hunks.md`, the per-hunk index) |
| Tools | rust-dashcore `contrib/node/pin-delta/inventory.py` (integration copy, unchanged), plus two scratch scripts for the sub-PR attribution (§1.2). Scratch: `/work/scratch/swv24-rc4` |
| Status | Draft for review (Sol, then manager). No code or pin changed |

## 0. Summary

**Scope.** rc.3..rc.4 is one first-parent merge, #7917 ("v24.0.x backports for v24.0.0-rc.4"). It holds 40 non-merge commits: 37 cherry-picks of develop merges (`cherry-pick -m1 -x`, one per develop PR) and 3 direct commits (a56ed47c98a, 79480ff2feb, 6c02b2ba87c). It changes 107 files (108 paths for `inventory.py`, which counts both sides of the `wallet/ismine.h` → `wallet/types.h` rename), +3,839/−1,070, of which about 2,600 lines are runtime code outside tests and GUI. There are **no vendored-library changes** (dashbls, secp256k1, leveldb, immer, crc32c, minisketch, univalue, depends: none differ). There are also no changes to chainparams, consensus params, `version.h`/`protocol.h` or `configure.ac`. So the deployments, protocol version and user agent are the same as rc.3.

**Totals.** 47 delta rows in three packets. The primary class decides the column:

| Class (brief) | Rows | Of which `gated` | Packet split |
|---|---|---|---|
| Consensus (CON) | 6 | 2 (CE4-01 validity, CE4-03) | CE4-01…05, MN4-08 |
| P2P (P2P, incl. relay/mempool policy) | 8 | 0 (1 `params`: MN4-01, test chains) | CE4-14, 15, 17; MN4-01, 02, 03, 05, 06 |
| RPC / behaviour (RPC, MN, INT) | 33 | 0 | CE4-06…13, 16, 18 (10); MN4-04, 07, 09–13 (7); WAL4-01…16 (16) |
| Build/test-only | 4 commits | — | #7894, #7895, 79480ff2feb, 6c02b2ba87c |
| Irrelevant (no observable effect) | 3 commits | — | #7814 (GUI), #7820 (lock scope), #7824 (session lifetime race) |

Partial not-deltas: the consensus-path refactor of #7909, most of #7537, the enum refactor of #7871, and #7888's cache refactor (§3). Every row except the two gated ones and MN4-01 is `adopt`: it is ungated, and rdashd takes the rc.4 behaviour on every network.

**Findings that change plans.**
1. **#7888 makes ExtNetInfo decoding strict, but only part of it is gated** (CE4-01, CE4-02, CE4-03; §4).
   - A repeated, out-of-order or unknown purpose now fails the decode of the whole ProTx payload (`bad-protx-payload`). It also fails evodb records and SML entries. Validity is V24-gated, because version-3 ProTxs exist only after V24. Before V24 the observable change is the reject string for the same bytes: rc.3 gives `bad-protx-version`, rc.4 `bad-protx-payload`.
   - `-allowprivatenet` no longer reaches masternode address validation. This is ungated and also covers legacy (v1/v2) addresses.
   - The PR calls the dropped `IsTriviallyValid` entry loop unreachable. It is reachable, because unknown-type or invalid-service entries still decode as cleared entries. Dropping it moves `bad-protx-netinfo-bad` later in the check order (gated).
2. **base-sdk #87 is pinned to rc.3's first-wins purpose handling, which rc.4 reverses** (§4.2). #87 is open upstream (head 09d132a, changes requested 10-25). Its "Keep the first group of each purpose" commit must become "reject a repeated, out-of-order or unknown purpose" before merge, or a follow-up must reverse it (decision D1). Its other reads (unknown entry type → invalid entry, invalid domain → invalid entry, invalid legacy address → unset, extended emptiness) stay correct at rc.4. rust-dashcore models rc.3 first-wins in three places: the dash-dml codec, the `dash` crate's SML `ExtNetInfo`, and the dash-dml netinfo tests.
3. **The port 443 Platform HTTPS change is not in this delta.** `IsAllowedPlatformHTTPPort` is #7741 (02bcbc14957). It is in rc.2 and rc.3 already, so BL-393's description needs that correction. Its rc.3 state is not fully in base-sdk: `NIEntry::check` allows 443 for domains only (`pkgs/primitives/src/types/netinfo.rs:257` at #87 head 09d132a), while Core also allows an IP service on 443 in the PLATFORM_HTTPS group (rc.4 `evo/netinfo.cpp:466-468`). That is an rc.3-era gap to fix in #87 (§4.2).
4. **The required Domain vector holds at both tags.** A short or oversized Domain body throws out of `NetInfoEntry::Unserialize` and rejects the payload at that entry, and the code is byte-identical in rc.3 and rc.4 (§4.3). A Domain body that reads fully but is not a valid domain is cleared, and decoding goes on. The vector set needs that contrast case too.
5. **#7909's refactor of the consensus list builder is behaviour-preserving on block connect.** Its per-tx body is identical once indentation is ignored (§1.4). Its observable change is in block templates (CE4-18). #7857 and #7846 add mempool and mining policy for provider transactions (CE4-14…17). Consensus is unchanged.
6. **#7876 closes BL-135** (CE4-04). The O8 state-digest branch already carries it. The oracle pin gets it with this re-pin.
7. **Credit-pool overflow is now defined** (CE4-05): three new reject strings, plus `failed-checkcreditpooldiff`. dash-dml wraps there by design, and the oracle refuses those inputs (EV3 D6). rc.4 lets both go: implement the rc.4 strings and lift the refusal, which widens the differential domain.
8. **Masternode-overlay changes with wire effects** (packet `mn-overlay.md`):
   - Spork handshake: rc.4 sends spork invs at VERACK on test chains and never sends `getsporks` from the sync tick (MN4-01/02). Regtest handshake transcripts change.
   - Governance objects: `vchSig` is limited to 96 bytes, and `governance.dat` moves to v17 (MN4-03/04).
   - ISDLOCK: the cached-recsig shortcut must authenticate (MN4-06).
   - Masternodes no longer InstantSend-sign provider transactions (MN4-09).
   - Three CoinJoin server outcome changes (MN4-11…13).
   - MN4-08 (#7839, IS outpoint index) is classed CON.
9. **Wallet** (packet `wallet.md`):
   - Mnemonic passphrases are capped at 248 bytes, or 256 with the new `-allow-deprecated-long-mnemonics` (WAL4-01…03). key-wallet and the WL1 regtest differential assert rc.3 here.
   - Wallet unlock semantics change (WAL4-05…07).
   - SIGHASH_SINGLE refusal and the PSBT prevout check (WAL4-14/15).
10. **The patch branches move cleanly.** A trial `git merge-tree` onto rc.4 gives one textual conflict each for the oracle shim and the state-digest branch, both in `src/instantsend/instantsend.h` (adjacent annotation lines). func-compat merges clean (§5).

## 1. Method

### 1.1 Inputs

- Dash Core: a `git clone --shared` of `~/workspace/dash` in `/work/scratch/swv24-rc4/dash`, with the two tags fetched from `dashpay/dash`; the shared clone was not modified. Tag to tag only: `git diff 0364ed626e 0e7382df80`, `git show <tag>:<file>`.
- O17's brief (`contrib/node/pin-delta/AUDIT-BRIEF.md`) and its DEC-37 observability standard, read under DEC-82. With one pin, the keyings are:
  - `gated`: a V24/evo_shares deployment check at runtime decides whether the difference can occur;
  - `params`: a per-network value;
  - `adopt`: ungated; rdashd implements the rc.4 behaviour on every network;
  - `none`: internal only.

  DEC-37 decides delta vs not-delta. Process-history state counts, cheap probes with crafted input count, reject strings count, RPC results, help and errors count, and persisted formats users move count. Lock and race fixes count only when a deterministic input sequence changes the outcome.
- Three packets: `ce` (consensus, evo, mempool, mining, RPC/REST; written by the integrator), `mn-overlay` (CoinJoin server, governance, InstantSend, LLMQ, sporks) and `wallet`, each by an Opus auditor. The integrator spot-checked the auditors' highest-impact rows against source: MN4-08's erase semantics, #7858's gating, and #7842's limits. No `decisions.json` was needed: with one pin there are no keying overrides or profile homes.

### 1.2 The single-merge caveat and sub-PR coverage

`inventory.py` attributes files to first-parent commits, so it sees one PR, #7917: 1 reviewable PR, 108 files, 0 unattributed. Its `--report` check passes trivially once #7917 is cited. Sub-PR coverage therefore comes from two scratch scripts:
- `hunks.py` attributes every tag-to-tag hunk (default context) to its sub-PR commit. Added lines are attributed through `git blame 0364ed626e..0e7382df80`; pure deletions through the commits touching the file. 7 deletion-only hunks touched by several sub-PRs are shown in braces and resolved by hand.
- `hunks_md.py` gives every hunk a verdict and fails on an unclassified one.

Result: **349 hunks in 107 files, 0 unclassified** (`SW-V24-rc4-pin-delta/hunks.md`). Each of the 40 commits is cited by number in this report and the packets (37 by PR number, 3 by sha). The coverage check in §8 greps for each one.

**Tooling follow-up.** `inventory.py` should split a backport merge into its `cherry picked from` sub-PRs, so the next rc re-pin gets packets and `--report` coverage per sub-PR. The fix belongs in the SW-V24 tooling (task R15).

### 1.3 Vendored sources and build

`git diff --stat 0364ed626e 0e7382df80 -- src/dashbls src/secp256k1 src/leveldb src/immer src/crc32c src/minisketch src/univalue src/gsl src/tinyformat.h depends` is empty. Build changes are `src/Makefile.am` (the `wallet/ismine.h` → `wallet/types.h` rename, #7871) and two CI scripts (6c02b2ba87c). The oracle archives' toolchain and dependency inputs are therefore unchanged. Only Core sources move.

### 1.4 #7909: the consensus path is unchanged

#7909 splits `CSpecialTxProcessor::RebuildListFromBlock` into `BeginListForBlock` (rc.4 `evo/specialtxman.cpp:309-344`), `ApplyTxToList` (:346-730) and `FinishListForBlock` (:732-805), and composes them again at :807-819. Checks:
1. rc.3's per-tx loop body (rc.3 :351-725) and rc.4's `ApplyTxToList` body, extracted and diffed without indentation, differ only in the function signature, `continue` → `return true`, and the final `return true`.
2. The begin phase (confirmations, `DecreaseScores`) and the finish phase (the collateral-spend pass after all txs, payee bookkeeping, the `nConsecutivePayments` reset) are verbatim. `payee` and `isMNRewardReallocation` are still computed from `prevList` and `pindexPrev`.
3. Outside 309-856 the file is identical: rc.3 lines 1-308 and 797-end equal rc.4 lines 1-308 and 857-end.

So block connect builds the same list with the same reject strings. The behaviour change is in `CreateNewBlock`, which uses the phases through `CMNListBuilder` (CE4-18).

## 2. Delta rows

The full rows, each with both tags' `path:line` evidence and the rdashd impact, are in the packets. The table lists each row's rc.4 hunks and consumers.

### 2.1 Consensus, evo, mempool, mining, RPC (`ce.md`)

| id | Behaviour (rc.3 → rc.4) | Class | Keying | PR | rc.4 hunks | Consumers |
|---|---|---|---|---|---|---|
| CE4-01 | ExtNetInfo purposes must be known and strictly ascending, so a repeat is rejected. Otherwise the decode throws, which gives `bad-protx-payload`, a failed evodb record and a failed SML entry | CON | gated (validity); adopt (the pre-V24 reject string) | #7888 | evo/netinfo.h:422-437 | EV1, EV2a/EV2b (dash-dml codec), EV4-serve, BSX-PR (#87), BS4b, BS8, `dash` crate SML |
| CE4-02 | RFC1918 is never routable for MN addresses, whatever `-allowprivatenet` says (legacy and extended) | CON | adopt | #7888 | evo/netinfo.cpp:291-297, 453-459 | EV1, EV2b (dash-dml netinfo rules) |
| CE4-03 | An invalid entry is rejected in `CheckNetInfo` instead of `IsTriviallyValid`, so the reject order changes | CON | gated | #7888 | evo/providertx.cpp:319-324, 425-430 | EV1 (reject-order census) |
| CE4-04 | A state diff records `pubKeyOperator` on a scheme change with equal bytes. | CON | adopt | #7876 | evo/dmnstate.h:259-270 | EV2a (diff codec), EV4-serve (evodb importer), O8, BL-135 |
| CE4-05 | Checked credit-pool arithmetic. New `failed-creditpool-{lock,unlock}-overflow`; `unlock-too-much` on overflow; a `GetTotalLocked` throw becomes `failed-checkcreditpooldiff` | CON | adopt | #7816 | evo/creditpool.cpp:17-30, 43-53, 294-309, 313-323, 333-340, 344-350; evo/creditpool.h:100-108 | EV3, O2a-GV oracle refusal |
| CE4-06 | `insert_or_assign` of new lists. `protx info`, `protx listdiff` and `masternode payments` refuse blocks outside the active chain (-8) | RPC+CON | adopt | #7867 | evo/deterministicmns.cpp:726-732, 750-756; rpc/evo.cpp:1802-1811, 1936-1944; rpc/masternode.cpp:408-417 | RP (evo/masternode RPCs), EV2b list cache |
| CE4-07 | `masternode payments` stops at genesis | RPC | adopt | #7866 | rpc/masternode.cpp:421-428 | RP |
| CE4-08 | `masternode payments`: -1 / -32603 errors for missing inputs | RPC | adopt | #7810 | rpc/masternode.cpp:447-459 | RP |
| CE4-09 | `masternode status` help: `service` is a required STR | RPC | adopt | #7851 | rpc/masternode.cpp:181-187 | AR2 |
| CE4-10 | `getchainlockbyheight` help text | RPC | adopt | #7875 | rpc/quorums.cpp:1405-1416 | AR2 |
| CE4-11 | REST `deploymentinfo` returns HTTP 500 with the message instead of an escaping exception | RPC | adopt | #7818 | rest.cpp:665-680 | REST (RP) |
| CE4-12 | evodb repair: a missing snapshot is a repair error (fatal at startup, reported by RPC) | RPC+INT | adopt | #7792 | evo/deterministicmns.cpp:1229-1237, 1255-1261, 1263-1269 | RP (evodb RPCs) or N/A (D8) |
| CE4-13 | `protx shared_combine`: funding-shape checks, no partial path, help | RPC | adopt (reachable after evo_shares) | #7881 | evo/providertx_service.cpp:4-10, 24-30, 730-798, 1297-1306; rpc/evo.cpp:1469-1476 | WL5, AR2 |
| CE4-14 | Mempool: owner key IDs and Platform node IDs share one conflict namespace | P2P | adopt | #7846 | txmempool.cpp:996-1008, 1023-1035, 1669-1680, 1692-1708, 1741-1749 | MP1a |
| CE4-15 | Mempool: `protx-dup` for an unorderable ProUpServTx | P2P | adopt | #7857 | validation.cpp:1108-1114; txmempool.cpp:1113-1183; txmempool.h:542-560 | MP1a |
| CE4-16 | Templates put a masternode's pending ProUpServTx before its key change and never after it | RPC (mining) +MN | adopt | #7857 | node/miner.cpp:524-539, 640-689, 693-730, 799-808 | MN1, MN0 |
| CE4-17 | A reorg moves the MN-list tip before re-adding disconnected txs | P2P | adopt | #7857 | validation.cpp:425-436; evo/specialtxman.cpp:804-859 (`UpdatedMNListTip`) | CH3b, MP1a |
| CE4-18 | The template builds the MN list incrementally and skips conflicting provider packages | RPC (mining) | adopt | #7909 | node/miner.cpp:216-241, 250-257, 298-304, 508-515, 752-785, 799-808 | MN1, EV2b (phase API) |

### 2.2 Masternode overlay (`mn-overlay.md`)

| id | Behaviour (rc.3 → rc.4) | Class | Keying | PR | rc.4 hunks | Consumers |
|---|---|---|---|---|---|---|
| MN4-01 | One spork `inv` per active spork at VERACK, test chains only | P2P | params (`IsTestChain`) | a56ed47c98a | net_processing.cpp:4221-4235 | SP0, P2a-core, O13a, P2P vectors |
| MN4-02 | No `getsporks` from the sync tick on any network (the handler stays) | P2P+INT | adopt | a56ed47c98a | node/sync_manager.cpp:201-206 | SP0, P2a-core sim transcripts, IM1b |
| MN4-03 | Governance object `vchSig` ≤ 96 bytes at decode. Oversized: log-only drop, no score, request not consumed | P2P | adopt | #7831 | governance/common.h:34-41, 82-88 | GV1a/GV1b, node-fuzz `p2p_decode_model` GOVOBJ row (models/p2p_decode_matrix.rs) |
| MN4-04 | `governance.dat` v17 (invalid-vote field gone). A v16 file reads as a version mismatch | INT+P2P | adopt | #7831 | governance/governance.cpp:28-34; governance/governance.h:202-209, 222-228 | IM1b (dash-coreimport governance.rs) |
| MN4-05 | A failed trigger advances the masternode rate buffer | P2P+MN | adopt | #7521 | governance/governance.cpp:345-369, 712-734 | GV1a/GV1b |
| MN4-06 | ISDLOCK: the cached-recsig shortcut needs the same quorum and the same signature bytes | P2P+RPC | adopt | #7841 | instantsend/net_instantsend.cpp:127-132, 150-162 | IS rows (LK1b), LQ3, SG1a |
| MN4-07 | IS tracking is released on mempool removal (no retry signing for evicted txs) | MN | adopt | #7839 | instantsend/instantsend.cpp:134-144, 206-223 | LK1b/LK1c, MP1a |
| MN4-08 | IS outpoint index: owner-checked erase plus release at eviction. | CON+P2P | adopt | #7839 | instantsend/instantsend.cpp:239-254, 134-144 | LK1b, ChainHelper IS back end |
| MN4-09 | Masternodes don't IS-sign provider txs (two FT-asserted log lines) | MN | adopt | #7858 (+ #7895 tests) | instantsend/signing.cpp:33-57, 192-204, 450-457 | LK1c |
| MN4-10 | A malformed lazy sig share puts its source in `badSources` (the whole batch is dropped) | MN | adopt | #7822 | llmq/net_signing.cpp:417-423 | SG1b |
| MN4-11 | CoinJoin: `DSCOMPLETE(ERR_INVALID_TX)` is sent when the final tx fails ATMP | MN+P2P | adopt | #7537 | coinjoin/server.cpp:457-490 | CJ0/CJ1 (dash-coinjoin server.rs) |
| MN4-12 | CoinJoin: completion reaches every connected participant (no REJECTED storm, no early stop) | MN+P2P | adopt | #7537 | coinjoin/server.cpp:505-518, 1365-1385 | CJ0/CJ1 |
| MN4-13 | CoinJoin: a timed-out session that can still advance is not reset | MN | adopt | #7537 | coinjoin/server.cpp:693-753 | CJ0/CJ1 |

### 2.3 Wallet (`wallet.md`)

| id | Behaviour (rc.3 → rc.4) | Class | PR | rc.4 hunks | Consumers |
|---|---|---|---|---|---|
| WAL4-01 | `upgradetohd` passphrase cap 248 (256 with the flag), RPC -8 text, for both wallet kinds | RPC | #7842 | wallet/rpc/wallet.cpp:381-407; wallet/bip39.h:26-44 | WL1 (key-wallet), WL2b |
| WAL4-02 | `createwallet` refuses an over-cap `-mnemonicpassphrase` before touching the database (RPC -4) | RPC | #7842 | wallet/wallet.cpp:414-430 | WL2a/WL2b |
| WAL4-03 | New option `-allow-deprecated-long-mnemonics`, hidden in no-wallet builds | RPC (config) | #7842 | wallet/init.cpp:103-116; dummywallet.cpp:61-67 | dash-args (AR1), WL1 |
| WAL4-04 | Invalid-mnemonic errors no longer echo the words | RPC | #7828 | wallet/hdchain.cpp:69-75; wallet/wallet.cpp:4396-4402 | WL2b |
| WAL4-05 | `upgradetohd` relocks on every exit | RPC | #7821 | wallet/rpc/wallet.cpp:398-403, 440-446 | WL2b |
| WAL4-06 | `walletpassphrase` checks the passphrase when the wallet is already unlocked | RPC | #7838 | wallet/wallet.cpp:4171-4192 | WL2a/WL2b |
| WAL4-07 | `walletpassphrasechange` keeps a mixing-only wallet mixing-only | RPC | #7829 | wallet/wallet.cpp:590-634 | WL2b, WL6 |
| WAL4-08 | `restorewallet` removes only a directory it created; the exception path gives -4 | RPC+INT | #7817 | wallet/wallet.cpp:514-545 | WL2b, WL3 |
| WAL4-09 | `importelectrumwallet`: a negative index gives -8 before any import; help text | RPC | #7834 | wallet/rpc/backup.cpp:651-657, 681-691, 811-816 | WL3, AR2 |
| WAL4-10 | `importelectrumwallet`: exact-length JSON read, embedded NUL → -3, read failure → -8 | RPC | #7834 | wallet/rpc/backup.cpp:696-714, 759-771 | WL3 |
| WAL4-11 | `listlabels` purpose must be a known purpose (-8) | RPC | #7871 | wallet/rpc/addresses.cpp:666-680; wallet/wallet.cpp:2746-2759 | WL2b |
| WAL4-12 | `getaddressesbylabel` reports a nonstandard stored purpose as `unknown` | RPC | #7871 | wallet/rpc/addresses.cpp:622-628; wallet/walletdb.cpp:404-416 | WL2b, WL3 |
| WAL4-13 | `migratewallet` drops nonstandard purpose strings | INT+RPC | #7871 | wallet/wallet.cpp:4796-4860 | WL3 |
| WAL4-14 | The signer refuses SIGHASH_SINGLE without a matching output (PSBT and raw) | RPC | #7827 | script/sign.cpp:37-47, 405-410, 440-446 | WL4 |
| WAL4-15 | PSBT decode rejects a `non_witness_utxo` without the referenced output (-22) | RPC | #7836 | psbt.h:822-834 | WL4, RP (raw-tx PSBT RPCs) |
| WAL4-16 | CoinJoin pending-observation locks are persisted atomically, with retry | INT | #7782 | coinjoin/client.cpp:678-741, 787-800, 812-817, 873-887; coinjoin/client.h:217-225 | WL6 |

All WAL4 rows are `adopt` (wallet code has no deployment gate).

## 3. Not deltas

| PR / commit | Class | Reason |
|---|---|---|
| #7814 (0e75c226e4c) | irrelevant | GUI only: HTML escaping in the send dialog |
| #7820 (a1f37ea4497) | irrelevant | `cs_main` held across `IsValidInOuts`. Same checks in the same order; only a cross-thread race changes |
| #7824 (cc5716fb59f) | irrelevant | DKG session lifetime (`shared_ptr` under `cs_session`). Serial outcomes identical; DKG getdata answers "not found" in both after a failed `Init` |
| #7894 (cdaea5ffe65) | test-only | feature_pruning.py |
| #7895 (56337615d9c) | test-only | p2p_instantsend.py, release-note wording (evidence for MN4-09) |
| 79480ff2feb | test-only | wallet_upgradetohd.py follow-up to #7821 |
| 6c02b2ba87c | build-only | CI `DEPENDS_ARCH` override |
| #7909, consensus part | not delta | §1.4 |
| #7888, refactor part | not delta | the `m_all_entries` cache was removed. Decoded objects list entries in purpose order at both tags, and the `Validate()` restructure gives the same status (`ce.md` §2) |
| #7537, remainder | not delta | race-only hunks: TRY_LOCK single-flight, revalidation after decode, the collateral-reservation refactor (`mn-overlay.md` §2) |
| #7871, remainder | not delta | `AddressPurpose` enum: the same records are written; GUI interfaces (`wallet.md` §2) |
| #7839/#7858, timing part | not delta | `timingsTxSeen` feeds StatsD only |
| #7521, relay split | not delta | `ScheduleTriggerRelay` is net-same |
| #7842 / #7881, GUI and comments | not delta | `wallet.md` §2, `ce.md` §2 |

## 4. #7888 in detail

### 4.1 What is gated

| Effect | Before V24 (mainnet now; testnet as of the 10-20 EV3 walk) | After V24 |
|---|---|---|
| A ProRegTx/ProUpServTx v3 with a repeated, descending or unknown purpose | rc.3: payload decodes, then `bad-protx-version` (`specialtxman.cpp:1173`). rc.4: `bad-protx-payload` (:1168). Both TX_CONSENSUS (+100) | rc.3 **accepts** a repeat or reordering if the surviving map validates (first group kept), and rejects an unknown purpose as `bad-protx-netinfo-bad`. rc.4: `bad-protx-payload` for all three |
| evodb state/diff record or SML entry v3 with such a map | only what an rc.3 node stored (canonical, since rc.3 writes the map back) | rc.4 throws on read. Only constructed regtest/devnet chains can hold such data, and they fail to reindex under rc.4 as under Core |
| RFC1918 address with `-allowprivatenet=1`, mainnet/testnet | rc.3: accepted (legacy v1/v2 ProTxs today) | rc.4: `bad-protx-netinfo-addr-unroutable` everywhere |
| An entry that is cleared (unknown type, invalid service) plus a second fault earlier in the check order | — (v3 refused by version) | the reject string changes (CE4-03) |

Canonical history is unaffected:
- **Mainnet:** V24 is `NEVER_ACTIVE` at rc.4 (`chainparams.cpp:213`).
- **Testnet:** V24 starts 2026-09-24 with EHF signalling, and was inactive at the 10-20 walk.
- Honest nodes never accepted an RFC1918 masternode address.

The re-pin checks the testnet state again (R4): if V24 activates on testnet before the network runs rc.4, rc.3 miners could mine a repeated-purpose ProTx that rc.4 rejects.

### 4.2 base-sdk #87 and what rc.4 adds

#87 ("primitives%fix!: read network info as the reference implementation does", head 09d132a, 7 commits, pinned to rc.3 0364ed626e) has six rows. Their status at rc.4:

| #87 row | rc.4 |
|---|---|
| Keep the first group of each purpose (map semantics, purpose order, unknown purposes kept) | **Reversed.** A repeated, out-of-order or unknown purpose is a decode error (`evo/netinfo.h:422-437`) |
| Read unknown entry types as invalid entries | unchanged (`netinfo.h:243-263`) |
| Read invalid domains as invalid entries | unchanged. A short or oversized Domain body is a decode error at both tags (§4.3) |
| Read invalid legacy addresses as the unset service | unchanged (`netinfo.h:342-347`) |
| Extended emptiness (an empty group is not empty) | unchanged |
| `check` treats `NIEntry::Invalid` as malformed | unchanged in effect (rc.4's `Validate` returns `Malformed` for an entry that is neither service nor domain, `netinfo.cpp:604-621`) |

What rc.4 adds for base-sdk:
- `NetInfoV2::decode` must fail on a purpose that is unknown or not greater than the previous one. `check`'s duplicate-purpose and unknown-purpose branches then only apply to hand-built values.
- The corpus is regenerated by the same programs at rc.4 (0e7382df80), with the reference links moved to rc.4.
- Port 443 is rc.3 (#7741), not rc.4, but #87 implements only half of it: the domain branch of `NIEntry::check` allows 443 (`types/netinfo.rs:257` at 09d132a), and the service branch (:251) rejects it. Core allows 443 for a service in the PLATFORM_HTTPS group (`evo/netinfo.cpp:466-468`). The purpose-aware exception and a vector (IPv4 service on 443 in PLATFORM_HTTPS: valid; the same in CORE_P2P: `BadPort`) belong in #87.
- Routability (`RequireRoutableExternalIP`, RFC1918) is network-dependent and stays node-side (dash-dml).

### 4.3 The Domain-entry vector (DEC-456 (2))

Wire form, both tags (`netinfo.h:176-181, 243-263`; `serialize.h:777-781`): type byte `0x02`, then a CompactSize length, then the name bytes, then a big-endian u16 port. A length above `DOMAIN_MAX_LEN` = 253 throws "String length limit exceeded" before any allocation. A short read of the name or the port throws. The exception propagates out of the payload.

Vectors (ProRegTx v3, one purpose group CORE_P2P = 0 holding two entries; payload otherwise valid). They're for the base-sdk corpus, dash-dml codec tests and an oracle-verdict test:

| # | First entry | Second entry | Expected (rc.3 = rc.4) |
|---|---|---|---|
| D1 | Domain, length 254 (+254 bytes + port) | a valid service | decode error at the first entry (`bad-protx-payload` for the tx). The second entry is never read |
| D2 | Domain, length 0xFD 0xFF 0xFF (65,535) and 3 bytes left | — | decode error at the first entry, with no allocation of the claimed length |
| D3 | Domain, length 20, only 10 name bytes before the payload ends | — | decode error at the first entry |
| D4 | Domain, length 11 "example.com", port truncated to 1 byte | — | decode error at the first entry |
| D5 (contrast) | Domain, length 4 "a..b" (fully read; empty label, so `ValidateDomain` returns `BadLabelLen`) | a valid service | **decodes**: the first entry is cleared (invalid) and the second is read. Later `bad-protx-netinfo-bad` (V24 active) or `bad-protx-version` (pre-V24) |
| D5b (control) | Domain, length 3 "a.b" (a valid domain: `DOMAIN_MIN_LEN` = 3, `netinfo.cpp:24, 90-113`) | a valid service | decodes with both entries kept. In CORE_P2P a domain is `BadInput` (`netinfo.cpp:610-615`), so `bad-protx-netinfo-entry` (V24 active) or `bad-protx-version` (pre-V24, the version check comes first, `specialtxman.cpp:1158-1175`) |
| D6 (contrast) | unknown type `0x07` | a valid service | decodes: the first entry is invalid, and reading goes on at the next byte |

In rust-dashcore the error must carry the offset of the first entry, and the decoder must not attempt the second entry. The oracle test asserts Core's verdict for D1–D6 and D5b through the rc.4 shim (`oracle_netinfo` decode, `dash-oracle-sys/tests/netinfo.rs`). One more vector pair covers CE4-01 for D1's group: a repeated CORE_P2P group and a descending pair (PLATFORM_P2P = 1 before CORE_P2P = 0). Both are decode errors at rc.4; at rc.3 the first decodes first-wins and the second decodes in map order.

## 5. Impact map

"Recertify" names the `node-fuzz/targets.toml` legs (all `pins = ["v24.0"]`) whose compared behaviour changes. Their certificates bind to the tree revision and its oracle build (fuzzctl `read_certificate`), so the rc.3-era certificates do not cover rc.4. For legs whose compared code doesn't change, see decision D2.

| # | Artifact | What changes at rc.4 | Rows | Evidence to produce | Legs to recertify |
|---|---|---|---|---|---|
| 1 | **Oracle shim**: Core `node/bl211-oracle-v24.0` (77dbec8242, 150 commits on rc.3, ABI 28); `dash-oracle-sys/pins.txt` | Rebase onto rc.4. Trial merge: one conflict, `instantsend.h` (RemoveConflictingLock's return type next to #7839's annotation). Auto-merges in specialtxman.cpp (hooks are outside #7909's moved body), deterministicmns.cpp, net_processing.cpp, rpc/evo.cpp and validation.cpp; review each. Semantics to re-check: netinfo entry points (purpose strictness, RFC1918), the credit-pool step (the overflow refusal can go), DML block effects through the #7909 phases, IS hooks (#7839), governance object decode (vchSig ≤ 96), CoinJoin rules (#7537), and O2c's p2p decode overlay (the govobj predicate). Bump the ABI if any entry point's verdict domain changes | CE4-01…05, MN4-03, MN4-06…13 | new branch (never rewrite the old one); plain + fuzz archives byte-identical across two cache roots (`check-oracle-repro.sh`); `--update-pins`; archives installed under `/work/cache/oracle/<new-sha>/` before scratch cleanup; gate `oracle-diff` green | all `l1` legs re-run on the new archive (D2). Changed domain: `diff_payload_1`, `diff_payload_2`, `diff_payload_decode`, `diff_contextual_check_tx`, `diff_credit_pool`, `diff_dml_apply`, `diff_dml_block`, `diff_coinjoin` |
| 2 | **State-digest patch**: Core `node/state-digest-v24.0` (cc11b5d525, 18 commits on rc.3) | Rebase onto rc.4. 9d287cbd2f and cc11b5d525 (#7876) drop as already upstream; check with `git range-diff`. Same `instantsend.h` conflict. The taps (`TapEhf`, `QueryBestChainLock`, `is_rm`) sit outside the changed bodies | CE4-04 | range-diff; `make check` incl. `evo_dip3_activation_tests`, `feature_statedigest`; O8 r6's differential replay (`-loadblock` of every stored blocks.dat through cc11b5d525 vs the rc.4 build) identical on every FT node; P-1 testnet node binary swap, no reindex (it already runs #7876) | `diff_chain` (O2b merges the state-digest branch; its BL-135 note at O2b-diff-chain.md:215 resolves) |
| 3 | **func-compat**: Core `node/func-compat-v24.0` (916be21115, 13 commits) | merges clean (test_framework.py auto-merges with #7858's `bury_tx` change). rc.4 changes 14 FT files; none of them is in the passlist (feature_logging, feature_settings, `rpc_bind --ipv4`, feature_filelock) or the expected-failures list. The framework change affects every DashTestFramework test. New asserted log lines in p2p_instantsend.py (MN4-09) | MN4-09, all FT-visible rows | O11 tier-1 matrix on rc.4 (pass/fail/expected-fail/skip equal to the rc.3 run, or explained); `contrib/node/asserted-logs.py` regenerated | — |
| 4 | **Conformance goldens** (conformance/, fixtures `fingerprint-v24`, render, verbose, `dash/tests/data` vectors made by rc.3) | RPC renderings of netInfo and txs don't change for canonical data. Regtest and testnet handshakes gain spork invs after VERACK (MN4-01), so the fingerprint fixtures captured on regtest change. `dash/tests/core_v24_vectors.rs` checks `ExtNetInfo::canonical` (first-wins) against Core's `addresses`; at rc.4 a repeat is a decode error | MN4-01, CE4-01 | regenerate with `contrib/node/regen-core-vectors.sh` / `sync-core-vectors.sh` at rc.4. The diff must equal the predicted rows (no other byte change); PROVENANCE pin lines updated | — |
| 5 | **Oracle archives** `/work/cache/oracle` | a new `<shim-sha>/{plain,fuzz}` for the rc.4 pin | — | sha256 against pins.txt; installed into a dot-tmp dir, then renamed into place (PROCESS "Oracle pin moves"). The rc.3 directories stay until no live worktree pins them | — |
| 6 | **O8 StateDigest goldens** (test-vectors/statedigest: 244 files cite rc.3; mainnet golden 0..1,028,159, BL-308; testnet P-1) | no consensus change on canonical history, and #7876 was already in the O8 build. Goldens are re-stored only if item 2's replay differs | CE4-04 | replay identical → only PROVENANCE pin lines change; otherwise re-record and explain each changed digest | `diff_chain` |
| 7 | **base-sdk decode corpora** (#87 `netinfo.json5`; BS4b decode-domain evidence; BS8 overlay; BSX catalogue) | §4.2: first-wins → strict purposes; corpus programs re-run at rc.4; the D1–D6, D5b and purpose vectors (§4.3); the BSX entry on ExtNetInfo map leniency updated. Port 443 is already rc.3: verify it, no change | CE4-01 | `just comb` on every commit; corpus regenerated by programs at rc.4; fork `node/base-sdk` merge, then the rdc re-pin with `bump-base-sdk.sh`, `check-base-sdk-pins.sh` and the DEC-239 fork push | `diff_bsdk_tx_decode`, `diff_payload_1`, `diff_payload_2`, `diff_payload_decode`, `diff_p2p_smlentry`, `diff_p2p_mnlistdiff` (BS4b/BS8 certify windows) |
| 8 | **dash-evo / dash-dml NetInfo and the `dash` crate** | dash-dml codec `net_info` (codec.rs:483-518, first-wins); netinfo rules (`allow_private_net` into `is_routable`, mod.rs:120-122; entry.rs:235-267); the reject order (specialtx); the diff field mask (CE4-04); the credit pool (CE4-05, creditpool.rs:18-19 wraps); the builder phases (CE4-18). Tests that assert rc.3: dash-dml/src/netinfo/tests.rs:533-541; dash-oracle-sys/tests/netinfo.rs; dash-dml/tests/consumers/ev2b.rs:133. The `dash` crate `sml/masternode_list_entry/net_info.rs` documents and implements first-wins (`ExtNetInfo::canonical`) on the node branch; compare it with upstream `dev` (#797) | CE4-01…05, CE4-18 | L0 vectors (§4.3, the CE4-02 RFC1918 × `allow_private_net` table, the CE4-03 operator-reward precedence, the CE4-05 overflow strings); L1 differentials through the rc.4 oracle; `check-private-tests.sh` receipts (PROCESS, private tests) | `netinfo_validate`, `netinfo_add_entry`, `dml_codec_roundtrip`, `diff_mnlist_codec`, `diff_mnlist_diff`, `diff_mnlist_order`, `diff_dml_apply`, `diff_dml_block`, `diff_credit_pool`, `diff_contextual_check_tx` |
| 9 | **rdashd RPC and option tables** (AR2 `dash-rpc-spec/src/generated.rs`, `CORE_VERSION = "v24.0.0-rc.3"`; AR1 `dash-args/src/options/generated.rs` from da567d7dba; `dash-args/tests/data/core-v24`) | one new option (`-allow-deprecated-long-mnemonics`, wallet HD category, hidden without wallet). Help changes: `masternode status` (result), `getchainlockbyheight`, `protx shared_combine`, `importelectrumwallet` (arg 2). No method added or removed | CE4-09, 10, 13; WAL4-03, 09 | the generators re-run at rc.4; the diff is exactly those 4 help strings and 1 option (DEC-469); rdash-cli unchanged | — |
| 10 | **P2P vectors and transcripts** (EV4-serve rc.3 vectors; `contrib/node/{p2a,p2b,p3,p4a}-transcripts`, `sp0-transcripts`; `dashd-p2p/src/sim/transcript.rs`; O13a handshake transcripts) | regtest/testnet VERACK spork invs (MN4-01); no sync-tick `getsporks` (MN4-02). mnlistdiff/QRINFO serving is unchanged for canonical data. New vectors: a govobj with a 97-byte `vchSig` (log-only, request kept, MN4-03); an ISDLOCK with a cached recsig and foreign sig bytes (+20, MN4-06); `DSCOMPLETE(ERR_INVALID_TX)` (MN4-11) | MN4-01…03, 06, 11 | re-capture on rc.4 regtest. The only change is the predicted spork invs/getsporks; EV4-serve's 840 vectors re-verified unchanged | `diff_p2p_multi_handshake`, `diff_p2p_multi_relay`, `diff_p2p_spork`, `p2p_decode_model` (GOVOBJ row), `diff_p2p_isdlock` (if its model reaches the verification batch) |
| 11 | **PF0 bench inputs** | no bench data comes from the Core pin: dash-dml/benches/data, dashd-chainstate/benches/data and dash-spv bridge headers are historical mainnet extracts with no ExtNetInfo (pre-V24). Code changes in bench packages (dash-dml) are judged normally. A base-sdk re-pin changes `Cargo.lock`, a bench input → `--approve-bench-change` and an immediate re-anchor | — | PF0 PASS per merge; re-anchor after the base-sdk re-pin (PROCESS "After a merge that changes bench inputs") | — |
| 12 | **Gate and live oracles** (`ST2_DASHD` `DASHVERSION=24.0.0-rc.3` in PROCESS and gate.sh; `contrib/setup-dashd.py`; BS2 mainnet/testnet oracles in /work/chains) | move to the rc.4 release binaries (published, 21 assets). BL-135: oracles synced with rc.3 may hold state diffs that rc.4 would have recorded differently | CE4-04 | binaries' sha256 against the release; an rc.4 `evodb verify` on a reflinked **copy** of each oracle datadir (never the live one); oracle restart only after a clean verify, or a `-reindex` ruling (D4) | — |
| 13 | **Core-format importers** (`dash-coreimport/src/netstate/governance.rs`, v16 only; netfulfilled `spork-sync` now inert; evodb importer) | `governance.dat` v17 (MN4-04); rc.3 evodb datadirs and CE4-04 | MN4-04, CE4-04 | importer reads v17 and treats v16 as a version mismatch (empty store), as Core does; fixture round trips | `coreimport_governance_dat`, `coreimport_netfulfilled_dat` |
| 14 | **dash-coinjoin, dashd-masternode, key-wallet** (code that exists today and follows rc.3) | dash-coinjoin/src/server.rs:808-820, 915-930, 991-1004 (MN4-11…13); the SP0 VERACK push and sync tick (MN4-01/02); key-wallet passphrase limits and text (key-wallet/src/mnemonic/dash_core.rs:27-31, compat.rs:59) and the WL1 differential (dash-spv/tests/wallet_mnemonic_differential.rs:57, 260-265) | MN4-01, 02, 11…13; WAL4-01…03 | unit and differential tests against rc.4 dashd | `diff_coinjoin` |
| 15 | **Citations** (611 rust-dashcore files cite rc.3) | most cite lines that are unchanged at rc.4 | — | a checker (R15) lists every `rc.3`-cited `path:line` range that rc.4 changes; only those cites and the pin constants are rewritten (D3) | — |

## 6. Re-pin task list

Order: R0 → (R1 ∥ R2 ∥ R3 ∥ R4) → (R5 … R13, in parallel where they don't share a crate) → R18 and R19 → R16 → R17. R14 and R15 are planning and tooling and run independently. R18 waits for the IS back end; until it exists, R16/R17 certify everything else and R18's scenario is a recorded open dependency of the final gate. Risk classes follow PLAN §4 (CON, STATE, P2P, OTH, +SEC). Sizes are PLAN sizes (S ≤ 1 wk, M 1–2 wk, L > 2 wk).

| ID | Task | Owner (STATUS row) | Deps | Size | Risk | Evidence |
|---|---|---|---|---|---|---|
| R0 | Review and approve this delta and §7's decisions; correct BL-393 (port 443 is rc.2/rc.3) | SW-V24 (manager) | — | S | — | Sol review of this report; DEC entry |
| R1 | **Oracle shim to rc.4.** New branch `node/oracle-v24.0-rc4` from rc.4 with the BL-211 bundle replayed. Resolve `instantsend.h`; review every auto-merged hook against #7909/#7839/#7831/#7537; lift the credit-pool overflow refusal; ABI bump if a verdict domain moves; rebuild, repro, pins, cache install | O2a (oracle shim; BL-211 bundle owner) | R0 | M | CON | §5 item 1: repro pair, pins.txt, cache dirs, `oracle-diff` green on a tree where only the pin moved (expected failures are exactly the CE4/MN4 rows the node still implements at rc.3) |
| R2 | **State-digest patch to rc.4**: rebase, drop the #7876 cherry-picks, rebuild, differential replay, P-1 binary swap. Closes BL-135 with R1 | O8 | R0 | S | STATE | §5 items 2, 6 |
| R3 | **func-compat to rc.4**: rebase, O11 tier-1 matrix, asserted-log list | O11 | R0 | S | OTH | §5 item 3 |
| R4 | **Gate and oracle binaries**: `ST2_DASHD` → 24.0.0-rc.4 in PROCESS and gate.sh; setup-dashd.py; BS2 oracles to rc.4 after `evodb verify` on copies (D4); confirm testnet V24 status | SW-OPS (BS2) | R0 | S | OPS | §5 item 12; gate green with the rc.4 `ST2_DASHD` |
| R5 | **dash-dml / dash-evo consensus at rc.4** (one implementer task): CE4-01 strict purposes in every record reader; CE4-02 RFC1918; CE4-03 reject order; CE4-04 diff mask; CE4-05 checked credit pool and strings; flip the rc.3-asserting tests | EV1 (D) + EV2b + EV3 (one task "EV-RC4", impl O-h; rev Sol→O-h) | R1 | M | CON | §5 item 8; L0 vectors incl. §4.3; L1 smoke 0 divergences; farm and certify on the legs listed; DEC-32 receipts |
| R6 | **base-sdk rc.4 NetInfo**: retarget #87 to rc.4, or a follow-up (D1); strict purposes; corpus re-run at rc.4; D1–D6 and D5b plus the purpose vectors; then the fork merge and the rdc re-pin | BSX-PR (manager-opened; Opus-h fixer) | R0 (and R1 for the oracle corroboration) | M | CON | §5 item 7; `just comb` on every commit; the upstream review thread; `check-base-sdk-pins.sh`; PF0 re-anchor after the re-pin |
| R7 | **Domain first-entry vector** (DEC-456 (2)): D1–D6 and D5b in the base-sdk corpus (R6), the dash-dml codec tests (R5) and an oracle-verdict test (R1) | EV1 with BSX-PR | R1, R5, R6 | S | CON | all three agree on D1–D6 and D5b. The offset of the first entry is asserted in rust-dashcore |
| R8 | **Decode-domain recertification**: payload types 1/2 v3, the SML entry v3, mnlistdiff | BS4b (+ BS8 legs) | R1, R6 | S | CON | certify windows on rc.4 for the §5 item 7 legs |
| R9 | **RPC/option tables**: regenerate dash-rpc-spec and dash-args at rc.4 | AR2/AR1 (ND1 row for dash-args) | R0 | S | OTH | §5 item 9: the diff equals the predicted 4 help strings + 1 option (DEC-469) |
| R10 | **Sporks and handshake**: VERACK spork invs on test chains (MN4-01), drop the sync-tick `getsporks` (MN4-02), sim transcripts, sp0/p2a/O13a re-captures; fix SP0's PLAN text | SP0 (reopen) + P2a-core | R4 | S | P2P | §5 item 10; transcripts equal rc.4 dashd |
| R11 | **Governance**: `vchSig` ≤ 96 (GV1a codec + node-fuzz GOVOBJ row), rate buffer order (MN4-05), `governance.dat` v17 importer (MN4-04) | GV1a (+ IM1b for the importer) | R1 | S | P2P | MN4-03…05 vectors; `coreimport_governance_dat` smoke and certify |
| R12 | **CoinJoin server**: MN4-11…13 in dash-coinjoin | CJ0 | R1 | S | MN | `diff_coinjoin` on rc.4; the coinjoin_inouts_tests table as L0 vectors |
| R13 | **Wallet now**: key-wallet passphrase cap and text (WAL4-01…03); WL1 differential re-baselined on an rc.4 dashd | WL1 | R4, R9 | S | OTH +SEC | wallet_mnemonic_differential green against rc.4 |
| R14 | **PLAN annotations** for rows not yet built: MP1a (CE4-14, 15, 17; MN4-07), MN1 (CE4-16, 18), CH3b (CE4-17), the IS rows (MN4-06…09), SG1b (MN4-10), the RP rows (CE4-06…12; WAL4-15), WL2a/WL2b/WL3/WL4/WL5/WL6 (WAL4-04…16, CE4-13), GV1b (MN4-05) | SW-V24 (manager) | R0 | S | — | PLAN rows cite their CE4/MN4/WAL4 ids |
| R15 | **Tooling**: `inventory.py` splits backport merges into sub-PRs; the citation checker for §5 item 15; the weekly core-watch restarts (`research/core-watch-<date>.md`) | SW-V24 | R0 | S | — | the checker's report; the first core-watch file |
| R16 | **Recertification and PF0**: certify the §5 legs after R1/R5/R6 merge; carry the others forward per D2; PF0 re-anchor after the base-sdk re-pin | O3 (SW-FUZZ) + PF0 | R1, R5, R6, R8 | M (CPU-bound) | — | certificates on the rc.4 tree; a STATUS line per leg; the PF0 anchor justification |
| R17 | **Full replay**: O9a/diff_chain replay of the mainnet and testnet windows on the rc.4 oracle and state-digest builds; Platform P-1 fixtures re-recorded only if R2's replay differs | O9a + O8 | R1, R2, R5 | M | CON | 0 mismatches over the recorded windows; a ledger line |
| R18 | **MN4-08 IS outpoint index** at rc.4 semantics when the IS back end is built; scenario test in the NFR-CEQ-08 IS set | IS row (LK1b) | R1 | S | CON | the scenario test against the rc.4 oracle. |
| R19 | **CE4-06 header-only RPC then connect** (full-node scenario, after Core's `rpc_masternode.py:110-120`): build a block with `generateblock(..., submit=False)`, submit only its header, call `protx info <hash>`, `protx listdiff` and `masternode payments` and require RPC -8 `Block is not in main chain` from all three, then submit and connect the block and connect the next block. Keep the dash-mnlist cache unit test (§5 item 8) as the lower-level check | ND1 (rdashd RPC) with the RP rows | R1, R5 | S | CON | the scenario passing against rdashd and the rc.4 dashd in the dashd test harness; a prerequisite of R16/R17 |

The merge of R1+R2 is the pin move. From then on every new branch targets rc.4. R5, R6 and R10–R12 land as normal task merges against the moved pin: until each lands, `oracle-diff` reports only its own predicted rows.

## 7. Decisions for the manager

- **D1, base-sdk #87.** (a) Retarget #87 to rc.4 before merge: replace the first-wins commit and move the corpus and links to 0e7382df80. Recommended: #87 is still in review, and merging rc.3 semantics upstream would land a rule the current reference rejects. (b) Merge at rc.3, then open a follow-up PR. Either way it is an outward action on an upstream PR.
- **D2, certification carry-forward.** Legs whose compared Core code is byte-identical rc.3→rc.4 keep their certificate after a corpus replay on the rc.4 oracle with 0 divergences. Alternatively, recertify every `l1` leg. The first costs one replay per leg; the second costs a certify window (≥ 3,600 CPU-s each) for each of the 76 `l1` legs.
- **D3, citations.** Rewrite only cites of lines rc.4 changes, plus the pin constants (proposed), or rewrite every rc.3 cite (611 files).
- **D4, live oracles.** Approve an rc.4 `evodb verify` on reflinked copies of the BS2 datadirs (read-only for the live nodes), then the binary swap. Rule whether a dirty verify means `-reindex` of the oracle.
- **D5, in-flight work.** Branches now in review that touch CE4/MN4/WAL4 areas (EV2b's dash-dml codec, CJ0, GV1a, the #87 fixer) either finish at rc.3 and adopt rc.4 in R5/R11/R12, or switch now. Proposed: finish at rc.3 and keep the rc.4 change as its own reviewed commit.
- **D6, BL-393 text.** Port 443 is #7741, in rc.2 and rc.3. Remove it from BL-393's "after rc.3" list.
- **D7, `evodb verify/repair` RPCs (CE4-12).** Port them, or record them as not applicable to rdashd's own store (DEC-11).
- **D8, core-watch cadence.** rc.4 went unnoticed for a week. Confirm that the weekly core-watch restarts with R15.

## 8. Coverage

- `inventory.py --core /work/scratch/swv24-rc4/dash --old v24.0.0-rc.3 --new v24.0.0-rc.4 --out /work/scratch/swv24-rc4/pin-delta --report designs/SW-V24-rc4-pin-delta.md designs/SW-V24-rc4-pin-delta/*.md`: exits 0, 0 uncited PRs (#7917), 0 unattributed files.
- Sub-PR coverage: every one of the 40 commits is cited, either by its PR number (#7814, #7851, #7816, #7828, #7822, #7810, #7838, #7792, #7821, #7834, #7818, #7829, #7820, #7841, #7831, #7521, #7866, #7867, #7871, #7876, #7875, #7839, #7827, #7824, #7836, #7782, #7881, #7858, #7817, #7894, #7842, #7857, #7846, #7895, #7909, #7537, #7888) or by sha (a56ed47c98a, 79480ff2feb, 6c02b2ba87c).
- Hunks: 349 of 349 index rows classified (`hunks.md`). The tag diff has 350 unified-diff hunks: the `src/wallet/ismine.h → src/wallet/types.h` rename's two hunks (`+3,19` and `+50,18`, `wallet.md`) are one row. The packets end with "all N files swept": ce 19, mn-overlay 18, wallet 23. The rest are tests, GUI, docs and CI, classified in `hunks.md`.
