# DW DP2-01 — Sol r3

Reviewed **84596d96196370ea9af2c7ae62678b24105f4175**, six signed commits on desktop main **070a276**, in the detached worktree `/work/scratch/dp2-01-r3-sol/wt`. Scope: the E0-10b/DEC-183 adaptation in 84596d9, the first commit's rebase resolutions, m4 versioning, and continued closure of Sol r1/r2. This is not a fresh review of the entire trust provider or the forthcoming payment gate.

The reviewed source and branch are unmodified. No commits, pushes, GitHub actions, funding or live testnet reads were performed. Independent probe source and logs are outside the checkout; this report is uncommitted. The accepted temporary build break in commits 1–5 and the reported, isolated-green `dw-desktop::test_QT_031` timeout are not findings.

## Findings

### R3-01 — Minor: delayed label publication can overwrite a newer landing or withdrawal

**Location:** `rust/crates/dw-engine/src/platform/contact_names.rs:233–234`, together with `:76–80`, `:152–166`, and `rust/crates/dw-engine/src/platform/provenance.rs:172–178`.

`land` clears `in_flight` and releases the names mutex. `label` then clones the current label under another acquisition, and `show_label` publishes that clone under the separate provenance mutex. There is no ordering check on the publication. The retry floor reduces the chance of overlapping publishers; it does not serialize them or bound how long a worker can be delayed. The absence of an await between these calls prevents ordinary Tokio cancellation there, but does not prevent OS preemption.

**Concrete schedule:** Bob already has a verified `bob` record. Worker A's due fetch fails, retaining `bob` and setting the 60-second retry timer. A clones `bob` for `show_label`, then is delayed. Once the timer expires, another read can claim Bob and worker B can successfully land `B0b`, registering `B0b`. A resumes and overwrites that registration with its older `bob` clone. The read model now shows `B0b`, but the re-fetch registry contains `bob`. Since the latter is already verified, `held_unverified` at `reverify.rs:685–690` skips it and never sees the currently shown `B0b`. A successful landing has the one-hour freshness interval, so subsequent re-fetch passes cannot repair the registry before another name fetch publishes it. If B instead lands `None`, A similarly resurrects the registry entry after the successful withdrawal. Successful A landings have the same ordering problem if delayed past the longer freshness interval.

**Impact:** This is a re-fetch coverage/withdrawal defect, not a false-verification bypass. The row independently constructs `Held::label("B0b", contact)` and remains unverified against `bob`'s digest. When B lands `None`, the row shows no username, even if A resurrects the obsolete registry entry. There is no blocker or major finding.

**Evidence:** The independent source-extraction harness uses the unchanged `ContactNames` state methods, `Held` digest implementation, and actual `Provenance`. It deterministically interleaves the three production operations and advances only the test's `checked` timestamp to model a worker delayed past the retry floor. It first verifies that the unchanged `due` predicate permits a new claim at that floor. Both required publication invariants then fail, without a timeout:

```text
shown=B0b, registry=bob, current_unverified=true; next success retry=3600 s
assertion failed: the refetch must still walk the current shown label

shown=None, registry=bob; delayed publication resurrected the registry entry
assertion failed: a completed withdrawal must remain withdrawn
```

Sources: `/work/scratch/dp2-01-r3-sol/make-source-probe.py` and `source-probe.rs`; output: `source-probe.log`. This models a legal delayed-worker schedule; it is not a claim that an hour-long delay was observed in the real session under ordinary load. The branch's `shown_usernames_reach_the_refetch` test exercises consecutive landings, not this interleaving.

**Fix:** Serialize each landing through its provenance publication. For example, call `show_label` while still holding the names-state guard, using the resulting label, before another claim/landing can pass; it only updates memory. Keeping `in_flight` set until publication, or an equivalent checked generation protocol, can also establish the required order. Merely returning a label from an atomic `land`/`label` operation while publishing it afterward leaves the overwrite window. Add a deterministic delayed-publication test covering both re-spelling and `None`; do not rely on the retry duration as a mutex.

## Provenance confirmations

- **Profiles:** `owner_contacts` at `contacts_model.rs:183–202` constructs the facade profile and `Held::Profile { id: contact, shown: ShownProfile::of(p) }` from the same cached profile under the wallet read guard. `profile_of` and `ShownProfile::of` agree on public-message/bio fallback and avatar fields. A snapshot retains both the shown content and its corresponding entity. A later live build with a replaced profile carries a different digest; `provenance_class_profile` proves that its old record no longer verifies it.
- **Requests:** Each established row carries both full requests; a pending row carries its single request; an ignored row carries none. The exhaustive canonical encoding covers key indices, account reference, encrypted label/key, auto-accept proof, height, sender/recipient and creation time. The new in-place key-index rotation assertion verifies that an unchanged record key does not verify changed request content. E0-10b's re-fetch compares full requests and its record step admits only unchanged held content. Dropping the contact identity class agrees with the merged base and m4: the verified requests bind the channel's key indices and encrypted xpub.
- **Names:** The summary captures one username and uses that exact spelling and contact for its v2 entity and output. Normalized labels remain record keys; spelling and owner are digest inputs. The `bob`/`B0b` session regression and owner/spelling trust tests pass. A record of another spelling or another owner cannot bless the shown username. Sequential `Some`, unchanged `Some`, failure, and successful `None` landings behave as specified; the race above is the remaining exception to publication order.
- **Staleness:** A failed refresh intentionally retains the previous label and may retain its verified flag when that exact content has a record. m4 §5 expressly says verification attests content, not currency. Successful `None` removes the UI label, and successful re-spelling checks the new digest. Retained historical records and labels retained after a forgotten wallet/dropped relation cannot make a different shown spelling verified. Contention snapshots likewise attest their own shown content, not current wallet state.

I found no in-scope path that reports replaced profile/request/name content verified against a different recorded digest once the fallback latch is set. Before the latch, the existing provider policy still reports nothing unverified, as specified.

## Rebase and contract

`git range-diff b839dfe..backup/dp2-01-pre-e0-10b-3dd7cb5 070a276..84596d9` is retained in `range-diff.txt`. It pairs all five original DP2 commits, shows commits 2–5 patch-identical, and identifies 84596d9 as the new adaptation. No original implementation hunk was silently dropped.

- Commit 1's `provenance.rs` is identical to the new base. E0-10b already supplies the shared request key helper and the content-digest API, so discarding the old pre-digest hunk was correct. The final adaptation only removes `show_label`'s now-unneeded dead-code allowance.
- `recovery.rs` reapplies `profile_of` without changing its old inline mapping, and preserves the base's `identity_and_profile` entities and owner field. The verified-profile and re-fetch tests pass.
- m4 §5 preserves the base's DEC-183 content comparison, v2 spelling/owner binding, pre-digest fail-closed behavior, migration latch and shown-name scope. It adds the contact summary's §2.4 reference. The adaptation corrects §2.4's old identity class and obsolete owner-binding residual.
- The first rebased commit temporarily retained the base's version-9 surface fingerprint while adding `ContactsPage.updating` to the listing. The final head resolves this: **Contract-Version 10**, generated SHA-256 **4e66f3cca4fbd147c571b7a2ab091fbd95b5badac2f9ecf77a7c5ae993fa9148**, and **change-log item 37**. E0-10b's item 36/version 9 remains intact. All nine m4 checks pass without blessing.

## R1/r2 closure status

All previous items remain closed; R3-01 is a distinct publication-order defect.

- **R1-01:** setup-queue addition/removal classification still emits owner-scoped, deduplicated Contacts invalidations without journal news; decrypt-only work stays excluded. Both queue regressions pass.
- **R1-02:** an empty contention refresh still publishes its snapshot and emits Identities. The cold-wallet regression passes.
- **R1-03:** task admission/registration and close still share the tasks mutex. Session close drains operation guards, then aborts and awaits contacts tasks before SessionClosed. `Owed` remains outside the spawned future and cancellation preserves any previous label. The close regression passes; adding the provenance handle did not reopen the shutdown race.
- **R1-04:** Username still falls directly to the common id tie-break. The opposing-alias/unknown-username regression passes.
- **R2-01:** 2cf1200 preserves the wallet's HistoryChanged re-query contract and facade documentation. Its real `TransactionDetected`/`BlockProcessed` regression passes: an already-Confirmed payment gets a block time, activity/order change, and no Contacts signal is needed.

## Validation and limits

All review build/test processes used `nice -n 10`, `umask 022`, `CARGO_BUILD_JOBS=12` for Cargo, and mode-0700 `TMPDIR=/work/scratch/dp2-01-r3-sol/tmp`. The review used its own `wt/rust/target`, initially seeded with an independent copy of existing artifacts. Cargo rebuilt the engine test binary from the detached source; the separately selected integration tests ran that same fresh binary. No writable target was shared.

- `cargo fmt --check`: pass; `git diff --check 070a276..84596d9`: pass.
- `cargo test --locked -p dw-engine contacts --lib -- --test-threads=1`: **36 passed, 1 ignored**. `contacts-test.log`.
- `cargo test --locked -p dw-engine --lib platform::contact_names`: **3 passed**; `platform::provenance`: **5 passed**; `platform::reverify`: **4 passed**. `names-test.log`, `provenance-test.log`, `reverify-test.log`.
- Exact integration selections for contact-profile/label walking, all request relations, record-step accounting, unchanged-content admission, digest coverage/canonical encoding and failed record writes: **7 passed**. `provenance-integration-test.log`.
- Exact trust/re-fetch selections for verified matches only, replaced content and spelling/owner binding: **3 passed**. `refetch-integration-test.log`.
- `cargo test --locked -p dw-engine --test m4_dashpay_contract`: **9 passed**. `m4-test.log`.
- `cargo clippy --locked -p dw-engine --all-targets -- -D warnings`: pass. `clippy.log`.
- External regression harness: **9 passed, 2 failed**, both deterministic assertions for R3-01. Its unchanged-name/failure control and original name/provenance tests pass. These are external reviewer tests, not failures in the branch's existing suite.

The private build target and temporary test data were removed after validation. Logs, probe source/binary and the clean detached checkout remain under `/work/scratch/dp2-01-r3-sol/`. No full workspace gate, UI test or live testnet fixture was rerun. The new username session test supplies assumed verified entities to the real record step; the separate trust tests exercise proof-source tallying and exact-content comparisons, but this review does not claim a new live-network proof run.

The content-digest adaptation and rebase resolutions are sound apart from R3-01. Establish publication ordering before declaring the requested race closed.

**Verdict:** CHANGES-REQUIRED
Sol gate satisfied: no
