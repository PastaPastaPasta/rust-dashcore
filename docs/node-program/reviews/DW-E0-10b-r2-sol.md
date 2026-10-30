# DW E0-10b — Sol r2

Reviewed 2026-10-11. Exact head: `1491d109a6ff1485146415458da2e6bb6adecd11`, branch `dw/e0-10b-trust-provider`. Narrow follow-up range: `84991f3aed1ffd61a2a49924168d1975d376e5cb..1491d109a6ff1485146415458da2e6bb6adecd11`.

Reviewed the real range and the connected display, digest, re-fetch, persistence and caller paths against DEC-183, `docs/design/E0-10b-trust-provider.md` §§4/5/8, DASHPAY §2.2, m4 §5/item 36, REVIEW-COMMON.md and the manager's rulings. The detached review worktree is `/work/scratch/e0-10b-r2-sol/wt`. Experiments use a separate source copy in `/work/scratch/e0-10b-r2-sol/probe`; the reviewed worktree and branch were not edited, committed or pushed.

## Closure status

1. **R1 — closed by mechanism.** `Held::label` retains the exact spelling and derives only the query/record key from the normalized form (`reverify.rs:118`). The digest covers normalized label, spelling and owner (`:165`), `reverify` matches the returned spelling/owner tuple (`:311`), and `confirmed` compares complete `Held` values (`:330`). The SDK queries `normalizedLabel` under `dash`, retains the domain's `label`, and reads the owner from `records.identity`, not `$ownerId` (`:483`, `:491`, `:495`). Homographs and case changes share a record key but change the digest and fail a mismatching fetch.

   I traced the display side as well: `recovery::owned_identities` derives names and entities from the same managed identity (`recovery.rs:416`, `:438`), using the same `owned_names` as `held_of` (`reverify.rs:335`). `shown_names` filters and clones those actual spellings. A main-name preference can select a normalized equivalent, but `owned_spelling` returns the spelling from the owned-name list (`names.rs:331`), so the preference cannot inject an unhashed display spelling. The stale identity snapshot displays no names/main name. The separate `main_name` API returns a string without a verified flag. Contact, scanned-contact, user-search and resolution methods capable of returning another flagged label are still stubs at this exact head; I found no implemented flagged-label path bypassing `Held::label`.

2. **Contact profiles, labels and registration — closed for this provider; DP2-01 integration remains an explicit prerequisite.** `contact_profiles_of` includes cached `Some(profile)` entries, binding the contact id and the same `ShownProfile::of` projection that a read model must show (`reverify.rs:371`). Confirmed-absent profiles have no displayed fields to verify. Deduplication now uses the full `Entity`, including its digest, so two cached revisions under one profile key are not silently collapsed (`:393`, `:638`). Profile fetching checks `$ownerId` against the requested contact before returning its fields (`:448`). Label digests and re-fetches bind the contact as `records.identity`.

   `show_label` is a work-registration protocol, not a source of positive classification (`provenance.rs:173`). Replacement and `None` affect the list rechecked by `confirmed`; withdrawal before confirmation prevents a write. A registration changed after confirmation can still permit a historical record for the old, actually proven content, but a reader hashing its current spelling/owner cannot use that record for different content. Missing `None` can schedule unnecessary re-fetches; missing a replacement registration can delay verification of the new spelling. Neither alone makes a different displayed label verified. `None` does not erase an existing verification record: a later return of the identical content can be verified under accepted L3. Registrations are empty on reopen. The session-global, one-label-per-contact map can also delay one of two consumers showing different spellings; their actual-content digests still fail closed.

   At this head there is no production caller of `show_label`, because contacts are stubbed. DP2-01 must register each label it shows (withdraw with `None`) **and independently classify the actual displayed content** with `Held::label(spelling, contact)` and `Held::Profile { id: contact, shown: ShownProfile::of(..) }`. Checking registration membership or a record's presence would violate item 36. This review does not certify another branch's contact model.

3. **v1 → v2 — closed.** The version enters both the canonical preimage and the stored digest string (`reverify.rs:223`, `:234`, `:269`). With the fallback latch set, every production entity computes a v2 digest and therefore mismatches its old v1 record, for all four classes. Old rows can remain loaded without granting positive provenance; no destructive clearing/migration is needed. A successful v2 re-verification replaces the stored digest. Pre-digest NULL rows remain excluded by `verified_entities`. The reviewer probe seeds actual old canonical v1 hashes, including the old DPNS field set, and checks all classes live and after reopening.

4. **R2 — closed for failed writes.** `record_all` counts only nonempty-digest calls for which `record_verified` succeeds (`reverify.rs:618`). The async caller uses that returned count (`:609`). `record_verified` still updates its in-memory cache only after SQLite succeeds (`provenance.rs:160`). The committed locked-database test exercises an actual failed write, then a successful retry. There is a separate, nonblocking remaining-count edge case below.

5. **Network scope — closed.** Records and latch remain in the per-network app.sqlite selected by the session. The new test checks testnet/mainnet isolation on one data root, both live and after mainnet reopen. These are isolated local sessions, not live mainnet probes; adding a label-key network prefix is unnecessary for the reviewed session API.

6. **Dropping contact-identity provenance — sound for the stated row-content scope.** No held contact identity/public-key map exists to authenticate; adding an empty-key identity record would not prove the remote keys. A proof-verified `contactRequest` authenticates an accepted DashPay document whose `$ownerId` is the sender. Platform's identity-signed batch validation supplies the owner's authorization; the reader's inclusion proof is not a separate sender signature verification. `request_from_document` takes the sender from `doc.owner_id()` (`reverify.rs:409`), and the record binds sender, recipient, both key **indices**, account reference, ciphertext and the other parsed fields (`:174`). Thus an incoming request cannot become a request from another sender merely by sharing a timestamp, nor can a different encrypted xpub inherit its digest. Both directions of an established contact are enumerated.

   This proves ownership of the request document, not a human identity, current key validity, the public-key bytes at those indices, or the correctness/freshness of a separately decrypted payment account. The pinned library fetches and validates a contact identity during crypto setup (`rs-platform-wallet/.../network/contact_requests.rs:2390`, `:2440`) and stores the derived external account separately (`.../network/contacts.rs:415`). That dependency does not make an empty contact-identity provenance class useful. The present record is appropriately about the request's indices and encrypted bytes; it must not be presented as an attestation of all current channel state. DP3-04/H-09 remain the deferred payment gate.

7. **Docs — closed.** DP3-04's ROADMAP condition now requires a record matching the entity's current digest. The L1 upstream draft identifies the omitted sent-side `autoAcceptProof` and the same-account-reference re-ingest guard; it preserves the fail-closed comparison instead of relaxing the digest. No upstream action was taken.

8. **Test strength — the requested mechanisms are exercised; one parser gap remains (N1).** The committed tests now reject the field-boundary mutation that survived r1. Additional reviewer probes cover case changes through the real identity/main-name read paths, registration replacement/withdrawal/staleness, differing contact-profile copies and actual v1 hashes across reopen. The mutation results below distinguish executed assertion failures from harness/setup failures.

## New findings

### Minor R3 — successful overwrites can still understate the remaining count

**Location:** `rust/crates/dw-engine/src/platform/reverify.rs:612`, `:393`; `rust/crates/dw-appdb/src/provenance.rs:17`.

**Scenario:** Two managed identities/wallets cache different revisions of one contact's profile. The new digest-aware enumeration correctly retains both. Platform changes between their independent re-fetches, so each matching revision verifies. Both are still cached at confirmation and both SQLite writes succeed under the same `(profile, contact)` key. The second write replaces the first digest, leaving one cached revision unverified, but `held.len() - written` returns zero.

**Evidence:** The isolated profile-copy probe establishes that both revisions can be held concurrently. `reviewer_r2_successful_overwrites_can_understate_the_remaining_count` then exercises the production `reverify`, `confirmed`, `record_all` and classification against simulated verified fetches and real SQLite; it prints:

```text
successful writes=2; reported remaining=0; actual remaining=1
```

The final arithmetic is the expression from `reverify_once`; the probe does not invoke that session method's hard-wired live SDK. The old revision correctly remains `unverified=true`. This affects the method's documented remainder/logging, not positive provenance, and the ten-minute retry still runs.

**Fix:** After the writes, compute the current unverified count while the manager read lock is still held, rather than treating every successful write as one permanently resolved held value. Keep the correct successful-write count for write metrics.

### Nit N1 — the SDK DPNS extraction boundary lacks a regression test

**Location:** `rust/crates/dw-engine/src/platform/reverify.rs:489`; `rust/crates/dw-engine/src/platform/trust_tests.rs:507`.

**Scenario/evidence:** Making the SDK domain extractor read `normalizedLabel` instead of `label` survives the provider and parser tests. That fault drops the actual document spelling before the otherwise correct tuple comparison, allowing a shown normalized spelling to match a domain with a different original spelling. The provider's fake re-fetch supplies a prebuilt `(spelling, owner)` tuple, and the parser suite covers profiles/requests but not this domain extraction. The production code currently reads the correct field; this is a test-coverage finding, not a demonstrated defect at 1491d10.

**Fix:** Extract the small domain parser and test a document with an original-case `label`, its normalized equivalent and deliberately distinct `$ownerId`/`records.identity`. Assert the returned spelling and records identity, plus fail-closed behavior for missing/malformed fields. Retain the existing tests of both tuple comparisons and digest binding.

## Validation

All successful runs use `nice -n 10`, `CARGO_BUILD_JOBS=8`, `umask 022`, disk `TMPDIR=/work/scratch/e0-10b-r2-sol/tmp` (0700), and reviewer-created test directories at 0700. Cargo runs use `--locked --offline`. Builds use independent target directories for the exact-head worktree and the probe copy (`target` and `probe-target`), seeded by copying artifacts without sharing the implementer's target. Long checks run as user systemd units. No wall-clock test flakes required a rerun.

**At the exact head, 94 existing targeted tests passed, 0 failed:** provider/trust 24; provenance 5; parsers 3; contact profile/label walk 1; r1 identity spelling probe 1; network isolation 1; dw-appdb unit/migration/storage 50; m4 contract 9. `cargo fmt --all -- --check` and `git diff --check 84991f3..1491d10` passed.

**Six additional reviewer probes passed their stated assertions**, including the diagnostic assertion exposing R3. They cover all four old v1 canonical hashes live/reopened and subsequent v2 persistence; initial case mismatch; profile-copy deduplication and replacement/absence; label replacement/withdrawal before confirmation and stale registration without `None`; main-name preference spelling through the actual facade; and successful same-key overwrites. The guarded mutation copy also passed all six probes with every fault disabled.

**Twelve targeted faults were killed**, each by an executed failing assertion, with no compiler error counted as a kill:

- Omit spelling from the digest, or ignore it in the re-fetch comparison: 2.
- Omit owner from the digest, or ignore it in the re-fetch comparison: 2.
- Confirm by record key instead of complete content: 1.
- Skip cached contact profiles, or skip registered contact labels: 2.
- Ignore `show_label(..., None)`: 1.
- Count a failed record write as successful: 1.
- Revert the digest encoding's preimage version and stored prefix to v1: 1.
- Remove canonical byte-length prefixes: 1.
- Deduplicate differing cached profiles by kind/key alone: 1.

Two mutation runs used the additional reviewer probes: v1 migration and profile-copy deduplication. In particular, the new profile-copy probe catches key-only deduplication. The v1 probe tests migration semantics across every class, beyond the existing prefix-format assertion.

**One targeted fault survived:** reading `normalizedLabel` instead of `label` in the SDK domain parser. All 27 provider/trust tests (24 committed plus 3 reviewer tests) and all 3 parser tests still passed. See N1. The implementation at the reviewed head uses `label` correctly.

To limit repeated compilation under load, the disposable copy compiles mutually exclusive runtime guards once; each mutation run sets `DW_REVIEW_MUTATION` to exactly one fault. The version guard changes both functional uses of the encoding constant and is equivalent to reverting it. Standalone equivalent patches and the actual combined guard patch are preserved. The production source files in the copy were restored and compared byte-for-byte with 1491d10 afterward; the exact review worktree remained clean throughout.

Evidence/reproduction: `/work/scratch/e0-10b-r2-sol/verification.json`, `results-baseline.json`, `results-probes.json`, `results-mutations.json`, `baseline-*.log`, `reviewer-probes-baseline.log`, `mutation-*.log`, `reviewer-probes.patch`, `multiplexed-mutations.patch`, the standalone mutation patches, and `run_review.py`. A service PATH error, a stale-binary run selecting zero probes, a temporary mutation-helper lifetime error, and an initial runner misclassification of Cargo's normal test-failure footer were corrected and excluded. The final runs selected the intended tests, compiled successfully and produced the counts above. Temporary build targets were removed after validation; sources, scripts, patches and logs are retained.

## Limits

No live Platform queries, GitHub actions or private-file access. I inspected the supplied `/work/scratch/e0-10b/gate8.log`: 893 passed, 0 failed, 4 ignored and `GATE EXIT 0`; I did not independently repeat the full gate. Proof-source policy and parsers were tested locally; BLS proofs and the Platform protocol were not independently re-audited. The already accepted chain-anchoring, L1, L3 and deferred payment-gate limits remain. The registration protocol's actual DP2-01 caller cannot be certified from its stub in this head.

**Verdict:** APPROVE-WITH-NITS
Sol gate satisfied: yes
