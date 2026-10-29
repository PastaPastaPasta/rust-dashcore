# EV1 special-transaction design: Opus high review, round 2 (narrow)

Target: `node/ev1-design` at `f8a64e7d6`, `docs/node-program/designs/EV1-specialtx.md` (cited as "doc:line"). The range reviewed is `d4c52e4cb..f8a64e7d6`: fix round 2 `dd0711045` (D17–D31) and the Sol r3 nit `f8a64e7d6`.

Reference: Dash Core `v24.0.0-rc.3` (`da567d7dba`), extracted read-only with `git archive`. Core paths are under `src/`.

Other references:
- base-sdk at `dbe245f7`, for D17;
- dash-p2p at integration `7d3062b82`, for D28.

Nothing was built, and the design worktree was not edited.

Settled rulings, not reopened: D9 (EV2b owns migration), D17 (Core's definition, with no rationale text) and D27 (EV1-J replaces the CH7a/MP1a dependency).

## Round-1 items

| Item | Status | Evidence |
|---|---|---|
| M1: `key_changed` equality | CLOSED (see N0) | doc:458 and D17 (doc:973) restate `CBLSLazyWrapper::operator==` branch by branch, in Core's order (`bls/bls.h:514-535`, over `CBLSWrapper::operator==` at `bls/bls.h:77-80`). The call site is `specialtxman.cpp:1461-1465`. The Rust target matches: base-sdk `BlsLazy::eq` (`pkgs/pkc/src/bls/wrap_lazy.rs:252-285` @dbe245f7: doc comment at 252, impl at 258) and `BlsValue::eq` (`wrap_value.rs:119-124`). |
| M2: coinbase on the block path, `check_cbtx` | CLOSED | B1–B7 (doc:305-318) match `specialtxman.cpp:816-837, 853-875` line by line: the B1 guard at 816, `bad-cbtx-invalid` at 818-819, `bad-cbtx-payload` at 831, the `CheckCbTx(…, pindex->pprev, rules.v24, …)` mapping at 823-828, the B4 presence check at 834-837, the template outputs at 856-862, the skip at 865, and the inner check at 871-875. The `fScriptChecks` binding is at `validation.cpp:2659-2664`, and rollforward passes `false` at `:5197-5200`. Pre-DIP3, `ContextualCheckTransaction` accepts any `nType` (`validation.cpp:356-379` only checks types when DIP3 is active), so the pre-DIP3 vector is reachable. §2.4 exports `check_cbtx`, and §6.2 is narrowed to non-coinbase type 5. |
| M3: per-element order and pairwise first reject | CLOSED for the tables. New defects in the generator rule: N2 and N3 | §3.14 (doc:629-661) matches `providertx.cpp:25-130` step for step, including the payout loop at 34-55, the share loop at 77-115 with both scripts inside it, and the order of the post-loop checks at 118-129. §3.7 rows 8–9b (doc:484-489) match `specialtxman.cpp:1717-1746`. |
| M4: bare unknown ProTx version | CLOSED | D6/D20 (doc:962, 976). Types 1–4 bail out after `nVersion` (`providertx.h:167-171, 274-278, 338-342, 407-411`). Types 10–12 have no bail-out, so restricting the vectors to types 1–4 is correct. `GetTxPayload` accepts an empty stream (`evo/specialtx.h:20-29`). v > max gives `bad-protx-version` at `specialtxman.cpp:1113-1116`. v = 0 gives `bad-protx-version` in `IsTriviallyValid` (`providertx.cpp:267-268` and the matching sites for types 2–4). |
| m1: mempool rules from the tip | CLOSED | doc:138-141, the MP1a row, and D21. The citations are exact: `validation.cpp:1103` (`GetSpecialTxRules(tip)`), `:822` and `:825` (`check_at(chain[first_height] / [last_height])` with the captured `rules`), and `:1400` (replacement: tip, tip rules, `CoinsTip`, `check_sigs = true`). |
| m2: three BLS schemes | CLOSED | doc:217-225 and D22. The eager fallback with `Reset()` is at `bls/bls.h:183-201`. ProUpServ and ProUpRev decode the signature under `nVersion == LegacyBLS` (`providertx.h:301, 419`), with no signature check in either `IsTriviallyValid`. So "legacy-only canonical → accepted with `check_sigs = false`" holds. |
| m3: preimages over non-canonical wire forms | CLOSED. Nit n2 on the "differs" assertion | D23 (doc:860, 979). `MakeSignString` hashes `SerializeHash(*this)` (`providertx.cpp:375-395`). `MakeSharedRegConsentHash` streams the decoded `netInfo` (`:357-358`). `ExtNetInfo::Unserialize` decodes into the map (`netinfo.h:422-434`). |
| m4: converter ownership | CLOSED | `convert_for_version` is gone from §0, §1, §2.4, §2.6, §3.16.1, §5.1 and D16. No stale reference remains (checked with grep). D24 makes the EV2b test compare two implementations. |
| m5: Core throws behind seams | CLOSED. Nit n3 | §2.2 (doc:113-133), D11 and D25 match the inner catch at `specialtxman.cpp:267-271`. The storage deviation is recorded. |
| m6: slice dependencies | CLOSED. N4 and nit n4 | D26–D30 and §7. The D28 citations are exact at integration `7d3062b82`: `netcore.rs:1436` `split_host_port`, `dial.rs:235` `is_bad_port`, `dial.rs:389` `lookup_numeric`, and `local.rs:192` `maybe_flip_ipv6_to_cjdns`. See n1 on the lookup's contract. |
| m7: nits | CLOSED. New defect N1 in the f8a64e7d6 vector | The voting key is in the signature (doc:648). Ancestor above `prev` returns `None` (doc:182). The zero divisor is labelled defensive (doc:494, 911). On 6c: `GetPrimary()` is `CService{}` (`netinfo.cpp:343-349`), the `ExtAddr` return is at `providertx.cpp:233-241`, the mainnet ports check is at 243-250, the default-port check at 251-256, and the comparison at 257-260. All match doc:697-699. |

## New findings

### N0. D17 state matrix (§6.1, doc:855)

Possible security note: see manager.

### N1 (minor). D31 vector (a)/(b) uses ProReg version 1, which Core rejects before `CheckProviderNetworkFields`

**Doc:** doc:857 (a) and (b) "version 1 or 2"; doc:699; D31 (doc:987).
**Core:** `CProRegTx::IsTriviallyValid`, `providertx.cpp:270-272`: `nVersion < BasicBLS && nType == Evo` gives `bad-protx-evo-version` (C). That runs inside `GetValidatedPayload` (`specialtxman.cpp:1117`), long before `CheckProviderNetworkFields` (`:1174`).

A v1 Evo ProReg therefore returns `bad-protx-evo-version`, not `bad-protx-platform-dup-ports`. The vector fails against `oracle_check_special_tx` as written. On the ProReg path (the only caller with `allow_empty = true`), "Evo with `version < ExtAddr`" means exactly version 2 (`BasicBLS`; `evo/types.h:17-19`).

**Fix:**
- Make (a) and (b) version 2 only.
- Optionally add a v1 Evo case that expects `bad-protx-evo-version`.
- Qualify the doc:699 wording the same way.

### N2 (minor). D19's expectation "the expected reject is step i's" contradicts Core for cross-entity pairs

**Doc:** doc:875 and D19 (doc:975).
**Core:** the loops at `providertx.cpp:34-55` and `77-115`, and `specialtxman.cpp:1724-1737`, run in (entity, step) order.

When the later-numbered step fails on the earlier entity, that step's reject comes first.

Take the round-1 M3 example under the new numbering. Share 0 violates 3e.i (P2PK refund) and share 1 violates 3a (amount < 100 DASH). Core returns 3e.i's `bad-protx-shares-payee`. D19's rule (i = 3a < j = 3e.i) expects 3a's `bad-protx-shares-amount`, which is the two-pass answer that M3 was raised to catch.

L1 still asserts equality with the oracle, so detection survives. But the stated expectation is wrong, and so is any pair-coverage accounting or L0 pair test derived from it.

**Fix:** define the expected reject as the first in Core's execution order:
- (entity index, step) inside a per-element loop;
- table step order for checks outside loops.

### N3 (minor). D19's "every ordered pair i < j" includes pairs that cannot occur and pairs whose verdicts cannot be told apart, so the pair-coverage gate is undefined

**Doc:** doc:875, D19, and the EV1-3 checkpoint (doc:943).

Three cases:
- **Unreachable steps.** `IsValidPayoutScript` is P2PKH ∨ P2SH (`providertx.cpp:20-23`), and `ExtractDestination` always succeeds on those. So 2d `bad-protx-payee-dest` (doc:639) and 3e.ii `bad-protx-shares-payee-dest` can never fire, and no pair that contains them exists.
- **Contradictory conditions.** Some pairs cannot both hold on the same entity, for example 3a's two bounds.
- **Same reject string.** These pairs give equal strings and cannot be distinguished by verdict:
  - 3e.iii / 3e.iv `bad-protx-shares-payee-reuse` (doc:657-658);
  - steps 5 / 6 `bad-protx-shares-penalty` (doc:660-661);
  - §3.7 8a / 9a `bad-prodis-payee` (doc:486-488).

**Fix:**
- Define pair coverage over feasible pairs with distinct reject strings.
- Check in a per-table list of infeasible and indistinguishable pairs, each with its Core reason, and have the census script validate it.

The same list should mark 2d and 3e.ii as unreachable. Otherwise §6.1 "Census closure" (doc:858, unchanged) and `census_matches_pin`'s "at least one L0 vector reaching it" cannot pass for those two rows. That part was already true before this range, and N3 makes it matter.

### N4 (minor). D30 contradicts the unchanged A-4 ask

**Doc:**
- D30 (doc:986) and the EV1-4 row (doc:944): legacy-flag cases are excluded, and "they do not block EV1-4's gate".
- A-4 in §8.2 (doc:996): "EV1-4 is gated on it".

**Fix:**
- Update A-4 to match D30, so that it no longer gates EV1-4. It still gates the parity claim for legacy-flag verification.
- State what L0 asserts for these cases until A-4 resolves. With no legacy verifier, L0 can only assert the fail-stop `VerifyUnavailable` (§0 item 4), not Core's verdict, so "covered by L0 only" (doc:879) overstates it.

## Nits

- **n1. D28 numeric lookup contract (doc:984).**
  - dash-p2p's `lookup_numeric` (`dial.rs:388-395`) is Core's `LookupNumeric` as used at the net.cpp sites. Its default port comes from `default_port_for(name, params.default_port)` (I2P → 0, `seeds.rs:36-41`), and it always applies the CJDNS flip.
  - NetInfo calls `Lookup(addr, /*portDefault=*/port, false)` with its own split port: `MnNetInfo` at `netinfo.cpp:321`, with no flip, and `ExtNetInfo` at `:542-543`, which flips explicitly.
  - Neither difference is reachable today. The `MnNetInfo` path is IPv4-only by `SAFE_CHARS_IPV4` (`:316`), and `ExtNetInfo` handles I2P/onion names before `Lookup` (`:520-528`).
  - Still, D28 should give the extracted signature as `(name, port_default, cjdns_reachable)`, with the port taken from the caller and the flip kept separate. Then "without the `NetCore` receiver" does not silently keep `default_port_for`.
- **n2. D23 "differs from the hash of the wire bytes" (doc:860).** As written this is vacuous or undefined:
  - the ProUpServ/ProReg payload hash excludes `vchSig` under `SER_GETHASH`;
  - the consent hash is field-wise (`providertx.cpp:344-370`).

  Define the contrast value as the same preimage construction with the `netInfo` field's wire bytes spliced in, sig excluded.
- **n3. `shared_collateral_spends` (doc:188)** returns a bare `SeamError`, which now includes `CoreThrow`. It runs outside the inner catch (CH3a/MP1a call sites). Say that it never returns `CoreThrow`, or narrow its error type to `StoreError`.
- **n4. D29 checkpoints.** The `netinfo_add_entry` migration-agreement property "joins with ProUpReg at EV1-3 and ProUpServ at EV1-4" (doc:985, 914), but the EV1-3 and EV1-4 checkpoint cells (doc:943-944) do not list its re-certification. Add it.
- **n5. D18 pre-DIP3 vector (doc:869).** "Valid v1 or v2 CbTx … accepted" depends on what `oracle_check_special_tx_block` (A-1) covers:
  - if A-1 runs all of `ProcessSpecialTxsInBlock`, then "valid" must include `merkleRootMNList` = the empty-list root, and for v2 the quorum root (`specialtxman.cpp:924-955` region, gated on `opt_cbTx` and not on DIP3);
  - if A-1 covers only the loop, say so.

  Separately, doc:318 "steps 3.8–3.10 apply to it" for `i > 0`: only 3.8 is reachable there, because `CheckBlock` rejects a second coinbase. 3.9 and 3.10 are reachable only through direct calls. That is harmless, but worth one clause.
- **n6. Outer catch.** The B-table (doc:305-316) runs inside `ProcessSpecialTxsInBlock`'s outer `catch (std::exception)` → `BLOCK_CONSENSUS failed-procspectxsinblock` (`specialtxman.cpp:993-995`), which is separate from the inner `failed-check-special-tx`. Nothing EV1 exports throws on B1–B6 (`GetTxPayload` catches, and `CheckSharedCollateralTemplateOutputs` does not throw), so this is a one-line note for CH7a, not an EV1 change.

## Verdict rationale

Every round-1 item is closed with correct Core citations. All the new defects are in the test specification:
- N1 has a wrong expected reject on one vector;
- N2 and N3 concern the D19 pair rule and its coverage accounting;
- N4 contradicts an unchanged ask.

None changes a ported verdict, reason or class, and L1's oracle-equality assertion still catches order bugs. N0 goes to the manager.

APPROVE-WITH-NITS
