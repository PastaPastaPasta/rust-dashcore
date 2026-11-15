> **O17 integrator rows.** Deltas found after the nine packets were merged: by the second-Opus review (m6) and by the integrator's sweep of vendored library sources. Same format and rules as the auditor packets.

> **Integrator note (DEC-37, after the GPT r2 review).** INTG-01 is keyed `profile` in `decisions.json`; the "v24-all because" prose in its row is superseded. Row-text corrections from that review are inlined and marked "(GPT r2)".

# O17 PIN-DELTA — integrator rows (INTG)

Pins: v23.1.8 = 728f505583, v24.0.0-rc.3 = 0364ed626e. Evidence is `path:line` at the tag, paths under `src/`.

## 1. Delta rows

| id | Difference: v23.1.8 → v24.0.0-rc.3 | Class | Keying | Both? | Profile field | Consumers | PRs | Evidence | A1 |
|---|---|---|---|---|---|---|---|---|---|
| INTG-01 | **`ValidateDSTX` check order.** v23 checks `IsValidStructure` first (failure: `DSTXValidationScore::INVALID`, +10) and only then whether the DSTX hash is already known (`NONE`, no score). v24 checks "already have" first, then the (now tip-dependent) structure check, which gives `PREMATURE` (+1) for a wrapper valid only under V24 rules one block ahead. So a peer resending a known DSTX hash inside a wrapper that fails the structure check (for example with a null masternode identity) gets +10 from v23 and nothing from v24. Honest relayers resend the same valid wrapper, so only a crafted resend sees the difference. The `PREMATURE` part is gated (MNO-16). | P2P | v24-all | no | — | P6a, CJ1, O13b | #7052 | v23: net_processing.cpp:3627-3640 / v24: net_processing.cpp:3637-3660 | new (handed from mn-overlay Notes; second-Opus review m6) |

## 2. Not deltas

| PR | Reason |
|---|---|
| #7726 | dashbls subtree bump (bls-signatures#122, #125, #131). The library sources change only in `src/dashbls/src/{schemes,threshold,legacy}.cpp` and the new `bn_helpers.hpp`: RAII guards for `bn_t` arrays, which free memory when an exception is thrown, and `std::call_once` for two static curve constants. Every computed value is the same. The relic library (`src/dashbls/depends/relic`) is unchanged. The rest is CI, bindings, the Python reference implementation and tests. |
| #7772 | `gsl::not_null` no longer re-captures a `source_location` when relocated. This changes only the diagnostic location reported when a precondition fails. |
| #7378 (secp256k1 part, bitcoin#28093) | secp256k1 subtree update. Normalisation moved inside `secp256k1_fe_equal_var` (callers drop `normalize_weak`), there is a guard for an empty point table in `ecmult_strauss_wnaf`, and the rest is renames, comments, a magnitude assertion macro, tests and CI. ECDSA verification results are unchanged. The ellswift module already existed at v23.1.8. The other parts of #7378 are rpc packet rows (RPC-14). |
| #7358, #7666 (univalue part) | `UniValue::__pushKV` renamed `pushKVEnd`; same behaviour. |
