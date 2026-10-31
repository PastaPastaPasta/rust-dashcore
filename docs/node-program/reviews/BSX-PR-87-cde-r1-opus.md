# BSX PR #87 commits c/d/e, review r1 (Opus)

- **Repo:** dashpay/base-sdk PR #87, branch `node/bsdk-netinfo-b3-cde`
- **Range:** `09d132a..54ac4f1`
  - 6232e0f `primitives%fix!: allow port 443 for services in the Platform HTTPS group`
  - 3638a49 `primitives%fix!: refuse unknown or out-of-order NetInfo purposes`
  - 54ac4f1 `primitives%test: pin extended NetInfo domain entries to the reference`
- **Reference:** dashpay/dash `v24.0.0-rc.4` (0e7382df80), with `v24.0.0-rc.3` (0364ed626e) for the 443 rule
- **Reviewer:** Claude Opus 5.5, review sub-agent; read-only, nothing committed, pushed or posted
- **Date:** 2026-10-26

## Verdict: CHANGES REQUIRED

The code in the three commits matches the reference. Every new vector reproduces byte for byte against rc.4, and each fix's tests fail when the fix is reverted. Two Major findings block the push:

- M1: held under the existing batch-3 hold (DEC-411/DEC-432); see the manager.
- 3638a49 rewrites the PR's own earlier commit 6d8ed080, which maintainer policy forbids.

## What I ran

- **Corpus regeneration.** I copied the rc.4 build tree to my own scratch directory and extracted the four `/* ... */` programs verbatim from `netinfo.json5` at 54ac4f1. I appended them to `evo_netinfo_tests.cpp`, rebuilt `test_dash` and ran each one. Results against the corpus:
  - `netinfo_entry_reads`: 32/32 match;
  - `netinfo_legacy_reads`: 13/13 match;
  - `netinfo_extended_reads`: 26/26 match;
  - `netinfo_extended_checks`: 9/9 match.
- **rc.3 for 6232e0f.** Between rc.3 and rc.4, `ValidateService` changes only the `RequireRoutableExternalIP` line, which adds an RFC1918 check. No `extended_check` address is RFC1918 (1.2.3.4, 2606:4700:4700::1111, an I2P address, domains), so the rc.3 output equals the first seven rc.4 lines.
- **Bisect.** `cargo test -p dash-primitives --all-features` passes at each of the three commits: 276, 280 and 280 unit tests, plus 3 others. dash-primitives is the only package these commits touch, and no other package uses `NetInfoV2` or `NIPurpose`.
- **Lint.**
  - `cargo fmt --check --all` passes at each commit.
  - `cargo clippy -p dash-primitives --all-features --all-targets -- -D warnings` passes at each commit.
  - `--no-default-features` passes at 3638a49, which matters because of the prelude change.
  - `maint/lint/lint_commit.py run -r 09d132a..54ac4f1`: PASS ×3. The widest line is 72 columns, and each message has at most 5 lines.
  - All three commits are signed (`%G?` = G).
  - The tip merges cleanly onto develop 8982803 (`git merge-tree`).
- **Reverts and mutants** (`--lib netinfo`):

| Mutant | Fails |
| --- | --- |
| 6232e0f: back to `if let Some(e) = entry.check()` | `check_v2_group::service_https_port_443`, `extended_checks_match_vectors` |
| 6232e0f: 443 allowed in every group | `service_core_p2p_port_443`, `extended_checks_match_vectors` |
| 6232e0f: I2P 443 also allowed in HTTPS | `i2p_https_port_443`, `extended_checks_match_vectors` |
| 3638a49: plain `NIPurpose::from_base` read | all 4 `decode_v2_purpose_refused`, `info_reads_match_vectors::extended` |
| 3638a49: order rule dropped | `repeated`, `descending`, `after_last_purpose`, `extended` |
| 3638a49: unknown rule dropped | `unknown`, `extended` |
| 3638a49: `>` → `>=` | `repeated`, `descending`, `after_last_purpose`, `extended` |
| 54ac4f1 vectors: short port read as invalid | `entry_reads`, `extended` |
| 54ac4f1 vectors: domain length unlimited at read | `entry_reads`, `extended`, `nientry_domain_decodes_at_most_253_bytes` |
| 54ac4f1 vectors: empty label valid | `extended` (+ 3 `check_entry_domain`, `entry_reads`) |
| 54ac4f1 vectors: domain allowed in every group | `extended_checks_match_vectors`, `domain_wrong_purpose` |
| 54ac4f1 vectors: `DOMAIN_MIN` 4 (kills `a.b`) | `extended_checks_match_vectors`, `extended` |
| 54ac4f1 vectors: short name read as invalid | survives, but it is equivalent: `CompactSize::decode_len` already ensures the bytes (types/src/compact.rs:174) |

## Findings

### Major

**M1.** Held: this finding falls under the existing batch-3 hold (DEC-411/DEC-432); the full text is kept by the manager outside the tracked tree.

**M2. 3638a49 churns an earlier commit in the same PR, against kwvg's "commits that don't churn each other" rule.**
- **What churns:**
  - 6d8ed080 ("keep the first NetInfo group of each purpose") switches `NetInfoV2::decode` to a `BTreeMap` and adds `BTreeMap` to the prelude.
  - 3638a49 takes both out again; the net prelude diff from 5c136f25 to 3638a49 is 0 lines.
  - 3638a49 also rewrites six vectors and the struct doc that 6d8ed080 added.
  - 3638a49 also rewrites 6232e0f's own citation (rc.3 L463-L475 → rc.4 L459-L471).
  - The PR body has to explain this in its own row: "Superseded by ..." (body line 18).
- **Fix:** the PR has only bot reviews so far (no maintainer review), so rewriting is still cheap.
  - Drop 6d8ed080.
  - Put the purpose refusal in its place, directly on develop's `Vec` decode. That is the same `read_purpose` call; develop's decode differs only in the purpose line.
  - Pin the corpus header at rc.4 from 5c136f25 onward. I reproduced every non-purpose vector at rc.4, so the earlier sections don't change.
  - Cite rc.4 in 6232e0f from the start.
  - This removes the "Superseded" row and the rc.3/rc.4 two-pin story (body line 11, line 49), and makes 3638a49 a pure addition.
  - Run the comb per commit again.

### Minor

**m1. `check` accepts out-of-order purposes that `decode` now refuses, so a valid hand-built value doesn't roundtrip.**
- **Where:** netinfo.rs:360-367 and 377-380.
- **Problem:** `NetInfoV2 { version: 1, entries: vec![(PlatformHttps, [domain]), (CoreP2p, [svc])] }` passes `check()`. Its `encode` writes the purposes as 2 then 0, which this crate's `decode` and the reference both refuse. The reference can't build such a value (it uses `std::map`).
- **Body:** line 25 says `check` keeps the repeated- and unknown-purpose checks "for hand-built values". Order is the third leg of the same rule.
- **Fix:** replace the O(n²) purpose-duplicate loop with one ascending check, for example `self.entries.windows(2).any(|w| w[0].0.to_base() >= w[1].0.to_base())`. Keep `Duplicate` for an equal pair and use `Malformed` for a descending pair, or one kind for both. Add a `check_v2_*` case.

**m2. The 443 rule works by matching on an error value and discarding it.**
- **Where:** netinfo.rs:409-415.
- **Problem:** `NIEntry::check` refuses the service and `NetInfoV2::check` discards `BadPort { port: 443 }` again. This is exact today, because `BadPort` is the last service check (netinfo.rs:253-256). But it ties the group rule to the order and the payload of another function's errors: a check added after the port check would be skipped silently for HTTPS services on 443. The reference passes the purpose into `ValidateService` instead.
- **Fix:** make the purpose an input. Use a private `fn check_service(purpose: NIPurpose, svc: &ServiceV2) -> Option<NIError>` that both `NIEntry::check` (with `CoreP2p`-like semantics) and the group loop call, or check the service port in the group loop itself. The `NIEntry::check` behaviour then stays unchanged.

**m3. Most refused-purpose vectors sit away from their citation.**
- **Where:** pkgs/primitives/corpus/netinfo.json5:294-298 vs 311-318 and 325-326.
- **Problem:** the heading "A purpose must be known and above ..." and the netinfo.h#L425-L435 link sit above only the first purpose vector. The other five refused-purpose vectors come after six unrelated vectors and have no citation next to them.
- **Fix:** either move the five vectors, and their hex strings in the program list (output order must follow program order), under the heading, or repeat the link above line 311 and line 325.

**m4. The body's "validity" link points to code rc.4 no longer runs for this rule.**
- **Where:** body line 19, the "Read unknown entry types as invalid entries" row.
- **Problem:** the row says `NIEntry::Invalid` "checks as `NIError::Malformed`, as the reference implementation's validation reports it", and links netinfo.cpp#L209-L234 (`NetInfoEntry::IsTriviallyValid`). At rc.4, `ExtNetInfo::Validate` no longer calls it; reference PR #7888 commit 51a09b78 removed that call. Malformed now comes from `ret{NetInfoStatus::Malformed}` falling through (netinfo.cpp#L606-L619).
- **Why it matters:** the body re-pinned every link to rc.4, so this link no longer shows the claimed content.
- **Fix:** point it at #L606-L619.

**m5. The body's disclosure and test text will be stale after this push.**
- **Where:** body lines 47 and 63.
- **Disclosure:** line 63 says the last three commits "were not reviewed ... by a separate agent". This review is that review, so state it, with the model and the outcome.
- **Base:** line 47 tests per commit "on top of 09d132a" while the base stays at develop 3153d6f. kwvg's policy is to rebase onto develop. The tip merges cleanly onto 8982803, but if M2 means rewriting the branch anyway, rebase it onto develop 8982803 and run the comb once there.

### Nit

**n1. Shadowed names in `read_purpose`.**
- **Where:** netinfo.rs:495-504.
- **Problem:** the closure parameter `|value: u8|` shadows the outer `value`, and `.map(|purpose| ...)` shadows `purpose`.
- **Fix:** `|candidate: u8| last.is_none_or(|last| candidate > last.to_base())`, and `.map(NIPurpose::to_base)` in the list.

**n2. An empty `expected` list renders oddly.**
- **Where:** netinfo.rs:500-508.
- **Problem:** after `PlatformHttps`, `expected` is empty, so the message reads "expected one of [], got 1". The `after_last_purpose` case tests this.
- **Fix:** acceptable as is. Mention it in the body row if kwvg asks about it.

**n3. The domain-443 vector has no citation for its rule.**
- **Where:** netinfo.json5:383-384, "Port 443 is allowed for a domain".
- **Problem:** the rule is in `ValidateDomainPort` (netinfo.cpp#L482), outside the cited #L459-L471.
- **Fix:** add the link. The same applies to the `a.b` HTTPS check vector at :388-390 (netinfo.cpp#L476-L494).

**n4. The empty-label citation is broad.**
- **Where:** netinfo.json5:339.
- **Problem:** #L90-L139 covers `ValidateDomain`, `Set` and `Validate`. The cleared entry comes from #L105-L107 (empty label) plus netinfo.h#L261 (`Clear()`).
- **Fix:** narrow the link.

## Checks with no finding

1. **443 rule per purpose group.** At rc.3 netinfo.cpp#L463-L475 and rc.4 #L459-L471:
   - I2P must be on port 0 in every group (Rust: `ServiceV2::check`, addrv2.rs:360-363, giving `BadAddr{BadPort}`);
   - otherwise port 0 is refused, and a bad port is refused unless the group is HTTPS and the port is 443 (`IsAllowedPlatformHTTPPort` only allows 443, #L59-L66).

   The Rust code matches for CoreP2p, PlatformP2p and PlatformHttps, and for IPv4, IPv6 and I2P. Reference PR #7741 is present from rc.1 on, so it is in rc.3.
2. **Purpose order and uniqueness.** rc.4 netinfo.h#L425-L435 reads the purpose count, then for each group reads the purpose byte. It refuses before reading the entry count if the purpose is unknown or not above the last one. `read_purpose` does the same, in the same read order. The error kind is `DecodeError::InvalidValue` listing the purposes still allowed, the same style as `ProTxVersion` (provider/version.rs:45-48). A refusal of the whole value matches the reference's `ios_base::failure`.
3. **Domain vectors.**
   - A domain over 253 bytes or cut short refuses the whole value (`LIMITED_STRING` netinfo.h#L178, no try in #L253-L261).
   - `a..b` is cleared: `ValidateDomain` returns `BadLabelLen` for the empty label, then `Clear()` runs, and the next entry is read (read 21, written `...02ff...`).
   - `a.b` is valid: read 20 and written back unchanged. It is refused in CORE_P2P (netinfo.cpp#L611-L614) and allowed in PLATFORM_HTTPS.

   All three were confirmed at rc.4.
4. **Corpus header pin.** The header pin `v24.0.0-rc.4, 0e7382df80` matches the tag. The rc.4 output equals the whole corpus, so the commit-message claim "read vectors move to ... rc.4" holds.
5. **Tests aren't tautological.**
   - The `decode_v2_purpose_refused` cases pin both `actual` and `expected`.
   - The `check_v2_group` cases pin exact error values.
   - The corpus tests compare against reference output.
6. **Policy wording.** None of the added code, comments, corpus text or commit messages says "Dash Core". The commit messages contain no C++ identifiers; "NetInfo" is the existing commit-subject term, not a reference class name. All characters are Latin-1. The new comments are a one-line const doc and a `# Errors` rustdoc, with no exposition.
7. **API, naming and `!` markers.**
   - `PLATFORM_HTTPS_PORT`, `read_purpose` and the `#[case::...]` names follow the surrounding style.
   - `fix!` is justified on 6232e0f, because `NetInfoV2::check` now accepts values it refused, and the body lists that.
   - `fix!` is justified on 3638a49, because `decode` refuses values it accepted.
   - 54ac4f1 is test-only and correctly unmarked.
8. **PR body.**
   - Rows 24-26 are accurate for these three commits, and the "six purpose vectors" count is right.
   - The 443, purposes, domain-purpose and domain-read links resolve at 0e7382df80 to the cited lines, and the commit exists upstream (REST).
   - The other links I checked resolve to the cited content: #L242-L263, #L326-L348, #L408-L436, #L194-L198, #L227-L240, #L443, providertx.cpp #L318-L321 and tests #L319-L404.
   - The Breaking Changes list covers the decode refusal and the 443 acceptance.

## Cleanup

These are removed after the report: `/work/scratch/bsdk87-cde-review` (git worktree), `/work/scratch/bsdk87-cde-review-core` (reflink of the rc.4 tree, with my programs appended) and `/work/scratch/bsdk87-cde-review-gen`. The author's `/work/scratch/bsdk-netinfo-b3` was only read from.
