# EV2a — Opus-high review (after Sol r2)

Review of `node/ev2a-dml` at `233d06af58142437d81669c681b49dd54c54a597` (`14f6bed58` plus one DEC-182 docs commit with no source change), over integration `76ae54e61`. L1 is on `node/ev2a-l1-oracle`. This review follows REVIEW-COMMON.md. The spec is the PLAN row EV2a, `designs/EV2-dml.md` rev4 with the BL-67 erratum (§3.7), the EV addendum FINAL rev6 §4.1, SB0 rev6.1, and DEC-109/117/122/182.

I reviewed in a detached scratch worktree under `/work/scratch/ev2a-opus/`, with `nice -n 10`, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=`, and TMPDIR on disk at mode 0700. The branch was not edited. This report is uncommitted.

Private material for EMBARGO-5: see the private erratum package.

## Findings

### M1 (major): the erratum's point clause has no gated test

- **Location:** `dash-dml/src/diff.rs:248-253` (`operator_key_changed`), tests from `diff.rs:423`.
- **Problem:** I ran mutations against `operator_key_changed`. Removing its third clause (the `BlsLazy` `!=` on clones) passes every tracked test: 79 unit tests plus 2 doctests. So does reducing it to captured-bytes equality alone. The private suite catches both mutants, but no gate runs it: `contrib/node/check-private-tests.sh` covers only the EMBARGO-1 test.
- **Failure scenario:** a later refactor "simplifies" the comparison to bytes. All tracked gates and L1 stay green. Online update, the recorded diff, the snapshot and reconstruction then diverge in the way the erratum exists to prevent.
- **Fix:**
  1. Add a scheme-free tracked guard next to `operator_keys_are_recorded_by_point_or_encoding`. Compare a default operator key with a key set from a failed decode, with every other field equal. Both serialize to zeros under any scheme, but Core's `operator==` says they differ. Assert that `MnStateDiff::between` records `PUB_KEY_OPERATOR | VERSION` in both directions. I checked this in scratch: it passes at head and fails on the bytes-only mutant. It names nothing beyond EV0's existing failed-decode guard.
  2. Separately, the manager should wire the private suite into `check-private-tests.sh` as a receipt, now that EV2a's targets exist.

### m1 (minor): D17 is stated but not checkable

- **Location:**
  - `dash-dml/src/list.rs:780` (`in_state_domain`);
  - `list.rs:521-526` (the `update` docs);
  - `list.rs:730` (`check_invariants`, I-S);
  - `API.md:77`.
- **What D17 is:** a non-default operator key must be captured under its version's scheme.
- **Evidence:** every rc.3 write site keeps D17:
  - ProRegTx reads the payload key under the payload version;
  - the ProUp* paths re-encode through `SetStateVersion`;
  - pre-V24 ProUpServ doesn't change the version;
  - a reset key is the default.

  D17 is therefore part of Core's state domain, in the same way R1-M2's version-hidden fields are. Yet the domain check doesn't enforce it, and neither does I-S.
- **Failure scenario:** an EV2b or importer bug installs a state that violates D17. The key is not decoded yet, so `to_bytes_as` under the version's scheme yields zeros, Core's stale-object path, which `BlsLazy` mirrors. Those zeros then reach the diff record, the canonical snapshot and EV4's SML leaf. The rebuilt list stops matching the online one, and nothing reports it. The second clause of `operator_key_changed` can't repair this, because it records the zeros.
- **Fix:**
  1. Refuse a non-default key whose `scheme()` differs from `scheme_for(version)` in `in_state_domain`, with a new `field` label on `VersionHiddenField` or a sibling variant. Mirror the check in I-S.
  2. Drop the "caller precondition" wording in API.md §4.
- **Cost of the fix:** I tried it in scratch. Every tracked test passes, and every Core-semantics private replay passes. The only failures are negative controls that build such a state on purpose; they would need to expect the refusal.
- **Follow-on:** once D17 is enforced, the version-scheme clause in `operator_key_changed` becomes defensive only. Removing it passes both suites. Keep it, but comment it as such.

### n1 (nit): the V24 gating of `has_operator_key_under_any_scheme` is in the docs only

- **Location:** `dash-dml/src/list.rs:408`.
- D11 leaves the gating to EV2b. That is correct: every rc.3 call site is V24-gated, either directly or because its transaction check refuses before V24. EV2a ports none of them.
- **Suggestion:** a `V24Active` token argument, or a method on a V24-only view, would make the gate checkable by type, in DEC-117's spirit. This is optional.

### n2 (nit): the `unique_keys` cache vs an in-place failed decode

- **Location:** `dash-dml/src/types.rs:165` (`ColdDerived::unique_keys`).
- **Behaviour:** the cached `StateKeys` is computed once. Suppose a stored key's first decode fails later, in place. Core's `RemoveMN` would then skip a now-"default" key. The Rust code deletes the cached slot instead.
- **Reachability:** none while stored keys are valid, which is the importer's and the transaction checks' job.
- **Suggestion:** a one-line note at the cache saying the cached keys assume decodes don't change identity. No code change is needed.

## Confirmations

- **EMBARGO-5 paths:** I checked online update, diff build, diff apply, snapshot write/read and reconstruct against the private rules. Details are in the private package. Apart from M1 (test coverage) and m1 (enforcement of an assumed precondition), the implementation matches.
- **Unique properties:** the map is keyed by the serialization preimage, so the operator key uses its captured-scheme bytes. The update re-keys on byte change, which is rc.3's rule. On every transition v23 can reach, this rule equals v23's, and the tracked test `operator_slots_move_with_the_serialization` covers rc.3's re-key. `StateKeys::build`'s null test never decodes. The add, delete and replace ordering matches Core.
- **Core order:** `order.rs` matches immer's CHAMP order:
  - `MAX_DEPTH` 13 at 5 bits per level;
  - collision buckets newest-set first;
  - `bucket_set`/`bucket_remove` maintenance;
  - restoration from record trailers.
- **State domain and budget reader:** I re-derived Sol's R1-M1 and R1-M2 closures against rc.3 and they hold, including the uncharged ADDRv2 length. I found no new issue.
- **API soundness for EV2b and EV4:**
  - D16 (the importer leaves keys lazy) is sound. Hashing uses captured bytes, and decodes happen only where Core decodes.
  - D18's version-projected `SameRecord` is consistent with `codec::state_bytes`.
  - D20's atomic domain refusal on apply is correct. It clones a candidate and installs it only after every step succeeds.
  - D17 is m1 above.
- **Concurrency and memory:**
  - Every list structure is an `imbl` persistent map or an `Arc`, plus `OnceLock` caches, so clones are O(1).
  - `try_add` and `apply_diff` clone, then swap.
  - `BlsLazy` serializes its decode behind a mutex. Comparisons work on clones, so shared lists see no mutation.
  - DEC-182's SipHash-1-3 with random keys is fine. Equality is by content, and nothing iterates the map, so hash order can't leak into the output.
- **Leak scan:** I searched the branch's tracked text and commit messages for embargo specifics and found none.

## Validation

| Check | Result |
|---|---|
| `cargo test -p dash-dml --all-features` at head | 79 unit + 2 doctests pass |
| Private suite at head (see the private package) | all pass |
| Mutant: drop the point clause / bytes-only rule | tracked **all pass** (M1); private fails |
| Mutant: drop the version-scheme clause | both pass (see m1) |
| Mutant: bytes-only `same_operator_key` | tracked fails (EV0 guard) |
| Mutant: point-rule unique re-key | tracked fails (rc.3 re-key test) |
| Scratch: M1 guard | passes at head, fails on the bytes-only mutant |
| Scratch: D17 enforcement in `in_state_domain` | tracked all pass |

The scratch evidence is in `/work/scratch/ev2a-opus/mut/`.

**Verdict:** CHANGES-REQUIRED
