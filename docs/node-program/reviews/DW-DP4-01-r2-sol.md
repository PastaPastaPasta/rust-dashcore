# DP4-01 independent narrow review — fix round f1 (796c43ef)

## Findings

None in the requested scope.

## Disposition

- **R1 key eligibility: closed.** `requests.rs:316-355,385-393,410-470` filters authentication keys for purpose, security level, secp256k1 type, disabled/expired state, DashPay document bounds, and unlimited-first order in both the pre-redemption candidate path and the final signer path. This matches the pinned library's `usable_authentication_key` policy for the supported key type. `profile_tests.rs:505-579` covers an ineligible lower-ID key, a preferred unlimited key, and refusal with the grant unused when no eligible key remains.
- **R1 contract-version risk: closed.** `requests_net.rs:252-257` fetches the live contract through the SDK's proved `Fetch`; `profiles.rs:120-135,327` rejects a live profile property missing from or typed differently than the bundled schema before signing. `profile_tests.rs:666-718` covers both shipped contract versions and incompatible schemas. A live schema with fewer properties is permitted because it cannot cause an unknown fetched field to be dropped.
- **Stale baseline: closed.** `profiles.rs:209-246,302-327` compares the baseline with the same content shape that `profile()` shows: empty strings become absent, untrusted avatar URLs are omitted, avatar hashes are case-normalized, and `updated_at` is ignored. Raw whitespace remains significant in both the read and comparison; trimming applies to the proposed edit. The document-level no-op returns `Ok` before the conflict check. A changed profile returns `profile.conflict { current }` before grant redemption, and the cache follows the current document. The read-to-write window is guarded by the fetched document ID/revision; a concurrent first creation is constrained by DashPay's unique `$ownerId` index. An exact-content ABA change is accepted by the documented content-baseline policy.
- **Delta and evidence.** The dwcli baseline/force plumbing and boxed app error preserve the intended API. No Swift-parsed file appears in `git diff 7668c4a6 796c43ef`, so the stated Swift T0 non-rerun is consistent. This was a read-only source review; I relied on the brief's gate5 and T2 results and did not rebuild or write to testnet.

VERDICT: ACCEPT
