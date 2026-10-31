# MG-04b slice 2b review (Sol, round 1)

Verdict: **ACCEPT**

Findings: **0** (M-0, m-0, n-0)

## What I checked

- Traced `create_standbys` → durable standby row → `export_standby` → import → `send_standby` and resend, including both full and immediate variants, penalty/fee charging and refunds, duplicate/partial-row merge behavior, foreign-share rows, and D23 maintenance exclusion. The stored bytes are checked against `BuildDissolution` before storage and checked again before sending; no path found that changes output scripts, output order, amounts, actor, fee, or variant.
- Checked the Core v24.0.0-rc.3 parity points against `src/evo/providertx_service.cpp:629-668,1230-1256` and `src/evo/specialtxman.cpp:1679-1776,1779-1803`: one collateral input with empty scriptSig, ordered non-actor outputs plus optional actor output, penalty-free height and `bad-prodis-penalty-floor`, fee/penalty ceilings, unilateral signature count and actor-owner signature, signature hash including the count, and the output/payee/fee rules. The Rust builder/checker matches these rules and intentionally checks full validity at the boundary so a full standby made early remains importable.
- Checked validity height selection (`max(list height, header tip) + 1`), immediate/full swap guard, dust refusal, current-list recheck, and the list-based settlement fallback. Registration history is required and reorged-out registrations do not settle; a shared collateral can leave the list only through the corresponding dissolution path under Core’s collateral-spend rule. The documented D87 residual remains the stated display ambiguity for a wallet with no share.
- Checked M10: export still opens a `Dissolution` lease and calls `charge_tx_signer(..., 0)`. A wrong-purpose, wrong-wallet, revoked, or unknown grant is rejected by lease issuance/signer acquisition; a zero-cap `MasternodeOp` grant is the intended valid export grant, so omitting a positive charge does not bypass the grant gate.
- Reviewed the mutant/test coverage described in the brief; no cheap survivor was found on the import or sending paths that permits incorrect funds movement.

## Tests run

From a reflink copy at `/work/scratch/mg04b-2b-r1-sol` (disk 234 GB free; load below 100 before building):

- `cargo test -p dw-protx shared::standby --all-features` — 3 passed.
- `cargo test -p dw-engine test_QT_127_export_standby_needs_the_wallets_masternode_grant --all-features` — 1 passed.
- `cargo test -p dw-engine masternodes::shared::tests::standby --all-features` — 10 passed.
