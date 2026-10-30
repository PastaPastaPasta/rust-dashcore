ACCEPT-WITH-NITS

## Major

None.

## Minor

### 1. Oversized spork signatures report the base-sdk error instead of Core's limited-vector error

- **Rust:** `dashd-masternode/src/sporks/file.rs:40-44,52`
- **Core:** `src/spork.h:113-117`; `src/serialize.h:1018-1031`
- **Problem:** `Spork::decode` uses the base-sdk limited-byte-vector decoder. For a canonical signature length in `(65, MAX_SIZE]`, that decoder returns `DecodeError::CompactSizeExceedsLimit { limit: 65, ... }`; `spork_error` sends it through `ReadError::Other(e.to_string())`. The resulting `CoreRead` text is `compact size value <n> exceeds limit 65: iostream error`, while Core's `LIMITED_VECTOR` throws `Vector length limit exceeded: iostream error`. This length also bypasses the shared flatdb compact-size reader.
- **Failing scenario:** A checksum-valid `sporks.dat` containing one otherwise valid spork whose signature length is 66. rdashd clears it but logs a different `CoreRead` line from dashd v24.0.0-rc.4.
- **Fix:** Decode the signature length through the shared reader and apply the 65-byte limit separately, or map the base-sdk `CompactSizeExceedsLimit { limit: 65, .. }` case to `ReadError::Other("Vector length limit exceeded".into())`; retain the shared reader's canonical and `MAX_SIZE` errors for larger lengths.

## Nit

None.

## Checked and correct

- `flatdb::read_compact_size` checks canonicality before `MAX_SIZE`, including the 9-byte form (`0xff`), and accepts `MAX_SIZE` itself. All netfulfilled and mncache string, map, vote, info, and used-masternode counts use it.
- `LoadError::Format(ReadError)` preserves the relevant Core `e.what()` text; the 15 crafted clear vectors cover short input, over-limit values, exact-limit end-of-data, canonicality, and 9-byte ordering. The flatfile test passes.
- The flatfile readers clear/recreate on format errors and keep Core's short-file distinction; round-trip and Core-import comparisons pass.
- `mnsync` is registered, has Core's status field order and result strings, and delegates `next`/`reset` with the correct state mutations. RPC unit tests pass. The live differential test was intentionally not run per the brief.
- `mn_sync` is constructed before chainstate loading, attached to the RPC slot, registered on validation signals at step 7c, and detached during shutdown. The construction-time test passes.
- No other regression was found in the touched netfulfilled, mncache-meta, sync, or RPC paths.

## Validation

- Confirmed `git -C ~/workspace/rdc-wt/sp1 rev-parse HEAD` is `9b5150394e497f5e5522ae7189c3f9dbe3936648`; worktree is clean.
- `CARGO_TARGET_DIR=/work/scratch/sp1-r2-sol/target CARGO_BUILD_JOBS=8 cargo test -p dashd-masternode --all-features` — passed.
- `CARGO_TARGET_DIR=/work/scratch/sp1-r2-sol/target CARGO_BUILD_JOBS=8 cargo test -p dashd-node --all-features --lib` — 183 passed, 2 ignored.
- `CARGO_TARGET_DIR=/work/scratch/sp1-r2-sol/target CARGO_BUILD_JOBS=8 cargo test -p dashd-rpc --all-features` — 86 passed.
- `cargo fmt --all -- --check` — passed.
