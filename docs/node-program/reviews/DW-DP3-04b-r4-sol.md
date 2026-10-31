# DP3-04b storage pin and pool rotation: Sol review, round 4

**Verdict: CHANGES REQUIRED.** Reviewed platform `cb2988b285..425f4d498f` and desktop `1b7842d1..9faeeff5` in detached worktrees. Both round-3 High schedules are closed, and a live WAL source restores its committed frames. One finding requires manager handling before this pin is accepted.

## Finding

possible security note: see manager

## Narrow-scope assessment

- The staged snapshot comes from SQLite's online backup on the validated source connection (`packages/rs-platform-wallet-storage/src/sqlite/backup.rs:311-325`). The staged file is switched to DELETE mode, checked, and given a new generation token before proof (`:317-369`). The WAL schedule below confirms that uncheckpointed committed frames are present in the restored file.
- `prove_opens` copies the staged bytes, calls `SqlitePersister::open_inner`, and removes the disposable directory on normal completion (`backup.rs:495-510`). `open_inner` applies the real integrity, history, migration, reconcile, and shape-check path (`persister.rs:308-402`). The stage is not written after the proof and before the rename (`backup.rs:369-456`). A concurrent destination writer or a second restore can change which proved snapshot wins, or lose a write in the documented lock-release window, but cannot make this restore rename unproved stage bytes. A proof refusal returns before the destination lock or rename.
- The two tested proof refusals left no staged file or proof directory. Ordinary early-error paths use `NamedTempFile` and `TempDir` drop cleanup; the success path calls `TempDir::close`. Cleanup itself can fail on a filesystem error, and a power loss can leave an artifact. The documented extra space and proof-versus-caller configuration limits are acceptable for this pin, subject to the finding above.
- Desktop changes are confined to `rust/Cargo.toml`, `rust/Cargo.lock`, and `docs/design/DP3-04-trust-gate.md`: all eight direct platform revisions and all 31 lockfile platform package sources move together to `425f4d498f`; the design note adds D38 and updates the pin description. No other desktop behavior changed.

## Validation

- Built the platform CLI at `425f4d498f` with `CARGO_TARGET_DIR=/work/scratch/dw-dp304b-r4-sol/target CARGO_BUILD_JOBS=12 cargo build -p platform-wallet-storage --locked --bin platform-wallet-storage --features dpp/ed25519-dalek` (exit 0).
- Independently ran the two round-3 CLI schedules against that binary with `--no-auto-backup`: deleting fork V1's history row and setting upstream V1's checksum to `0` each made restore exit **3**; `live.db` stayed SHA-256 identical, `migrate` exited **0**, and no staged/proof artifacts remained.
- With a live source connection holding `birth_height=111` only in WAL and a destination at `222`, restore exited **0** and the reopened destination read **111**. A copy of the source main file alone had no wallet row. Restore over itself also exited **0** and reopened successfully. The first WAL fixture retained its destination connection and got `RestoreDestinationLocked`; it passed alone after the fixture closed that connection.
- Desktop `cargo metadata --locked --offline --format-version 1` exited **0**. Programmatic manifest/lock checks found 8/8 direct revisions and 31/31 package sources at the new head. `git diff --check` passed in both repositories.

No branch was edited, committed, or pushed.
