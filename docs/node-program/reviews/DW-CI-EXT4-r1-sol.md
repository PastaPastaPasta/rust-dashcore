# Review of 5b21845a

## Major

None.

## Minor

None.

## nit

None.

## Verification and reasoning

- `rust/crates/dw-appdb/src/lib.rs:397-450`: the first descriptor remains open across the second path open and identity comparison. On Unix, an unlinked-but-open inode cannot be reallocated, so remove/create cannot reproduce the ext4 inode and pass `(dev, ino)`; rename-over also produces a different inode. The comparison is made before `sole` is dropped. A replacement after that comparison remains the documented accepted residual because the shared lock is acquired on the already opened descriptor.
- The release ordering at `:438-450` does not introduce a new concurrent-open deadlock. `alone` is only an opportunistic exclusive-lock result; simultaneous opens are allowed to proceed to shared locks. Releasing the exclusive lock before opening/locking `alive` was already required by the old close-and-reopen sequence. If another opener wins the gap, it releases its own exclusive handle before taking its shared lock, so both opens can eventually succeed; a swap causes both identity checks to refuse as intended. No process can make both callers fail forever absent an external filesystem failure or continued replacement.
- Error paths are RAII-safe: connection/init/identity failures drop `conn`, `alive` (when present), and `sole`; an unlock failure returns while dropping the still-held descriptor; a shared-lock failure drops `alive`. The successful path stores only `alive` in `_alive`, so the unlocked first descriptor cannot be mistaken for the lifetime lock holder. `sole.unlock()` is conditional on `alone`, matching Windows `UnlockFile` behavior when no lock is held.
- Platform semantics checked against the Rust standard library: Linux and macOS use `flock`; Windows uses `LockFileEx` and `UnlockFile`, with `UnlockFile` requiring a lock for its first call. Windows `std::fs::OpenOptions` includes `FILE_SHARE_DELETE`, so an open handle does not prevent the documented delete/rename residual. Windows has `FileId = None`, so a swapped lock can still pass the identity comparison; this is the pre-existing and explicitly documented DEC-210/contract gap, not introduced by this commit.
- `rust/crates/dw-appdb/src/migration_tests.rs:1416-1441`: the hook runs synchronously at the exact inter-open point, so the test has no timing sleep/race dependence. It covers both remove-then-create (the ext4 inode-reuse regression) and rename-over. The implementer’s ext4 mutant evidence is consistent with the code, and the new test passes on this checkout.

## Tests run

- `CARGO_TARGET_DIR=/work/scratch/dw-ci-ext4-sol/target cargo test -p dw-appdb migration_tests::a_lock_file_replaced_during_the_open_is_refused -- --exact --nocapture` — passed.
- `CARGO_TARGET_DIR=/work/scratch/dw-ci-ext4-sol/target cargo test -p dw-appdb --lib` — 106 passed, 0 failed.
- `git diff --check fb914927 5b21845a` — clean.

ACCEPT
