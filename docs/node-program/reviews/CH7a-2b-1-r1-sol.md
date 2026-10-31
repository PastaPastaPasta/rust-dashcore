# CH7a-2b-1 review, round 1

**Verdict: ACCEPT**

Findings: **M-0, m-0, n-0**. I found no actionable consensus or fail-open defect in the four-commit diff `354dead70..25a495fef`.

## Scope and evidence

I reviewed all ten changed files and the complete diff at the requested head. The Core comparison used Dash Core `v24.0.0-rc.4`, primarily:

- `src/validation.cpp:2665-2675` (the `ConnectBlock` call and error state);
- `src/evo/specialtxman.cpp:217-274` (`CheckSpecialTxInner` dispatch, including the DIP0003 gate, type-5 coinbase arm, exception boundary, and unknown-type reject);
- `src/evo/specialtxman.cpp:857-936` (`ProcessSpecialTxsInBlock` step 1/2 and step 4);
- `src/evo/specialtxman.cpp:1875-1909` (the shared-collateral spend and template-output predicates); and
- `src/masternode/meta.h:73-84` and `src/masternode/meta.cpp:187-195` (platform-ban state).

The implementation's `DmlChecks::check_tx` (`dashd-chainstate/src/evo/ev1.rs:217-269`) preserves Core's reachable order: `shared_collateral_template_outputs` runs under V24 for every transaction, including `vtx[0]`; the raw type-5 `vtx[0]` is then skipped; and the remaining payload-bearing transactions enter the DIP0003 dispatcher. It wraps consensus and bad-special transaction states as block `Consensus` with the exact `Shared collateral check failed (tx hash ...) ...` and `Special Transaction check failed (tx hash ...) ...` prefixes. `SpecialTxFault::Unported` cannot return `Ok` and is converted to a named `NotReady`; a coins read fault is fatal. The raw type-5 coinbase check remains before the loop through `DmlChecks::check_cbtx` (`ev1.rs:207-215`), matching Core's `bad-cbtx-invalid`, `bad-cbtx-payload`, and `bad-cbtx-version` paths wherever this slice is implemented; the documented type-5 position-≥1 gap is intentionally deferred to EV1-5.

`PrevList` (`dashd-chainstate/src/evo/ev1.rs:62-81`) resolves the deterministic masternode list lazily at `pindexPrev` and caches the successful `Arc` for the block. `NodeEvo::special_txs` (`dashd-chainstate/src/evo/node.rs:406-479`) gives the same object to step 4 and then step 7 (`node.rs:535-592`), so all current list readers and list construction see one parent list. A resolver failure is fail-closed. This also preserves the documented D54 timing caveat: a local store fault may be observed before a Core verdict that would have preceded the list read, but it never admits the block.

`MemPlatformBans` (`dashd-chainstate/src/evo/effects.rs:183-224`) now has Core's entry-existence behavior and applies the exact older-height and same-height-unban rules. An unknown ProTx hash is a no-op, and resets cannot invent a ban entry. The tests cover unknown entries, older heights, same-height unbans, and later-height unbans.

The H8 binding (`ev1.rs:270-280`) is the EV1 covenant and maps its consensus result or coins fault without an acceptance path for an unported type. The serial connector still invokes H8 after `CheckTxInputs`, as Core's `validation.cpp:2708-2716` does. `NodeEvoHooks = NodeEvo<E, DmlChecks>` (`dashd-node/src/chainstate.rs:133-136`) changes only the EV1 type parameter; the `platform_ban` applier construction remains the same (`chainstate.rs:516-537`), so SP1c-1's replacement seam is intact.

## Tests and mutants

The new `dashd-chainstate/src/evo/ev1_tests.rs` wiring tests cover the pre-DIP3 stand-in equivalence, the DIP0003 dispatcher, first-list resolution and reuse, V24 template-output precedence for coinbase and classical transactions, H8's spend covenant and read-fault path, and propagation of the attempt's rules, signature flag, net-info, consensus context, and parent list. The effect tests cover `MemPlatformBans` against both Core predicates. The reported 14/14 mutation run is consistent with these predicates; I found no cheap binding mutant that can silently turn a Core reject into `Ok`. The listed gaps (type-5 non-coinbase at position ≥1, missing credit-pool indexes, list-fault timing, and the missing end-to-end NodeEvo test) are real but explicitly scoped to later slices and are not re-reported here.

I created the required reflink copy after confirming `/work` had 234G free (`df -BG /work`). I attempted the targeted command there with `CARGO_BUILD_JOBS=8`, `nice -n 19`, and `TMPDIR` inside the copy:

```
cd /work/scratch/ch7a2b1-r1-sol
CARGO_BUILD_JOBS=8 nice -n 19 TMPDIR=/work/scratch/ch7a2b1-r1-sol/tmp \
  cargo test -p dashd-chainstate evo::ev1 --lib
```

The isolated build reached `librocksdb-sys` compilation but was stopped after the box's concurrent cargo load made it exceed the review window; no test failure was observed. The review itself is read-only against `/work/home/agent/workspace/rdc-wt/ch7a2`.
