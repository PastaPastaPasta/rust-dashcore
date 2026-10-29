# LQ1b r1 — independent consensus/LLMQ review

Target: `node/lq1b-rotation` at `c2bf4f3ba6e6bf8c3b937060356d809b34be0a60`. I reviewed the LQ1b commits against the workspace Dash Core v24 rotation implementation (`src/llmq/utils.cpp` and `src/llmq/snapshot.cpp`) and the LQ1 design. The target worktree was clean; I did not edit it or query the live oracles.

## Findings

No Blocker, Major, Minor, or Nit findings.

## Review basis

- `dash-evo/src/llmq/rotation.rs:128-188` matches Core's snapshot reconstruction: score-sorted work list, unused then used split, one wrapping cursor for modes 0 and 1, the mode-1 skip cursor that never rewinds, and empty quarters for modes 2, 3, or unknown. `SkipListEncoder` and `decode_skip_list` preserve Core's first-nonzero-base rule and narrowing to signed 32-bit entries.
- `dash-evo/src/llmq/rotation.rs:217-314` preserves the previous-cycle and member iteration order, both independent `AddMN` attempts, the testnet/V19 `skipRemovedMNs` rule, the fill guard and full-loop early return, and the absence of a row on either early return. `dash-dml/src/views.rs:75-118` uses the same state-domain, proTxHash, internal-id, and unique-property acceptance steps as `MnList::try_add`; the scratch list's count and valid-entry iteration are the values rotation reads.
- `dash-evo/src/llmq/rotation.rs:333-434` reads at most three prior snapshot rows, breaks on Core-style absence, returns no prediction if a required row is absent, assembles quarters oldest to newest, and stores the current row only after a successful non-predictive computation. `SnapshotRows::get` documents undecodable persisted rows as `Ok(None)`; its concrete CH7b store and crash recovery are downstream work, not implemented in this slice.
- `dash-evo/src/llmq/reference.rs:250-297` models Core's base-hash and `(cycle-hash, index)` caches, including the reset and out-of-range ordering. A reorg changes the relevant hashes; a cache hit keeps Core's cached members even if a later fresh computation would change after work-block data availability changes. The production facade is a CH7b contract, so this review does not certify that future implementation.
- The checked-in capture test recomputes members and row bytes from 12 Core-derived cycles and runs Core's commitment verification, including seven swapped-signer negative controls. The supplied full-history replay compares every index and trace over 1,223 mainnet and 2,788 testnet cycles, with the no-row testnet cycle included. I treated those replay and gate totals as supplied evidence; I did not repeat the live-history collection.

I spot-checked discrimination in an isolated scratch worktree: deleting the testnet clause of `skip_removed_mns` made `removed_masternodes_are_skipped_from_v19_and_always_on_testnet` fail at its testnet assertion; restoring the code made that test pass. The scratch worktree and its private target directory were removed. This directly checks the regression that the supplied history replay reports detecting.

Assumption: the LQ1b slice owns the pure rotation, row effects interface, and reference cache model; CH7b owns persistent snapshot rows and the process-wide production members facade, as the design assigns. The brief's D15–D29 notes were not present as a separate file in the target tree, so I used the committed LQ1 design, code comments, and Core source for those decisions.

VERDICT: ACCEPT
