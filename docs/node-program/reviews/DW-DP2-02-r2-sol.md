# DW DP2-02 — Sol review, round 2

Reviewed `5cbfe8c..ba85951` in a detached scratch worktree. The three commits have good signatures. The implementer's branch and worktree were untouched.

## Findings

1. **Medium — possible security note: see manager.** The fixed static missing-key cases pass, but the round-1 guarantee is not complete. A scratch-only adversarial reproduction passed; details and the proposed boundary are in chat. The claim in m4 §7 item 39 that no grant is used by a write the vault cannot sign needs to be made true or narrowed.

No other fresh bypass was found in send, accept, key upgrade, retry, scan cancellation, or lease reuse. Existing lease ids check their signer before the Credits charge. A Lock during the vault probe fails through the epoch/grant checks; after a successful probe, a Lock invalidates the grant or revokes the lease. The lock-after-signature broadcast remains the accepted E0-04 P3 residual.

### Vault API and ordering

`Vault::grant_identity_key_matches` is a bounded yes/no probe of an identity derivation path under a wallet-bound `PlatformOp` grant with a positive Credits cap. It neither signs nor exports a key. The grant already authorizes the corresponding identity signer, so this probe does not disclose a stronger capability to its bearer. It copies an own key with zeroization, rechecks the grant under the signer-issuance guard, and the signer's operation gate enforces the epoch during derivation. The race test covers redemption and Lock between the two checks. The remaining issue is outside that guarded probe.

With no eligible candidate key, the cap or lease error can precede `identity.keys_missing`. This ordering is acceptable: no grant is redeemed and no lease is charged in either refusal, and the contract does not require a key-error precedence. E0-04 §16.1 and N-6 now agree with m4 version 12 on another-flow leases; m4 §4 and §7 item 39 accurately record the accepted Lock residual. The one overbroad m4 sentence is tied to finding 1.

## Round-1 disposition

| Item | Disposition | Evidence |
|---|---|---|
| 1. Grant redeemed before key possession check | **Reopen.** The ordinary absent-key cases are fixed for send, accept, and key upgrade; possible security note: see manager. | `requests.rs:214-220,253-280,353-363`; focused tests pass; adversarial reproduction passed. |
| 2. Another-flow lease error contract | **Closed.** | E0-04 amendment 3, N-6 and §16.1 specify `platform.grant_invalid`, matching `requests.rs:223-225` and m4 v12. |
| 3. Lock after signature | **Closed as accepted P3 residual.** | m4 §4 and §7 item 39 record that signed bytes can broadcast after Lock; no fence added. |

## Mutation table

Each mutant was applied alone, run against a focused test, and restored; the source files were touched after restoration to avoid stale Cargo mtimes. All six were killed by test assertions, not compilation errors. The first standalone vault-test commands lacked the workspace's dpp feature unification; the vault mutants were rerun with both `dw-vault` and `dw-engine` selected.

| Mutant | Focused test | Result |
|---|---|---|
| M1: omit the grant-path key precheck | `a_send_without_a_held_key_keeps_the_grant` | Killed |
| M2: move the key check after lease redemption | `a_send_without_a_held_key_keeps_the_grant` | Killed |
| M3: consume the grant in the probe | `a_grant_tells_whether_the_vault_holds_an_identity_key_without_being_used` | Killed |
| M4: skip the grant recheck under the signer guard | `a_key_probe_does_not_outlive_its_grant` | Killed |
| M5: allow a zero-Credits grant through the probe | `a_grant_tells_whether_the_vault_holds_an_identity_key_without_being_used` | Killed |
| M6 (own): skip `can_sign_with` for an existing lease | `a_lease_is_not_charged_without_a_held_key` | Killed |

## Validation

- Request tests: 18 passed, 1 public-testnet test ignored.
- Focused vault grant-probe and race tests: both passed.
- Scratch-only adversarial reproduction: passed, then removed from the worktree.
- E0-04 design model: passed. `cargo fmt --all -- --check` and `git diff --check 5cbfe8c..ba85951`: passed.
- Implementer's reported full gate: 981 passed, 0 failed; not independently rerun.

**Verdict:** REQUIRES CHANGES

Sol gate satisfied: yes
