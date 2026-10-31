# EV1-3a review, round 1

**Verdict: ACCEPT**

Findings: **M-0, m-0, n-0**. I found no actionable divergence in the eight commits `1737b7e82..d63a52b1a`.

## Scope and evidence

I reviewed the complete eight-commit diff at `d63a52b1a617c1d6a9516305b99472f221624c48`, including the dispatcher, check context/results, signature and inputs-hash wrappers, shared-collateral covenant, ProRegTx and ProUpRegTx checks, shared-masternode checks, node-l1 wiring, vectors, D17 corpus/oracle, and check-layer tables. The Core comparison is Dash Core v24.0.0-rc.4 (`0e7382df80d`), primarily `src/evo/specialtxman.cpp`, `src/evo/providertx.cpp`, `src/evo/providertx.h`, `src/evo/sharedcollateral.h`, and the associated deterministic-masternode/net-info code.

The review build copy was `/work/scratch/ev1-3-rv-sol`:

```
$ git -C /work/scratch/ev1-3-rv-sol rev-parse HEAD
d63a52b1a617c1d6a9516305b99472f221624c48
$ git -C /work/scratch/ev1-3-rv-sol status --short
# clean
```

The frozen copy `/work/scratch/ev1-3-frozen` remained read-only and clean.

## Review conclusions

* `check_special_tx_inner` follows Core's no-payload acceptance, DIP0003 gate, type dispatch, and unknown-type `bad-tx-type-check` behavior (`dash-dml/src/specialtx/mod.rs:131-163`; Core `src/evo/specialtxman.cpp:217-274`). Types 2 and 4 and 5 through 9 are explicit `Unported` faults after the gate; there is no silent fall-through. The D10 catch placement preserves Core's per-type `failed-check-special-tx` boundary.
* The ProRegTx and ProUpRegTx paths preserve Core's ordering and classes/reasons for version gates, V19/V24 payload validation, network fields, internal versus external collateral, the pprev UTXO lookup, quirk 26 behavior, payout and operator-reward restrictions, uniqueness checks, inputs-hash, and signatures (`dash-dml/src/specialtx/provider.rs:80-254,257-353`; Core `src/evo/specialtxman.cpp:1208-1381,1488-1588`). The operator-key comparison correctly uses the lazy-key point equality at the D17 seam and probes both scheme encodings where Core does.
* The shared transaction implementations match Core's ordering and arithmetic for ProDisTx, ProUpShareTx, and ProUpSharedRegTx, including signature-count modes, output ordering, pro-rata floors, fee/bonus limits, payee separation, and reject classes (`dash-dml/src/specialtx/shared.rs:34-260`; Core `src/evo/specialtxman.cpp:1623-1873`). The `zip` used for shared signatures is safe in the decoded production domain because Core/base serialization enforces one join signature per share (`src/evo/providertx.h:192-206`).
* The covenant uses exact script matching and the correct consensus reason for both creation and spend (`dash-dml/src/specialtx/covenant.rs:17-80`; Core `src/evo/sharedcollateral.h:11-26` and `src/evo/specialtxman.cpp:1875-1908`). The shared-spend caller is intentionally owned by the CH3a/MP1a integration described by the design; its absence from this EV1-3a node check table is therefore not a defect.
* Node-l1's context binds pprev, buried deployment gates, V24/EVO_SHARES rules, the deterministic masternode list, and the coins view as Core expects (`node-l1/src/dml_checks.rs:664-740`; `node-l1/src/dml_block.rs:233-254`).
* The D17 matrix covers both lazy-key operand orders and the eight observable oracle cells, including same-scheme equality, legacy equality, another key, cross-scheme bytes, undecodable keys, and zero bytes (`node-l1/tests/l1_specialtx_d17.rs:226-270`). No additional constructible payload/state cell changes the verdict; the state-side forms excluded by the test are not serialized production states.
* The three final mutation survivors in the implementer's 231-mutant run are verdict-equivalent in the supported input domain: the covenant `||`/`&&` mutation still reaches the same rejection because non-ProReg/version-2 transactions do not decode a usable payload, and the two `is_scheme_migration` mutations are masked by the same-scheme uniqueness guard or the update's own unique slot for valid V24 states. This was checked against the D20 rationale and the D17 oracle rather than counted as coverage.

## Tests and checks run

* Targeted `dash-dml` special-transaction tests: **70 passed**, 0 failed, covering dispatcher vectors, payload/trivial checks, collateral and quirk 26, covenant, shared transactions, signatures, D17 equality, and rejection census.
* `TMPDIR=/work/scratch/ev1-3-rv-sol/tmp CARGO_BUILD_JOBS=8 cargo test -p node-l1 --test l1_specialtx_d17 --test l1_dml_block --all-features`: **19 passed, 0 failed** (18 `l1_dml_block`, 1 D17 oracle).
* Reviewed the reported final mutation run: 231 mutants, 202 caught, 26 unviable, 3 verdict-equivalent survivors as documented in `/work/scratch/ev1-3/notes.md`.
* Compared the implementation and expected classes/reasons against the cited Dash Core v24.0.0-rc.4 paths and checked the check-layer census/table closure.
