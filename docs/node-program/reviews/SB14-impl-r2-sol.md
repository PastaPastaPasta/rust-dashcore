# SB14 implementation round 2 review

Head: `c4177a609` (`node/sb13-spv-sqlite`), reviewed against round-1 head `3329bc303` in a detached read-only worktree.

## Findings

### Blocker

None.

### Major

None.

### Minor

1. **D39 refusal can alter an existing lock file's contents.** [`dash-spv/src/storage/lockfile.rs:17-25`](dash-spv/src/storage/lockfile.rs:17) opens `<dir>.lock` with `truncate(true)` before attempting `try_lock`. When `DiskStorageManager` already holds the lock, a refused direct open therefore erases the holder's PID text before returning `DirectoryLocked`. The advisory lock remains held, and the path is not left behind by the failed opener, so this does not deadlock migration; it does violate D39's unchanged-refusal requirement and loses useful diagnostics. Acquire the lock without truncating, then replace/write the PID only after acquisition, and add a test that compares the lock bytes after a refused direct open.

### Nit

1. **The `read_syscalls` sample median is noisy.** The current store samples are `[6, 6, 5]` versus `[5, 5, 6]` at the previous head, while the base is 4. D14 documents the one-time glibc `/proc/sys/vm/overcommit_memory` read, and the round-2 changes do not add a read in the measured store region. The attribution is credible and does not justify a code fix before merge. Recheck it in the full performance gate because the three-sample current median is nominally two above base despite the same bimodal behavior at the prior head (design bound: [`docs/node-program/designs/SB14-spv-header-layout.md:619-624`](docs/node-program/designs/SB14-spv-header-layout.md:619)).

## Round-1 findings and requested decisions

1. **R1 #1 / D39: closed for the destructive direct-open bug, with the Minor lock-file caveat above.** `SpvStore::open` now creates and holds the sibling lock, calls `admit_direct_open`, and refuses pre-v4 or unversioned data before opening the store. The direct-open regression covers `PersistentBlockHeaderStorage::open`, the `PersistentStorage` implementation, and `SpvStore::open`; it compares the v2 tree before and after refusal. The manager/direct-open exclusion test passes.
2. **R1 #2 / D40: closed.** `PersistentStorage::persist` now calls `sync_wal`; the file-slot branch also uses `Commit::Durable` after its barrier. The power-cut regression preserves B3..B4 after `persist`, while the `raise` control reopens at A2.
3. **R1 #3 / D35: closed.** `Store`, `HdrWriter`, and `Hdr` share one lock-free `Latch`. Mutating entries check it, the fast append checks again while holding the lookup lock, SQLite commits and barriers check it, and `remove_segments` latches listing failures. The latch and concurrency/crash probes pass. The lookup lock and writer lock order remain unchanged.
4. **R1 #4 / D41: closed.** Migration scans the retained v2 run, uses an 8-byte candidate key followed by full-hash verification, and sets all v2 data aside before writing a marker on a repeated hash. Tests cover within-batch, across-batch, header-only, and shared-prefix cases.
5. **R1 #5 / D29: closed.** The design now explicitly documents `mn_log` as a validated, untrusted exception to the marker-height statement.
6. **D42: accept.** `check_recovered` still starts at the acknowledged boundary. Its lower floor includes only a later lowering prefix, and only for the legacy append path; the step path receives no cut. The durable `persist` regression demonstrates that an acknowledged `sync_wal` write is not covered by the relaxation.
7. **D39: accept with the Minor finding above.** The normal refusal path removes its temporary lock on return and leaves v2 data untouched; the lock exclusion test passes. The remaining issue is the PID-content truncation when another process already owns the lock.
8. **D35: accept.** The second check under the lookup lock closes the entry race for fast appends, while the shared latch prevents later writes, barriers, and slot commits. Existing operations that already passed their check may finish, as required by the unchanged lock contract.
9. **Performance question: accept.** D14's one-time glibc read explains the one-event floor, and the prior-head samples already contain the same 5/6 pattern. No measured-path source change in this round warrants a fix; rerun the complete gate as a measurement follow-up.

## Item verdicts

1. **Crash consistency — ACCEPT.** D42 is a valid legacy-only recovery relaxation, and `persist` acknowledgements remain durable.
2. **D27, D36, D38 — ACCEPT.** The oracle covers acknowledged writes, and the required sync ordering remains present.
3. **D35 poisoning — ACCEPT.** The store-wide latch and lookup-lock recheck are implemented and exercised.
4. **D29 / D37 — ACCEPT.** The MN-log exception is documented and repeated active hashes are set aside.
5. **D22 barrier — ACCEPT.** The forced SQLite barrier and reviewed wrappers preserve the barrier semantics.
6. **Public API and compatibility — ACCEPT WITH NIT.** Direct opens refuse old data without destructive migration and exclude the manager; fix the lock PID truncation for a fully unchanged refusal.

## Validation

- `cargo test -p dash-spv --lib storage::block_headers::tests --all-features` — 5 passed.
- `cargo test -p dash-spv --lib storage::migrator::v3::tests --all-features` — 15 passed.
- `cargo test -p dash-spv --lib storage::store::crash::tests --all-features` — 23 passed.
- `cargo test -p dash-spv --lib storage::store::tests --all-features` — 30 passed (also run during the review setup).
- `cargo fmt --all -- --check` — passed.
- `git diff --check 3329bc303..c4177a609` — passed.
- Manual Linux lock reproduction confirmed that opening the lock path with truncation before a failed advisory lock changes the holder's PID file bytes.

VERDICT: ACCEPT WITH NITS
