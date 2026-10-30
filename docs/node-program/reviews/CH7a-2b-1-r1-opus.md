# CH7a-2b-1 r1 review (Opus): `DmlChecks` bound into `NodeEvo`

- **Branch:** `node/ch7a-2b`, head 25a495fef, base 354dead70 (4 commits, 10 files, +623/−76). Worktree `rdc-wt/ch7a2`, read only.
- **Pin:** Dash Core 375c64d2317, which is the `v24.0.0-rc.4` tag 0e7382df80d plus the test-oracle hooks (CH7 §0.1). `stm:`/`v:` citations below are at the pin. Where the tag's numbering differs, I say so.

## Verdict: ACCEPT-WITH-NITS

| Severity | Count |
|---|---|
| M | 0 |
| m | 3 |
| n | 4 |

The binding is Core's step-4 body in Core's order:
1. The V24 template-output check, on every transaction including the coinbase, with Core's "Shared collateral check failed" text.
2. The raw-`nType` skip of a type-5 `vtx[0]`.
3. `CheckSpecialTxInner`. Its reject carries Core's "Special Transaction check failed" text, and a class other than TX_CONSENSUS or TX_BAD_SPECIAL is an assert, as in Core.

Every reject is BLOCK_CONSENSUS with the tx's reason and `<what> (tx hash <txid>) <debug>`, which is `stm:941-945` and `:955-958` exactly.

The parent list is the same `resolve_list(prev, …)` that step 7 called before. Core's ProTx checks each read `GetListForBlock(pindexPrev)` and never see earlier transactions of the same block. So one resolution per block is Core's list for every transaction, and step 7's reuse is Core's `BuildNewListFromBlock` read (`stm:305` in the tag numbering).

`MemPlatformBans` and `PlatformBan::set` are `ResetPlatformBan`/`SetPlatformBan` exactly.

I found no input on which the binding returns `Ok` for a block Core rejects. Every non-Core outcome is a `NotReady` or a `Fatal`.

The three minor findings are test gaps. In each, a cheap mutant of the new binding survives, and two of those mutants are fail-open. None is a behaviour bug.

## Findings

### m-1: the step-4 coins-fault arm is untested, and its fail-open mutant survives

- **Where:** `dashd-chainstate/src/evo/ev1.rs:266` (`Err(SpecialTxFault::Coins(e)) => Err(read_failed(e))` in `DmlChecks::check_tx`).
- **Mutant:** replace the arm with `Err(SpecialTxFault::Coins(_)) => Ok(Ok(()))`. It compiles.
  - No test reaches it. Every step-4 test uses `NoCoins`, and the only ProRegTx in them (`tx(3, 1, false, 2)`) has an empty payload, so it fails with `bad-protx-payload` before `coin_script` (`dash-dml/src/specialtx/provider.rs:63`, `:131`) reads a coin.
  - `h8_is_the_covenant` covers H8's `Coins` arm (`ev1.rs:277`), but not this one.
- **Failing input:** a ProRegTx with a valid payload and an external collateral outpoint, in a block past DIP0003, over a coins view whose read fails (the test's own `FailedReads`).
  - Today the result is `Err(Fatal "coins read: …")`.
  - Under the mutant it is `Ok(Ok(()))`, so the transaction is accepted on a failed read.
- **Fix:** add a step-4 case with such a ProRegTx over `FailedReads` that asserts `Err(fatal)` with `fatal.0.starts_with("coins read: ")`.
  - If building the payload in `dashd-chainstate` is awkward, export dash-dml's ProRegTx fixture (`specialtx/testctx.rs`) under `test-utils`.
  - Asserting the arm as `Fatal` is enough; it doesn't need a real collateral.

### m-2: the raw-type test in the `vtx[0]` skip is not pinned, and on the live pre-DIP0003 path its mutant fails open

- **Where:** `dashd-chainstate/src/evo/ev1.rs:234` (`if i == 0 && tx.tx_type().to_base() == TRANSACTION_COINBASE`). The test corpus is `ev1_tests.rs:60-74` (`blocks()`).
- **Mutant:** `if i == 0 { return Ok(Ok(())) }`. It survives every test:
  - Every `vtx[0]` in `blocks()` is either raw type 5 (skipped either way) or a v1 type-0 coinbase (no payload field, so `Ok` either way).
  - The step-4 tests past DIP0003 and under V24 have the same shape.
- **Failing input:** a regtest block below DIP0003 whose coinbase is `nVersion = 3, nType = 1` (any payload).
  - Core admits that coinbase to `ConnectBlock` because `ContextualCheckTransaction`'s type rules are all under `fDIP0003Active_context` (`v:358-381`), and `CheckTransaction` has no type rule (`consensus/tx_check.cpp`).
  - At step 4, `vtx[0]` is not skipped (`stm:949` tests `nType`), so `CheckSpecialTxInner` returns `bad-tx-type-dip3-inactive` (`stm:251-253`).
  - The code gives the same reject today. The mutant accepts the block.
- **Why it matters now:** `NodeEvoHooks` is `NodeEvo<E, DmlChecks>`, and G1 lets pre-DIP0003 blocks through, so this `check_tx` is the node's live pre-DIP0003 step 4 from this slice on.
  - `chain::tests::node_evo` still binds `Ev1NotBuilt` (`node_evo.rs:98`), and its `bodies()` has the same gap.
  - Past DIP0003 the mutant is equivalent on reachable input, because `bad-txns-cb-type` (`v:376-377`) forces a special-version coinbase to type 5.
- **Fix:** add `vec![tx(3, 1, true, 0)]` (and, say, `vec![tx(3, 8, true, 0)]`) to `blocks()`. The `both_bindings_…` test then pins the skip against `pre_dip3_special_txs`, which gives `bad-tx-type-dip3-inactive` for it. Optionally add the same body to `node_evo.rs`'s `bodies()`.

### m-3: H4's new context construction is untested; mutants of its fields survive

- **Where:** `dashd-chainstate/src/evo/node.rs:410-439` (`ConsensusCtx { height: h, prev: at.prev, bls, median_time_past: prev_entry.mtp, … }`, `SpecialTxCtx { check_sigs: check, net_info: *net_info, … }`).
- **What the tests cover:** `the_dispatcher_context_is_the_attempts` pins only `dml_ctx`'s mapping from a `SpecialTxCtx` that the test builds itself. No test checks what H4 puts into that context:
  - The chainstate's `NodeEvo` tests bind `Ev1NotBuilt`, which reads only `cons.prev` and `cons.deployments`.
  - The `DmlChecks` tests build their own `cons`.
- **Surviving mutants:** `height: h + 1` (the dispatcher's `prev_height`/`dip3_enforced`), `bls: BlsCtx::LEGACY`, `median_time_past: 0`, `net_info: <other rules>`. These are cheap, and a wrong height or scheme can turn a ProTx reject into an accept once G1 lifts.
- **Relation to known gap (d):** this overlaps (d), but it does not need a chain past DIP0003. H4 builds the context below DIP0003 too, and step 1/4 receive it there.
- **Fix:** in `chain/tests/node_evo.rs`, wrap `Ev1NotBuilt` in a test `Ev1Checks` that records `cx` on its first `check_tx`. Then assert:
  - `cons.height == at.height`, `cons.prev == at.prev`, `cons.bls == attempt's H1 scheme` and `cons.median_time_past == parent's MTP`;
  - `check_sigs == check`;
  - `net_info` equals the parts' rules.

  Run it in the existing below-DIP0003 loop.

### n-1: two citations use the tag's numbering, not the pin's

- **`connect/evo.rs:623` and `evo/ev1_tests.rs:246`:** these cite `stm:944-948` for the shared-collateral reject. At the pin that reject is `stm:941-945` (the check is `:940-946`); `:948` is the "we validated CCbTx above" comment.
- **CH7-notes D54:** cites `stm:247-249` for the type-5 arm's `bad-cbtx-invalid`. That is the tag's numbering; at the pin it is `stm:272-274` (CH7 §0.1 puts every `stm:` at the pin).

### n-2: D55's "every reset is Core's no-op" holds only because the stand-in has no entries

- **What Core does:** `ResetPlatformBan` on an existing, unbanned entry is not a no-op. With the default `m_platform_ban_updated{0}` (`meta.h:47`), `SetPlatformBan(false, h > 0)` sets `updated = h` and returns true (`meta.h:73-84`). That raises the bar a later `SetPlatformBan(true, requested_height)` must clear. Core creates entries without any ban too: `GetMetaInfo` on dsq, outbound attempts and governance votes.
- **What the stand-in does:** the code is right for its scope, since it starts empty and nothing inserts. SP1b's `MasternodeMetaMan::reset_platform_ban` (`dashd-masternode/src/meta.rs:458`) already ports this exactly.
- **Fix:** reword D55 to "no entry exists in the stand-in, so each reset is Core's missing-entry no-op".

### n-3: `PlatformBan::set` is a second port of the same rule

- **Where:** `evo/effects.rs:92-106`. It ports the same rule as `dashd_masternode::meta::MasternodeMetaInfo::set_platform_ban` (`meta.rs:125`), which is already in the base through `node/sp1b`.
- **Fix:** none needed now. `dashd-chainstate` does not depend on `dashd-masternode`, and the stand-in is test-only (DEC-695). SP1c-1 should delete `PlatformBan` rather than keep two ports, and a D55 line saying so would help.

### n-4: the below-DIP0003 resolver arm returns a list where it is unreachable

- **Where:** `evo/node.rs:425-429`. With no view, the resolver returns `MnList::default_pre_dip3()`.
- **Why it is unreachable:** `DmlChecks` reads the list only after its own DIP0003 test, and step 7 only with a view. Both use the same `active_after(at.prev, Dip0003)` predicate as `view`.
- **Risk:** if that ever drifts, an empty list would turn into verdicts such as `bad-protx-collateral` instead of a refusal.
- **Fix:** `None => Err(StoreError::…)` or a `Fatal` "list read below DIP0003" would fail closed. Optional.

## The implementer's claims and known gaps

- **Step 0 is `BlockPayloads::new`:** confirmed (`ev1.rs:249-251`).
- **Step 4 order:** confirmed against `stm:937-960`.
  - The template check runs under `rules.v24` for every `i`, including the coinbase and classical transactions.
  - Then comes the `i == 0 && raw nType == 5` skip.
  - Then, below DIP0003, `pre_dip3_special_tx`. It tests `HasExtraPayloadField` and only then the DIP0003 gate, as `stm:248-253` does.
  - Past DIP0003 the order is: no payload field → `Ok`, the list, then `check_special_tx_inner`.
  - `rules` is `GetSpecialTxRules(pindex->pprev)` (`v:2711`), and `check_sigs` is `fCheckCbTxMerkleRoots`.
- **Reject texts and states:** confirmed. Both are BLOCK_CONSENSUS, with `stm:942-944`'s and `:957-958`'s formats. The class assertion matches `stm:956`.
- **`Unported` → `NotReady` naming its slice:** confirmed. It is fail-closed, and the `_` arm of `unported` cannot be reached from the dispatcher (`dash-dml/src/specialtx/mod.rs:155-161`).
- **H8 uses the EV1-3 covenant:** confirmed. `Coins` → `read_failed`. `Unported` → `Fatal`, and it cannot be reached because `shared_collateral_spends` returns only `Coins`.
- **Step 1 keeps the stand-in:** confirmed (`ev1.rs:253-260`).
  - Past DIP0003 a checked block with a raw type-5 `vtx[0]` is refused at step 1; one without it gets `bad-cbtx-version` at step 2.
  - So with `fCheckCbTxMerkleRoots` set, no block past DIP0003 reaches step 4. With it unset, G1 refuses first.
- **Parent list once per block, step 7 reuses it:** confirmed (above). The view is pinned before step 1 now. Nothing between there and the old position writes the layer, so the view is the same one.
- **`MemPlatformBans` resets only an existing entry with `SetPlatformBan`'s height rule:** confirmed against `meta.cpp:187-195` and `meta.h:73-84` at the pin. The tests cover each branch of `set`, so the `<`/`<=` and `&& !banned` mutants are killed. See n-2 for the wording.
- **Gap (a), a type-5 transaction at `i ≥ 1` → `NotReady`:** confirmed fail-closed. Core gives TX_CONSENSUS `bad-cbtx-invalid` (`stm:272-274` at the pin). G1 refuses every block past DIP0003 before H4.
- **Gap (b), no credit-pool indexes:** confirmed harmless now. dash-dml's `SpecialTxCtx` has no `indexes` field and type 9 is `Unported`, so when EV1-5 adds the field, the compiler forces the binding to fill it.
- **Gap (c), a list fault can precede Core's verdict:** confirmed fail-closed. `store_failure` gives `NotReady(capability)` or `Fatal::abort_node` and never `Ok` or `Invalid`. Core's own evodb inconsistency path is `state.Error`, not invalid (`stm:1075-1078`).
- **Gap (d), no end-to-end `NodeEvo` + `DmlChecks` test past DIP0003:** confirmed. See m-3 for the part that is cheap now.
- **Seam:** `Chainstate::node_evo` changes only `ev1: DmlChecks` (`dashd-node/src/chainstate.rs:536`) and the `NodeEvoHooks` alias (`:135`). `platform_ban: Box::new(MemPlatformBans::default())` is unchanged, so SP1c-1's swap of that line merges cleanly. `MemPlatformBans`' public field was renamed (`resets` → `infos`), and nothing outside the effects tests read it.

## What I checked and ran

- Read the full diff, and `ev1.rs`, `node.rs` H4 (`:365-600`), `connect/evo.rs` (`pre_dip3_*`, `tx_reject`), `effects.rs`, and both test files.
- Read dash-dml's `specialtx/{mod,covenant,payload}.rs` and the `coin_script` read in `provider.rs`.
- Read at the pin 375c64d2317 in `~/workspace/dash`:
  - `evo/specialtxman.cpp`: `ProcessSpecialTxsInBlock` `:881-1085`, `CheckSpecialTxInner` `:241-298`, `CheckSharedCollateral*` `:1901-1935`;
  - `validation.cpp`: `ContextualCheckTransaction` `:353-381`, the H8 site `:2740-2769`, `GetSpecialTxRules` `:2711`;
  - `consensus/tx_check.cpp`'s type rules;
  - `masternode/meta.h:28-85` and `meta.cpp:181-208`;
  - the ban reset site `deterministicmns.cpp:735-752`.
- Checked the tag against the pin. Step 4's body is identical; only the line numbers move (n-1).
- Compared SP1b's `dashd-masternode/src/meta.rs` (in the base) for the platform-ban port.
- **Not run:** no build, tests or mutants. `df -BG /work` showed 223G free, under the brief's 230G floor, so I made no reflink copy. Mutant survival in m-1 to m-3 comes from reading the tests' inputs, not from runs. The implementer's 14/14 mutants, tier 1 and bench claims are not re-verified.
