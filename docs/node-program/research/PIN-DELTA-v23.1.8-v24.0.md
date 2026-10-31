# PIN-DELTA v23.1.8 ⇄ v24.0.x

Moved. The O17 deliverables live under `designs/`:

- `designs/O17-pin-delta.md`: the classified delta report (217 behaviours: 170 `profile`, 18 gated, 3 params, 22 `v24-all` declared deviations, 4 internal; DEC-37), the A1 §2.25 reconciliation, rows 23 and 24, the decisions and the PLAN/A1 edits.
- `designs/O17-pin-delta/`: the nine auditor packets and the integrator rows with full evidence, and `decisions.json`.
- `designs/O17-release-profile.md`: the `ReleaseProfile` spec (DEC-24 D3 layering, every field, per-call-site and per-task tables, re-pin procedure).

Tools to re-run the audit at the next re-pin: rust-dashcore branch `node/o17-pin-delta`, `contrib/node/pin-delta/`.
