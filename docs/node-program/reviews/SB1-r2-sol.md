# SB1 layer gates — Sol r2 (narrow)

Reviewed `node/sb1-layer-gates` at **1b2c28f1b** (merge base 9dfa318bf) against SB0 §2.3 rev6.1 and the accepted limits. **Verdict: CHANGES-REQUIRED.** Sol gate satisfied: no. Evidence: `/work/scratch/sb1-r2-sol/`.

## r1 closure
| Finding | Status |
|---|---|
| M1 | Closed. Also checked: a renamed external `Dashd_R2_Helper` build dependency fails L2. |
| M2 | The unification defect is closed. An external hot-path copy still escapes (R2-2). |
| M3 | Closed within the denylist scope. |
| M4 | Closed for Android; Apple remains BL-74. |
| M5 | Partly closed: committing a record resets the reference during bootstrap (R2-1). |
| M6 | Partly closed: a normal `dylib` dependency is unchecked (R2-3). |
| M7 | Closed. An extra target edge and a vanished target edge both fail. |
| m1 | Closed. NDK and API are machine-readable, and a mismatch fails. |

The BS16 baseline entry is acceptable: it is a single named edge, and a stale entry fails.

## New findings
- **R2-1 (major).** `check-layers.py:568–586` falls back to `HEAD` when the merge base has no size record. Recording +200 KiB, committing it, then recording +200 KiB again passes: 409,600 B of growth against a 252,315 B allowance.
- **R2-2 (major).** `check-layers.py:482–486`: the hot-path loop visits workspace members only. An external `dashd-p2p` → `dashd-wallet` (allow_w) → `key-wallet` passes; the same graph with the crate inside the workspace fails.
- **R2-3 (major).** `check-shared.sh:69–72, :88`: the artifact scan accepts only `.rlib`. A proc-macro-built `#[global_allocator]` in a normal `dylib` dependency passes on host and Android, and the products define the Rust allocator.
- **R2-4 (minor).** `check-layers.py:357–358`: L6 maps a displayed pair to every resolved edge without its target condition. Adding a Linux-only edge to the same package fails L6 under both Android roots.
- **R2-5 (minor).** `check-shared.sh:72, :77–82, :111`: a build-only helper with an allocator fails L5 through the uplifted dep-info and its host rlib, although nothing links it.

Manager ruling: **DEC-170**. The fix is by mechanism: an immutable L7 reference, name-keyed rules on every resolved ID, a linked-product allocator check, and cargo's platform filter.
