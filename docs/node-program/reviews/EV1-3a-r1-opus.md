# EV1-3a review, round 1 (Opus high, second reviewer)

Target: `node/ev1-3` at **d63a52b1a**, the 8 commits `1737b7e82..d63a52b1a`. Frozen copy `/work/scratch/ev1-3-frozen`, read only.
Core reference: v24.0.0-rc.4 (`0e7382df80d`), read with `git show`. Core lines below are rc.4 `src/evo/specialtxman.cpp` unless marked.

**Verdict: ACCEPT-WITH-NITS** (0 M, 1 m, 1 n).

I found no divergence from Core in the ported checks: order, class, reason, gates and the consensus/policy split all match. The one should-fix (m-1) is a test gap. It also shows that one of the implementer's equivalent-mutant claims (D20) is wrong.

## Findings

### m-1. D20's mutants (b) and (c) are not equivalent; the ProUpReg first-reject order of step 5a vs the later checks is untested

- **Ours:** `dash-dml/src/specialtx/provider.rs:53-55` (`is_scheme_migration`) and `:284-292` (the step-5a probe). The D20 claim is in `/work/scratch/ev1-3/notes.md` ("D20 equivalent mutants") and in the comment on `up_reg_dup_key_migration`, `dash-dml/src/specialtx/tests.rs:694-703`.
- **Core:**
  - `:1520-1527`: with `migrating` true and the key held under the other encoding, Core rejects `bad-protx-dup-key` at 5a.
  - Checks that come after 5a:
    - the 5b migration (`:1529-1538`);
    - the payout list (`:1542`);
    - the Evo payouts (`:1543-1545`);
    - the collateral reads (`:1547-1557`);
    - key safety (`:1558-1561`);
    - the per-encoding dup check (`:1563-1568`).
  - Core's own comment at `:1519` allows for a pre-existing cross-scheme pair.
- **D20 says:** the 1566 check "rejects with the same class and reason". That holds only when 5a is the sole fault. Once the probe is gone, any check between 5a and 1563 that also fails fires first.
- **Failing input** (reproduced in my copy, test `rv_opus_migration_probe_precedes_payout_list`):
  - masternode 1 at state version 1 holds point K under the legacy encoding;
  - masternode 2 holds K under the basic encoding;
  - the ProUpRegTx is version 2 with K, and its payout script pays masternode 1's owner key.
  - Core and the unmutated port both give TX_BAD_SPECIAL `bad-protx-dup-key`.
  - Mutant (b) `is_scheme_migration → false` and mutant (c) `> → <` give TX_BAD_SPECIAL `bad-protx-payee-reuse`.
  - With either mutant, the shipped specialtx suite passes in full; only the new test fails (71 passed, 1 failed).
  - Control `rv_opus_payout_fault_alone`: without masternode 2, the same transaction gives `bad-protx-payee-reuse`, so the payout fault really is present.
- **Fix:**
  - Add this two-fault vector.
  - Add one with a migration fault as the second fault: state version 1 with a port-22 legacy address, ProUpReg v3. Core gives `bad-protx-dup-key`; the mutant gives `bad-protx-netinfo-version`.
  - Correct D20 and the `up_reg_dup_key_migration` comment: the probe is observable whenever a later check also fails.
  - Re-score the run as 204 caught, 1 missed. (a) stands; see below. EV1-3b's D19 pairwise generator should produce this pair class: dup-key with any later ProUpReg fault.

### n-1. Quirk 26's `core` lines are rc.3-era, and its "tested" status rests on a coin lookup, not on the view binding

- **Ours:** `docs/node-program/quirks.toml:265` still cites `src/evo/specialtxman.cpp:381-385,1187`. At rc.4, `:381-385` is inside `ApplyTxToList` and `:1187` is inside `GetValidatedPayload`.
- **Core:**
  - The external-collateral lookup is at `:1245-1249`.
  - "pprev view" is a property of the caller: `validation.cpp:2669` (`ProcessSpecialTxsInBlock` with `view`) runs before `UpdateCoins` at `validation.cpp:2773`.
- **Scenario:** `quirk_26_collateral_from_pprev_utxo` (`dash-dml/src/specialtx/tests.rs:1543-1559`) passes for any caller, including one that hands the checks a view that already has the block's outputs. It shows only that a coin missing from the view gives `bad-protx-collateral`. The binding half is CH7a's, and EV1-3b's same-block L1 case covers it.
- **Fix:**
  - Cite `specialtxman.cpp:1245-1249; validation.cpp:2669,2773`.
  - Note in the register that the binding half is EV1-3b's L1. Or keep the row at `scaffold` until then. The manager already holds this question (validator m2).

## Checked and ran

**Copy:** `/work/scratch/ev1-3-rv-opus` (reflink of the frozen copy), target `/work/scratch/ev1-3-rv-opus-target`, `TMPDIR` under the copy.
- `git rev-parse HEAD` = `d63a52b1a617c1d6a9516305b99472f221624c48`.
- `git status --short` = ` M dash-dml/src/specialtx/tests.rs` (my two probe tests, appended), `?? .rv-tmp/`.
- Mutants were applied only in the copy, and `provider.rs` was restored from a saved original after each run.

**Runs** (all green except the mutants):
- `cargo test -p dash-dml --lib`: 243 passed, 2 ignored. That is 241 shipped plus my 2 probes.
- `cargo test -p node-l1 --features oracle`:
  - `--test l1_specialtx_d17`: 1/1;
  - `--test l1_dml_block`: 18/18 (327 s at load ~75);
  - `--test l1_specialtx_trivial`: 3/3.
- Mutants (b) and (c) as in m-1: each 71 passed, 1 failed, killed by the new probe only.

**Focus 1, Core parity** (every step side by side with rc.4):
- **`CheckProRegTx` (`:1208-1381`) against `provider.rs:80-255`:**
  - `GetValidatedPayload`, then V24 legacy disallowed, then the Evo/evo_shares pair (class S), then `CheckProviderNetworkFields` with `allow_empty`;
  - external collateral: GetCoin/spent/value, then dest, then PKH only (P2PK maps to `PubKeyHash`, as Core's `ExtractDestination` does);
  - internal collateral: index, then value, then shared template script or dest;
  - `IsPayoutListKeySafe` with `nVersion >= ExtAddr`, skipped for shared;
  - net-info uniqueness, with the same-collateral exemption and the unhashable-entry branch;
  - the shared and non-shared dup-key branches, then the V24 any-scheme probe with `self` = 0, then Evo `platformNodeID`;
  - `key-not-same` below DIP3 enforcement (`dip0003_enforced(prev_height)`);
  - inputs hash;
  - the external string signature only under `check_sigs`; internal collateral requires an empty `vchSig` (C); consent signatures go through `VerifyHashCanonical` with class S;
  - all classes and reasons match.
- **`CheckProUpRegTx` (`:1488-1588`) against `provider.rs:259-353`:**
  - hash (C), shared-mn (S), downgrade (C), the 5a probe, 5b `SetStateVersion` against `migration_reject` (`max(state, ptx)` target, the stored `nType`, an undefined type read as regular);
  - payout list against the state's owner key, then Evo payouts under `!evo_shares`, then the collateral read and dest (both C, "never happens");
  - key safety with `max(...) >= ExtAddr`, per-encoding dup key with the self exemption, key-not-same, inputs hash, then `CheckHashSig` (C) under `check_sigs`.
- `HasOperatorKeyUnderAnyScheme` (`deterministicmns.h:471-482`) against `build/mod.rs:339-347`: the invalid-key early-out and both slots probed.
- Version gates: V24 for types 10–12 (`too-early`, C, after `GetValidatedPayload`), DIP3 at the dispatcher (S), V19 through `version_max`.
- `CheckInputsHash` and the signature wrappers (`sig.rs:145-202`): reasons and classes as at `:1129-1155` and `providertx.h:559-566`.

**Focus 2, dispatcher** (`mod.rs:126-152` against `:217-274`):
- No payload field → accept; DIP3 → S; types 1, 3, 10, 11 and 12 → ported; types 2, 4 and 5–9 → `Unported`; anything else → `bad-tx-type-check`.
- `every_unported_type_faults` covers all 7 unported types past DIP3, with 3 payloads each, and the DIP3 reject before.
- `ported_types_reach_their_checks` covers the 5 ported types.
- Core's catch: the only ported throw site is `ProRataFloor`'s zero division (`providertx_util.cpp:88-95`, `arith_uint256 /=`), mapped to `failed-check-special-tx` (`prodis_throw`).
- **D10:** the catch row now sits as the first row of the type 2 and 4 tables, and in the `C` rows of the ported tables. That matches Core's control flow: the catch wraps only the `switch`, after the DIP3 gate, so a throw lands inside the type's own steps.
- D12's two-row tables still compare reasons exactly.

**Focus 3, D17:**
- The L0 eq half (1008 corpus pairs, both orders, serialization afterwards) and the 8 oracle cells run and pass.
- `src/bls/` is unchanged from rc.3 to rc.4 (`git diff v24.0.0-rc.3 0e7382df80d -- src/bls/` is empty), so the rc.3-probed corpus still applies.

**Focus 4, types 10–12 and the covenant, path by path:**
- **`CheckProDisTxForList` (`:1736-1847`) against `shared.rs:63-169`:**
  - input shape, actor, signature-count modes, `early` in 64-bit, `required_penalty`, payee count;
  - the per-share loop: payee, then floor, in Core's interleaved order;
  - the actor output (payee, then zero), penalty sum, fee ceiling, unilateral bonus ceiling;
  - `MakeSignHash` with the signature count;
  - signatures: all shares when unanimous, else the actor's only.
  - The wrapping sums (D5) cannot be reached from list amounts.
- **`CheckProUpShareTx` (`:1623-1677`) and `CheckProUpSharedRegTx` (`:1679-1734`):** the order matches, including the ungated any-scheme probe in type 12.
- **Covenant:**
  - `CheckSharedCollateralSpends` (`:1875-1891`): a missing coin reads as spent, and the coinbase is exempt.
  - `CheckSharedCollateralTemplateOutputs` (`:1893-1910`): `GetTxPayload` is whole-or-nothing, and an unknown-version payload is not shared.
  - node-l1 runs template outputs for every transaction before the coinbase skip, as Core's loop does (`:914-923`).

**Focus 5, mutants:**
- **(a)** `covenant.rs` `|| → &&` is equivalent. A non-ProReg or non-special-version transaction has no `ProRegTx` payload (`Tx::payload` is `None` without a payload field), so the payload arm rejects with the same `bad-shared-collateral-create`.
- **(b) and (c)** are not equivalent; see m-1.

**node-l1 wiring:**
- `CheckCtx::check_special_tx` builds a buried-only `DeploymentView`. `active_after` is `prev.height + 1 >= height(d)`, as Core's `DeploymentActiveAfter`.
- `rules.evo_shares` comes from the oracle context's deployments.
- `cx.height` is cross-checked against the replay height (`dml_block.rs`).
- `check_sigs` is false, matching the oracle call.
