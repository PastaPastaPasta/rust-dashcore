# DW-MG-03 — independent security and correctness review, Sol round 3

Target: `dw/mg-03-masternodes` at `c245a294`, inspected in a detached worktree. The MG-03 fix delta was compared with `cde7e167`; unrelated DP3-04 follow-merge changes were excluded. No branch files were edited, committed or pushed.

## Verdict: CHANGES REQUIRED

### Major 1 — `--verbose-events` bypasses the `mn` value redactor

Locations: `rust/bin/dwcli/src/main.rs:189-210,683`, `rust/bin/dwcli/src/log.rs:18-31,68-101`, `rust/crates/dw-engine/src/session.rs:281-301`, `rust/crates/dw-engine/src/engine.rs:128-137`, `rust/crates/dw-engine/src/events.rs:34-40,75-79`.

Scenario: invoke `mn list-state` with `--verbose-events --network devnet:<64-lowercase-hex> --dapi http://127.0.0.1:1 --peer 127.0.0.1:1 --no-platform`, using an owner-only data directory. A valid devnet name is copied into `EngineEvent::SessionOpened { network }`; `StderrSink::emit` prints the event with `eprintln!("event: {event:?}")`. More sensitively, on mainnet an HTTP `--quorum-url` with a dummy user-info token is refused by the pinned trusted-context provider with an error quoting the URL; `NetworkSession::open` emits that text as a `Notice`, and the same sink prints it. Neither path enters the `tracing_subscriber` writer in `log.rs`, so the value appears in stderr even with `DWCLI_LOG=info`. The fixed setup errors do drop the engine's quoted `--peer` and `--ca-cert` messages, but the wider no-value-echo rule is still not met. The new test matrix does not pass `--verbose-events`. The redactor also deliberately retains values shorter than four characters (`log.rs:14-16,61`); that exception should be reconciled with the literal D32 promise.

### Major 2 — power loss can restore the old key ID after the new vault key is durable

Locations: `rust/crates/dw-engine/src/masternodes/tracking.rs:400-429`, `rust/crates/dw-engine/src/masternodes/tracking.rs:178-195`, `rust/crates/dw-engine/src/masternodes/follow.rs:177-188`, `rust/crates/dw-appdb/src/lib.rs:495-496,1022-1035`, `rust/crates/dw-vault/src/file.rs:230-240`.

Scenario: attach key A, then cleanly close so its recorded ID is durable. Reopen and replace it with B while the list has no entry, so the attach is `unchecked`. `attach_key` deletes A's ID via `AppDb::set_setting`, but app.sqlite uses SQLite WAL `synchronous=NORMAL`: that successful commit is not synced. The vault then fsyncs its temporary file, renames it and fsyncs its directory, making B durable. If the machine loses power before SQLite's WAL is synced or checkpointed, recovery can restore A's ID beside B's vault record. `reconcile_key_ids` only examines *missing* IDs, so it skips A. When the list later names A, the facade can report `matched` and a held role although the vault contains B. This is the same false-role result as round 2, reached through a power-loss window rather than a failed-write restore. The two injected I/O tests cover returned errors at read-back and directory fsync, not the cross-store durability ordering. A's ID must be durably cleared before a different vault key can be made durable, or each read must verify a recorded ID against the vault's actual key.

### Major 3 — the sticky version-3 marker is not durable against power loss

Locations: `rust/crates/dw-engine/src/masternodes/follow.rs:393-409,425-429`, `rust/crates/dw-appdb/src/lib.rs:495-496,1022-1035`, `rust/crates/dw-engine/src/masternodes/reads.rs:203-212,244-254,382-410,432-461`.

Scenario: the follower sees a version-3 masternode and writes `masternodes.any_shared_seen`; that setting also commits under `synchronous=NORMAL`, without a durability barrier. The version-3 node leaves the current list, then the machine loses power before SQLite syncs or checkpoints the WAL. On restart the marker can be absent and the current list contains no version-3 entry. A recent shared coinbase share paid to a known version-2 payout address is then marked `unattributed: false`, included in that row and the overview's `payouts_30d`, excluded from `unattributed_30d`, and shown with `payouts_incomplete: false`. The new arithmetic partitions records correctly while the marker is present, but the marker's promised stickiness is not crash durable. The separately documented fresh-restore gap has the same silent over-counting result when a version-3 masternode disappeared before this installation's first sync; I treat that as an explicitly accepted limitation, not a new finding.

### Low 4 — reconciliation changes an uncompressed WIF's recorded ID

Locations: `rust/crates/dw-engine/src/masternodes/tracking.rs:45-74,389-416`, `rust/crates/dw-vault/src/vault/masternode.rs:104-122,218-231`, `rust/crates/dw-engine/src/masternodes/reads.rs:295-305`.

Scenario: attach an uncompressed WIF for a known uncompressed owner or voting key, then reattach that same WIF when the vault write returns an ambiguous read-back or directory-fsync error. `parse_secret` computes the hash160 using the WIF's uncompressed public key, but sends only the 32-byte secret to the vault. After restart, `masternode_key_id` always serializes a *compressed* public key, so the reconciled ID differs from the one `attach_key` would record for that WIF. The row can become `key_mismatch` and lose a legitimate role. The contract accepts WIF without excluding uncompressed WIF; the D31 tests cover only 64-hex secrets. Preserve the compression choice or constrain the accepted key format consistently.

## Disposition of the four round-2 findings

- **M1:** setup JSON and stderr now use fixed messages, but the direct event sink above still echoes option values.
- **M2:** returned ambiguous vault-write errors no longer restore an old ID, and a later read reconciles a missing ID after a vault-directory fsync. The power-loss ordering above still permits a stale ID.
- **M3:** while version 3 is current or remembered, every generated wallet output is flagged unattributed, omitted from row and overview payout totals, and counted once in `unattributed_30d`. The known fresh-restore gap and the marker durability gap above can silently over-count.
- **L4:** teardown now precedes the result line; a successful command whose teardown fails becomes one `shutdown` error line and exit 1. Closed as ruled.

The write-fault module and both injection points are guarded by `cfg(all(debug_assertions, any(test, feature = "fault-inject")))` (`dw-vault/src/lib.rs:50-51`, `file.rs:235-238`, `vault.rs:1756-1759`); they are compiled out of release builds. The `fault-inject` feature is included from `dw-engine` dev-dependencies only. The reconciliation lock prevents an `attach_key` or `untrack` interleaving within one session; its main risk is the persisted ID pre-filter described above. The `DWCLI_LOG` writer redacts complete tracing events, but it does not cover direct stderr writes.

## Validation

- `cargo test -p dw-engine masternodes`: passed (27 targeted unit tests; command exit 0).
- `cargo test -p dw-engine --test mn_masternodes_contract`: passed (8 tests).
- `cargo test -p dwcli`: passed, including all 3 `mn_cli` tests (command exit 0). Several deadline tests ran longer than 60 seconds under box load but passed; no rerun was needed.
- `cargo test -p dw-vault -p dw-engine masternode_key`: passed (2 engine and 4 vault tests; command exit 0).
- Dash Core v24 `regtest/functional/dwd_masternodes.py`: passed (`Masternodes suite passed`, `Tests successful`, exit 0).
- Direct CLI probe against the built `dwcli`, with `DWCLI_LOG=info` and `--verbose-events`: both devnet-name and HTTP-quorum-URL cases exited 0 with one JSON stdout line, while stderr contained the corresponding supplied value in `event: ...` lines. This confirms Major 1 independently of the source reading.

Majors 2 and 3 are power-loss durability findings from the SQLite setting and cross-store write order; the test suite does not simulate sudden power loss.
