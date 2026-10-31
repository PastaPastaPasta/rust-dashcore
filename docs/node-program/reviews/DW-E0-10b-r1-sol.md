# DW E0-10b — Sol r1

Reviewed 2026-10-11. Base: desktop main `85ab16e6646f5e3e70fb3e8434d0e7daedd3a947`. Exact head: `84991f3aed1ffd61a2a49924168d1975d376e5cb`, branch `dw/e0-10b-trust-provider`.

The review covers all seven branch commits, including `1e82b58`, `13594b6` and `84991f3`, against DEC-183, the accepted checkpoint decisions, DEC-18/09, DASHPAY §2.2, the E0-10 design/roadmap rows, m4 Contract-Version 9 §5/item 36, REVIEW-COMMON.md and the E0-10a spike note. Source inspection and baseline tests used the clean detached worktree `/work/scratch/e0-10b-sol/wt`. Experiments modify an independent source copy under `/work/scratch/e0-10b-sol/probe`; this review did not edit, commit or push either reviewed worktree.

## Findings, by severity

### Major R1 — DPNS drops the displayed spelling before verification and hashing

**Location:** `rust/crates/dw-engine/src/platform/reverify.rs:320`, `:151`, `:283`, `:443`; displayed content comes from `rust/crates/dw-engine/src/platform/recovery.rs:356` and `rust/crates/dw-engine/src/platform/names.rs:371`.

**Scenario:** An identity and its `Alice` domain have positive verification records, with the fallback latch set. The library subsequently replaces the name with fallback-sourced `Al1ce`, keeping the identity/keys and normalized-name owner unchanged. The summary displays `Al1ce` and still returns `unverified=false`, although that domain spelling was not verified. The same problem occurs if the spelling changes during a re-fetch: the write-time comparison cannot detect it.

**Evidence:** `held_of` converts the library's displayed label to homograph-normalized text before constructing `Held::Label`. Both `Alice` and `Al1ce` become `a11ce`. The label digest contains only that normalized text and the owner; `reverify` compares only the resolved owner; `SdkRefetcher::dpns_owner` discards the document's actual `label`. Thus both the digest and `confirmed` equality remain unchanged when the displayed spelling changes. `shown_names` preserves the original spelling for display.

The isolated regression `reviewer_dpns_spelling_replacement_must_be_unverified`, re-run after restoring every copied production source to the exact head, printed:

```text
verified spelling ["Alice"] replaced by ["Al1ce"]; unverified=false
unproved displayed spelling retained the verified flag
```

This is a collision in the projection being authenticated, rather than a SHA-256 collision or a missing length prefix. It does not change the proven owner or demonstrate payment redirection, but it violates DEC-183's binding to the content shown. It is outside L3: the newly displayed spelling was never verified. No accepted residual excludes domain spelling.

**Fix:** Keep the normalized label as the record/query key, but carry the original displayed label in `Held::Label`, include it in the digest and write-time equality, and compare it with the proof-verified domain document's `label` as well as `records.identity`. Bump the digest encoding version so existing incomplete label records fail closed. Add regressions for a spelling mismatch on the initial re-fetch and a spelling replacement after verification/during the network wait.

### Minor R2 — Failed record writes are counted as successful

**Location:** `rust/crates/dw-engine/src/platform/reverify.rs:561`, `:565`, `:572`.

**Scenario:** Re-fetches verify, but another SQLite writer keeps `app.sqlite` locked until `mark_verified` times out. All record writes fail, yet `reverify_once` can report zero entities remaining unverified.

**Evidence:** `written` counts records with encodable digests before attempting any writes. The blocking writer logs `record_verified` errors without adjusting that count, then the method returns `held.len() - written`. This also contradicts the newest commit's claim to count records actually written. The classification remains safe: `record_verified` updates the in-memory cache only after a successful database write. The periodic task still retries after ten minutes; this finding concerns its remainder/reporting, not a false verified flag.

**Fix:** Have the blocking writer return the count of successful, nonempty-digest record writes and subtract that returned count. Test an actual failed record write rather than only an unencodable digest.

## Confirmations

- **Network scope:** No additional network prefix is needed in DPNS keys through the current session API. `Engine::network_dir` (`engine.rs:108`) selects `<data_root>/<network>`, and `NetworkSession::open` (`session.rs:242`) opens `app.sqlite` inside it. Mainnet, testnet, regtest and distinct devnet names therefore have separate records and latches. “Global” means across wallets in one network database. Sharing a same-network record across wallets is sound for identical entity content; changed owner/request/key content changes its digest.
- **Encoding:** The current `Canon` includes length-prefixed domain/version/kind tags, u64 LE integers, option discriminants and length-prefixed byte/string values. Identity keys use deterministic `BTreeMap` order, include the map key IDs/count, and serialize through dpp's tagged key enum. The reviewed serializer includes key type, purposes, security level, contract bounds, disabled state and V1 limits. I found no byte-encoding ambiguity in the current implementation. R1 occurs before encoding.
- **Proof/status policy:** The pinned SDK fetch paths parse proof-derived responses before returning them, and the Tenderdash verifier calls the installed quorum provider before accepting the BLS signature. Desktop maps only `Verified` to `SpvAnswer::Verified`; `Unknown` and every `Skipped` reason map to held keys. Tests explicitly cover `MissedList` and `NotMarkedForVerification`. Invalid answers refuse. When both sources are consulted, a disagreeing held/verified key refuses; Degraded may serve SPV-Verified keys when the trusted service is unavailable. The trusted-only/fallback paths durably establish the latch before serving an unverified key.
- **Scope and write-time binding:** Re-fetches require a successful matching result and a nonempty, wholly verified lookup tally. Errors, fallback lookups and empty tallies cannot mint a record. Identity keys, shown profiles and complete parsed requests are compared against the snapshot and again against current state. The manager read lock stays held across record writes, and subsequent replacements are checked by their digests. This works for the fields actually retained; R1 identifies the dropped label field.
- **Contact requests:** The sender/recipient query and `$createdAt` filter select candidate documents, then full `ContactRequest` equality selects a verified match from all candidates. It does not bless another request solely because its timestamp matches. Sender, recipient, key indices, account reference, encrypted fields, auto-accept proof, core height and creation time enter the digest. A same-timestamp record-key collision with different content cannot falsely verify the other content; incomplete pages or inconsistent duplicate copies can instead leave data unverified.
- **Migration/storage:** `rust/crates/dw-appdb/migrations/V2026101101__trust_digest.sql` keeps old rows but `verified_entities` excludes NULL digests. Reopening, replacement digests and replacement timestamps pass the migration/storage tests. Failed persistence cannot populate the in-memory positive cache. Backup export excludes positive verification records.
- **L1:** The omitted sent-request auto-accept proof prevents equality with the verified document. It is a safe availability limitation, not permission to omit that field from matching/hashing; an upstream bookkeeping fix remains appropriate.
- **L3:** Returning to the recorded content can return to verified status. This is safe under DEC-183's explicit historical-content meaning, without any freshness promise. A different successful verification replaces the sole stored digest, so the superseded content then fails that record.
- **Balance:** Exclusion is safe only for the specified structural identity/key provenance. The flag does not attest the displayed credit balance, current state or spending capacity. The accepted H-09/DP3-04 work must not treat this digest as a balance or freshness check.

## Validation

All runs use `nice -n 10`, `CARGO_BUILD_JOBS=8`, `umask 022` and disk `TMPDIR=/work/scratch/e0-10b-sol/tmp` (0700). Reviewer-created test roots use 0700. Cargo uses an independent target directory; cached build artifacts were copied into it without sharing the implementer's target directory. Builds/tests use `--locked --offline`.

At the exact head, **92 targeted existing tests passed, zero failed**:

- Provider/trust tests: 21.
- Provenance: 5; re-fetch parsers: 3.
- Library-level key replacement: 1; fallback/session reopen: 1; headers plus masternode sync: 1.
- dw-appdb unit/migration/storage tests: 50.
- m4 contract: 9; persisted trusted-only toggle/session reopen: 1.

The exact commands/results are in `/work/scratch/e0-10b-sol/run_checks.py`, `checks.json` and the corresponding logs; the initial provider run is `trust-tests-seeded.log`. `git diff --check 85ab16e..84991f3` passed.

The existing tests rejected these mutations in the independent copy, each with an executed failing assertion rather than a compilation error:

- Treat `Unknown`/`Skipped` keys as Verified: `only_verified_counts_and_invalid_refuses` fails.
- Replace digest equality with record-presence checking: the replacement-content test fails.
- Check only the first same-timestamp contact document: the re-fetch matching test fails.
- Remove the current-content comparison before writing: the write-time test fails.

**A plausible security mutation survives the existing provider suite:** removing `Canon::bytes`' u64 byte-length prefix still passes all 21 existing trust tests. The added boundary probe rejects it: profiles with fields `("a\u{1}b", "c")` and `("a", "b\u{1}c")` then have the same digest. Option-tag bytes embedded in field values defeat delimiter-only encoding. The current length-prefixed implementation is correct; retain the boundary/option/key-order probe as a regression test. Evidence: `mutation-no-length-prefix.log` and `mutation-no-length-prefix-extra-probe.log`.

After restoring the copied production sources to the exact head, the seven additional reviewer probes produced **6 passed and 1 failed**. The sole failure is R1's intended regression assertion: displayed spelling must become unverified after replacement, but does not. The six passing probes cover network isolation/shared-wallet records; field boundaries, option tags and key ordering; the omitted auto-accept proof and a wholly spawned lookup; accepted rollback/version mismatch; balance exclusion; and failed-record-write classification. In particular, the real busy-SQLite probe confirms R2 does not turn a failed write into positive provenance. Evidence: `reviewer-probes-baseline.log`. Initial probe-harness setup failures are excluded from test evidence.

Reproduction sources are preserved in `/work/scratch/e0-10b-sol/reviewer-probes.patch`; mutation patches and logs are alongside it. Temporary build targets were removed after validation. No changes or GitHub actions were made against the reviewed branch. This report is left uncommitted.

## Limits

I did not reproduce the full claimed gate7 total of 887 passed/4 ignored, run macOS/Windows validation, or use live public Platform probes. Provider policy tests use fake keys/sources; the production SDK proof/signature boundary was traced in the pinned source, not re-audited or exercised with a live adversarial BLS proof. The accepted lack of chain anchoring/full R7 enforcement and the deferred money-move gate remain limits of this head. The contact facade remains stubbed here: request provenance production is reviewed and tested, but an end-to-end DP2-01 contact-model assertion is outside this exact branch.

**Verdict:** CHANGES-REQUIRED
Sol gate satisfied: no
