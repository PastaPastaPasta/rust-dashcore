# P4a inbound policy review, round 1

**Verdict: CHANGES REQUIRED** for the stated bit-for-bit Core parser parity. Reviewed `a5d2a9c59..021cbd885` in a detached worktree against Dash Core v24.0.0-rc.3 (`0364ed626e`). I made no branch edits.

## Findings

1. **P2 — Core accepts numeric address forms that P4a rejects (D3, D6, D17–D18).** `dash-p2p/src/lookup.rs:12-16,21-26,32-37,44-52` calls `pre_lookup`, which accepts only Rust `IpAddr` literals (`dash-p2p/src/seeds.rs:66-75`). Core's `LookupIntern` calls `WrappedGetAddrInfo` with `AI_NUMERICHOST` even when DNS lookup is disabled (`src/netbase.cpp:44-65,147-174`), and the new interfaces use it through `NetWhitebindPermissions::TryParse` (`src/net_permissions.cpp:90-93`), `NetWhitelistPermissions::TryParse` (`src/net_permissions.cpp:113-116`), and `setban` (`src/rpc/net.cpp:777-784`). **Failure schedule:** start with `-whitebind=127.1:9999`. On this glibc host Core resolves that to `127.0.0.1:9999` and binds; `parse_whitebind` returns `Cannot resolve -whitebind address`. Likewise `-whitelist=11.1` resolves to `11.0.0.1/32` in Core but fails here, and `setban 11.1 add` returns an invalid target. D18 records this inherited BL-96 mismatch, but the new parser and RPC node half expose it in P4a's claimed parity. Use Core-equivalent numeric lookup or explicitly narrow the parity claim until BL-96 lands.

2. **P3 — The `qfcommit` replay cannot detect a wrong solicitation hash (D7–D8).** `dashd-p2p/src/processing/stubs.rs:130-142` hashes the decoded commitment and passes that inventory to `consume_getdata_response`; Core uses `SerializeHash(qc)` in `src/llmq/blockprocessor.cpp:106-118`. Every `inbound-qfcommit` peer is unsolicited, so `dashd-p2p/tests/dashd_inbound_transcripts.rs:215-220` gets `Unrequested` regardless of the hash. **Failure schedule:** replace `qc.hash()` with a constant or the hash of the raw payload, then replay the fixture: the ten/nine-message score and disconnect results still pass. After a request for a canonical commitment, send its payload with trailing bytes. Core consumes the request because it hashes the decoded object's re-serialization; the mutated node would score +10. Add a targeted requested/late-response test that records the canonical inventory hash before delivering that payload.

## Evidence and limits

- The permission flag table, implicit flag expansion, banlist JSON layout and ordering, non-IP ban reload, `notfound` limit, `qfcommit` score threshold, capture record layout and paths, and transcript replay checks otherwise matched the pinned Core code paths I inspected. The pinned base SDK's `Commitment::hash()` encodes the decoded object before SHA256d, including Core's version-dependent BLS representation.
- `inbound-evict-groups` checks five different group choices. `inbound-evict-mn` checks eviction after the replay manually marks the peer verified; it does not test MNAUTH verification, which belongs to AM1. The `notfound` oracle case checks the 50,000/50,001 limit and scoring with no outstanding requests; it does not prove response-tracker completion. The brief says three mutation checks are recorded in P4a notes, but `P4a-notes.md` contains no mutation procedure or result to audit.
- The `sim_dial::connect_only` gate timeout is not plausibly caused by the P4a `dashd-net` diff: `lib.rs` only exports modules, `store.rs` adds a warning only to ban-blob loads, and the new `bans.rs`/`capture.rs` are not on the dial or connection-finalization path. The brief reports the same load timeout on five other branches.

## Validation

Test commands were run offline with `CARGO_TARGET_DIR=/work/scratch/p4a-r1-sol/target` (0700):

| Command | Exit | Result |
|---|---:|---|
| `cargo test -p dash-p2p` | 0 | Unit and integration tests passed. This default-feature run does not include `permissions.rs`'s feature-gated unit tests. |
| `cargo test -p dashd-p2p --test dashd_inbound_transcripts --test sim_m2` | 0 | 13 transcript and 44 `sim_m2` tests passed. |
| `cargo test -p dashd-net --test dashd_inbound_policy` | 0 | 3 tests passed. |
| `cargo test -p dash-p2p --features inbound permissions::tests` | 0 | 3 permission parser tests passed. |

The local numeric-host probe `socket.getaddrinfo("127.1", 9999, ..., AI_NUMERICHOST)` resolved to `127.0.0.1` (exit 0), while `pre_lookup` has only `IpAddr::parse` for numeric hosts. No wall-clock failure occurred in these runs.
