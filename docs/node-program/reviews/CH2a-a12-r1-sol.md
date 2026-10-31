# CH2a a1+a2 — Sol r1

**Reviewed:** `4cba2def → 57e2239de2f49563f14179ecb9f4c733c82a44bf`, CH2a a1+a2 only: `dashd-chainstate` coins layers, tip, overlay, accounting, test twin/model, and their node-fuzz adapters. Later a3 work and the DEC-131 documentation commit are excluded from the implementation review. DEC-131 itself is binding.

**Basis:** CH2 rev5 (`640d37f9`, with the branch's cite updates), DEC-95/99/102/103/108/131, REVIEW-COMMON, and Core `v24.0.0-rc.3` (`0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`): `coins.cpp`, `coins.h`, `validation.cpp`, `txdb.cpp`, and `test/coins_tests.cpp`. PLAN §7 handling: codenames only; no private material read.

No blocker or major implementation finding. One nonblocking model coverage finding remains. This is the Sol pass; the subsequent Opus high pass belongs to the manager's sequential review gate.

## Finding

### S1 — minor: the model does not check `best_block`

**Location:** `dashd-chainstate/src/coins/model.rs:1022` (`view`), also the commit checks at `:761`; `dashd-chainstate/src/coins/reference.rs:196`.

The twin carries and flushes its best block, but the model compares coin values, errors, membership, accounting and coin DB rows without comparing the reported best block. `freeze` also resets the twin's best block from the model height before flushing it.

**Concrete missed bug:** replace `CoinsCache::merge`'s `self.best = best` at `cache.rs:429` with `let _ = best`. All **six** existing `coins::tests::model` tests still pass, including both model drivers and the planted-bug checks. A separate probe that opens an empty overlay, sets block 1 and commits then fails: the tip returns `None` instead of block 1. Thus a plausible regression in the tip transition escapes the model. The implementation under review contains the correct assignment; this finding is a coverage gap, not a current incorrect tip.

**Suggested fix:** give the twin a best-block query with Core's null-hash/base fallback semantics, and compare the overlay and tip best block after successful commits, aborted overlays, disconnects, freezes and releases. Add the one-block commit regression and keep this mutation as a negative control. Check before replacing the twin's best block during freeze.

Evidence: `/work/scratch/ch2a-a12-r1-sol/best-block-mutant.log` (6 passed), `best-block-probe.log` (probe fails, exit 101), and `reviewer-probes.rs`.

## Specific re-review of `57e2239d`

The change at `coins/cache.rs:388` is correct. With no active entry, F's tombstone represents an already-erased base coin in the equivalent Core cache immediately after its flush. It must take Core's **absent parent** branch: preserve a live child's FRESH bit, and discard a child that is both FRESH and spent. An active tombstone remains a parent entry, so its DIRTY spentness is preserved; a live F entry still participates in the defensive FRESH check and in non-FRESH shadowing.

I checked these paths against Core `coins.cpp:191-247`, including an active entry taking precedence over F. A re-created coin over an F tombstone can now be spent away without leaving a tombstone that survives release. The earlier `f1586ceb` fix also correctly makes a spend of an F-only tombstone return false without consulting the older DB coin or adding an active tombstone.

**Counterfactual:** restoring `cache.rs` from `f1586ceb` makes the supplied fresh-add/commit/two-spends sequence fail with `Mismatch("spend")` in **both** cases: F still in flight, and after both W steps plus release. The exact `57e2239d` suite passes its regression. Evidence: `f-tombstone-mutant.log` records both failures; `f-tombstone-mutation.diff` records the reverted change. Restoring the reviewed source makes both scratch probes pass (`restored-probes.log`, 2 passed). This closes the specific unreviewed-fix concern.

## Correctness and accounting audit

- **Active, F and child transitions:** active follows Core's reachable tip states. Tombstones take precedence over lower layers. Fresh active spends erase their entry; non-FRESH spends retain DIRTY spentness. A child merge preserves an existing active FRESH bit, removes a fresh parent when the child spends it, and never promotes an existing DIRTY active tombstone to FRESH. Live F entries get non-FRESH active shadows. F tombstones take the absent-parent path described above. I found no further divergence beyond the design's declared differences and DEC-131.
- **Overlay ownership and failure:** `have_inputs` caches in the tip and leaves the child without input copies or pointers into movable shards. Spending a parent-owned long coin copies it into a tracked undo value; the parent stays intact until commit. Spending a child-owned long coin moves its box. Abort/drop leaves the parent whole. The rehash, failed-connect and connect/disconnect witnesses pass. Read errors propagate through the guarded read path after tripping the latch and notifying the abort sink; they are never reported as a missing coin. Production abort execution and the durable flush protocol belong to their later owners.
- **Prefetch and release:** active/F shield the DB while W writes subsets of F. Positive prefetch entries resolve only below those layers. The release-generation fence, clearing on release/disconnect, stale completion cases, and spent/re-created F keys pass. Release requires W's handle to have been dropped before F leaves the ledger. X9, X10 and X13 are caught.
- **Membership and deviations:** membership is first-hit active then F, unspent, with no DB/prefetch reads, per DEC-102. The earlier overlay overwrite error is accepted per DEC-131; the differential's exemption is evaluated against the relevant memberships rather than accepting arbitrary add/commit errors. No shadow of Core's flush epochs or stricter memory ceiling is required. The child tombstone's absent undo result and the no-copy read behavior are the §2.4.2 choices. The malformed/trailing-record abort is the design's D7.
- **Native Σ:** capacity accounting matches the tagging allocator at checked operation boundaries, including long boxes, slab/free-list vectors, prefetch, scratch and held-out undo; R is accounted separately. The presize is reduced beside F, table rounding/routing remain tied to the presize, and valid blocks can grow shards. DEC-108 rebuilds remove DELETED buckets without shrinking the table. The 76.99 MiB churn test reports 57 blocks, 71 rebuilt shards, zero short-room boundaries and zero growth without F; its control reports 202 short-room boundaries. DEC-131's lower load band applies only to add-only fills.
- **`StableBytes`:** borrows point to separately allocated byte boxes, not into the mutable pointer vector. Growth moves pointers only; no payload is mutated or reboxed while borrowed. The store is not Sync, and Drop needs exclusive ownership. Both Miri borrow modes pass the growth/held-borrow and empty-value/drop tests. The tagging allocator's checked layout arithmetic, header alignment, null handling and cross-thread accounting were also inspected. These Miri runs deliberately exclude the tagging global allocator, as `lib.rs` documents; they establish the `StableBytes` evidence, not Stacked-Borrows compatibility of the allocator header scheme.

## Independent validation

All final baseline evidence uses a **disk archive of exactly `57e2239d`** at `/work/scratch/ch2a-a12-r1-sol/source`. Before testing, 39 scoped files were checked byte-for-byte against that commit (`snapshot-sha256.txt`). Build directories are separate from the active branch's target. Every run uses `TMPDIR=/dev/shm/ch2a-a12-r1-sol-tmp` (mode 700), `RUSTC_WRAPPER=`, and `CARGO_BUILD_JOBS=12`. Experiments change scratch copies only; no branch edit, commit, push or GitHub comment was made.

| Check | Result |
|---|---|
| `cargo test -p dashd-chainstate --all-features --target-dir …/exact-target -- --nocapture` | **80 passed**, 0 failed; includes L0 (a)/(b), cursor order, both cache simulation ports, `updatecoins_simulation`, ownership, prefetch, membership, accounting and witnesses (i)–(iv) |
| `cargo build -p dashd-chainstate --all-features --release --target-dir …/exact-release-target` | Passed |
| `cargo fmt -p dashd-chainstate -- --check` | Passed on the unchanged source |
| `cargo clippy -p dashd-chainstate --all-features --all-targets --target-dir …/exact-target -- -D warnings` | Passed |
| `cargo +nightly-2026-07-15 miri test -p dashd-chainstate --target-dir …/exact-miri coins::stable::tests` | 2 passed under Stacked Borrows |
| Same Miri command with `MIRIFLAGS=-Zmiri-tree-borrows` | 2 passed under Tree Borrows |
| Independent seeded soak against the exact release library | **249,000 distinct runs passed**, no mismatches or leaks |

The soak installs the tagging allocator and calls the actual model entry point for **124,500 cache-model runs and 124,500 overlay-differential runs**. It covers the nine existing configuration combinations, including f ∈ {0.25, 0.5, 0.75}, 2/4/8 MiB shares, and mempool allowance variants. There are 240,000 inputs of 32/64/128/256 bytes and 9,000 additional 3,000-byte inputs, with distinct seeds. Each run verifies that allocator checking was enabled and that live tagged coins bytes return to zero after the model drops. This is a seeded model soak, not a claim of additional libFuzzer CPU hours.

Coverage totals: **12,385,908 cache operations**, 805,831 committed blocks, 451,859 dropped blocks, 335,186 disconnects, 259,036 freezes, 172,175 releases, 61,567 admitted entries, 136,391 stale batches and 710,172 boundaries; plus **242,705 overlay-differential operations**, 47,750 committed overlays and 54,780 frozen-parent cases. Logic-error stops and injected fail-stop cases are expected modeled outcomes. Artifacts: `soak.rs`, `run-soak.py`, `soak.log`, per-worker logs, and `soak-totals.json` under the scratch directory.

The rerun reproduced the reported witness numbers: (i) boundary Σ 5,609,472 B, transient peak 5,730,992 B; (ii) connect boundary Σ 473,612,292 B and peak 944,829,448 B, disconnect boundary 703,872 B; (iii) long connect boundary Σ 714,096,644 B and peak 1,185,313,800 B with F in flight; (iv) the shard table grows 2,392,080 → 4,784,144 B, with that 4,784,144 B transient counted. These are completion/accounting evidence, not new per-op limits or a CH2c RSS gate. I did not independently remeasure Core's meter or the a1 density sweep; their probe/harness and the DEC-108 decision were inspected.

The claimed **gate a2c exit 0** is supported by its log: the initial SPV failure was followed by a permitted BL-20 rerun with 54 passed and zero failed. The PF0 advice shows zero regressions/noisy-unresolved results, but its overall verdict is **ERROR** because the anchor is missing; it is not a merge-gate PASS. A manager-run authoritative PF0 gate remains separate from this review. I did not rerun the unrelated workspace/functional gate or inspect private inputs.

## Verdict

**APPROVE-WITH-NITS.** No blocker or major. S1 is a nonblocking request to strengthen the model's tip coverage; the reviewed tip assignment is correct. The two tombstone fixes are accepted, including the specific `57e2239d` re-review.

**Sol gate satisfied: yes.**
