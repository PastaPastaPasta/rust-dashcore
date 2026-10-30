# CS0 implementation review, round 1 (designated reader, EMBARGO-1)

- **Reviewed:** `node/cs0-node-primitives` at f5f079c2 (26 commits on `node/integration` 17bc2e66), in a fresh worktree.
- **Role:** designated EMBARGO-1 reviewer (PLAN §7.2 item 3). The findings' details are in the reader's private review (PLAN §7 handling); this file gives only the verdict and generic titles.
- **Scope:** the EMBARGO-1 parts of CS0: the marked module, `quirk_01_embargo_1`, the private-seed replay and its manifests and receipts (DEC-24 D6), the fuzz runner and private sink (DEC-30 N5), the D7 replacement (DEC-30 N2), and a leak review of the whole diff and every commit message. The general review is the GPT reviewer's.
- **Checks run:** Core at v23.1.8 and v24.0.0-rc.3 (read with `git show`); both private tests with `DASH_EMBARGO_DIR` set (both pass); the unset case (loud skip); tamper and mutation checks; the runner self-test; direct-start refusal of a decode target; clippy on the marked crates.

**Verdict: CHANGES-REQUIRED.** The marked code matches Core at both pins, and the private tests pass and catch regressions. One major finding concerns enforcement in the gate. Its fix is small and the manager can check it without a full new round.

Findings: blocker 0, major 1, minor 1, nit 3.

| ID | Severity | Title |
|---|---|---|
| R-M1 | Major | Private-test enforcement gap in the gate (see private note) |
| R-m1 | Minor | quirk-1 coverage gap (see private note) |
| R-n1 | Nit | Private-tree manifest state ahead of the merge checklist (see private note) |
| R-n2 | Nit | Manifest-loader hardening (see private note) |
| R-n3 | Nit | Receipt fields vs the design's §9.4 wording (see private note) |

Leak review: no finding. The commit messages, comments, test names, docs and committed vectors name only the codename and "PLAN §7 handling". The markers are bare.

Outside my scope I saw nothing serious.
