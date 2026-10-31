# DW DP2-01 — Sol r1

Reviewed **cb6c6d01d2a82f393f56bee5182d8f187c96dbdb** on base **85ab16e6646f5e3e70fb3e8434d0e7daedd3a947**, including commits 7a7d66d, 16db415 and cb6c6d0. The detached checkout at `/work/scratch/dp2-01-sol/wt` remains clean. No source edits, commits, pushes or GitHub actions were performed on the reviewed checkout or branch. Reproduction tests and mutation switches exist only in a disposable source archive outside that checkout.

Scope: DASHPAY, ROADMAP DP2-01, m4 contract v9, REVIEW-COMMON, the cited catch-up/provenance decisions, the pinned platform-wallet implementation, and the Android reference. The E0-10b/DEC-183 API adaptation and the manager's identity/profile/owner-bound-label class changes are excluded as instructed. They are not findings or conditions of this verdict.

## Findings

### R1-01 — Major: deferred-crypto queue writes do not invalidate the read model

**Location:** `rust/crates/dw-engine/src/platform/contacts_model.rs:199` and `:213` consume the queue; `:599` counts it. The missing integration is in `rust/crates/dw-engine/src/platform/journal.rs:290`, reached through `rust/crates/dw-engine/src/store.rs:101`.

**Scenario:** An incoming QR request is queued for auto-accept while locked. The queue addition changes `pending_setup_count()`. If its proof expires before unlock, the library removes the auto-accept operation while retaining a manually acceptable incoming request. The count changes again. Both operations can be standalone queue changesets. A host following m4's `Contacts`/`Identities`/`Payments` re-query instructions receives no invalidation and can retain an obsolete “Unlock to finish setting up N contacts” banner. Queue-only account operations can likewise affect `SetupPending`. The contention refresher cannot repair this generally: it starts only when a read actually collides with a writer.

**Evidence:** `classify()` ignores `pending_contact_crypto_added` and `pending_contact_crypto_cleared`; `WalletStore::store()` calls the tap only for a non-`None` classification. At platform pin `ebe37f8a679b1a552483dc7ff12c306291e9a32b`, `wallet/identity/network/contact_requests.rs:1853` and `:2036` persist standalone additions; `:2968` clears expired proofs, and `:3100` persists only their dequeue. Verify-failure marks are memory-only and produce no compensating contact write.

The independent `review_queue_only_writes_should_signal_the_pending_setup_reader` probe injected those queue deltas through the real session `WalletStore`, with successful stores. Fresh facade reads returned **0 → 1 → 0**, while both pending signal maps were empty:

```text
pending count 0 -> 1 -> 0; added signals={}; cleared signals={}
queue addition sent no Contacts signal
```

The probe fails on the missing signal, not a timeout. Evidence: `/work/scratch/dp2-01-sol/queue-probe.log`; the probe also reproduced alone in `queue-probe-alone.log`.

**Fix:** Classify additions and removals of `RegisterReceiving`, `RegisterExternal` and `AutoAccept` as `PlatformChange::Contacts` for their `owner_identity_id`, deduplicated per owner. These are invalidation signals, not journal news. Add queue-only addition/removal tests, including the expired auto-accept path. Keep `ContactInfoDecrypt`'s unrelated, repeatedly queued work out of the setup-count notification policy.

### R1-02 — Minor: a cold empty-wallet contention refresh has no completion signal

**Location:** `rust/crates/dw-engine/src/platform/contacts_model.rs:505` and `:514`.

**Scenario:** A registered wallet has no owned identities and no previous contacts snapshot. A writer holds its state; `contacts()` returns an empty page with `updating: true`, and `pending_setup_count()` returns `platform.unavailable`. When the writer releases, refresh creates the empty snapshot but derives all completion signals from its empty identity list. A caller waiting for the advertised signal cannot learn that the read is now live without some unrelated event or another poll.

**Evidence:** `review_empty_wallet_refresh_should_wake_the_busy_reader` waits for the refresh's in-flight flag to clear, then observes **zero Contacts signals**. It reproduced when rerun alone. Logs: `probes.log` and `empty-probe-alone.log` under `/work/scratch/dp2-01-sol/`.

**Fix:** Ensure an empty refresh still emits an observable invalidation, for example `PlatformChange::Identities`, which contact consumers already re-query on. Add the cold zero-identity case to the contention test. This is minor because the normal Contacts screen already requires an identity; the wallet-wide pending count still has this edge.

### R1-03 — Minor: an in-flight username fetch survives session close

**Location:** `rust/crates/dw-engine/src/platform/contact_names.rs:209`.

**Scenario:** Close the network while a DPNS fetch is awaiting its answer. The weak manager check occurs only before the await, and the spawned batch does not participate in session cancellation. Close can return and emit `SessionClosed`, after which that fetch still completes and lands its answer. This contradicts the fetch comment and DASHPAY's “nothing runs while the session is closed” lifecycle rule.

**Evidence:** A controlled source was held in flight, the session was closed, and then the source was released. The probe printed **`name after SessionClosed: Some("bob")`** and failed because the source continued after close. It reproduced alone; see `close-probe-alone.log`. The stopped event pump prevents a visible post-close contact event, and the SDK operation is normally bounded by 30 seconds, so this is a bounded cleanup issue, not a money or journal write.

**Fix:** Make name batches participate in the session's cancellation mechanism, cancelling an active fetch on close and dropping `Owed` to clear outstanding claims. A manager-reference count is not a close notification. Add an in-flight-close test.

### R1-04 — Minor: Username ties disagree with the blessed contract

**Location:** `rust/crates/dw-engine/src/platform/contacts_model.rs:358`; m4 §2.4, `docs/contracts/m4-dashpay-engine.md:231`.

**Scenario:** Two contacts have unknown usernames, which is ordinary on a cold start or offline. m4 specifies ties by id. The comparator first breaks the equal `None` usernames by the shown name, and reaches id only if those names also tie.

**Evidence:** A standalone Rust probe compiled the three comparator functions directly from the exact-head source, unchanged, with the same field types they use. For the fixtures' Bob id/alias `Zed` and Carol id/alias `Alpha`, the contract's id comparison was **`Less`**, but `Username` returned **`Greater`**. See `username-tie-probe.rs` and `.log` under `/work/scratch/dp2-01-sol/`. The existing four-sorts test has only one contact without a username and misses this case.

**Fix:** Remove the shown-name secondary comparator from the Username arm so the common id tie-break applies, and test two unknown usernames. If shown-name ties are the intended product behavior, specify that secondary order explicitly in the contract instead.

## Confirmations

- Relation precedence is correct: established > ignored > incoming > outgoing, with one row per owner/contact. Ignoring an established contact preserves its relation, private details, requests and payment activity. Hidden contacts and ignored senders enter Hidden; Requests and Pending remain newest first. Profile joins, aliases, search and section deduplication match the v9 semantics.
- The live readiness predicate matches the pinned library's `external_account_needs_rebuild` rule: a paying account must exist, and `external_account_reference` must match the current incoming request. Missing or unknown markers are conservative; `Broken` wins. Mutations removing either the existence check or reference check were caught by the existing channel test with `Ready` versus expected `SetupPending`. I found no new live-read path that blesses a stale paying account.
- Snapshot publication occurs under the wallet read guard, preventing an older read from overwriting a post-write snapshot. Contended reads queue a deduplicated refresh and normal nonempty refreshes signal their owners. The existing busy-wallet test passes. `contact()` can still return snapshot-era `Ready` without an `updating` field, as the contract allows: that is not current payment authorization. Contact payments remain unimplemented; DP3-01 must validate current state before preparing a payment.
- Before the deferred DEC-183 adaptation, the current provenance plumbing uses the session's positive records, normalized labels, and the shared sender/recipient/request-time key. Both requests are required for a contact, profile/name records are required when used, and absence after the fallback latch is conservative. The accepted identity-class and owner-bound-label changes are left for the separately scoped review.
- `since` and activity use request `$createdAt` and confirmed Core block times, with failed/unconfirmed payments excluded from payment-derived activity. Local observation clocks do not date rows. Replay and catch-up tests pass, and the repeated facade reads leave the journal unchanged. Username memory and the key-rotation date residual remain as declared. `publish_state` and `payment_lock` follow the declared DP2 deviations.
- Contract-Version rose 8 → 9, the generated surface/fingerprint includes `ContactsPage.updating`, the three implemented methods are removed from stub enforcement, and the new helper files are classified as non-facade. All nine m4 checks pass. No blessing was run during review.
- Android exposes all four sort choices, but its checked-out `UsernameSearchResult.kt:78` leaves Last Activity unimplemented; its contact date uses the incoming request. The desktop's actual Last Activity order and later-of-two-requests date are explicit m4 behaviors, not literal Android equivalence. The latter is consistent with the declared rotation residual.

## Validation

All review builds/tests used `nice -n 10`, `CARGO_BUILD_JOBS=8`, `umask 022`, and the on-disk, mode-0700 `/work/scratch/dp2-01-sol/tmp`. Targets were private to this review; independent reflink copies of existing build artifacts avoided sharing writable target directories.

- Exact-head `cargo fmt --check`: pass. `git diff --check 85ab16e..cb6c6d0`: pass.
- `cargo test --locked -p dw-engine contacts --lib`: **28 passed, 0 failed, 1 ignored**.
- `cargo test --locked -p dw-engine --lib platform::contact_names`: **3 passed, 0 failed**.
- `cargo test --locked -p dw-engine --test m4_dashpay_contract`: **9 passed, 0 failed**.
- Independent timeout probe: a never-answering first name times out, releases its in-flight claim as failed, and the next name in the batch lands successfully. Pass.
- Three targeted mutations were killed: omit paying-account existence; omit current-request marker comparison; omit the username in-flight guard. Unmutated controls pass. Logs are `mutation-*.log`; the disposable instrumentation is retained in `review-probes.patch`.
- The queue, empty-refresh and close probes fail on the concrete behaviors reported above. All three also fail when run separately. The comparator probe fails deterministically on the contract tie order. These are review probes outside the branch, not failures in its existing test suite.
- `cargo test --locked --workspace dashpay`: **46 passed, 0 failed**, including the CLI unit and integration cases. `cargo clippy --locked --workspace --all-targets -- -D warnings`: pass. The validation service recorded **VALIDATION EXIT 0**. Logs: `workspace-dashpay.log` and `clippy-workspace.log`.
- The supplied `/work/scratch/dp2-01/gate-test.log` independently totals **886 passed, 0 failed, 5 ignored**. Its script runs fmt, workspace clippy and workspace tests, but the retained test log alone does not attest the claimed `GATE EXIT 0` or record the commit hash. Treat that full-gate claim as implementer evidence, separately from the checks above.

## Limits

Android source comparison used the available checkout `ef552d410763ad01271f7cec78d36786ca80f156`; the inventory's `a8de1f0794` object is not present locally. The four choices also match the written inventory.

The public testnet fixture was not rerun: no fixture path was supplied, and no private directory was read. Its `!unverified` assertion at this head is not evidence of DEC-183/SPV content-bound verification: before the fallback latch, this implementation reports no entity as unverified. The live fixture claim supports observed relation/channel behavior, not the deferred trust-provider integration.

Replay validation exercises the library's restore application calls with equal model/join inputs; it is not a new end-to-end crash/restore test. Cold username fetches can change name-based sorting, as expected from the declared memory-only residual. No transactions, funding, platform writes, UI execution or GitHub actions were performed. The review does not approve the forthcoming payment gate or DEC-183 adaptation.

R1-01 needs correction before this read model can be relied on through its advertised event contract. The three minor findings are independent of the deferred provenance class changes.

**Verdict:** CHANGES-REQUIRED
Sol gate satisfied: no
