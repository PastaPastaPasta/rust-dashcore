# DW DP2-02 — Sol review, round 3

Reviewed `ba85951..8bca12e` in a detached scratch worktree. All six commits have good signatures. The implementer worktree and branch were untouched.

## Findings

1. **Medium — possible security note: see manager.** The round-2 grant-redemption race is fixed for direct DP2-02 writes, but a separate claim in m4 §7 item 39 needs the manager's disposition. Scratch-only timing evidence was reproduced; substance is in chat.

## Disposition

| Question | Disposition | Evidence |
|---|---|---|
| Round-2 reopened item 1: deletion between advisory probe and redemption | **Closed for direct send, accept and key-upgrade grants.** A deletion that wins leaves the grant unused and returns `identity.keys_missing`. | Scratch reproduction and `a_removal_after_the_key_check_keeps_the_grant` passed. `redeem_grant_holding_key` checks a candidate and redeems under `writer()`. |
| Other DP2-02 direct write entry points | **No unchecked fallback found.** Accept and key upgrade use `write_lease`; candidate selection includes the key-upgrade MASTER level. | `requests.rs`, `lease/session.rs`; four focused absent-key and existing-lease tests passed. |
| Explicit lease creation, rebind and background crypto | **Distinct E0-04 paths.** `begin_flow` and `rebind_lease` use generic redemption without a concrete write's candidate keys; a later contact write checks its signer before charging Credits. Background crypto creates a signer without redeeming a PlatformOp grant. | `flows.rs`, `lease/session.rs`, `requests.rs` and `lease/table.rs`. |
| Vault lock order and seed removal | **No new lock cycle or unguarded seed deletion found.** The OS-store read can wait under `writer()`, before entering the signer operation gate. Wipe, delete, recovery, passphrase change and destroy use the same writer guard; the wallet-removal facade calls wipe. | `vault.rs`, `wallets.rs`; vault race test passed. |
| Removal-loses race; m4 §7 item 39 | **possible security note: see manager.** | Scratch-only timing probe and code-path review. |

## Mutation table

Each mutant was applied alone in the scratch worktree and restored. The affected files were touched after restoration to avoid stale Cargo mtimes. Every final result below was killed by a test assertion, not a compile failure.

| Mutant | Focused test | Result |
|---|---|---|
| M7: omit the vault writer guard | `a_removal_waits_for_a_key_checked_redemption` | Killed |
| M8: use unchecked PlatformOp redemption in `issue` | `a_removal_after_the_key_check_keeps_the_grant` | Killed |
| M9: redeem with no candidate | scratch reproduction | Killed |
| M10: ignore the candidate's public-key match | scratch reproduction | Killed |
| M11: pass an empty candidate list from `write_lease` | `a_send_refuses_before_it_redeems_the_grant` | Killed on rerun; the first, unrelated removal-refusal test survived |
| M12: map no held key to grant-invalid | `a_removal_after_the_key_check_keeps_the_grant` | Killed |
| Own: consider only the first candidate | scratch reproduction | Killed |

## Validation

- Original round-2 probe → removal → redemption interleaving: refused without consuming the grant. The scratch-only reproduction was removed afterward.
- `a_removal_*`: three passed; `without_a_held`: four passed; successful send and vault candidate-redemption tests passed.
- Additional scratch-only timing probe: **possible security note: see manager**. It was removed afterward.
- `cargo fmt --all -- --check`, `git diff --check ba85951..8bca12e`, and final scratch worktree cleanliness: passed.
- Implementer's full gate (983 passed; one load-sensitive timing test passed alone) was reported, not independently repeated.

**Verdict:** REQUIRES CHANGES

Sol gate satisfied: yes
