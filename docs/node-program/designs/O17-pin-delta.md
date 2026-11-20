# O17 PIN-DELTA: Dash Core v23.1.8 ⇄ v24.0.0-rc.3

> **Superseded by DEC-82 (v24 only, RP-V24).** The node targets v24.0.0-rc.3 alone: the `profile` rows now have their v24 behaviour unconditionally, and `ReleaseProfile` does not exist. This audit is kept as history of the v23.1.8 ⇄ v24 differences.

| | |
|---|---|
| Task | PLAN O17 (M0): PIN-DELTA audit and `ReleaseProfile` spec (rev1, M-2) |
| Date | 2026-10-08 |
| Pins | **v23.1.8** = `728f505583` (= head of `v23.1.x`; mainnet parity pin, DEC-05) · **v24.0.0-rc.3** = `0364ed626e` (= head of `v24.0.x`, the latest v24 tag; testnet/devnet/regtest pin) |
| Outputs | this report · `O17-pin-delta/` (the nine auditor packets, `decisions.json`) · `O17-release-profile.md` (the spec) · rust-dashcore `node/o17-pin-delta`: `contrib/node/pin-delta/{inventory.py, merge.py, spec_tables.py, AUDIT-BRIEF.md}` (the last commit lacks the DEC-37 changes to `merge.py` and `AUDIT-BRIEF.md` and the new `spec_tables.py`; uncommitted per DEC-17) |
| Status | Complete. Second-Opus review (APPROVE-WITH-CHANGES, all findings addressed) and GPT r2 review (REWORK; 42 rows disputed). DEC-37: all 42 moved to `profile`, the remaining 22 `v24-all` rows approved as declared deviations (§2, §5.11). A1/PLAN edits applied (DEC-34c, §6.2–§6.3) |

## 0. Summary

**Scope.** v23.1.8 is an ancestor of v24.0.0-rc.3, so every difference is something v24 added or changed: 423 merged PRs (296 that touch runtime code or vendored library sources), 1,662 non-merge commits and 1,633 changed files (counted without rename detection), 618 of them runtime code. Nine Opus auditors each read one area's PRs and swept every file it owns hunk by hunk, against tag-to-tag source. The integrator added one row and swept the vendored library sources. The 283 rows merge into **217 distinct behaviours** (66 cross-packet duplicates merged; the GPT review found the 66th, P2P-39 = CONS-30).

| Class | profile | gated | params | v24-all | none | total |
|---|---|---|---|---|---|---|
| CON (consensus) | 16 | 13 | 3 | 1 | 0 | 33 |
| P2P (P2P-observable, incl. relay policy) | 43 | 3 | 0 | 4 | 0 | 50 |
| MN (masternode behaviour) | 13 | 2 | 0 | 1 | 0 | 16 |
| RPC (RPC/REST/ZMQ/CLI, init/config) | 94 | 0 | 0 | 8 | 0 | 102 |
| INT (internal or persisted-file only) | 4 | 0 | 0 | 8 | 4 | 16 |
| **total** | **170** | **18** | **3** | **22** | **4** | **217** |

Classes count each row once, by its primary class. Keyings are defined in §1.3: `profile` rows need both behaviours, selected by `ReleaseProfile`; `gated` and `params` rows come from chain params; `v24-all` rows are implemented once and are declared deviations on mainnet, each with its policy, verification list and written argument (§2, §5.11). Before DEC-37 the split was 128 `profile` / 65 `v24-all` over 218 rows.

**Where the 170 `profile` rows land** (DEC-24 D3; the spec gives every field):

| Home | Crate | Rows |
|---|---|---|
| `UngatedRules` | `dash-consensus` (CS0) | 23: CONS-04, -06, -07, -09, -10, EVO-02, -06, -14, -15, -16, -17, DKG-07, -08, SIG-01, -12, -13, MPIX-01, -16, -19, MNO-02, -12, P2P-16, RPC-46 (decode JSON) |
| `ChainstateRules` | `dashd-chainstate` | 1: CONS-11 |
| `Identity`, `P2pRules` | `dashd-p2p` | 4 + 16 |
| `PolicyRules` | `dashd-mempool` | 10 |
| `MiningRules` | `dashd-mining` | 1: MPIX-21 |
| `SporkRules`, `SyncRules` | `dashd-masternode` | 3 + 1 |
| `LlmqRules` | `dashd-quorums` | 11 |
| `LockRules` | `dashd-locks` | 3 |
| `GovernanceRules` | `dashd-governance` | 3 |
| `CoinJoinRules` | `dashd-coinjoin` | 5: INTG-01, MNO-17..-20 |
| `IndexRules` | `dashd-indexes` | 3: MPIX-08, -10, -11 |
| `WalletRules` | `dashd-wallet` | 24 |
| `InitRules` | `dashd-node` | 13 |
| `RpcRules` | `dashd-rpc` | 19 |
| `RpcSurface` (generated per release), `ArgsTable`, `CliRules` | `dash-rpc-spec`, `dash-args`, `rdash-cli` | 32 + 2 + 1 |

(Some rows have two homes, e.g. P2P-25 is both an init and an RPC switch, so the column sums to more than 170. Some fields carry two rows, e.g. `P2pRules.qrinfo_request` = DKG-12 + RPC-39.)

**Findings that change plans or designs.**
1. **A1 row 3 is refuted** (CONS-03, confirmed independently by the evo packet and both reviews). `IsValidMNActivation` is byte-identical at both tags, and each pin's mainnet params accept the same MNHF bits. Drop `consensus.mnhf_valid_bits`. The real difference is the masternode EHF *signer* window (SIG-19). It is unobservable on mainnet, where the bit-12 signal is already mined and neither pin signs, but the v23.1 functional tests (`feature_mnehf.py`) need it.
2. **A1 row 26 stays, and is wider than A1 says** (CONS-09, DEC-37). Besides a failed or dry-run connect at the V19 boundary, a successful `verifychain` at level 3 whose depth reaches below V19 leaves v23's global BLS flag switched (GPT r2 §1). The last checkpoint does not exclude that path. `UngatedRules.bls_scheme_scope` selects the scope, and A1's `Effect::BlsSchemeLeak` stays, extended to the `verifychain` path. (This report first proposed dropping it; that proposal is withdrawn.)
3. **Two consensus rows can change block acceptance under the mainnet profile** (DEC-37, GPT r2 blockers): asset-unlock index membership for a range ending at 2^64−1 (CONS-10; a two-index witness, no V24 gate) and v23's never-invalidated `merkleRootQuorums` caches after a reorg that mines a different commitment for the same quorum base (DKG-08). Both are `UngatedRules` fields. DKG-08's v23 variant keeps process-wide cache state, so O10 and O15 test both a fresh and a warmed reference.
4. **A1 rows 14 and 15 stay `profile`** (DKG-03, -05, -09..-13; DEC-37). QRINFO sorting and deduplication are byte-identical and common code; the decode bound, malformed-request scoring, the 4,096 cap, QGETDATA decoding, tracking and scoring, DKG intake and the `-watchquorums` observer are per release. DKG-04 (retention quota) stays a declared deviation.
5. **A1 row 9 is narrower than written** (P2P-05..-11). The 2 s non-preferred `MSG_TX` delay and the 100,000 announcement cap exist in **both** pins. What differs is overload delay, preferred-first random selection, expiry/fallback timing, and v23's *global* `g_erased_object_requests` stamp, which stops a v23 node from re-fetching an object it received once from any peer until the stamp is evicted from its 50,000/100,000-entry limited map (P2P-07: the most probe-visible downloader difference).
6. **Ungated v24 behaviours a mainnet node would expose today**, beyond A1's list:
   - inv type 34 is understood from peers of any version (P2P-04): one `inv`/`getdata` tells a probe which pin it talks to. The ≥ 70242 gate is send-only (A1 row 18 corrected).
   - asset-unlock mempool admission (MPIX-15): v23.1.8 admits already-mined and duplicate-index v1 unlocks; Platform sees different `sendrawtransaction` results.
   - Platform-quorum recovered sigs keyed per message (SIG-16): a v24-rule MN signs a second msgHash for an id where v23 members refuse.
   - interop breaks if v24 behaviour ran on mainnet:
     - EVO-15: a ProUpServTx type check at mempool entry scores +100 a transaction v23 relays;
     - MPIX-12: v24 rejects a ProTx pair honest v23 nodes accept;
     - MNO-07: v24 gives +20 for orphan votes in an MN-list race.
   - cheap fingerprints GPT r2 found among the former `v24-all` rows: a short `getqrinfo` (DKG-12), QWATCH followed by a 68-byte QGETDATA or an unregistered type (DKG-09/-10), a non-MN relaying recovered sigs to any `QSENDRECSIGS` subscriber (SIG-18), CoinJoin session replies (MNO-17..-20, INTG-01), and the score for an asset unlock outside the receiver's window (MPIX-16: +100 in v23, none in v24).
   - other ungated differences:
     - sig-share recovery target selection (SIG-17);
     - the governance vote re-request cap per object, 1 → 3 (MNO-11, from a #7808 sub-PR);
     - the invalid-vote cache (MNO-06);
     - mandatory script flags (MPIX-01): four unscored rejects become +100, for transactions honest v23 nodes never relay.
7. **The clock change is wider than A1 row 4.** Header `time-too-new`, the miner/GBT `curtime` (MPIX-19) and four net_processing sync decisions (P2P-16) all move from adjusted time to `NodeClock`. One `UngatedRules.time_source` switch covers them.
8. **Row 24 has a real-connect effect** (DKG-07, finished in §4): besides the `fJustCheck` verdict, v24 changes reject-string precedence on real connects, and v23 erases the mineable-commitment maps on a failed connect (an effect that survives the failure and changes later templates).
9. **Configs that run on v23.1.8 fail to start on v24** (P2P-24, -33, -34, -35, -36, -37, -38, CONS-23, MPIX-08, MPIX-10): stale `debug=` categories, an occupied port, `bind=` with the default `listenonion`, `txindex=0` with governance, an unused datadir `dash.conf`, the new masternode refusals (#7754), a very large `-maxsigcachesize`, and `-prune` with `-addressindex`/`-spentindex`. On mainnet rdashd keeps v23's tolerance. For the indexes this means IX2 must keep address and spent indexes usable with `-prune` under the v23 profile, either with a prune lock that pins undo data until indexed or by indexing at connect time (`IndexRules.dash_index_model`).
10. **RPC surface:** v24 adds 19 methods and removes none; removals are field- and argument-level behind `-deprecatedrpc`. The defaults that matter most to Platform: `getblockchaininfo.softforks` is opt-in in v24 (RPC-02), and the legacy `service`/Platform-port fields are opt-in (RPC-40; dashmate already sets `deprecatedrpc=service`). RPC-10 (batch requests from `-rpcwhitelist` users) is now `profile`: v23 refuses an allowed-only batch with HTTP 403.
11. **Downgrade and import hazards** for IM/WL3/CH6: v24's one-way index migration leaves a v23 datadir with silently empty indexes (MPIX-08); v24 descriptor IDs hash the `h` marker that v23 recomputes as `'` (WAL-21); `sporks.dat` formats are mutually unreadable but harmless (MNO-04); `governance.dat` stays compatible (MNO-06).

**Decisions** (§6.1): the keying policies and the reclassification are decided (DEC-34, DEC-37); DEC-02 is amended (DEC-34a); the Platform `softforks` dependency went to pasta (DEC-34d). The A1 and PLAN edits are applied (§6.2–§6.3). What remains for the manager: forwarding the CS0 changes (§6.3 item 7), spot-checking this rework, and confirming one departure from DEC-34(c): `Effect::BlsSchemeLeak` is **kept**, not dropped, because DEC-37 keys CONS-09 `profile` (§6.1 item 5).

## 1. Method

### 1.1 Pins and the backport trap

- `v23.1.8` resolves to `728f505583`, which is also the head of upstream `v23.1.x`; `v24.0.0-rc.3` resolves to `0364ed626e`, the head of upstream `v24.0.x` (checked with `git ls-remote` on 2026-10-08). There is no v24.0.0 final yet; DEC-23 keeps rc.3 as the v24 baseline until it ships.
- v23.1.8 is an ancestor of rc.3, but it joined the v24 line only through #7534 ("merge master (v23.1.8) back into develop"). Many develop PRs in the range were backported to v23.1.x first, sometimes adapted. A PR diff is therefore not proof of a delta. Every row was checked against tag-to-tag source (`git show <tag>:<file>`), and the inventory flags candidate backports:
  - by patch-id (`yes`, 15 reviewable PRs);
  - by reverse-applying the PR's diff to v23.1.8's tree (`tree`, 12 more);
  - by PR number in a release-branch commit message (`ref`, 44; a hint only, since "not on this branch" is cited the same way, as with #7343).

  The `tree` signal reproduces the auditors' manual verdicts, e.g. #7244, #7348, #7440.
- Examples the trap would have caught: #7414 and #7442 (A1 row 23) and #7348 (A1 row 11's penalty) are all in v23.1.8.

### 1.2 Process

1. `contrib/node/pin-delta/inventory.py` lists every first-parent PR between the pins, attributes every changed file to the PRs that touched it, flags backports, and splits the reviewable PRs and files into auditor packets. This audit used nine: consensus, mempool-index, evo, llmq-dkg, llmq-sig, p2p-init, mn-overlay, rpc, wallet. The script now splits `util` into a tenth packet and reviews vendored library sources; see §7.
2. Nine Opus auditors worked from one brief (`contrib/node/pin-delta/AUDIT-BRIEF.md`): classify every PR in the packet as a delta row or a "not delta" with a reason, sweep every owned file's tag-to-tag diff, and cite `path:line` at both tags for every row. Their outputs are in `O17-pin-delta/<packet>.md`.
3. The integrator merged the packets with `contrib/node/pin-delta/merge.py` and `O17-pin-delta/decisions.json`: duplicates, keying overrides, homes, the policy and verification list of every `v24-all` row, and titles. The integrator also:
   - finished rows 23 and 24 independently (§4);
   - swept the vendored library sources (§7.2) and added one row (`O17-pin-delta/integrator.md`, INTG-01);
   - spot-checked the highest-impact claims against source: #7755 in rc.3; `softforks` behind `-deprecatedrpc`; `IsValidMNActivation` identical; the `createwallet` default; inv type 34 handling; the v23 mempool's `CheckSpecialTx` without credit-pool indexes; Platform recsig keying.
4. An independent second Opus reviewed the report and spec and re-verified 36 rows against source, a code reviewer checked the scripts, and an independent GPT review re-examined every `v24-all` row and 15 `profile` rows against source (§7.4). DEC-37 applied the GPT review's reclassification.
5. Coverage (§7): `inventory.py --report` confirms that every reviewable PR is cited by the report or a packet, and every packet ends with "all N files swept".

### 1.3 Classes and keyings

**Class** (primary; a `+` adds a secondary): `CON` block/tx/header validity, txids, consensus reject strings, MN-list/credit-pool/quorum state · `P2P` anything a peer sees on the wire, including relay and mempool policy · `MN` what a masternode does that other masternodes or quorums depend on · `RPC` RPC/REST/ZMQ/CLI surface and results, plus init and config behaviour an operator sees · `INT` internal or persisted-file-only.

**Keying:**
- `profile`: rdashd implements both behaviours and selects one through `ReleaseProfile` (spec: `O17-release-profile.md`).
- `gated`: behind a deployment checked at runtime (V24, `evo_shares`); same code on every network.
- `params`: a per-network chain-params value that differs per pin; BS5's `dash-params` carries both pins' tables.
- `v24-all`: only the v24 behaviour is implemented, on every network. On mainnet it is a **declared deviation** from v23.1.8 (§2).
- `none`: internal; no observable difference.

## 2. Keying policies (decided: DEC-34, DEC-37)

These rules decide the keying where auditors disagreed and for every `v24-all` row. Every `v24-all` row records its policy, its verification list and its written unreachability or unobservability argument in `decisions.json`; `merge.py` fails if one is missing, and §5.11 prints them.

**History.** The first integration keyed 65 rows `v24-all` under six policies (H, R, B, U, A, I). The GPT r2 review (`reviews/O17-r2-gpt.md`, REWORK) disputed 42 of them. **DEC-37 moved all 42 to `profile`**: parity with the release each network runs outranks the implementation cost. The 23 rows GPT kept were approved as declared mainnet deviations; P2P-39 then merged into CONS-30 (same decoder and PR), leaving **22 distinct `v24-all` rows**.

**Parity rule (DEC-37, binding).** On mainnet rdashd reproduces v23.1.8's decision and relevant state for the same input and event history. That includes:
- stateful behaviour (caches, process-global flags, request trackers), modelled as state rather than dismissed as "process-history dependent" (DKG-08, CONS-09);
- historical verification (`verifychain`, reindex), which checkpoints do not exclude (CONS-09, MNO-12);
- bootstrap states, where the active tip is below an activation height that mainnet passed long ago (EVO-17);
- cheap probes from an ordinary peer, an MNAUTH-verified masternode or a QWATCH observer, even when only crafted input reaches them (DKG-03, DKG-09..-12, SIG-18, MNO-17..-20, INTG-01);
- stable RPC results, errors, authorisation and startup behaviour (RPC-10, WAL-35, MPIX-10, P2P-31).

A behaviour is `v24-all` only when one of the narrow policies below applies **and** its argument holds for every input an honest or probing party can produce under the mainnet profile.

**Interop rule (binding, unchanged).** If the v24 behaviour would mis-score, disconnect, mark bad or reject something an honest v23.1.8 node sends or expects, the row is `profile`. A1 row 17 (DKG bad-connection criteria) is the model case. Others:
- EVO-15: a ProUpServTx type check at mempool entry gives +100 for a tx v23 relays;
- MPIX-12: v24 rejects a ProTx pair v23 accepts and relays;
- MNO-07: v24 gives +20 for orphan votes in an MN-list race.

**The `v24-all` policies (22 rows).** The per-row arguments are in §5.11.

- **Policy H, narrow hardening (7 rows): CONS-26, CONS-30 (with P2P-39), DKG-04, RPC-07, RPC-47, SIG-11, WAL-25.** Only malformed input, an abuse quota or a quorum-signed inconsistency reaches the difference, and its scope is stated exactly: the same bytes accepted and rejected (CONS-26); an encoded NUL in a wallet name (CONS-30); duplicate-flooding a DKG quota (DKG-04); an RFC-invalid URI (RPC-07); counts where v23's arithmetic is undefined (RPC-47); a quorum signing inputs out of order (SIG-11); malformed or mixed-format wallet records (WAL-25). A probing peer or RPC client can see the six that peers and clients reach; O13/O14 list those as known probes (WAL-25 needs a malformed wallet file and is verified by `none`). None of them becomes a general allowed-difference category.
- **Policy B, race or crash fix (5 rows): RPC-08, RPC-09, SIG-20, WAL-23, WAL-45.** v24's outcome is one v23 can also produce (a race resolved one way: RPC-08, RPC-09), or v23's branch is a non-terminating loop, an unfulfilled future or an unloadable file that no client can rely on (WAL-45, SIG-20, WAL-23).
- **Policy U, unreachable under the mainnet profile (3 rows): DKG-14, MPIX-03, RPC-17.** The code runs only on regtest/devnet params or behind a regtest-only RPC guard (DKG-14, RPC-17), or the default it changes is identical on mainnet and regtest (MPIX-03). Under `-releaseprofile=23.1` on regtest, DKG-14's cap is a declared debug-option limit.
- **Policy R, bounded reject-path proof (1 row): EVO-18.** GPT r2's bounded proof: above mainnet's first non-null coinbase ChainLock the new range guard cannot fire. The first-candidate and corrupt-parent cases keep v24's null-ancestor guard as a malformed-input exception (a null dereference is not reproduced).
- **Policy I, operator-only or unspecified (6 rows): CONS-22, P2P-30, WAL-30, WAL-32, WAL-34, WAL-44.** Disk exhaustion and read errors, PID-file bookkeeping, scheduling during `-loadblock`, a startup rescan that finds nothing under the shared birthday model, and randomised coin or funding selection that neither release specifies.

Policy A (architecture) is retired: its three rows (MPIX-08, -10, -11) are `profile`, because DEC-11's storage freedom does not cover supported configurations or documented query results (GPT r2 §4).

**Policy S: the RPC surface is data.** Method sets, arguments, result fields, help, error strings and `-deprecatedrpc` defaults are generated per release by AR2 (`dash_rpc_spec::surface(Release)`), not hand-coded switches. Only behaviour that data cannot express (ordering, parsing strictness, checks, authorisation) becomes an `RpcRules` field. RPC rows are `profile` because RPC clients (functional tests, Platform, dashmate) observe them; the cost per row is a table entry.

**Policy F: persisted formats.** Importers (WL3, IM1a/b, CH6a/b) read both pins' formats on every network. Writers emit the form of the active profile only when a Core user is expected to move the file back to that release (wallets: WAL-21, -22, -24). Otherwise rdashd keeps its own format (DEC-11), so there is no profile field: for example `sporks.dat` (MNO-04, keyed `none`). Storage freedom stops at observable semantics: the index model's startup and query behaviour (MPIX-08, -10, -11) is profiled even though the index layout is rdashd's own.

**Shared values.** A row may stay `profile` where its observability alone would allow `v24-all`, when it shares one field with rows that must be `profile`. Examples: MPIX-01's mempool scoring shares `mandatory_script_flags` with CONS-07's block reject string; SIG-13's ChainLock-signer half shares `is_gate` with SIG-12; RPC-39's RPC cap shares `qrinfo_request` with DKG-12.

Low-value `profile` rows that could be downgraded to `v24-all` with explicit sign-off (no client is known to depend on them): RPC-14 (CLI display), RPC-22 (unordered array order; O14 may prefer an allowed diff), WAL-17, WAL-22, WAL-29, MNO-09. DEC-37's parity rule argues against downgrading them; the list stays for the record.

## 3. A1 §2.25 reconciliation

| A1 row | Verdict | Final rows | Change |
|---|---|---|---|
| 1 V24/`evo_shares` set | ok, extended | EVO-01, -04, -05, -07, -08, -10..-13, -21, -22, MNO-16, MPIX-17/-18/-22, WAL-38, RPC-46 | Every element confirmed gated; adds three gated elements A1 missed: CbTx v4 asset-unlock root (EVO-05), DIP-0026 payout lists (EVO-10), cross-scheme operator-key uniqueness (EVO-13). Types 10–12 are **not** gated at `ContextualCheckTransaction` (CONS-06) |
| 2 V24 bit 12 → 13, `evo_shares`, testnet start | ok | CONS-01, CONS-02 | `params` |
| 3 MNHF valid bits | **refuted** | CONS-03 (`params`); signer part SIG-19 | Drop `consensus.mnhf_valid_bits`. Remove row 3 from the "rows 3–5 and 24 affect only invalid blocks" paragraph |
| 4 `time-too-new` clock | ok, extended | CONS-04, MPIX-19, P2P-16 | One `UngatedRules.time_source` for header validation, miner/GBT and sync decisions |
| 5 AU v2 stable txid | ok, corrected | SIG-01, EVO-02, EVO-06, P2P-04 | Trigger also needs `nVersion ≥ 3`; the MUTATED-vs-ConnectBlock direction depends on which txids the block's author used (both directions occur); AL v2 reject string differs too; inv part is P2P-04 |
| 6 IS conflict gate | ok, refined | SIG-12, SIG-13 | Both pins also require SPORK_2 via `GetConflictingLock`; v24's per-tx filter is `HasLockInputs`. Mainnet hard-codes every spork active in both releases, so there the v23 gate reduces to "blockchain-synced" and SIG-13's CL-signer half is unobservable (kept for O11). SP0 implements that hard-coding under both profiles; a mainnet SPORK_3 of "off" would split consensus |
| 7 Sporks | corrected | MNO-01..-03, P2P-14; MNO-04 (`none`) | Mainnet hard-codes every spork, so SPORK_3/9 and multi-key sporks are invisible there but required by the v23.1 functional tests (O11). "No longer caches true values" is unobservable (INT). GBT `superblocks_enabled` is kept as constant `true`. `sporks.dat` formats differ (MNO-04) |
| 8 `getsporks` dedup | ok | P2P-14 | v24 also answers one copy per spork id |
| 9 Object download | corrected | P2P-05..-09, P2P-11 | See §0 finding 5 |
| 10 Unsolicited getdata-only objects | ok, detailed | P2P-12 | Grace of 2 request intervals, 256-entry per-peer record, bound to the requested inv type |
| 11 NOTFOUND limit | ok | P2P-10 | 116 vs 50,000; the +20 is in both |
| 12 Orphan work set | corrected | P2P-13 | Also synchronous (v23) vs deferred (v24) processing, and v24 no longer punishes block-triggered invalid orphans |
| 13 MNAUTH inbound cap | ok | P2P-18 | `profile` per the interop rule (v24-all would also be defensible) |
| 14 QGETDATA/QRINFO | ok, extended | DKG-09..-13, RPC-39 | `profile` (DEC-37): `LlmqRules.qgetdata_decode`, `.qgetdata_unregistered_type_score`, `.qgetdata_tracking`; `P2pRules.qrinfo_request` (P2P decode bound and malformed score, RPC cap), `.mnlist_data_unavailable_score`. QRINFO sorting and deduplication are byte-identical, so they are common code |
| 15 DKG intake | ok, extended | DKG-03, DKG-04, DKG-05 | DKG-03 and DKG-05 `profile` (DEC-37): `LlmqRules.dkg_intake`, `.observer_dkg_push` (the `-watchquorums` observer is new). DKG-04's retention quota stays a declared deviation (policy H) |
| 16 Coinbase ChainLocks | ok | SIG-15 | Activation conditions listed |
| 17 DKG bad-connection criteria | ok | DKG-01, P2P-02 | (5)/(6) apply only to MN-flagged connections after the version test; #7755 is in rc.3 and ungated |
| 18 New P2P surface | corrected | P2P-03, P2P-04 | Send-side gates follow `protocol_version`; inv type 34 needs its own switch (receive side has no gate) |
| 19 Mandatory script flags | ok, extended | MPIX-01, CONS-07 | Also changes the inline-check block reject string; the value lives in `UngatedRules` |
| 20 `MIN_STANDARD_TX_SIZE`, testnet standardness | corrected | MPIX-02, MPIX-03 | The testnet standardness default needs no field (only v24-pinned networks differ, so `-releaseprofile=23.1` is regtest-only) |
| 21 MN init | corrected | P2P-35, P2P-36 | Full refusal list; "governance requires txindex" applies to every node (P2P-36) |
| 22 RPC | corrected | RPC-01, RPC-02, RPC-03 … | 19 v24-only methods, not 3 |
| 23 Governance | **finished (§4.1)** | MNO-06, MNO-07, MNO-08, MNO-11, P2P-21 | #7414/#7442 are in v23.1.8; #7518 is perf only; #7527 and #7569 are deltas; three governance deltas A1 lacked |
| 24 `fJustCheck` duplicate commitments | **finished (§4.2)** | DKG-07 | Adds real-connect reject precedence and v23's surviving mineable-map erasure |
| 25 (c) items | ok, one correction | EVO-25 (`none`), RPC-33 | `gettxoutsetinfo` loses `bip30` (an RPC delta) |
| 26 BLS scheme flag | ok, extended | CONS-09 | `profile` (DEC-37): `UngatedRules.bls_scheme_scope`. Keep `Effect::BlsSchemeLeak` and extend it to a successful `verifychain` at level 3 below V19 (GPT r2), which the checkpoint does not exclude |

**Mainnet parity argument (A1 §2.25, "How mainnet v23.1.8 parity is proven", item 1).** It still holds for the canonical history, with amendments (applied to A1):
- Row 3 leaves the "invalid under both" list.
- Add the rows that only change reject strings or classes on transactions and blocks both pins reject: CONS-06, EVO-06, EVO-14, EVO-16, MPIX-16 and SIG-01's companions.
- Four `UngatedRules` rows change acceptance or process state, but not on the canonical history:
  - CONS-10 needs a stored asset-unlock index range that reaches 2^64−1 (mainnet's withdrawal indexes are small; O7's `getassetunlockstatuses` cross-check confirms it);
  - DKG-08 needs a reorg that mines a different commitment for an unchanged quorum base;
  - CONS-09 and MNO-12 change only what historical verification (`verifychain`, a synced node's reconnects) leaves behind or accepts.

  The v24 oracle's per-block goldens therefore stay valid for mainnet history. O10 tests these rows on constructed histories against both references, DKG-08 with a fresh and with a warmed v23.1.8 process.
- **O15 caveat, reversed.** O15 no longer files a DKG-08 divergence as a known v23 defect: rdashd's `V23_1` profile reproduces v23's quorum-root caches, so a divergence from the v23.1.8 reference after such a reorg is an rdashd bug **given the same process history since start**. v23's caches depend on what the process has seen, so O15 records restarts on both sides and does not compare across one.

## 4. Rows 23 and 24 (finished)

### 4.1 Row 23: governance request authorisation, throttling and caching

A1 listed #7414, #7442, #7518, #7527 and #7569. Hunk by hunk (integrator and mn-overlay auditor, independently):

| PR | In v23.1.8? | Verdict | Evidence |
|---|---|---|---|
| #7414 throttle per-object vote sync | yes (merge 89bdf7c0e3 on v23.1.x) | not a delta: the govsync "-votes-" fulfilled-request throttle (+20) and the empty-filter object fetch are identical | v23 `governance/net_governance.cpp:113-142`, `governance/governance.cpp:140-170`; v24 `:114-143`, `:148-178` |
| #7442 authorise governance responses through the per-peer request tracker | yes (merge 4b4d96a693) | not a delta in itself: both pins accept a `govobj`/`govobjvote` only from a peer that announced it or that we asked (`PeerConsumeObjectRequest`). The tracker underneath differs (legacy vs `TxRequestTracker`), and v24 adds `PeerForgetObjectRequest` after a successful accept → part of P2P-05..-07 | v23 `net_governance.cpp:186-195`, `net_processing.cpp:1590-1606`; v24 `net_governance.cpp:194, 244` |
| #7518 memoise vote-signature checks | no | not a delta: verdicts are identical; the memo is keyed on (key, sighash, signature) | v24 `governance/vote.cpp:22-56, 164-209` |
| #7527 valid signature before caching an orphan vote | no | **MNO-07** (`profile`, `GovernanceRules.orphan_vote_requires_valid_sig`): v23 caches any vote for an unknown parent with no check or penalty and sends `govsync(parent, empty bloom)`; v24 requires `IsValidForUnknownParent` (MN exists, right key, time ≤ adjusted + 1 h) or answers PERMANENT_ERROR +20 (once synced) with no cache entry and no parent request. Honest v23 peers trigger it in a key or MN-list race, so the interop rule makes it `profile`. Under the v23 variant an orphan vote never enters the invalid-vote cache (MNO-06), as in v23.1.8 | v23 `governance.cpp:812-821`, `net_governance.cpp:261-269`; v24 `governance.cpp:820-836`, `vote.cpp:244-250` |
| #7569 stop caching invalid votes | no | **MNO-06** (`profile`, `GovernanceRules.invalid_vote_cache`): v23 stores a vote that failed with PERMANENT_ERROR/20 in `cmapInvalidVotes` (≤ 1,000,000, persisted in `governance.dat`) and answers any later delivery "Old invalid vote" +20 without re-validating, even after it became valid; v24 re-validates. An honest v23 peer can trigger it | v23 `governance.cpp:798-810, 835-838`, `governance.h:187, 203, 223`; v24 `governance.cpp:802-851`, `governance.h:200-232` |

Also found in the same code, and not in A1:
- **MNO-08** (#7738, `profile` since DEC-37, `GovernanceRules.record_rejected_votes`): v23 leaves an empty vote record (shown as a "none" vote with time 0 in `gobject getcurrentvotes`, persisted) for a vote rejected after the MN lookup; v24 writes only accepted votes.
- **MNO-11** (#7791 inside #7808, `profile`): within one sync round both pins ask up to 3 peers per object (`nPeersPerHashMax`). The difference is the cap `FetchGovernanceObjectVotes` applies across later rounds, before an object rests for an hour: 1 peer on mainnet in v23 (`nMaxObjRequestsPerNode`), 3 in v24.
- **P2P-21** (#7330, `profile`): a `-disablegovernance` v23 node still requests announced governance objects and drops the replies; v24 never requests them.
- Not a delta: v24 removed the `IsBlockchainSynced` check inside `MNGOVERNANCEOBJECT` (7bbba5d3b1), which repeats the gate at the top of `NetGovernance::ProcessMessage` in both pins.
- `governance.dat` stays compatible both ways: same magic and `"CGovernanceManager-Version-16"`; v24 writes the invalid-vote field as an empty map and discards it on read (v24 `governance.h:200-232`). IM1b reads either.

### 4.2 Row 24: duplicate rotated commitments under `fJustCheck`

Only rotated LLMQ types can carry two commitments of one type in a block (non-rotated duplicates fail `bad-qc-dup` in `GetCommitmentsFromBlock` on both pins). For two non-null commitments with the same `(llmqType, quorumHash)`:

| | v23.1.8 | v24.0.0-rc.3 (#7736) |
|---|---|---|
| Order of checks | `GetCommitmentsFromBlock` → count checks → BLS verification of every commitment → `ProcessCommitment` per commitment (multimap order) | `GetCommitmentsFromBlock` → count checks → **duplicate pre-pass** (`bad-qc-dup`) → BLS → `ProcessCommitment` |
| `fJustCheck` (TestBlockValidity: GBT `proposal`, `generateblock`, the miner's self-check) | `ProcessCommitment` returns before the EvoDB write, so the second copy passes `HasMinedCommitment`: **block reported valid** | **`bad-qc-dup`** |
| Real connect | first copy is written into the EvoDB transaction **and its mineable entries are erased** (`minableCommitmentsByQuorum`, `minableCommitments`, has-mined cache); second copy → `bad-qc-dup`. The EvoDB write is discarded with the failed connect, the erasure is not: the node stops serving that commitment and its templates fall back to a null commitment for that index until it receives it again | `bad-qc-dup` before any `ProcessCommitment`; nothing erased |
| Multi-fault blocks | an earlier error wins: BLS `bad-qc-invalid`, or a per-commitment error of the first copy (`bad-qc-block`, `bad-qc-invalid-null`, `bad-qc-height`, `bad-qc-invalid`). Two copies claiming the same `quorumHash` with different `quorumIndex`: the second fails `bad-qc-block` (after the first copy's mineable entries may already be erased) | `bad-qc-dup` wins over all of those; `bad-qc-payload`, `bad-qc-commitment-type`, non-rotated `bad-qc-dup`, `bad-qc-premature`, `bad-qc-not-allowed`, `bad-qc-missing` still come first |
| Null duplicates | accepted (a null commitment returns before the dup check) | accepted (exempt from the pre-pass) |

Evidence: v23 `llmq/blockprocessor.cpp:165-264` (ProcessBlock), `:267-380` (ProcessCommitment; `fJustCheck` return at :349, erasure at :370-375), `:422-457`; v24 `llmq/blockprocessor.cpp:212-290` (pre-pass :255-264), `:326-453`.

**Keying:** `profile`, `UngatedRules.qc_dup_prepass: bool`. The v23 variant must keep the erasure as an effect that survives the failed connect (A1 §2.10.1's effect channel already lists mineable-commitment erasure). Consumers: LQ2, CH7b, MN1, O10.

**Not part of the row:** v24's `ProcessCommitment` also accepts a byte-identical stored `(qc, blockHash)` and aborts the node on a `WriteDerived` mismatch. Both need a block reconnected while its commitment is still stored, which requires a snapshot chainstate; rc.3 has no `loadtxoutset` RPC and no production caller of `ActivateSnapshot`, so neither path is reachable (llmq-dkg Notes; consensus Not-deltas #7553).

**Reachability:** a miner can produce such a block, and any GBT `proposal` client reaches the `fJustCheck` path. Both pins reject the block on a real connect, so mainnet history is unaffected (A1's parity argument holds). The observable differences are GBT proposal verdicts, reject strings for multi-fault blocks, and a v23 node's templates after it has seen such a block.

## 5. Index of all rows

Generated by `contrib/node/pin-delta/merge.py --decisions O17-pin-delta/decisions.json`. Full text, evidence (`path:line` at both tags) and A1 notes are in the packet file named by each row's prefix: `CONS` consensus, `EVO` evo, `DKG` llmq-dkg, `SIG` llmq-sig, `MPIX` mempool-index, `MNO` mn-overlay, `P2P` p2p-init, `RPC` rpc, `WAL` wallet, `INTG` integrator. "Home / field" is the final `ReleaseProfile` location for `profile` rows (spec §4). A row moved to `profile` by DEC-37 shows "(DEC-37, GPT r2)" after its keying; its packet prose may still argue for `v24-all`, and this index and the spec override it (GPT r2 §7 item 4). A `v24-all` row shows `[policy → verification list]` (§2); that list's task is part of its consumers, and §5.11 states the row's unreachability or unobservability argument. A kept row also carries the PRs and consumers of the duplicates merged into it.

### 5.1 consensus (CONS)

| id | Behaviour | Class | Keying | Home / field | Consumers | PRs |
|---|---|---|---|---|---|---|
| CONS-01 | V24 deployment bit and testnet start | CON | params | — | BS5, CS4 | #7739, #7725 |
| CONS-02 | New deployment `evo_shares` | CON (+RPC) | params | — | BS5, CS4, AR2, RP1a | #7808 (evo packet PR), #7808 (#7807) |
| CONS-03 | Which MNHF signal bits are valid: no difference | CON | params | — | CS4, BS5 | #7739 (map); signer: SIG-19, #7808 (#7807) |
| CONS-04 | Clock for the `time-too-new` header rule | CON | profile | UngatedRules.time_source | CS3, CH1, MN1, ND1, O10 | #7430 (bitcoin#28956) |
| CONS-06 | Special tx types passed by `ContextualCheckTransaction` | CON (+RPC) | profile | UngatedRules.special_tx_max_type | CS3, EV1, MP1a, O10 | #7437 (evo packet PR) |
| CONS-07 | Block reject string for script failures checked inline | CON | profile | UngatedRules.mandatory_script_flags | CS3, CS6, CH3a, MN1, O10 | #7710 (bitcoin#26291; p2p-init packet PR) |
| CONS-09 | BLS scheme flag around failed connects, dry runs and historical verification | CON | profile (DEC-37, GPT r2) | UngatedRules.bls_scheme_scope | CH3a, CH7a, BS11a-t, MN1, O10, CH5 | #7345, #7586 (+ the `enter` variant from #7456/#7553) |
| CONS-10 | `CRangesSet::Contains` for a range ending at 2^64−1 | CON | profile (DEC-37, GPT r2) | UngatedRules.asset_unlock_index_membership | EV3, RP3b, O10 | #7587, #7590 |
| CONS-11 | Persisting failure flags found by FindMostWorkChain | RPC | profile (DEC-37, GPT r2) | ChainstateRules.persist_cl_conflict_flags | CH3b, RP1a, O14 | #7356, #7452 (bitcoin#27905) |
| CONS-12 | BIP9 statistics | RPC | profile | RpcRules.bip9_stats_window | CS4, RP1a, AR2, O14 | #6888 (bitcoin#23508; rpc packet PR); #7745 adds a comment only |
| CONS-21 | PSBT finalized inputs are verified | RPC | profile | WalletRules.psbt_verify_finalized | WL4, AR2 | #7631 (bitcoin#25595 partial) |
| CONS-22 | Node stops on low disk space or a coins read error | INT (operator-visible) | v24-all [I → RL2] | — | ND1, CH2, RL2 | #7187 (bitcoin#26331) |
| CONS-23 | `-maxsigcachesize` sizing | RPC (CLI/InitError) | profile (DEC-37, GPT r2) | InitRules.sigcache_sizing | CS6, AR1, ND1, O11 | #7630 (bitcoin#25527) |
| CONS-24 | Importing with `-loadblock` in prune mode | INT | none | — | CH5, CH8 | #7553 (bitcoin#27746 adaptation) |
| CONS-25 | Reorg disconnect-pool accounting | P2P (mempool contents) | profile (DEC-37, GPT r2) | PolicyRules.disconnect_pool_accounting | CH3b, MP1b, O15 | #7735 (bitcoin#28385) |
| CONS-26 | DYNBITSET pre-allocation bound | P2P | v24-all [H → O13b] | — | BS9a, BS8, P5a, O13b | #7532 |
| CONS-28 | Already-pruned block files are unlinked at startup | INT | none | — | ST2, CH8 | #7431 (llmq packet PR) |
| CONS-30 | `/wallet/<name>` URL decoding | RPC | v24-all [H → O14] | — | RP0, O14 | #7085 (wallet packet PR), #7733 (bitcoin#29904) |

### 5.2 evo (EVO)

| id | Behaviour | Class | Keying | Home / field | Consumers | PRs |
|---|---|---|---|---|---|---|
| EVO-01 | Asset lock / unlock payload v2 acceptance | CON | gated | — | EV3, EV1, CS3, CH7a, MP1a | #7639, #7294 |
| EVO-02 | Version-2 asset payloads on mainnet (V24 inactive) | CON | profile | UngatedRules.reject_strings | CS3, EV3, P6a, O10, EV1 | #7639, #7294 |
| EVO-04 | Asset-unlock limit when V24 is active | CON | gated | — | EV3, CH7a | #7712 |
| EVO-05 | CbTx version 4 (`merkleRootAssetUnlocks`) | CON | gated | — | EV4, CH7a, MN0, BS6a, MN1 | #7639 |
| EVO-06 | CbTx payload nVersion ≥ 4 while V24 is inactive (absorbs SIG-05) | CON | profile (cheap reject-path difference: one flag, no second decoder; absorbs SIG-05) | UngatedRules.cbtx_v4_known | CS3, EV4, O10, BS6a | #7639 |
| EVO-07 | Shared-collateral covenant | CON | gated | — | EV1, EV3, CH3a, MP1a, MN1 | #7437 |
| EVO-08 | Decentralized MN shares (types 10–12, shared ProRegTx) | CON | gated | — | EV1, EV2b, EV3, BS6a, WL5 | #7437, #7808 (#7807) |
| EVO-10 | DIP-0026 owner payout lists | CON | gated | — | EV1, EV3, BS6a | #7340, #7302 |
| EVO-11 | ProTx version unification (V24 rules only) | CON | gated | — | EV1, EV2b | #7302, #7473, #7741, #7748 |
| EVO-12 | ExtAddr netinfo, port 443, entry limits | CON | gated | — | EV1, BS6a | #7741, #7385 |
| EVO-13 | Cross-scheme operator-key uniqueness | CON | gated | — | EV1, EV2a | #7473 |
| EVO-14 | v3 ProTx payload layout and DomainPort decoding (adversarial) | CON | profile (DEC-37, GPT r2) | UngatedRules.protx_payload_decode | BS6a, BS4a, EV1, O10 | #7340, #7437, #7487 |
| EVO-15 | ProUpServTx `nType` check moved to CheckSpecialTx (ATMP) | P2P+CON | profile | UngatedRules.proupserv_type_in_special_tx | EV1, MP1a, P4, MN1 | #7488 |
| EVO-16 | Reject precedence for multiply-invalid ProRegTx/ProUpRegTx | CON | profile (DEC-37, GPT r2) | UngatedRules.protx_check_order | EV1, CS3, O10 | #7340, #7302 |
| EVO-17 | Special payload before DIP3 activation | CON | profile (DEC-37, GPT r2) | UngatedRules.reject_strings | EV1, CS3, O10 | #7342 |
| EVO-18 | CbTx `bestCLHeightDiff` range | CON | v24-all [R → O10] | — | EV4, CH7a, LK1a, O10 | #7363, #7107 |
| EVO-21 | MN payee matching with multiplicity | CON | gated | — | EV3, CH7a, CS2 | #7377 |
| EVO-22 | Superblock payment matching on distinct outputs | CON | gated | — | GV1a, EV3, CH7a | #7362 |
| EVO-25 | MN-list cache bounds | INT | none | — | EV2a, P6b | #7753 |
| EVO-30 | `protx update_registrar` on a legacy-BLS MN | RPC | profile | WalletRules.protx_legacy_key_reencode | WL5, AR2 | #7473 |

### 5.3 integrator (INTG)

| id | Behaviour | Class | Keying | Home / field | Consumers | PRs |
|---|---|---|---|---|---|---|
| INTG-01 | `ValidateDSTX` check order | P2P | profile (DEC-37, GPT r2) | CoinJoinRules.dstx_check_order | P6a, CJ1, O13b | #7052 |

### 5.4 llmq-dkg (DKG)

| id | Behaviour | Class | Keying | Home / field | Consumers | PRs |
|---|---|---|---|---|---|---|
| DKG-01 | DKG bad-connection criteria | MN (+CON via PoSe) | profile | LlmqRules.dkg_bad_connection + Identity.min_mn_proto_version | LQ4a, DK1, P2a (keep VERSION fRelay + services per connection), O13c, O12b | #7755 (+ #7724 version bump, p2p-init P2P-02) |
| DKG-02 | `quorum dkgstatus` → `session.status.phase` timing | RPC | profile | LlmqRules.dkg_status_phase | DK1, O11, O14 | #7769 |
| DKG-03 | DKG push intake validation | P2P | profile (DEC-37, GPT r2) | LlmqRules.dkg_intake | DK1, BS9a, O3, O13c | #7557 (baseline from #7397/#7408 already in v23.1.8) |
| DKG-04 | Pending-DKG retention quota | P2P | v24-all [H → O13c] | — | DK1, O13c | #7583, #7557 |
| DKG-05 | `-watchquorums` non-MN observer and DKG push messages | P2P | profile (DEC-37, GPT r2) | LlmqRules.observer_dkg_push | DK1, QM1b, O13b | #7557 |
| DKG-06 | Startup initialisation of quorum duties | MN (+P2P) | profile (DEC-37, GPT r2) | InitRules.initial_tip_fanout | QM1b, DK1, P7, ND1, O13c | #7240, #7247 |
| DKG-07 | Duplicate non-null rotated commitments in one block | CON (+RPC) | profile | UngatedRules.qc_dup_prepass | LQ2, CH7b, MN1, RP3a, O10 | #7736 (+ #7456 dup-test rewrite, evo) |
| DKG-08 | CbTx `merkleRootQuorums` hash caches | CON | profile (DEC-37, GPT r2) | UngatedRules.quorum_root_cache | EV4, LQ2, CH7b, O15, O10, CH7c | #7491 |
| DKG-09 | QGETDATA with trailing bytes | P2P | profile (DEC-37, GPT r2) | LlmqRules.qgetdata_decode | QM1b, BS8, O13c | #7605 |
| DKG-10 | QGETDATA for an unregistered llmqType | P2P | profile (DEC-37, GPT r2) | LlmqRules.qgetdata_unregistered_type_score | QM1b, O13c | #7529 |
| DKG-11 | QGETDATA tracking and rate limiting | P2P | profile (DEC-37, GPT r2) | LlmqRules.qgetdata_tracking | QM1b, O13c | #7519 |
| DKG-12 | GETQUORUMROTATIONINFO decode bound and malformed-request score | P2P | profile (DEC-37, GPT r2) | P2pRules.qrinfo_request | P6b, BS8, LQ3, O13b | #7629 |
| DKG-13 | Serving MN-list data without block data | P2P | profile (DEC-37, GPT r2) | P2pRules.mnlist_data_unavailable_score | P6b, LQ3, O13b | #7456 (+#7553 sentinel constant; evo packet deferred the scoring to A1#14) |
| DKG-14 | Custom LLMQ size cap | RPC (CLI/init) | v24-all [U → O11] | — | BS5, ND1, O11 | #7448 |

### 5.5 llmq-sig (SIG)

| id | Behaviour | Class | Keying | Home / field | Consumers | PRs |
|---|---|---|---|---|---|---|
| SIG-01 | Asset-unlock stable txid | CON (+RPC) | profile | UngatedRules.au_stable_txid | CS0, BS6b, CS3, O10, RP1b, MP1a, P6a | #7639 |
| SIG-02 | AU payload version 2 acceptance | CON | gated | — | EV1, CS3, BS6b | #7639 |
| SIG-10 | InstantSend for asset unlocks | MN | gated | — | LK1b, LK1c, MP1a | #7639 |
| SIG-11 | ISDLOCK input consistency | P2P | v24-all [H → O13b] | — | LK1b, O13b | #7639 |
| SIG-12 | IS conflict gate in ConnectBlock | CON | profile | UngatedRules.is_gate.connect_block | CH3a, CH7b, LK1b, SP0 | #7212, #7278, #7639 |
| SIG-13 | IS gate at the miner and the CL signer | MN | profile | UngatedRules.is_gate.{miner, cl_signer} | MN1, LK1c, SP0 | #7212, #7278 |
| SIG-14 | Missing quorum in an ISDLOCK verification batch | P2P | profile (DEC-37, GPT r2) | LockRules.islock_batch_missing_quorum | LK1b, O13b | #7662 |
| SIG-15 | ChainLocks embedded in the coinbase are adopted | P2P (+CON: CL enforcement) | profile | LockRules.coinbase_cl_processing | LK1a | #6236 |
| SIG-16 | Platform-quorum recovered sigs are kept per message | MN (+P2P, +RPC) | profile | LlmqRules.platform_recsig_per_msghash | SG1a, SG1b, RP3b, PL1 | #7639 |
| SIG-17 | Concentrated sig-share recovery target | MN | profile | LlmqRules.sigshare_recovery_skip_unreachable | SG1b, LQ5 | #7757 |
| SIG-18 | Non-masternode nodes relay recovered sigs | P2P | profile (DEC-37, GPT r2) | LlmqRules.non_mn_recsig_relay | SG1a, P5a, O13b | #7115 |
| SIG-19 | EHF signal signing window | MN | profile | LlmqRules.ehf_sign_window | SG1a, CS4 | #7808 (#7787) |
| SIG-20 | DKG contribution verification, empty input | MN | v24-all [B → none] | — | DK1, LQ4a | #7486 |

### 5.6 mempool-index (MPIX)

| id | Behaviour | Class | Keying | Home / field | Consumers | PRs |
|---|---|---|---|---|---|---|
| MPIX-01 | `MANDATORY_SCRIPT_VERIFY_FLAGS` | P2P+RPC | profile | UngatedRules.mandatory_script_flags (read by policy) | MP1a, P5, RP3a, CS3, CS6, O13b, P4 | #7710 (bitcoin#26291) |
| MPIX-02 | `MIN_STANDARD_TX_SIZE` 83 → 65 | P2P | profile | PolicyRules.min_standard_tx_size | MP1a, P5, RP3a | #7666 (bitcoin#26265) |
| MPIX-03 | Default of `-acceptnonstdtxn` / require-standard | P2P | v24-all [U → none] | — | ND1, MP1a, AR1 | #7710 (bitcoin#28354) |
| MPIX-04 | Min-relay-fee check placement | RPC | profile | PolicyRules.min_relay_fee_checked_first | MP1a, RP3a | #7237 (bitcoin#26933), #7525 (bitcoin#25648 makes it a pool member) |
| MPIX-05 | `-blocksonly=1` soft-sets `-maxmempool` | RPC | profile | PolicyRules.blocksonly_max_mempool_mb | ND1, MP1b, CH2 | #7684 (bitcoin#26471) |
| MPIX-07 | Fee-estimator persistence policy | RPC | profile | PolicyRules.fee_estimates | MP2, IM1a | #7711 (bitcoin#27622) |
| MPIX-08 | addressindex, spentindex and timestampindex storage model | INT+RPC | profile (DEC-37, GPT r2) | IndexRules.dash_index_model | IX2, CH6a, CH6b, IM1a, RL2, O11 | #7101 (+refactors #7547, #7462, #7525) |
| MPIX-10 | Prune incompatibility | RPC | profile (DEC-37, GPT r2) | IndexRules.dash_index_model | IX2, CH8, ND1, RL2, O11 | #7101 |
| MPIX-11 | BaseIndex rewind timing | RPC | profile (DEC-37, GPT r2) | IndexRules.rewind_on_disconnect | IX1, O14 | #7101 (commit 0695e976b1) |
| MPIX-12 | ProRegTx collateral reuse versus pending MN updates | P2P | profile (interop rule: v24 rejects a pair honest v23 nodes accept and relay (second-Opus M2b)) | PolicyRules.protx_collateral_vs_pending | MP1a, MN1 | #7489 |
| MPIX-13 | Cross-BLS-scheme operator-key conflict in the mempool | P2P+RPC | profile | PolicyRules.protx_cross_scheme_dup | MP1a, RP3a | #7473 (#7555 is a comment-only follow-up) |
| MPIX-14 | Eviction of a pending ProUpServTx when a registrar update is mined | P2P | profile | PolicyRules.proupserv_operator_key_binding | MP1b, ZQ1, MP1a | #7808 (#7776) |
| MPIX-15 | Asset-unlock mempool admission (ungated part) | P2P+RPC | profile | PolicyRules.asset_unlock_mempool | MP1a, MP1b, RP3a, PL1 | #7639 |
| MPIX-16 | AU window failures no longer score the relayer | P2P | profile (DEC-37, GPT r2) | UngatedRules.au_window_failure_class | MP1a, P5, O13b, P4, EV1, O10 | #7744 |
| MPIX-17 | DMS and shared-collateral mempool policy | P2P | gated | — | MP1a, EV1 | #7437, #7807 (in #7808), #7473 |
| MPIX-18 | AU v2 mempool lifecycle | P2P | gated | — | MP1a, MP1b, MN1, P3, RP3a | #7639, #7744 |
| MPIX-19 | Miner clock | RPC+CON | profile | UngatedRules.time_source (read by the miner) | MN0, MN1, RP3a, CS3 | #7430 (bitcoin#28956) |
| MPIX-21 | Per-package template validation | RPC+MN | profile (DEC-37, GPT r2) | MiningRules.package_special_checks | MN1, O14 | #7570, #7473 |
| MPIX-22 | Deployment-gated template fields (CbTx v4, payouts) | CON | gated | — | MN0, MN1, CH7a | #7639, #7437, #7807 |

### 5.7 mn-overlay (MNO)

| id | Behaviour | Class | Keying | Home / field | Consumers | PRs |
|---|---|---|---|---|---|---|
| MNO-01 | Spork table | RPC | profile | SporkRules.defs | SP0, RP2b, O11, O14 | #7278 |
| MNO-02 | SPORK_9 superblock gate | CON+RPC | profile | UngatedRules.superblocks_gate | EV3, CH7a, MN1, RP3a, SP0, O11, O14 | #7278 |
| MNO-03 | Spork signer model | P2P+RPC | profile | SporkRules.signer_model | SP0, AR1, ND1, O11 | #7505, #7303 |
| MNO-04 | sporks.dat format | INT (persisted file, observable on up/downgrade) | none (Policy F: sporks.dat is a Core flat file rdashd does not write; IM1b reads both (second-Opus m4)) | — | SP0, IM1b, IM2 | #7505 |
| MNO-06 | Invalid-vote cache | P2P+RPC | profile | GovernanceRules.invalid_vote_cache | GV1b, P6a, IM1b | #7569 |
| MNO-07 | Orphan-vote gate | P2P | profile (interop rule: +20 to honest v23 peers in an MN-list race (second-Opus M2b)) | GovernanceRules.orphan_vote_requires_valid_sig | GV1b, P6a | #7527 |
| MNO-08 | Vote records left by rejected votes | RPC | profile (DEC-37, GPT r2) | GovernanceRules.record_rejected_votes | GV1b, GS1, RP2b, O14 | #7738 |
| MNO-09 | Vote rate-limit error text | RPC | profile | RpcSurface (errors) | RP2b, WL5, O14 | #7459 |
| MNO-11 | Govsync vote re-request cap per object (1 → 3) | P2P | profile | SyncRules.gov_vote_sync_peer_bound | SP1, GV1b, O13b | #7808 (#7791) |
| MNO-12 | Old-budget block-value window | CON | profile (DEC-37, GPT r2) | UngatedRules.old_budget_check | EV3, CH7a, CH5, GV1b, O10 | #7604 (+#7212, #7330) |
| MNO-16 | CoinJoin promotion/demotion (rebalance) sessions | MN+P2P | gated | — | CJ0, CJ1, WL6, P6a | #7052 (+#7745, #7808/#7794, #7622) |
| MNO-17 | CoinJoin server, duplicate collateral within a session | MN | profile (DEC-37, GPT r2) | CoinJoinRules.server_reject_duplicate_collateral | CJ1, O13c | #7507 |
| MNO-18 | CoinJoin server, entry admission bound to the session | MN | profile (DEC-37, GPT r2) | CoinJoinRules.server_entry_admission | CJ1, O13c | #7052 (f7d2bbbc7e, 9c29052a91), #7599 |
| MNO-19 | CoinJoin server, empty entries | MN | profile (DEC-37, GPT r2) | CoinJoinRules.server_reject_empty_entry | CJ1, O13c | #7052 (9c29052a91) |
| MNO-20 | CoinJoin server, dsa rejection order | MN | profile (DEC-37, GPT r2) | CoinJoinRules.server_dsa_check_order | CJ1, O13c | #7596 |
| MNO-21 | CoinJoin client, mixed inputs stay locked after success | RPC (wallet) | profile (DEC-37, GPT r2) | WalletRules.coinjoin_pending_locks | WL6, WL2a, RP2b, O14, WL8 | #7480 |

### 5.8 p2p-init (P2P)

| id | Behaviour | Class | Keying | Home / field | Consumers | PRs |
|---|---|---|---|---|---|---|
| P2P-01 | Handshake identity | P2P+RPC | profile | Identity.{protocol_version, ua_version, client_version} | P2a, P2b, O13a, RP1a | #7052, #7639, #7725 |
| P2P-02 | `MIN_MASTERNODE_PROTO_VERSION` 70240 → 70242 | MN | profile | Identity.min_mn_proto_version | LQ4a, DK1 | #7724 |
| P2P-03 | New version gates (send side) | P2P | profile | Identity.protocol_version (derived gates) | P2a, P6a, CJ1, WL6, BS9b, CJ0 | #7052, #7639 |
| P2P-04 | inv type 34 (`MSG_ASSET_UNLOCK`) understood from any peer | P2P | profile | P2pRules.msg_asset_unlock | P5, P6a, BS7, O13b, BS10, MP1a | #7639 |
| P2P-05 | Object download: candidate selection and request delays | P2P | profile | P2pRules.object_download | P5, P5a, P6a, O13b | #5943 (bitcoin#19988, #20162, partial #20171), #7673 |
| P2P-06 | Object download: in-flight limit, expiry, fallback | P2P | profile | P2pRules.object_download | P5, O13b | #5943 |
| P2P-07 | Cross-peer effect of a received object or `notfound` | P2P | profile | P2pRules.object_download | P5, P6a, LK1b, GV1b | #5943 |
| P2P-08 | `AskPeersForTransaction` | P2P | profile | P2pRules.object_download | P5, LK1b | #5943, #7673 |
| P2P-09 | Fetching an orphan's missing parents | P2P | profile | P2pRules.object_download | P5, O13b | #5943 |
| P2P-10 | `notfound` | P2P | profile | P2pRules.notfound_limit | P4, P5, O13b | #5943, #7740 |
| P2P-11 | `GetRequestedObjectCount`, used by the governance vote-sync gate | P2P | profile | P2pRules.object_download | SP1, GV1b | #5943, #7673 |
| P2P-12 | Unsolicited GETDATA-only objects are dropped and scored | P2P | profile | P2pRules.unsolicited_scoring | P5, P5a, LK1a, DK1 | #7484 |
| P2P-13 | Orphan reconsideration | P2P | profile | PolicyRules.orphan_processing | P5, MP1b, O13b | #7715 (bitcoin#26551) |
| P2P-14 | Repeated `getsporks` | P2P | profile | SporkRules.getsporks_dedup | SP0, P5a, O13b | #7343 (not on v23.1.x; only named in the message of the #7438 backport), P2P packet); #7303 |
| P2P-15 | Transaction invs queued during the handshake | P2P | profile | P2pRules.tx_inv_before_verack | P5, O13b | #7648 (bitcoin#26569) |
| P2P-16 | Clock used for sync decisions | P2P | profile | UngatedRules.time_source (read by sync) | P2b, P3, ND1 | #7430 (bitcoin#28956), #7630 |
| P2P-17 | Serving a block whose read fails | P2P | profile (DEC-37, GPT r2) | P2pRules.block_read_vs_prune | P4, O13b | #7350 (bitcoin#26326) |
| P2P-18 | Verified inbound MNAUTH legs per proRegTxHash | MN+P2P | profile | P2pRules.mnauth_inbound_cap | P7, AM1, RP1a | #7522 |
| P2P-20 | ChainLock seen-cache | P2P | profile | LockRules.seen_clsig_cache | LK1a, O13b | #7482 (#7494 and #7548 do not change the evicted set) |
| P2P-21 | Governance invs on `-disablegovernance` nodes | P2P | profile | P2pRules.request_gov_invs_when_disabled | GV1b, P6a | #7330 |
| P2P-24 | Invalid `-debug`/`-debugexclude`/`-loglevel` values abort startup | RPC | profile | InitRules.invalid_log_option_fatal | AR1, ND1, O11 | #7710 (bitcoin#27632) |
| P2P-25 | Block verification at startup, and `verifychain` | RPC | profile | InitRules.verifydb_require_full + RpcRules.verifychain_strict | CH5, RP1a, ND1, O11 | #7710 (bitcoin#25574, #27157) |
| P2P-29 | `settings.json` | RPC | profile | InitRules.settings_file | AR1, IM1a, O11 | #7354 (bitcoin#29144), #7433 (bitcoin#27150) |
| P2P-30 | PID file removal | INT | v24-all [I → none] | — | ND1 | #7354 (bitcoin#28946) |
| P2P-31 | Exit status after a fatal error post-init | RPC | profile (DEC-37, GPT r2) | InitRules.abort_exit_failure | ND1, RL1, O11 | #7433 (bitcoin#27708) |
| P2P-32 | `getblockfrompeer` while blocks are loading | RPC | profile | RpcSurface (errors) | RP1a | #7715 (bitcoin#26900) |
| P2P-33 | Bind failures | RPC | profile | InitRules.bind_failure_fatal | P2a, P8, ND1 | #7300 (bitcoin#22729) |
| P2P-34 | Default onion listener, and new InitErrors | RPC+P2P | profile | InitRules.onion_bind | P8, ND1 | #7808 (#7786, partial bitcoin#36170) |
| P2P-35 | MN init | MN | profile | InitRules.mn_init | AM1, ND1 | #7754 |
| P2P-36 | Governance requires txindex on every non-regtest node | RPC | profile | InitRules.governance_requires_txindex | ND1, GV1b | #7808 (#7785) |
| P2P-37 | Ignored config file is an error | RPC | profile | InitRules.ignored_conf_fatal | AR1, ND1 | #7710 (bitcoin#27302) |
| P2P-38 | `dashd -help`/`-version` now read the config | RPC | profile | InitRules.help_before_config | AR1, AR3, ND1 | #7433 (bitcoin#27150) |

### 5.9 rpc (RPC)

| id | Behaviour | Class | Keying | Home / field | Consumers | PRs |
|---|---|---|---|---|---|---|
| RPC-01 | RPC method set | RPC | profile | RpcSurface (methods) | AR2, RP1a, RP1b, RP3a, RP3b, WL5, WL2b, O14, O11, PL1, MP1b | #6888 (bitcoin#23508, #24187, #24528, #24579), #7712, #7192 (bitcoin#23549, #26307, #26341, #26352), #7107, #7727, #7085 (bitcoin#24836), #7245 (bitcoin#27511), #7358 (bitcoin#28523), #7437, #7275, #7131 (bitcoin#24118), #7752 |
| RPC-02 | `getblockchaininfo.softforks` | RPC | profile | RpcSurface (deprecations) | AR2, RP1a, ZQ1, PL1, O14, O11 | #6888 (bitcoin#23508) |
| RPC-03 | Shape and meaning of the BIP9 deployment object | RPC | profile | RpcRules.deployment_json | AR2, RP1a, O14, O11 | #6888 (bitcoin#23508,#24187,#24528) |
| RPC-05 | REST `/rest/deploymentinfo` | RPC | profile | RpcSurface (REST endpoints) | ZQ1, AR2, O14 | #6888 (bitcoin#25412, bitcoin#27853) |
| RPC-06 | REST `/rest/mempool/contents.json` query params | RPC | profile | RpcRules.rest_mempool_query | ZQ1, O14 | #7390 (bitcoin#26207) |
| RPC-07 | REST query-string parsing of URIs that are not valid RFC 3986 | RPC | v24-all [H → O14] | — | ZQ1, O14 | #7390 (bitcoin#27468) |
| RPC-08 | REST `getutxos` | RPC | v24-all [B → none] | — | ZQ1 | #7431 (bitcoin#25077) |
| RPC-09 | HTTP server shutdown | RPC | v24-all [B → none] | — | RP0, ND1 | #7357 (bitcoin#26742, bitcoin#28551) |
| RPC-10 | Batch JSON-RPC from users with `-rpcwhitelist` | RPC | profile (DEC-37, GPT r2) | RpcRules.batch_whitelist | RP0, PL1, O14 | #7722 |
| RPC-11 | Generic argument type checking | RPC | profile | RpcRules.arg_type_check | RP0, AR2, O11, O14, WL2b | #7715 (bitcoin#26039, bitcoin#26929), #7666 (bitcoin#26213), #26919) |
| RPC-12 | Parsing of composite-RPC arguments | RPC | profile | RpcRules.composite_args | RP0, RP2a, RP2b, RP3b, WL5, AR2, O11, PL1 | #7099 |
| RPC-13 | dash-cli client conversion table | RPC | profile | RpcSurface (CLI conversions) | AR2, AR3 | #7099, #7192, #7107, #7712, #7085, #7131, #7666, #7437, #7550, #7358 |
| RPC-14 | `dash-cli -netinfo` "v" (transport) column | RPC | profile | CliRules.netinfo_transport_column | AR3 | #7378 (bitcoin#29657) |
| RPC-15 | `-rpcdoccheck` depth | RPC | profile | RpcRules.doccheck_depth | RP0, AR2, O11 | #7502 (bitcoin#23083), #7684 (bitcoin#26887) |
| RPC-16 | `setmocktime` range | RPC | profile | RpcRules.setmocktime_max | RP1a, ND1, O11 | #7731 (bitcoin#29869) |
| RPC-17 | `mockscheduler` (hidden, regtest) | RPC | v24-all [U → none] | — | RP1a, O16 | #7124 (bitcoin#28118) |
| RPC-18 | `maxfeerate` cap | RPC | profile | RpcRules.maxfeerate_cap | RP3a, O11 | #7406 (bitcoin#29434) |
| RPC-19 | `testmempoolaccept` fees | RPC | profile | RpcSurface (results) | RP3a, MP1a, O14, AR2 | #7684 (bitcoin#26646) |
| RPC-20 | Mempool entry fields | RPC | profile | RpcSurface (deprecations) | RP3a, ZQ1, O14 | #7551 (bitcoin#25204) |
| RPC-21 | `getmempoolinfo.pendingassetunlocks` | RPC | profile | RpcSurface (results) | RP3a, ZQ1, O14 | #7639 |
| RPC-22 | Order of non-verbose `getrawmempool` | RPC | profile | RpcRules.rawmempool_order | RP3a, O14 (or an ALLOWED-DIFF), O14 | #7711 (bitcoin#29260) |
| RPC-23 | `getrawtransaction` / `getrawtransactionmulti` verbosity | RPC | profile | RpcRules.getrawtransaction_verbosity | RP1b, AR2, O14 | #7666 (bitcoin#23319, bitcoin#29003, bitcoin#26734) |
| RPC-24 | `getassetunlockstatuses` | RPC | profile | RpcSurface (results) | RP3b, PL1, O14, RP1b, RP3a, AR2, ZQ1 | #7639 |
| RPC-25 | `utxoupdatepsbt` | RPC | profile | RpcRules.utxoupdatepsbt_txindex | RP1b, WL4, O14 | #7237 (bitcoin#25939) |
| RPC-26 | Pubkey and sighash argument errors | RPC | profile | RpcSurface (errors) | RP1a, WL2b, WL4, O11, AR2, O14, RP1b | #7308 (bitcoin#28336), #7710 (bitcoin#28113, bitcoin#28162) |
| RPC-27 | `validateaddress` and address-decoding errors | RPC | profile | RpcRules.address_decoding | RP1a, WL2b, AR2, O11, O14, BS5 | #7294, #7578, #7492 (bitcoin#16807, bitcoin#23577), #7452 (bitcoin#16807, #7285 |
| RPC-28 | `getnetworkhashps` argument validation | RPC | profile | RpcRules.networkhashps_strict | RP3a, MN1, O11 | #7354 (bitcoin#28554) |
| RPC-29 | `generateblock` arg 3 `submit` | RPC | profile | RpcSurface (args) | MN0, AR2 | #7192 (bitcoin#18933) |
| RPC-32 | Error for an unknown `scantxoutset` action | RPC | profile | RpcSurface (errors) | RP1b, O11 | #7648 (bitcoin#26508) |
| RPC-33 | `gettxoutsetinfo` per-block info | RPC | profile | RpcSurface (results) | RP1b, IX1, O7, AR2, O14 | #7335 |
| RPC-35 | Address, spent and timestamp index RPCs | RPC | profile | RpcRules.index_rpc_errors | IX2, RP1b, AR2, O11, RP1a | #7101, #7547 |
| RPC-36 | `quorum dkgstatus.minableCommitments` | RPC | profile | RpcRules.dkgstatus_mineable_height | DK1, RP3b, O11 | #7770, #7808 (#7781) |
| RPC-37 | `quorum dkginfo` | RPC | profile | RpcSurface (args, results) | DK1, RP3b, O11, O16, AR2, O14 | #7428 |
| RPC-39 | `quorum rotationinfo` (RPC path, `use_legacy_construction=false`) | RPC+P2P | profile (DEC-37, GPT r2) | P2pRules.qrinfo_request | P6b, RP3b, O11, O14 | #7349, #7629, #7456, #7099 |
| RPC-40 | Legacy network fields behind `-deprecatedrpc=service` | RPC | profile | RpcSurface (deprecations: service) | RP2a, RP2b, RP3b, WL6, PL1, O14, AR2 | #7181 (#7360's structure is already in v23) |
| RPC-41 | `protx listdiff` addresses for legacy EvoNodes | RPC | profile | RpcRules.listdiff_legacy_evo_addresses | RP2a, PL1, O14 | #7808 (#7779) |
| RPC-42 | Block arguments of `protx diff`/`listdiff` and `evodb verify/repair` | RPC | profile | RpcSurface (errors) | RP2a, RP2b, O11 | #7099 (ac537fc223, 53a66ca2a4) |
| RPC-43 | ProTx wallet RPCs | RPC | profile | RpcSurface (errors) + WalletRules.protx_presign_checks | WL5, AR2, O11 | #7342, #7600, #7621, #7375, #7437 |
| RPC-44 | `masternode status` | RPC | profile | RpcSurface (results) | AM1, RP2b, PL1, O14 | #7456 |
| RPC-45 | `getcoinjoininfo` (wallet client) | RPC | profile | RpcSurface (results) | WL6, O14 | #7480 |
| RPC-46 | Deployment-gated RPC content | RPC | profile (decode JSON is ungated: instanceHash and types 10-12 payload JSON follow UngatedRules; the rest is gated (second-Opus m5)) | UngatedRules.{au_stable_txid, special_tx_max_type} (decode JSON) | RP2a, RP1b, WL5, AR2 | #7437, #7340, #7639, #7294, #7302, #7808 (#7807) |
| RPC-47 | `gobject list-prepared count` | RPC | v24-all [H → O14] | — | RP2b, WL5, O14 | #7382 |
| RPC-49 | Error precedence in `createrawtransaction`/`createpsbt` | RPC | profile | RpcRules.raw_tx_arg_order | RP1b, WL4 | #7646 (bitcoin#25344) |

### 5.10 wallet (WAL)

| id | Behaviour | Class | Keying | Home / field | Consumers | PRs |
|---|---|---|---|---|---|---|
| WAL-01 | `createwallet` defaults to descriptor wallets | RPC | profile | WalletRules.default_kind (DEC-02 conflict, see §6) | WL2b, AR2, O14 | #7204 (bitcoin#23002), #7745 (comment only) |
| WAL-02 | `dash-wallet create` defaults to descriptors | RPC | profile | WalletRules.tool_create_default | AR4 | #7204 (bitcoin#23002) |
| WAL-03 | New RPC `migratewallet` | RPC (+format) | profile | RpcSurface (methods) + dashd-wallet | WL2b, WL3, IM2, AR2 | #7275 (bitcoin#19602, #26910, #26761, #26594, #26595), #7277 (bitcoin#26740), #7808 (#7793, #7803) |
| WAL-04 | New RPC `sendall` | RPC | profile | RpcSurface (methods) + dashd-wallet | WL2b, AR2, AR3 | #7131 (bitcoin#24118, #26344, #26024, #26084, #26010), #7261, #7709 (bitcoin#25375) |
| WAL-05 | `minconf`/`maxconf` options | RPC | profile | RpcSurface (args) | WL2b | #7709 (bitcoin#25375) |
| WAL-06 | `use_cj` option | RPC | profile | RpcSurface (args) | WL2b, WL6 | #7261 |
| WAL-07 | `walletprocesspsbt` returns `hex` | RPC | profile | RpcSurface (results) | WL2b, WL4, AR2 | #7354 (bitcoin#28414) (xpkt init-node), #28980) |
| WAL-08 | PSBT signing needs a full unlock | RPC | profile (DEC-37, GPT r2) | WalletRules.psbt_sign_with_mixing_unlock | WL2b, WL4, WL6, O14, WL8 | #7663 |
| WAL-09 | Wallet-load warnings shape | RPC | profile | RpcSurface (deprecations: walletwarningfield) | WL2b, RP0 | #7711 (bitcoin#27279), #7277 (bitcoin#17261 follow-up `break`) |
| WAL-10 | `listunspent` | RPC | profile | RpcSurface (args, results) | WL2b | #7646 (bitcoin#25730, #19169), #7550 (bitcoin#25504) |
| WAL-11 | `parent_descs` | RPC | profile | RpcSurface (args, results) | WL2b, AR2 | #7550 (bitcoin#25504), #7648 (bitcoin#25934) |
| WAL-12 | `getreceivedbylabel` | RPC | profile | RpcSurface (errors) | WL2b | #7550 (bitcoin#25122) |
| WAL-13 | `-deprecatedrpc=exclude_coinbase` removed | RPC | profile | RpcSurface (deprecations) | WL2b, AR2 | #7492 (bitcoin#25171) (xpkt primitives) |
| WAL-14 | Label sanitisation | RPC | profile | WalletRules.import_label_sanitize | WL2b, AR2 | #7684 (bitcoin#26186) (xpkt consensus) |
| WAL-16 | Rescan and lock interplay | RPC | profile | WalletRules.rescan_requires_unlock | WL2b, WL2a | #7711 (bitcoin#26347) |
| WAL-17 | `encryptwallet` success string | RPC | profile | RpcSurface (results) | WL2b | #7354 (bitcoin#28980) (xpkt init-node) |
| WAL-18 | `importwallet` on a pruned node | RPC | profile | WalletRules.importwallet_pruned | WL2b, CH8, WL7 | #7666 (bitcoin#24865) (xpkt mempool-index) |
| WAL-19 | `getaddressinfo` accepts DIP-18 Platform addresses | RPC | profile | WalletRules.platform_addresses | WL2b, PL1 | #7294, #7578 |
| WAL-20 | Paying a Platform address with `sendtoaddress`/`sendmany` | RPC | profile | WalletRules.platform_recipients | WL2b, WL5 | #7294, #7577 |
| WAL-21 | Hardened-derivation marker `'` → `h` | RPC (+format) | profile | WalletRules.hardened_marker | WL2b, WL3, WL4, IM2, O14, RP1a, AR2 | #7237 (bitcoin#26076 + Dash adaptation a2a9d8c2ea), #7338 |
| WAL-22 | Legacy HD chain with no account record | RPC (+format) | profile | WalletRules.empty_hd_chain_counts_as_one | WL2b, WL3, WL1 | #7808 (#7803), #7503 (no-op rewrite) |
| WAL-23 | `sethdseed` on an encrypted (unlocked) legacy wallet | INT (format) | v24-all [B → none] | — | WL2b, WL3 | #7711 (9a55401a3e) |
| WAL-24 | New wallet DB record types | INT (format) | profile | WalletRules.extra_db_records | WL3, IM2, WL6, WL2a | #7581, #7480 (xpkt mn-overlay owns cj semantics) |
| WAL-25 | Load-time rejection of malformed or mixed-format wallet records | RPC (load errors) | v24-all [H → none] | — | WL2a, WL3 | #7646 (bitcoin#26462), #7550 (bitcoin#26021), #7383 |
| WAL-26 | `-rescan` startup option removed | RPC (CLI/init) | profile | ArgsTable + WalletRules.rescan_option | AR1, ND1, WL2a, WL7 | #7037 (bitcoin#23123, #23147) |
| WAL-27 | New `-dustprotectionthreshold=<n>` | RPC (init) | profile | ArgsTable + WalletRules.dust_protection | WL7, AR1 | #7265 |
| WAL-28 | Fast rescan | INT | profile (DEC-37, GPT r2) | WalletRules.fast_rescan | WL2a, IX1, O11, WL8 | #7085 (bitcoin#25957, #26920) |
| WAL-29 | New logging category `scan` | RPC | profile | InitRules.log_scan_category | AR1, RP1a | #7085 (bitcoin#25957) (xpkt rpc/init-node) |
| WAL-30 | Startup rescan is skipped | INT | v24-all [I → none] | — | WL2a | #7092 (bitcoin#26679) |
| WAL-31 | Wallet tx resubmission | P2P | profile (DEC-37, GPT r2) | WalletRules.resubmission | WL2a, P5, O11, WL8 | #7550 (bitcoin#25768, #26205), #7631 (bitcoin#26302) |
| WAL-32 | Coin-selection algorithm | INT (+RPC: the chosen inputs show in RPC results; GPT r2) | v24-all [I → O14] | — | WL2b, WL4, WL6, O14 | #7550 (bitcoin#25647, #26203), #7709 (bitcoin#25729, #26560), #7646 (bitcoin#26668, #25685), #7237 (bitcoin#26720, #26066), #7808 (#7796), #7631 (bitcoin#26643) |
| WAL-33 | Wallet tx-creation error strings | RPC | profile | RpcSurface (errors) | WL2b | #7709 (bitcoin#26661), #7646 (bitcoin#25685), #7711 (bitcoin#28863), #7131 (bitcoin#24118), #7631 (bitcoin#26643) |
| WAL-34 | ProTx fee-source funding | INT (+RPC: the chosen inputs show in RPC results; GPT r2) | v24-all [I → O14] | — | WL5, O14 | #7600 (xpkt rpc), #7646 (bitcoin#25685 removed `fRequireAllInputs`) |
| WAL-35 | Inputs freed by abandoning or conflicting a tx | RPC | profile (DEC-37, GPT r2) | WalletRules.reconcile_freed_inputs | WL2a, WL2b, WL5, O14, WL8 | #7617 |
| WAL-36 | CoinJoin accounting drops unconfirmable outputs | RPC | profile (DEC-37, GPT r2) | WalletRules.coinjoin_unconfirmable_filter | WL6, WL2b, O14, WL8 | #7634 |
| WAL-37 | CoinJoin rounds lookup not memoised for unknown txs | INT | profile (DEC-37, GPT r2) | WalletRules.coinjoin_rounds_memo_unknown | WL6, O14, WL8 | #7636 |
| WAL-38 | CoinJoin rebalance, wallet side | P2P (CJ client) | gated | — | WL6, CJ0 | #7052 (xpkt mn-overlay), #7808 (#7794) |
| WAL-39 | Address-book labels for multi-script descriptors | RPC | profile | WalletRules.label_all_descriptor_scripts | WL2b | #7275 (bitcoin#19602) |
| WAL-44 | `-loadblock` with pruning | INT | v24-all [I → RL2] | — | CH5, CH8, RL2 | #7237 (bitcoin#24957) |
| WAL-45 | `-fastprune` | INT | v24-all [B → none] | — | ST2 | #7237 (bitcoin#27191) |
| WAL-46 | Descriptor parse and inference | RPC | profile | WalletRules.descriptor_reject_hybrid | WL4, AR2 | #7711 (bitcoin#28602, #28067 partial) (xpkt rpc), #7356 (bitcoin#28587) |

### 5.11 Declared deviations (`v24-all` rows, by policy)

| Row | Behaviour | Policy | Verified by | Why it is unreachable or unobservable |
|---|---|---|---|---|
| RPC-08 | REST `getutxos` | B | none | Race elimination: v24's coherent tip and UTXO snapshot is one of the responses v23 can give, and no valid client can require v23's inconsistency. |
| RPC-09 | HTTP server shutdown | B | none | Shutdown lifecycle only: waiting for in-flight replies yields an outcome v23 can also produce. Method and result contracts are unchanged. |
| SIG-20 | DKG contribution verification, empty input | B | none | Contribution verification sets are identical for every non-empty input. Only the empty post-filter path differs, where v23 can leave a future unfulfilled; that is visible only to a deliberately arranged fault. |
| WAL-23 | `sethdseed` on an encrypted (unlocked) legacy wallet | B | none | v23's sethdseed path writes a plaintext HD seed into an encrypted wallet that neither release can reload. The importer reports such files as corrupt rather than recreating the broken writer. Successful RPC result shapes are unchanged. |
| WAL-45 | `-fastprune` | B | none | With -fastprune, v23 loops without bound on a block of 64 KiB or more, allocating until it runs out of memory. A non-terminating branch is not a behaviour to reproduce. |
| CONS-26 | DYNBITSET pre-allocation bound | H | O13b | Accepted and rejected bytes are identical: the new check rejects only bytes the old read must reject a few bytes later, with the same exception type and peer handling. Only allocation and timing differ, which only stress probes can measure. |
| CONS-30 | `/wallet/<name>` URL decoding | H | O14 | Only a wallet name containing an encoded NUL (%00) differs. Wallet files cannot contain NUL, and every well-formed name decodes identically. Covers P2P-39 (same decoder). |
| DKG-04 | Pending-DKG retention quota | H | O13c | An abuse-quota exception. Canonical, unique honest DKG messages stay below the shared 2×size cap under both keyings (NodeId vs proTxHash, charge-before vs after the duplicate check). Only a verified MN that floods duplicates or reconnects to reset its quota can see the difference. |
| RPC-07 | REST query-string parsing of URIs that are not valid RFC 3986 | H | O14 | Only an RFC 3986-invalid URI differs: v23 dereferences a null parse result, v24 answers HTTP 400. Correct REST clients see identical behaviour; the exception covers only the null guard. |
| RPC-47 | `gobject list-prepared count` | H | O14 | Only counts that make v23's mixed-width iterator arithmetic undefined differ; every count with a defined v23 result returns the same object sequence. |
| SIG-11 | ISDLOCK input consistency | H | O13b | Requires a quorum-signed ISDLOCK whose inputs differ from the locked tx. An ordinary peer cannot produce one without breaking quorum signature verification, and honest quorums sign the inputs in vin order. The exception assumes honest signing. |
| WAL-25 | Load-time rejection of malformed or mixed-format wallet records | H | none | Applies only to malformed or mixed-format wallet files: corrupt HD-chain records and undeserialisable descriptors (a v23 abort becomes a v24 load error), and legacy records inside a descriptor wallet (v23 tolerated some, e.g. a valid defaultkey). Wallets written by either release's descriptor code contain no legacy records. |
| CONS-22 | Node stops on low disk space or a coins read error | I | RL2 | Differs only under disk exhaustion (below 100 MiB free) or a coins-DB read error. Healthy P2P or RPC input cannot reach it; clients see only process availability under that fault. |
| P2P-30 | PID file removal | I | none | PID-file ownership after a partial initialisation is process bookkeeping, not part of any peer or RPC contract. On normal operation both releases own and remove their PID file. |
| WAL-30 | Startup rescan is skipped | I | none | Both releases skip transactions older than the earliest key time minus the same window. v24 skips only a startup rescan that would find nothing under that shared model. The explicit full-rescan option (WAL-26) stays profiled. |
| WAL-32 | Coin-selection algorithm | I | O14 | Coin selection is randomised and unspecified in both releases, so only the inputs, change and fee of a given selection differ. Explicit-input options, errors, fee units and fully-mixed requirements are profiled elsewhere. |
| WAL-34 | ProTx fee-source funding | I | O14 | Both releases fund only from the requested fee-source address and produce v23-valid transactions; neither promises a particular input subset. Source-address, error and signing semantics are profiled (RPC-43). |
| WAL-44 | `-loadblock` with pruning | I | RL2 | Pruning during -loadblock changes only intermediate progress and disk use. The final chain state and the validation results of imported blocks are identical. |
| EVO-18 | CbTx `bestCLHeightDiff` range | R | O10 | Bounded proof (GPT r2). For any valid parent after mainnet's first non-null coinbase ChainLock, both releases enforce non-null and child_diff ≤ parent_diff + 1, so the new range and ancestor guard cannot fire. Equivalence holds for extensions and reorgs above that history. The first-candidate and corrupt-parent cases keep v24's null-ancestor guard as a malformed-input exception; a null dereference is not reproduced. |
| DKG-14 | Custom LLMQ size cap | U | O11 | The custom LLMQ size options exist only in regtest and devnet params construction; mainnet quorum sizes already fit 400. Under -releaseprofile=23.1 on regtest the v24 refusal applies as a declared debug-option limit. |
| MPIX-03 | Default of `-acceptnonstdtxn` / require-standard | U | none | Mainnet and regtest defaults are identical: standardness is always required, and mainnet refuses -acceptnonstdtxn=1. Only the testnet and devnet defaults differ, and those networks are pinned to v24, with no v23 override outside regtest. |
| RPC-17 | `mockscheduler` (hidden, regtest) | U | none | mockscheduler refuses non-mockable chains before forwarding in both releases, so it is unreachable on mainnet. On regtest the added synchronisation yields one of the schedules v23 can produce. |

### 5.12 Merged duplicates (dropped row → kept row)

| row | kept as | note |
|---|---|---|
| CONS-05 | SIG-01 |  |
| CONS-08 | DKG-07 |  |
| CONS-13 | RPC-33 |  |
| CONS-14 | RPC-19 |  |
| CONS-15 | WAL-14 |  |
| CONS-16 | WAL-46 |  |
| CONS-17 | RPC-27 |  |
| CONS-18 | RPC-27 |  |
| CONS-19 | WAL-13 |  |
| CONS-20 | RPC-26 |  |
| CONS-27 | MNO-16 |  |
| CONS-29 | WAL-21 |  |
| DKG-15 | RPC-37 |  |
| DKG-16 | RPC-01 |  |
| EVO-03 | MPIX-16 |  |
| EVO-09 | CONS-06 | keying v24-all → profile (the kept row's keying wins: one type bound, a cheap reject-path difference) |
| EVO-19 | DKG-08 |  |
| EVO-20 | MNO-12 |  |
| EVO-23 | CONS-03 |  |
| EVO-24 | P2P-18 |  |
| EVO-26 | MPIX-13 |  |
| EVO-27 | MPIX-14 | keying v24-all → profile (observable in mempool contents and ZMQ; one flag) |
| EVO-28 | RPC-40 |  |
| EVO-29 | RPC-41 |  |
| EVO-31 | SIG-19 |  |
| EVO-32 | P2P-36 |  |
| EVO-33 | P2P-34 |  |
| EVO-34 | MNO-11 |  |
| EVO-35 | MNO-02 |  |
| MNO-05 | P2P-14 |  |
| MNO-10 | P2P-21 | keying v24-all → profile (honest peers see the extra GETDATA; one flag) |
| MNO-13 | EVO-22 |  |
| MNO-14 | EVO-21 |  |
| MNO-15 | P2P-03 |  |
| MPIX-06 | P2P-35 |  |
| MPIX-09 | RPC-35 |  |
| MPIX-20 | SIG-13 |  |
| MPIX-23 | RPC-23 |  |
| MPIX-24 | RPC-11 |  |
| MPIX-25 | WAL-18 |  |
| P2P-19 | MNO-11 |  |
| P2P-22 | MPIX-01 |  |
| P2P-23 | MPIX-03 |  |
| P2P-26 | RPC-26 |  |
| P2P-27 | RPC-28 |  |
| P2P-28 | WAL-07 |  |
| P2P-39 | CONS-30 | same URL decoder and PR (#7733 vs #7085 backports of the same change; GPT r2 accuracy finding 1) |
| RPC-04 | CONS-02 |  |
| RPC-30 | MNO-02 |  |
| RPC-31 | MNO-01 |  |
| RPC-34 | P2P-25 |  |
| RPC-38 | SIG-16 |  |
| RPC-48 | P2P-18 |  |
| SIG-03 | EVO-02 |  |
| SIG-04 | EVO-05 |  |
| SIG-05 | EVO-06 | keying profile kept on EVO-06 (overridden from v24-all: a cheap reject-path difference) |
| SIG-06 | MPIX-16 |  |
| SIG-08 | MPIX-18 |  |
| SIG-09 | P2P-04 |  |
| SIG-21 | RPC-24 |  |
| WAL-15 | RPC-26 |  |
| WAL-40 | RPC-11 |  |
| WAL-41 | RPC-22 |  |
| WAL-42 | RPC-01 |  |
| WAL-43 | RPC-25 |  |
| WAL-47 | RPC-32 |  |

## 6. Decisions and follow-ups

### 6.1 Decisions (all taken)

1. **Keying policies and the `v24-all` list.** DEC-34(b) held the 65 `v24-all` rows until the GPT review; DEC-37 moved the 42 disputed rows to `profile` and approved the rest. After the CONS-30/P2P-39 merge, 22 distinct rows remain `v24-all` under policies H, B, U, R and I (§2), each with its argument in §5.11.
2. **DEC-02 versus WAL-01/-02** (DEC-34a). Under the v23.1 profile the node wallet keeps v23.1.8's legacy-wallet default (`WalletRules.default_kind = Legacy`); descriptor wallets stay the default under v24. A wallet UI (the desktop app, `rdash-wallet`) can still ask for descriptors explicitly.
3. **Platform's dependency on `getblockchaininfo.softforks`** (RPC-02, rpc packet §4.1; DEC-34d). drive-abci (init, `mn_rr`), rs-dapi, rs-sdk and dashmate's status deserialize `softforks` as a required field; v24 emits it only with `-deprecatedrpc=softforks`, and dashmate's `dash.conf.dot` does not set it. This breaks Platform on any v24 network with stock dashd. It went to pasta as an FYI; rdashd reproduces dashd exactly, flag included.
4. **Mainnet masternode pilot (PLAN §10 Q1, §9 risk "O17 finds > 10 more MN-relevant rows").** The v23.1.8 masternode work is A1's rows 6, 7, 9–17 and 21: SIG-12/-13, MNO-01..-03, P2P-14, P2P-05..-13, P2P-18, DKG-01, DKG-03/-05/-09..-13, P2P-02, SIG-15 and P2P-35/-36. Masternode-behaviour rows beyond A1: **8**. SIG-16 (Platform recsigs per msgHash) and SIG-17 (sig-share recovery target) are observable on mainnet; SIG-19 (EHF signing window) only on regtest; DEC-37 adds DKG-06 (startup duties) and the CoinJoin server rows MNO-17..-20. Masternode-adjacent: P2P-20 (CL seen-cache), MNO-06/-07/-08/-11 (governance), INTG-01 (DSTX), SIG-14 and SIG-18 (every node). **The risk trigger does not fire, but it is close**; PLAN §9's row now says so.
5. **`Effect::BlsSchemeLeak` is kept (a departure from DEC-34(c), for the manager to confirm).** DEC-34(c) listed dropping it, on this report's earlier argument that CONS-09 was unobservable under the mainnet profile. GPT r2 refuted that argument (level-3 `verifychain`), DEC-37 keyed CONS-09 `profile`, and the effect is how A1 models v23.1.8's leaked flag. Proposed: a note on DEC-37 (or an amendment to DEC-34(c)) saying the effect stays.

### 6.2 PLAN edits (applied)

**Schedule.** `tools/cpm.py` was re-run with the size changes and the new task below. No milestone moves: M4 ≈ wk 34.5, M5 exit ≈ wk 41.5 and RL4 ≈ wk 49.3, the same as before. The two size changes nearest the critical chain keep slack: IX2 finishes at wk 36.0 against M5 exit's wk 40.0 input, and QM1b at wk 34.5 against O12b's wk 37.0. EVO-14's decoder parameter is a new S task, BS6c, after BS6a; EV1 takes it as an interface dependency, so BS6a (zero slack) is unchanged. Folding it into BS6a would put work on the chain without a size change, and making it a merge dependency of EV1 would cost 0.5 weeks. O13b absorbs eight more probes at zero slack for M5 exit; PLAN §9 names that risk. O13c and WL8 are outside the model (nothing in it depends on them).

**Size changes.**

| Task | Size | Why (rows) |
|---|---|---|
| IX2 | M → **L** | Two index lifecycles (`IndexRules.dash_index_model`). v23: enabling an index on an existing datadir needs `-reindex`, any of the three forces `-checklevel=4`, and `-prune` works (pin undo with a prune lock until indexed, or index at connect). v24: background catch-up, `-prune` refused with address/spent, timestampindex fails on already-pruned data. IX2 owns these init checks; ND1 only wires them (MPIX-08, MPIX-10) |
| QM1b | M → **L** | QGETDATA decoding, unregistered-type score, tracking and rate limits in both forms (DKG-09..-11), the `-watchquorums` observer's DKG push handling (DKG-05, with DK1), the startup hook (DKG-06) |
| O10 | M → **L** | Paired oracle witnesses for eight new `UngatedRules` rows: two-index wrapped range (CONS-10), warmed quorum-root caches across a changed-commitment reorg (DKG-08), archival level-3 `verifychain` BLS state (CONS-09), level-4 old-budget verification (MNO-12), v3 ProTx layout (EVO-14), multi-fault ProTx precedence (EVO-16), pre-DIP3 bootstrap rejection through `testmempoolaccept` (EVO-17), AU window class (MPIX-16) |
| O13c | M → **L** | Nine masternode-viewpoint probes per profile: DKG intake (DKG-03), restart duties (DKG-06), QWATCH + QGETDATA (DKG-09..-11), CoinJoin session replies (MNO-17..-20) |

**New tasks.**

| ID | Task | MS | Crates | Deps | Size | Risk | Why |
|---|---|---|---|---|---|---|---|
| BS6c | v23.1 ProTx payload layout as a decoder parameter (O17 EVO-14): version-3 ProRegTx/ProUpRegTx decoded with one `scriptPayout` (v23.1) or the payout list (v24), and DomainPort strictness | M2–M3 | base-sdk primitives | BS6a; EV1 consumes it as `i:BS6c` | S | CON | Keeps BS6a's size and the critical chain unchanged |
| CH7c | v23.1 quorum-root cache semantics (`UngatedRules.quorum_root_cache`, DKG-08): process-wide caches with v23.1.8's keys and lifetime across connect, disconnect and template calls; the v24 variant invalidates on every mined-commitment write | M5 | dashd-chainstate, dash-evo::cbtx | CH7b, EV4 | S | CON | Canonical replay (REL-1.1) never exercises it, so it stays off the critical chain; RL5 needs it |
| WL8 | v23.1-profile wallet behaviours: resubmission (WAL-31), PSBT signing under a mixing-only unlock (WAL-08), freed-input reconciliation (WAL-35), CoinJoin unconfirmable filter (WAL-36), rounds memo (WAL-37), CoinJoin pending locks (MNO-21), no fast rescan (WAL-28) | M6–M7 | dashd-wallet | WL2a, WL6 | M | OTH +SEC | WL2a, WL2b and WL6 are L already; their v23 variants are one reviewable unit |

**Task notes added** (size unchanged; the spec's §10 is each brief's field list):
- **EV1:** ProTx check order (EVO-16), pre-DIP3 reject string (EVO-17), AU window failure class (MPIX-16), the decode rule from BS6c. **EV3:** asset-unlock index membership (CONS-10), old-budget gate (MNO-12).
- **CH3b:** persisting ChainLock-conflict flags (CONS-11); disconnected transactions pass to the mempool's disconnect pool through `MempoolSink` (CONS-25). **CH5:** the level-3 `verifychain` BLS leak (CONS-09) and level-4 old-budget checks (MNO-12). **CH7a:** BLS scheme scope per profile, now on mainnet too (CONS-09).
- **MP1a:** reads the AU window class (MPIX-16) besides its `PolicyRules`. **MP1b:** disconnect-pool accounting as v23's logical budget (CONS-25). **MN1:** per-package special-tx checks (MPIX-21).
- **P4:** block read versus prune race (P2P-17), keeping a common I/O-fault fallback; unsolicited-object scoring with its grace window and type binding (P2P-12, also P5a). **P5:** A1 row 9's constants corrected (P2P-05/-06); the legacy downloader's global erase stamp (P2P-07) is part of the v23 behaviour; P2P-08, -09 and -11 come with the same enum. **P6b:** keeps "QRINFO caps per profile" (O17's earlier `v24-all` proposal is withdrawn) and adds the missing-block score (DKG-13).
- **SP0:** the v23 spork table and multi-key signer model are needed for O11 even though mainnet hard-codes every spork; SP0 implements that hard-coding under both profiles (every spork 0 = active, SPORK_21 = 1), so a mainnet SPORK_3 of "off" can never split consensus; `sporks.dat` is import-only (MNO-04).
- **LQ4a/DK1:** DKG-01 needs P2a to record each connection's VERSION `fRelay` and services. **DK1:** the observer's push handling (DKG-05) and the startup hook (DKG-06) besides its per-profile intake (DKG-03). **LK1b:** batch handling of a missing quorum (SIG-14). **SG1a:** non-masternode recsig relay (SIG-18). **AM1:** provides the MN init refusals (`InitRules.mn_init`, P2P-35).
- **CJ1:** five server switches (INTG-01, MNO-17..-20); if CJ0's pure state machine holds the admission logic, it takes them as parameters. **GV1b:** rejected-vote records (MNO-08). **IX1:** rewind on disconnect (MPIX-11).
- **ND1:** `InitRules`: the ten switches of P2P-24/-25/-29/-33..-38 and WAL-29, plus `-maxsigcachesize` sizing (CONS-23), the post-init abort exit status (P2P-31) and the initial-tip fan-out (DKG-06). **RP0:** batch authorisation under `-rpcwhitelist` (RPC-10). **RP3b:** the `quorum rotationinfo` cap (RPC-39).
- **O11:** each tag's functional tests need that tag's profile; the v24 framework calls v24-only RPCs (`getdeploymentinfo`, `quorum dkginfo`); asserted debug.log lines change from `ERROR: …` to `[error] …` (bitcoin#29236 partial) and in a few other places (WAL-28, P2P-25); the v23.1 framework runs `sporkupdate SPORK_3_INSTANTSEND_BLOCK_FILTERING 0` in every masternode setup.
- **O13b:** eight more probes (DKG-05, -12, -13, INTG-01, MPIX-16, P2P-17, SIG-14, SIG-18). **O14:** fixtures for eleven more rows (CONS-11, MNO-08, MNO-21, MPIX-11, MPIX-21, RPC-10, RPC-39, WAL-08, -35, -36, -37); seed `compat/ALLOWED-DIFFS.md` only with §5.11's O14 list (CONS-30, RPC-07, RPC-47, WAL-32, WAL-34) and the ordering note on RPC-22; run both pins with the same `-deprecatedrpc` set. **O15:** a DKG-08 or CONS-25 divergence from the v23.1.8 reference is an rdashd bug, not a known v23 defect, given the same process history since start; O15 records restarts on both sides.
- **CS4:** "MNHF valid bits per profile" is removed (A1 row 3 refuted, CONS-03); CS4 needs only both pins' deployment tables from BS5.
- **O17:** the output is `designs/O17-pin-delta.md` (M0 exit 8 now names it; a pointer stub stays at the research path), and the consumer list is every task in §5's Consumers column.
- **§9 risk "`ReleaseProfile` grows beyond estimate":** O17 sized all 170 `profile` rows; the trigger does not fire (8 new MN-behaviour rows, §6.1 item 4).
- **`-releaseprofile=23.1` is regtest-only** (spec §5); the U-policy rows assume that no devnet runs the v23 profile.

### 6.3 A1 edits (applied)

1. §2.25's struct is replaced by the layered design in `O17-release-profile.md` §3–§4 (DEC-24 D3): `Release` and `UngatedRules` live in `dash-consensus`, runtime groups in their owning crates, and `dashd_node::ReleaseProfile` is the aggregate. The `rpc: &'static RpcSurface` field is removed; `dash_rpc_spec::surface(Release)` replaces it. Appendix A follows.
2. §2.25's table now points to this report (§3 and §5) and keeps A1's row numbers only as the reconciliation key. `consensus.mnhf_valid_bits` (row 3) is dropped. **`Effect::BlsSchemeLeak` is kept** (DEC-37 keys CONS-09 `profile`; GPT r2 §8) and now covers a successful level-3 `verifychain` too (§2.10.1, the §1.4.1 table). Rows 14 and 15 stay `profile`. The row 9, 18 and 20 texts are corrected per §3.
3. The parity paragraph (item 1) is amended as in §3.
4. §4.10 checklist entries that are profile-dependent (p2p-init Notes):
   - #2: MN service bits may lack `NODE_COMPACT_FILTERS` under v23 with an explicit `peerblockfilters=0`;
   - #14: notfound limit;
   - #22: "silence" on block-data-unavailable is v24-only (DKG-13);
   - #23: "+10 when unrequested" is v24-only;
   - #28: "≤ 3 verified inbound legs" is v24-only;
   - +A: P2P-05..-11.
5. §2.18 init: the option table is per release (`dash_args::options(Release)`). Examples: `-minsporkkeys` and `-rescan` exist only in v23; `-allowignoredconf`, `-acceptstalefeeestimates` and `-dustprotectionthreshold` only in v24.
6. **`-releaseprofile=23.1`** (spelled `23.1` or `24.0`, not a tag) is regtest-only in §2.14, §2.25 item 4, §4.2 and the func-compat paragraph.
7. **CS0 alignment** (spec §3; CS0's design is not edited here, the manager forwards these):
   - `Release::V23_1_8` → `V23_1` and `UngatedRules::V23_1_8` → `UngatedRules::V23_1`, since variants name release lines; `Release` derives `Hash` and gains `ALL`, `name()` and `parse()`;
   - `header_time_source: HeaderTimeSource` → `time_source: TimeSource`, and the `ConsensusCtx.header_check_time` comment reads `rules.time_source`;
   - drop the CS0 §8.2 row-3 (MNHF valid bits) comment;
   - `default_for` takes `dash_params::Network`;
   - `Effect::BlsSchemeLeak` stays (not dropped);
   - `UngatedRules` field names fixed now so they don't drift (the owning tasks add them): `bls_scheme_scope`, `asset_unlock_index_membership`, `protx_payload_decode`, `protx_check_order`, `quorum_root_cache`, `old_budget_check`, `au_window_failure_class`, and `reject_strings.special_tx_before_dip3`.
   - Related, outside CS0: CS1's design names its script policies `ScriptPolicy::V23_1_8` and `ScriptFlags::MANDATORY_V23_1_8`, and says MP1a and CH3a pass "`ReleaseProfile.policy`'s value". Under the spec they become `V23_1` and come from `UngatedRules.mandatory_script_flags`.

### 6.4 Import, downgrade and file-format hazards (WL3, IM1a/b, IM2, CH6a/b, RL2)

| Row | File | Hazard | Handling |
|---|---|---|---|
| MPIX-08 | `blocks/index` address/spent/timestamp rows → `indexes/*` | v24 migrates and **erases** the legacy rows. A v23.1.8 restarted on that datadir with the index flag still true serves empty results with no error | CH6 reads both layouts; RL2 documents the one-way move. rdashd never writes Core's layout; the startup semantics (`-reindex` needed to enable, `-checklevel=4` forced) follow the profile (IX2) |
| WAL-21 | descriptor records | v24 stores `h`-marker descriptors whose IDs hash the `h` form; v23.1.8 recomputes them over `'`, so v23 may duplicate records and reset indexes | WL3 keys records by the stored ID; IM2 writes `'` under the v23 profile. Confirm with an O11 downgrade test |
| WAL-22, WAL-23 | `hdchain` | v23 writes an empty account map, and after `sethdseed` on an encrypted wallet a plaintext chain that neither pin can load | WL3 accepts the first; reports the second as corrupt |
| WAL-24 | `platform_data`, `cj_pending_obs` | v24-only records; v23 ignores them ("unknown records") | WL3 reads and preserves them; IM2 omits them under the v23 profile |
| MNO-04 | `sporks.dat` | v2 (nested per signer) vs v3 (flat); each pin loads the other's file as empty | IM1b reads both; harmless (refilled from peers). Keyed `none`: rdashd keeps its own spork store |
| MNO-06 | `governance.dat` | Same version string; v24 writes the invalid-vote field empty | IM1b reads both unchanged |
| P2P-29 | `settings.json` | v24 adds a `_warning_` member; v23 logs it as unknown | IM1a ignores the member |
| RPC-33 | coinstatsindex | v24 drops the `bip30` field; DB compatibility between pins is unconfirmed | IX1 follow-up |
| CONS-10 | EvoDB credit-pool index set (`cpm_S`) | Under v23.1.8, re-adding an index inside a stored range ending at 2^64−1 succeeds (`Contains` misses it) and inserts an overlapping `[v, v+1)`; v24's strict decoder (#7592) rejects such a set. The credit pool only adds; undo restores the previous block's pool | Under `V23_1` rdashd's store keeps the set exactly as v23's `Add` builds it (overlaps included); CH6a/b and IM accept overlapping sets from v23 datadirs. EV3 owns the rule |
| MPIX-10 | config | `-prune` with `-addressindex`/`-spentindex` runs on v23.1.8 and is refused by v24 | rdashd accepts it under the v23 profile (`IndexRules.dash_index_model`, IX2) and refuses it under v24; RL2 tells operators moving a mainnet node to the v24 profile |
| — | `mempool.dat`, `fee_estimates.dat`, `peers.dat`, `mncache.dat`, `netfulfilled.dat`, blk/rev | Unchanged between the pins | — |

### 6.5 Follow-ups (not blocking)

- Re-run this audit when v24.0.0 final ships (SW-V24; DEC-23). The three scripts make it a delta of rc.3 → final.
- Confirm WAL-21's downgrade hazard with an O11 test; confirm the coinstatsindex DB question (RPC-33) in IX1.
- Platform `softforks` (§6.1 item 3) is pasta's.
- The CS0 changes (§6.3 item 7) go to the CS0 thread through the manager.
- GPT r2 §8's priority witnesses are acceptance items of O10, O13b/c and O14 (§6.2); each new `profile` rule gets a paired oracle case.

## 7. Coverage evidence

### 7.1 Inventory

`inventory.py --old v23.1.8 --new v24.0.0-rc.3` (rust-dashcore `node/o17-pin-delta`):

| Item | Count |
|---|---|
| First-parent PRs between the pins | 423: 296 touch runtime code or vendored library sources; 127 only tests, GUI, docs, build or vendored non-source files |
| Non-merge commits | 1,662 (`git rev-list --no-merges v23.1.8..v24.0.0-rc.3`) |
| Files that differ between the tags | 1,633 without rename detection (1,621 with): 646 reviewable (618 runtime plus 28 vendored library sources), 544 tests, 148 Qt, 219 build/docs, 76 other vendored files |
| Reviewable PRs flagged as possibly already in v23.1.8 | patch-id 15, reverse-applies 12, PR-number reference 44, partial 2. Each was verified against v23.1.8 source by its auditor |
| Files not attributable to a PR | 0 runtime. Two docs changed only through #7534's merge resolution; they are attributed to their release-branch commit |

**Every reviewable PR is accounted for.** `inventory.py … --report O17-pin-delta.md O17-pin-delta/*.md` reports **0 reviewable PRs not cited**. A stricter check also passes: every packet's own PRs are each cited in that packet's file, as a row or as a "Not delta" with its reason.

**Every runtime file was swept** hunk by hunk by its owning auditor (counts from each packet's §3):

| Packet | Files |
|---|---|
| consensus | 208 |
| mempool-index | 47 |
| evo | 43 |
| llmq-dkg | 50 |
| llmq-sig | 30 |
| p2p-init | 94 |
| mn-overlay | 48 |
| rpc | 45 |
| wallet | 66 |

Behaviour-changing hunks from other packets' PRs are attributed in each packet's file sweep.

**Not deltas:** each packet's §2 lists every PR without a row and the reason: already in v23.1.8 (verified with `path:line`), refactor, internal perf, logging, test/GUI/build only, help text only, or subsumed by a row.

### 7.2 Vendored library sources

The audit packets skipped vendored code. The script review caught this; the integrator then swept those sources (`O17-pin-delta/integrator.md` §2). None changes behaviour:
- **#7726, dashbls:** the library sources change only for RAII cleanup and `call_once`; computed values are identical, and the relic library is unchanged.
- **#7378, secp256k1 (bitcoin#28093):** upstream refactors; ECDSA verification results are unchanged.
- **#7358 and #7666, univalue:** a method rename.
- **#7772, gsl:** diagnostic source locations only.

`inventory.py` now classifies vendored library sources as reviewable, so the next re-pin's packets include them.

### 7.3 Integrator row

INTG-01 (`ValidateDSTX` check order) was handed over in the mn-overlay Notes but never rowed. The second-Opus review found it. It was keyed `v24-all` (H) at first and is `profile` since DEC-37 (`CoinJoinRules.dstx_check_order`).

### 7.4 Reviews

- **Second Opus** (the PLAN row's acceptance), APPROVE-WITH-CHANGES, 0 blockers, 4 major, 11 minor, 10 nits. It re-verified 36 rows against source: every code-level claim held, including all of `UngatedRules` and every A1 refutation. All findings are addressed:
  - **M1:** mainnet sporks are hard-coded, corrected in §3, §6.2 and spec §4.1/§11;
  - **M2:** policies B/U/A/I added (A retired and most B/U rows moved to `profile` by DEC-37); MPIX-12 and MNO-07 → `profile`; RPC-10, WAL-08 and MPIX-16 moved to B (all three `profile` since DEC-37);
  - **M3:** spec §6/§10 split readers from parameter providers;
  - **M4:** every `v24-all` row has a verification list (§5.11);
  - **m1–m11 and n1–n10:** row and spec edits, e.g. EVO-17 → `v24-all` (U; `profile` since DEC-37), MNO-04 → `none`, RPC-46's decode JSON → `UngatedRules`, `-releaseprofile=23.1` regtest-only, P2P-13 → `PolicyRules`, `V23_1_8` → `V23_1`, the 1,662 commit count.
- **Scripts** (code-review-validator), REQUIRES CHANGES for the next re-pin; all fixed:
  - old-only commits when the old pin is not an ancestor;
  - vendored sources reviewed;
  - `merge.py` fails loudly on malformed rows, unknown keyings or classes, and unknown decision ids, and folds duplicates' PRs into the kept row;
  - reverse-apply backport detection;
  - one shared PR-reference pattern;
  - exit code 2 for git and usage errors;
  - `AUDIT-BRIEF.md` aligned with both scripts.

  Its only finding about this audit's content was the vendored-source gap (§7.2).
- **GPT r2** (independent source review, `reviews/O17-r2-gpt.md`), REWORK: 42 of the 65 `v24-all` rows disputed, 23 retained with narrowed limits, 15 `profile` rows confirmed. It reran both scripts (exit 0; 0 uncited PRs, 0 unattributed files). DEC-37 adopted the reclassification wholesale. Accuracy findings, all applied:
  - CONS-30 and P2P-39 were an unmerged duplicate (merged; 217 distinct rows);
  - CONS-09 missed the level-3 `verifychain` path, WAL-28 the OP_RETURN raw-descriptor case, WAL-37 its RPC observability (rows corrected, marked "(GPT r2)");
  - MNO-20's witness was blocked by a common earlier check (SIGNING, not ACCEPTING_ENTRIES), CONS-11's failed-child half is derived at load anyway, and WAL-25 was not all crashes (rows corrected);
  - CONS-10's "range is empty" wording was too broad (two-index witness added);
  - MPIX-12's stale "Why v24-all" prose (superseded in the packet) and "never re-fetch" for P2P-07 (now "until the stamp is evicted", §0 finding 5);
  - `Effect::BlsSchemeLeak` must not be deleted (kept, §6.3 item 2).

## 8. Re-pin

`O17-release-profile.md` §9 is the procedure. In short:
1. Run `inventory.py` for the new pin pair.
2. Hand each packet to an auditor with `AUDIT-BRIEF.md`.
3. Merge with `merge.py` and a new `decisions.json`; regenerate spec §6 and §10 with `spec_tables.py`.
4. Check coverage with `--report`.
5. Update the profile statics.

Files: `contrib/node/pin-delta/` on rust-dashcore branch `node/o17-pin-delta`.
