# REPIN BS7 review r1 — Sol

Reviewed `node/repin-bs7` at **7642b04eb507568a8f4b644fcf0cae62001c3cf7** against `node/integration` **a1792f7128b82174a968654b2afcf43685155fb5**, following `briefs/REVIEW-COMMON.md`. This review covers the re-pin, BL-43 and their checks. The previously approved BS7 content was checked only for a clean merge. Validation used a detached worktree under `/work/scratch/repin-bs7-sol/`, `TMPDIR=/dev/shm/repin-bs7-sol-tmp` (0700), and 12 Cargo build jobs.

No blocker or major finding.

## Finding

**F1 — minor: the updated BS7 selftests do not detect restoration of the removed exemption.**

**Location:** `contrib/node/selftest-checks.sh:194`, `:198`.

The relocated-manifest positive case and the stale-lock negative case retain their expected outcomes with the pre-BL-43 checker. The stale entry is not named by a direct fork dependency, so the former `TWO_REVISIONS` exemption also rejects it. A regression restoring that exemption would therefore escape these two BS7-specific cases.

I independently ran isolated `check_nested` fixtures through the current checker and its version from `4ae4f7f3`. Both accept the relocated current manifest/lockfile and reject an unnamed stale lock entry. When `dash-p2p-core` instead directly names fork revision `97a472f5a964c30a3bb744b5606671706070c5a4` at the original BS7 path and its lock entry names the same revision, the old checker accepts it and the current checker rejects both the declaration and the lock source. The implementation correctly closes the exemption; this finding concerns its regression coverage.

**Suggested fix:** add a negative case at `contrib/node/dde-probe/bs7` that names a second fork revision in a dependency and supplies its matching lock source while retaining the root `[patch]`. Assert rejection. This case should fail against a checker with the former exemption restored.

## Verified behavior

- The root, `cs2-diff`, `node-fuzz`, and `dde-probe/bs7` patch tables and lockfiles consistently resolve base SDK to **4b4e76b27c33d7c78ec4d6a5ae462b59291eabe2**. BS7 declares its SDK dependencies from upstream and adds `dash-p2p-core` to the repeated root patch. No stale SDK source remains in these lockfiles. The root, cs2-diff and node-fuzz lockfile diffs are exactly replacement of the old revision with the new one.
- The unpatched `dde-probe/oracle`, `bench`, and `bs7-bench` workspaces retain intentional comparison revisions under the existing documented rule in `BASE-SDK-FORK.md:33`. They are included in the pin check. Removing `TWO_REVISIONS` strengthens the patched-workspace checks and leaves that rule unchanged. The generic nested-workspace re-pin selftest also passes with BS7 included.
- `git merge-tree --write-tree 57dfc57d 745e5b7a` produces exactly the tree committed by `4ae4f7f3` (`c5886471d401ac05be46de522cf73423e66f5b1c`), with no conflicts. The merged side contains the stated 28 commits. The fork merge likewise reproduces `4b4e76b` cleanly, with the stated 39-commit side.
- In `~/workspace/base-sdk`, `git diff --stat a1d4dd3 4b4e76b` lists only `pkgs/p2p_core/**` and `Cargo.lock`. The lockfile adds only `bitcoin_hashes` and `divan` to the `dash-p2p-core` dependency list; it changes no package release or other package dependency list. The root manifest and all other SDK crate trees are unchanged. The pure-crate dependency graph is unchanged by the re-pin, and blst remains **0.3.17**. The audit-stamp move is justified.

## Independent validation

- `contrib/node/check-base-sdk-pins.sh`: **PASS**, including all eight nested workspaces.
- `contrib/node/check-pure.sh`: **PASS**, all three pure crates, with all features and without default features.
- `contrib/node/selftest-checks.sh`: **PASS**, **105 checks**, exit 0. This includes both updated BS7 cases and the audit-stamp controls.
- `git diff --check a1792f71 7642b04e`: **PASS**.
- Optional private wrapper: **PASS**, exit 0. Receipts: **522 cases**; **279 seeds (279 required)**; **3,827 transaction and 95 block classifier cases**. Only pass/fail and counts were retained; no private material was inspected. This run also builds bs7-tool's oracle feature at the committed pin.

The implementer's **3,130 passing tests** and **BL-20 54/0 rerun** remain reported evidence; I did not rerun that broader gate. No edits to the branch under review, commits, pushes or GitHub messages were made. The detached worktree, disk scratch and RAM temporary directory were removed.

**Verdict: APPROVE-WITH-NITS.**
