# DW-DP2-03 round 2 — Sol review

Range: `f4d0877..ae1cc6e` in the detached review worktree. This review covers only the round-1 findings and their fixes.

## Findings

1. **M2 — partial, requires change — `ScanClaim` can issue more than one proof source.** [`users.rs:500-516`](rust/crates/dw-engine/src/platform/users.rs) documents `proof_source` as made once, but the method takes `&mut self` and never checks `used` before returning another `SignWithKey`. One held claim can therefore yield two proof sources before Drop spends the scan, leaving the one-scan/one-send invariant dependent on a future DP2-02 caller. Make proof issuance consuming, or reject a second call once `used` is set; test it directly. The other M2 races are closed as described below.
2. **L4 — new documentation mismatch — `unverified` includes profile provenance.** [`users.rs:299-311`](rust/crates/dw-engine/src/platform/users.rs) sets each hit's `unverified` from the label **or** shown profile, and [`users.rs:399-406`](rust/crates/dw-engine/src/platform/users.rs) copies that value into `ScannedContact`. The new sentence in [`m4-dashpay-engine.md:1534-1536`](docs/contracts/m4-dashpay-engine.md) says it is about the name→identity lookup only. State that it describes the shown name/profile provenance, not the QR key claim. This is conservative for send decisions but the contract should match the data.

### Round-1 disposition

| ID | Decision | Evidence |
|---|---|---|
| M1 scan provenance | Closed | `ScannedCode` distinguishes a user link from an unconfirmed auto-accept claim, and the contract keeps send outcome `Pending`; `unverified` remains a separate trust signal. possible security note: see manager |
| M2 lifecycle and concurrent sends | Partial | `Scans::claim` serializes claims under one mutex; generation read before wallet validation and checked at insert rejects a scan paused across forget/reload; forget and close clear keys; eviction skips claimed entries. `proof_source` is repeatable on one claim, as above. |
| M3 key lifetime | Closed, with documented API residuals | The `dapk` value stays a slice of the zeroizing input; Base58 writes into a pre-reserved `Zeroizing<Vec<u8>>` and invalid-character exits drop it; `key_blob` copies once into boxed zeroizing storage. The only later conversion uses secp256k1's by-value constructor and hands a `Copy` key to platform-wallet. |
| L1 empty search | Closed | `hits` returns before the profiles query when `found` is empty. |
| L2 status summary | Closed | §2 now lists DP2-03's four calls, and the change log includes the r1 addendum. |
| L3 `.dash` handling | Closed | Each entry form trims and removes one suffix; `lookup` only validates and normalizes the resulting label. The link keeps iOS's case-sensitive suffix behavior. |

## Mutation table

Each production mutation was applied alone, run against its focused test, and restored. All failures below were test assertions, not compilation failures.

| Mutation | Focused test | Result |
|---|---|---|
| Bypass `claimed` guard | `one_send_at_a_time_claims_a_scan` | Killed |
| Bypass wallet-generation check | `a_scan_outlived_by_its_wallet_keeps_no_key` | Killed |
| Skip close cleanup | `closing_the_session_drops_its_scans` | Killed |
| Allow claimed-scan eviction | `scans_are_bounded_and_forgotten` | Killed |
| Leave `used` false after proof construction | `a_scan_gives_a_send_its_proof` | Killed |
| Bypass proof-time expiry check | `a_scan_gives_a_send_its_proof` | Killed |
| Remove empty-hit early return | `search_shows_profiles_and_relations` | Killed |
| Strip `.dash` again in `lookup` | `resolve_reads_one_name` | Killed |
| Percent-decode `dapk` before Base58 | `dip15_contact_code` | Killed |
| No production mutation: require the same claim's second `proof_source` to fail | Temporary `r2_one_claim_must_not_derive_two_proof_sources` | **Failed on ae1cc6e**: a second source was returned |
| Inject panic after setting `used` | Temporary `r2_panic_after_proof_construction_spends_scan` | Passed: Drop spent the scan during unwind |

## Validation and limits

- `cargo test -p dw-engine --lib users_tests -- --nocapture`: 16 passed, 1 ignored (the network fixture).
- `cargo test -p dw-uri --lib`: 30 passed.
- `cargo test -p dw-engine --test m4_dashpay_contract`: 9 passed.
- `cargo test -p dwcli`: 57 passed across unit and integration targets.
- `git diff --check f4d0877 ae1cc6e` passed; the detached worktree was clean after restoring all mutations.
- The injected panic occurs after `used` is set and before the source returns; Drop spent the scan even though the mutex was poisoned. Before that assignment, the only panic site is the internal scalar-validity `expect`, and no proof source has escaped.
- Key-erasure analysis is structural: `Zeroizing<Vec<u8>>` clears its full capacity on Drop, and the decoder pre-reserves that capacity so it cannot reallocate while decoding. It is not a memory-forensics measurement.

Not this round: the actual DP2-02 broadcast implementation, public testnet rerun, and full workspace gate.

**Verdict:** REQUIRES CHANGES

Sol gate satisfied: yes
