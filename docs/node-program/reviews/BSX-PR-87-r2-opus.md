# BSX PR #87 r2, narrow pre-push re-review (Opus)

## Verdict: ACCEPT WITH NITS

- **Repo:** dashpay/base-sdk PR #87, branch `node/bsdk-netinfo-b3-r2`
- **Head:** `d3bfecff`, 9 commits on develop `8982803`. All 9 are signed (`%G?` = G) and based directly on 8982803.
- **Reference:** dashpay/dash `v24.0.0-rc.4` (0e7382df80), checked out in a scratch worktree.
- **Inputs:** the r1 review (`reviews/BSX-PR-87-cde-r1-opus.md`), the DEC-474 ruling, both range-diffs and the proposed body `pr87-body-base-sdk-netinfo-r2.md`.
- **Reviewer:** Claude Opus 5.5, review sub-agent. Read-only: nothing was committed, pushed or posted.
- **Date:** 2026-10-26

M1 is held under the batch-3 hold (DEC-411/DEC-432), and this revision adds no commit for it, as DEC-474 rules. M2 and every r1 minor and nit item in scope are resolved. Nothing here blocks the push. The findings below are a test-coverage nit, two body-wording nits, an optional citation nit and the title.

## What I ran

- **Bisect.** I ran `cargo test -q -p dash-primitives --all-features` at each commit, with `RUSTC_WRAPPER=`, `CARGO_BUILD_JOBS=8`, `TMPDIR=/work/tmp`, in a scratch worktree. Every commit exits 0, with 0 failures. Tests passed per commit, in order: 272, 278, 278, 278, 278, 280, 280, 285, 285.
- **Corpus pin and order.** A script expands each embedded program's hex list, including `long_domain(n)`, and compares it with the section's `raw` list. They match in count and order at all 9 commits. The header reads `v24.0.0-rc.4, 0e7382df80` from commit 1 onward.
- **Corpus contents.**
  - The set of vector lines at d3bfecff is identical to the set at 54ac4f1. r1 reproduced all 80 of those vectors byte for byte at rc.4.
  - Every intermediate commit's vectors are a subset of the head's set, with 0 lines outside it.
  - So each commit's corpus is rc.4 output. I didn't rebuild rc.4.
- **Churn.** I blamed every line each commit deletes against its parent. All of them come from develop. No commit deletes or rewrites a line that an earlier commit in this PR added, and the corpus grows only by addition.
- **Residue.** No commit diff contains `BTreeMap`, a "first group" or "keep first" rule or vector, `rc.3` or `0364ed626e`. The only "kept" hits are entry-level vectors in commit 1 (reserved TLD, privileged port, legacy port 0), which are unrelated to purposes.
- **Mutants at head** (`--lib netinfo`):

| Mutant | Result |
| --- | --- |
| Descending pair accepted (`Greater => {}`) | killed by `check_v2_structure::descending_purposes` |
| Equal pair reported as `Malformed` | killed by `check_v2_structure::duplicate_purpose_key` |
| Group loop calls `check_service(svc, None)` | killed by `check_v2_group::service_https_port_443` and `extended_checks_match_vectors` |
| `NIEntry::check` calls `check_service(svc, Some(PlatformHttps))` | **survives** (see N1) |

- **Policy.**
  - "Dash Core" appears in neither the diff nor the body.
  - The commit messages contain no reference-implementation identifiers.
  - Subjects are at most 72 columns.
  - MSRV is 1.89, so `Option::is_none_or` (stable since 1.82) is fine.

## r1 items

| Item | Verdict |
| --- | --- |
| M1 | Held under the batch-3 hold (DEC-411/DEC-432). No commit was added, as ruled. |
| M2 | **Resolved.** 6d8ed080 is gone. 84dae1ac (commit 2) puts the strict `read_purpose` rule directly on develop's `Vec` decode, keeping `with_capacity` as develop has it, with no prelude change. The corpus is rc.4 from 169f487c. 39a25ebd cites rc.4 from the start, and the body has no "Superseded" row and no rc.3/rc.4 story. Each commit's tests pass at that commit. |
| m1 | **Resolved.** `netinfo.rs:365-371` uses `windows(2)`: equal → `Duplicate`, descending → `Malformed`. This matches the reference's single rule at netinfo.h#L431 (`purpose <= m_data.rbegin()->first`, refused). A value the reference can't represent (its `std::map` keeps keys ascending) is refused by `check`, so a hand-built value round-trips through `decode`. `descending_purposes` and `ascending_purposes` were added at :1024-1037, and `duplicate_purpose_key` already pins the equal case. |
| m2 | **Resolved.** `check_service(svc: &ServiceV2, purpose: Option<NIPurpose>)` at `netinfo.rs:246-255` matches `ValidateService` at netinfo.cpp#L459-L471: I2P must be on port 0 (`ServiceV2::check`, `I2P_SAM31_PORT{0}`), and otherwise port 0 or a bad port is refused unless the purpose is `PLATFORM_HTTPS` and the port is 443 (`IsAllowedPlatformHTTPPort`, #L59-L66). The group loop (:413-416) passes the purpose. `NIEntry::check` passes `None`, so its behaviour is unchanged. Nothing matches on an error value any more. |
| m3 | **Resolved.** All six refused-purpose vectors sit directly under the heading and the netinfo.h#L425-L435 link (json5:294-308). Their program lines (json5:234-239) are in the same order. |
| m4 | **Resolved.** The body's "validity" link is now netinfo.cpp#L606-L619. |
| m5 | **Resolved.** The body says "on `develop` 8982803", and the disclosure describes the r1 review and its outcome. |
| n1 | **Resolved.** The closure parameter is `candidate`, and the list uses `.map(NIPurpose::to_base)` (`netinfo.rs:499, 507-508`). The inner `\|last\|` in `is_none_or` is r1's suggested form and is acceptable. |
| n2 | **Acceptable as is**, as r1 said. `after_last_purpose` pins `expected: []`. |
| n3 | **Resolved.** netinfo.cpp#L482 sits above the domain-443 vector (json5:385), and #L476-L494 above the `a.b` HTTPS check vector (json5:392). |
| n4 | **Resolved.** The empty-label vector now cites netinfo.cpp#L105-L107 and netinfo.h#L261 (json5:339-340). |

## Citations

Every netinfo.h and netinfo.cpp link in the corpus and the body resolves at 0e7382df80 to the content it claims:

- **netinfo.h:**
  - #L176-L180 (`DomainPort` serialisation), #L194-L198 (type codes), #L227-L240 (entry write);
  - #L242-L263 (entry read), #L251 (invalid service cleared), #L253-L261 (domain branch), #L261 (invalid domain cleared), #L262 (unknown type cleared);
  - #L326-L348 (legacy read), #L408-L436 (extended write and read), #L425-L435 (purpose rule), #L443 (`IsEmpty`).
- **netinfo.cpp:**
  - #L59-L66 (allowed HTTPS port), #L90-L139 (see N4), #L105-L107 (empty label), #L264-L275 (text);
  - #L459-L471 (service port rule), #L476-L494 (`ValidateDomainPort`), #L482 (domain port), #L606-L619 (per-entry validation);
  - #L610-L615 (domain purpose). The exact `if` is L611-L614; the cited range adds one line of context on each side, which is acceptable.
- **Other files:**
  - netaddress.cpp #L135-L162 (`SetLegacyIPv6`) and #L426-L453 (`IsValid`);
  - providertx.cpp #L318-L321;
  - evo_netinfo_tests.cpp #L319-L404 (`netinfo_ser`).

The commit messages contain no links.

## Findings

### Nit

**N1. Nothing pins `NIEntry::check`'s refusal of a service on port 443.**
- **Where:** `pkgs/primitives/src/types/netinfo.rs:810` (`check_entry_service` cases), in 39a25ebd.
- **Problem:** after m2's refactor, `NIEntry::check` must keep its purpose-less meaning, refusing 443 for a service. The mutant that makes it call `check_service(svc, Some(NIPurpose::PlatformHttps))` passes every test.
- **Fix:** in 39a25ebd, add `#[case::ipv4_port_443(AddrV2::Ipv4([1, 2, 3, 4]), 443, Some(NIError::BadPort { port: 443 }))]` next to `ipv4_port_privileged`, then re-comb 39a25ebd and d3bfecff. This is optional. The group-level 443 rule is already pinned both ways (`service_https_port_443`, `service_core_p2p_port_443`).

**N2. Body bullet 1 names only the repeated purpose as the subject.**
- **Where:** body line 5.
- **Problem:** before this PR, unknown and out-of-order purposes were also read, keeping every group, but the bullet's subject is only "a purpose that repeats".
- **Fix:** "- a repeated, out-of-order or unknown purpose is read, with every group kept in wire order, where the reference implementation refuses the value;"

**N3. Body bullet 5 mixes the before and after states.**
- **Where:** body line 9, "`check` accepts groups out of purpose order, which `decode` now refuses;".
- **Problem:** the list describes behaviour before the PR, but "now" refers to after it.
- **Fix:** "- `check` accepts groups out of purpose order;". The refusal by `decode` is already the first bullet's subject.

**N4. One body link is broader than its claim (optional).**
- **Where:** body line 21, "[domain validity](…netinfo.cpp#L90-L139)".
- **Problem:** the range includes `DomainPort::Set` (#L116-L128), which the read never runs.
- **Fix:** link #L90-L114 (`ValidateDomain`) and #L130-L139 (`Validate`) instead, or leave it as is.

## PR body (DEC-474)

- **No parity claim.** The body doesn't claim full read parity, and it doesn't describe, hint at or enumerate anything that still differs.
- **Neutral list.** The motivation uses the neutral form DEC-474 asks for: "This pull request changes how it reads and checks network info in these cases:".
- **Scope sentence.** Line 28 ("Apart from the port 443 rule, … `check` is unchanged …") states this PR's scope, not a parity claim, so it's acceptable.
- **Disclosure.** The disclosure doesn't mention the held item.
- **Breaking Changes.** The list matches the code: the `decode` purpose refusal, `check` accepting 443 in HTTPS and returning `Malformed` for descending groups, `Invalid`, legacy and emptiness.
- **After this review.** Extend the disclosure (line 63) with one sentence, for example "This revision was re-reviewed by a Claude Code agent using Opus 5.5, which accepted it." Then read the body back after `gh pr edit` (CLAUDE.md rule).

### Title

The current title, "primitives%fix!: read network info as the reference implementation does", over-claims. Proposed (73 characters):

```
primitives%fix!: refuse bad NetInfo purposes, read bad entries as invalid
```

An alternative that also covers the emptiness and 443 commits (73 characters):

```
primitives%fix!: fix NetInfo purpose, entry, emptiness and port 443 rules
```

## kwvg policy

- **Wording:** "reference implementation" throughout, never "Dash Core".
- **Commit messages:** no reference-implementation identifiers ("NetInfo" is the existing subject term).
- **Comments:** no expository comments. The new comments are one-line docs (`check_service`, the `entries` field) and the `# Errors` rustdoc.
- **Tests:** none are tautological.
  - `ascending_purposes` is the positive control for `descending_purposes`.
  - The `decode_v2_purpose_refused` cases pin both `actual` and `expected`.
  - The corpus tests compare against reference output.
- **Churn:** none (see What I ran).
- **`!` markers:** `fix!` is correct on every behaviour-changing commit, and the two `test` commits are unmarked.

## Cleanup

These are removed after the report:
- `/work/scratch/bsdk87-r2-review/bsdk` (base-sdk worktree, including its reflinked target);
- `/work/scratch/bsdk87-r2-review/dash` (rc.4 worktree);
- the rest of `/work/scratch/bsdk87-r2-review`.

The author's `/work/scratch/bsdk-netinfo-b3` and the `bsdk-wt/netinfo-b3` worktree were only read from.
