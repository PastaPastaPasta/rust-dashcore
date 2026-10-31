# DW DP2-02 — Sol independent review, round 6

Reviewed detached `e8b1850` on `58b5510` in `/work/scratch/dp2-02-sol-r6/wt`. This review is limited to the rebase, the call-permit port and the r5 accessor fix. The implementer's worktree and branch were untouched.

## Closure of r5

- **(a), raw signer from a counted signer — closed in current production code.** `VaultIdentitySigner::signer` is private, `vault_signer()` is gone, and `derive_public_keys` returns `BTreeMap<KeyID, IdentityPublicKey>` after checking narrowing and identity index (`signers.rs:97-149`). `sign` still checks the key allow-list before `count.begin()` and reports a released signature through the count (`signers.rs:182-218`). Send, accept and key enablement obtain their transition signers from `CallPermit::identity_signer` (`requests.rs:408-419,442-446,609-620,723-737`); the registration caller now derives public keys through the wrapper (`registration_flow.rs:2769-2793`). No production path in these writes obtains the wrapper's inner `VaultSigner` through a field, `Deref`, conversion, clone, or closure. Separate `Lease::identity_signer` issuance remains for DP1-02 profile writes (`lease/table.rs:888-913`, `registration_flow.rs:2862,2882`); the DP2-02 call paths do not use it. The source guard has the P3 limitation below.
- **(b), adopted accept — accepted and recorded.** `accept_request` finishes the permit with `accepted.is_ok()` (`requests.rs:618-620`). `CallPermit::end(true)` is `Sent` (`lease/call.rs:866-877`), so the quote remains in the lease budget even if the library adopts an existing request without signing. The new residual says this explicitly and distinguishes lease charge from Platform credits spent (`m4-dashpay-engine.md:2099-2102`).
- **(d), early refusal after permit — still closes safely.** Early `?` returns drop the permit; `Drop` calls `end(false)` (`lease/call.rs:948-953`) and an unsigned call refunds its charge (`lease/call.rs:871-918`). The new unsigned/signed write regression test supplies committed coverage for the relevant outcome (`requests_tests.rs:670-725`).

## Scope results

| Item | Result and evidence |
|---|---|
| 1. Rebase | Pass. Both supplied range-diffs map the earlier commits without a dropped hunk; the changed mappings are the documented base conflicts and item-number edits. Main's DP1-02 item 40 and eight implemented rows precede DP2-02 item 41 and six rows (`m4-dashpay-engine.md:2021-2035`, `m4_dashpay_contract.rs:64-79`). `platform/mod.rs:34-45` retains registration modules and adds requests. The signer conflict is resolved by the public-key-only API, with DP1-02's caller ported (`registration_flow.rs:2769-2793`). Version 15 and marker `8a57eb15…` are present (`m4-dashpay-engine.md:3,541`); the unblessed contract test passed. All 26 commits in `58b5510..e8b1850` are `G` signed; the backup ref equals `60dc558`. |
| 2. `a80287c` | Pass. `write_permit` charges a Credits quote (`requests.rs:390-403`); send, accept and key enablement keep the permit through the awaited write and finish after dropping the counted signer (`requests.rs:564-572,618-622,723-744`). The reaper and Lock observe live calls through `CallPermit` (`lease/call.rs:508-555`, `lease/lock.rs:94-104,246-255`). The old `Lease::charge_credits` path is removed. |
| 3. `d6cce47` | Pass. The fake network's unsigned failure and signed timeout are tried for all three writes; the test asserts refund to zero before a signature, full quote retained after one, and no second write on an exhausted one-quote lease (`requests_tests.rs:670-725`). An independent wrong-finish mutant was killed by the accept assertion. |
| 4. `3f44cf3` | Pass with documentation nit N2. The contract and E0-04 amendment name version 15 and item 41, while item 39 supplies the call-permit rule (`m4-dashpay-engine.md:2031-2058`, `E0-04-grants-leases.md:37-40,3043-3044`). The generated surface marker and unblessed m4 test agree. |
| 5. `da7b0cd` | Pass. Live `PwLib` profile create and update hold the shared `(wallet,id)` write lock across the library call (`registration_flow.rs:2854-2890`, `identity_writes.rs:31-44`). The registration driver calls name and profile separately without already holding that lock (`registration_flow.rs:2204-2209,2275-2293`); its name call enters the same lock in `names.rs:989-1018`. Signer lookup finishes its lease-table section before the new awaited lock (`lease/table.rs:843-872,888-913`); vault signing takes its gate only inside the later signer call (`dw-vault/src/signer.rs:218-245`). No lock re-entry or gate-order cycle found. The Platform writes of existing identities on main are the named name, profile, request, accept and key-update paths; all hold this lock (`names.rs:1017-1018`, `requests.rs:530-531,594-595,700-701`). |
| 6. `04ea198` | Pass for current production behavior; guard nit N1. The only new derivation path returns public keys, rejects a narrowed signer with `Internal` and a foreign index with `InvalidArgument` (`signers.rs:129-149`). Its result equals registration-derived keys, including the two enabled keys (`requests_tests.rs:632-668,1021-1054`). The compile-fail doctest and source test pass (`signers.rs:87-95,548-580`). Key sign permission and count remain enforced (`signers.rs:182-218,252-271`). |
| 7. `e8b1850` | Pass. The accepted residual correctly names unsigned adoption, `finish(ok)` / `Sent`, a kept lease quote and zero Platform credits spent (`m4-dashpay-engine.md:2099-2102`). |

## New findings and nits

1. **P3 — the source guard is a tripwire, not a complete no-accessor proof.** Probe: added `type SignerAlias = VaultSigner` and `pub(crate) fn raw_alias(&self) -> &SignerAlias { &self.signer }` in the scratch worktree. `no_vault_signer_leaves_the_module` passed because it searches return text for the literal `VaultSigner` (`signers.rs:556-580`); the public `vault_signer()` compile-fail example would also remain unaffected. Such an accessor could let a future crate-internal DP2-02 caller clone and sign outside the count. This mutant introduced a new API; no such API exists at the reviewed head. **Fix:** describe the source test as a regression tripwire, or strengthen it with Rust-aware exported-type/alias analysis if it must enforce the claim. Keep code review on changes to `signers.rs` and `Lease` signer issuance.
2. **P3 — stale numbering/tense in comments.** `m4-dashpay-engine.md:2044-2046` still says DP1-02 writes will run under the lock “when they land,” although DP1-02 is on main and `da7b0cd` locked the profile writes. `requests_tests.rs:670-673` still calls DP2-02's row item 40. **Probe:** compare those lines with `m4-dashpay-engine.md:2021-2035` and `registration_flow.rs:2864-2889`. **Fix:** name DP1-02's current name/profile writes and change the test comment to item 41.

## Independent mutation probes

Each mutation ran alone, then the file was restored from a saved copy, touched and byte-compared. All failures below were assertion failures, not compilation failures.

| Mutation | Focused test | Result |
|---|---|---|
| Let narrowed `derive_public_keys` run | `a_permits_signer_hands_out_public_keys_only` | **Killed** at `requests_tests.rs:658`: expected `Internal`. |
| Let a foreign identity index derive | Same | **Killed** at `requests_tests.rs:653`: expected `InvalidArgument`. |
| Finish unsigned accept with `true` | `a_quote_goes_back_only_when_nothing_was_signed` | **Killed** at `requests_tests.rs:708`: quote was not returned. |
| Add an aliased `pub(crate)` raw-signer accessor | `no_vault_signer_leaves_the_module` | **Survived**; P3 N1 above. |

## Validation and limits

- `cargo test -p dw-vault -p dw-engine --lib`: **79 passed, 1 ignored** (vault); **729 passed, 5 ignored** (engine). The reported `registration_tests::rules::finish_gives_no_unindexed_lock_to_a_row_whose_funding_call_runs` passed in this full run. Its prior failure under load involved the pre-existing fake registration path (`registration_tests/rules.rs:344-386`), not the changed `PwLib` methods; five isolated passes make a load-sensitive race/flaky assertion plausible. The prior assertion can also reflect a latent DP1-02 interleaving, so it is not proof that the underlying behavior is impossible; this range does not touch its production or fake-library path.
- `cargo test -p dw-engine --doc`: **1 passed**, including the `compile_fail` example. Unblessed `cargo test -p dw-engine --test m4_dashpay_contract`: **9 passed**.
- `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check`, `git diff --check 58b5510..e8b1850`, signature and backup-ref checks: **passed**. The detached worktree was clean after every mutation was restored.
- Public testnet T2 was not run (its fixture is ignored); this review used the local fake network and static call-path checks. The supplied range-diffs and conflict notes were inspected; no earlier-round behavior was reopened without a new causal path.

**Verdict:** APPROVE-WITH-NITS
Sol gate satisfied: yes
