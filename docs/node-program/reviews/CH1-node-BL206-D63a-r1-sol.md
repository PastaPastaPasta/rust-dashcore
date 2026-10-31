# CH1-node BL-206 + D63a narrow review (round 1)

Reviewed **0c7d960ed..6372baa1d** in a detached worktree.

## Verdict

**ACCEPT WITH NITS**

I found no incorrect status result in the reviewed CH1-node paths. The D63a skip is sound under the single-writer protocol, and the journal walk handles open and rotated journals correctly. The remaining items are a deferred retention bound and a downstream API handoff that must be completed before P3 status readers are wired.

## Findings

### Minor — retained snapshots can keep an unbounded journal chain alive

**Where:** dash-chain/src/status_journal.rs:21, dash-chain/src/snapshot.rs:31

A snapshot retains its starting journal, and each full 32-entry journal links to the next one. The 32-entry rotation bounds one scan and one journal allocation; it does not bound the chain reachable from an old snapshot. A reader that holds an epoch-E snapshot while IBD continues can therefore retain every later journal. Alternating writes 'A, B, A' keep appending because D63a skips only an adjacent repeat of the same (id, epoch). The supplied estimate of roughly 24 bytes per retained entry is consistent with this behavior, so about 40,000 status writes can retain about 1 MB while one snapshot remains held.

The current production paths audited here do not intentionally keep an old snapshot forever: ArcSwap/TipState/the chainstate view keep the newest one, the subscriber driver drops its FeedSnapshot per step, and the current P2P snapshot is a value copy. RPC is not wired in this tree. A caller can still retain the public Arc or a returned snapshot indefinitely, so the implementation has no hard memory or lookup-time bound today.

**Suggested fix:** ship the retained-entry/oldest-epoch gauge in BL-224, make it cover FeedSnapshot, view/reply and RPC holders, and add the planned bound or compaction policy before a long-lived production reader is introduced. I consider T19 shipping with that explicit follow-up acceptable; a bound is not required before this narrow merge while the current holders remain short-lived.

### Minor (downstream handoff) — valid_scripts must carry the caller’s snapshot

**Where:** dashd-p2p/src/processing/seams.rs:143, dashd-p2p/src/processing/serve.rs:104

The new CH1 API correctly exposes ChainViewHandle::valid_scripts(&view, id), but P2a’s ChainAccess::valid_scripts still takes only id. block_request_allowed has a retained ChainSnapshot for epoch E; an adapter implemented as handle.view() or live entry(id).status can observe a status written at E+1 and mix it with the E tip/best-header facts. That violates D63’s coherent-view guarantee even though the CH1 implementation itself is correct. This is a handoff issue, not a defect in the changed CH1 files, and the trait is not yet backed by a production adapter in this tree.

**Suggested fix:** change the trait to valid_scripts(&self, snap, id) and carry an opaque Arc<ChainView>/view token in the P2P snapshot. The adapter should call ChainViewHandle::valid_scripts on that exact view, and the trait documentation should point away from live EntryView::status.

### Nit — add an explicit insertion-after-snapshot test

**Where:** dash-chain/src/reader.rs:166

The implementation documents that an entry inserted after a snapshot is read at its insertion status because insertion is not journalled. The committed tests cover a status write after a snapshot, lowering, same-id/same-epoch skipping, rotation and concurrent ordering, but do not pin this insertion boundary. A small regression test should take a snapshot, insert a header, assert status_at(old_snapshot, new_id) returns the insertion status, then modify it and assert the old snapshot returns that insertion pre-image.

This is only a coverage gap; the documented behavior follows the code and does not create a newer-than-snapshot result for an entry that already existed in the snapshot.

## D63a soundness

- Every production ChainReadSnapshot build goes through set_tip_cell at epoch 0 or publish_locked at TipState.epoch + 1. TipState::publish asserts the one-epoch increment, and the writer is the sole caller, so a build cannot race another build or a status write.
- modify records the old status before write_group. The seqlock’s release/acquire edge means a reader that observes the new live status also observes the preceding journal append. status_at reads the live group first and then walks the journal, so a write between those operations is restored from its pre-image, while a write after the walk cannot replace the already-read live value.
- status_at reads next before each journal log. A journal with next set is final; an open journal is the only place the writer can still append, and the live-read-first ordering makes an open-journal miss safe. At rotation, the old journal is linked before the new journal receives its entry, so the walk cannot skip a write.
- D63a’s skip is limited to the same block and the same tag. A publication changes the tag, so the skip cannot cross a build. A different block breaks the skip run. Lowering a status journals the complete old status just like raising it.
- I traced the Node status writers (set_status, the failed-ancestor walk, enforcement/failure handling and chainstate index modifications) through modify. SPV overlay stage 4 writes bypass the Node journal as intended and publish their status with the step snapshot.

No interleaving found here lets a reader observe a status newer or older than its retained snapshot, and no consensus or security issue was found.

## Rotation and test coverage

The journal unit tests exercise capacity 1 and 32, first-old-entry selection, later-journal lookup, a full-journal handoff, same-id rewrite skipping, and the open-journal walk. The chain tests add:

- same-id/same-epoch and same-id/across-build cases;
- status lowering;
- real concurrent writer/reader stress at rotation 1 and 32;
- the published ChainView stages (old view, pre-view publication and new view);
- SPV’s shared empty journal.

The missing explicit case is the insertion boundary noted above. The two skip conditions and the published-view guarantee are otherwise pinned.

## Validation

- cargo test -p dash-chain --lib — **402 passed, 0 failed, 1 ignored**.
- cargo test -p dashd-chainstate --lib chain::tests::view:: — **9 passed, 0 failed**.
- git diff --check 0c7d960ed..6372baa1d — clean.

