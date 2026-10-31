# CH1-node BL-224: Sol review, round 1

## Verdict

**ACCEPT WITH NITS**

## Findings

### Minor — `JournalRetention.entries` omits the current open journal

**Where:** `dash-chain/src/status_journal.rs:126-132, 258-261`

The gauge publishes `older.len() * rotate_at`, so it counts only full journals and does not include `current`'s actual log length. For example, after a snapshot is published, one status write leaves that snapshot retaining one journal entry while `journal_retention().entries` still reports `0` because no rotation has occurred. This is bounded and the API documentation explicitly says “full journals,” so it does not affect status correctness or the expiry bound; it can under-report retained memory to a monitor. If `entries` is intended to be a complete retention gauge, include the current log length (or document the partial-journal exclusion as an intentional approximation).

## Open questions / assumptions

- Holder coverage is structural: `FeedSnapshot`, `ChainView`, and reply types retain an `Arc<ChainReadSnapshot>`, which retains the journal. No RPC holder is wired in this range; the future RPC adapter must retain the view/snapshot in the same way and handle `SnapshotExpired`.
- The 65,536 entry bound is applied to full journals and preserves the newest snapshot's journals until the next publication, as documented. This means a command that performs an unusually large status-only burst without publishing can temporarily exceed the nominal bound; the submitted tests and design explicitly require that newest-snapshot guard.

## Validation

- `cargo test -p dash-chain --lib` — **410 passed, 0 failed, 1 ignored**.
- `cargo test -p dashd-chainstate --lib chain::tests::view::` — **10 passed, 0 failed**.
- `cargo test -p dash-chain --lib chain::tests::status_at_holds_under_concurrent_writes -- --exact --nocapture` — **passed**.
- `cargo check --workspace` — **passed**.
- `cargo fmt --all -- --check` — **passed**.
- `git diff --check d94c015c2 ac0219686` — **clean**.

## Summary

The weak-link journal ownership, upgrade-before-use walk, explicit `SnapshotExpired` propagation, newest-snapshot guard, insertion/pre-image behavior, holder tests, and concurrent stress test are sound in the reviewed range. Existing workspace callers compile with the new fallible API, and no incorrect status, use-after-drop, torn-read, or silent fallback path was found. The only follow-up is clarifying or including the current partial journal in the retention gauge.
