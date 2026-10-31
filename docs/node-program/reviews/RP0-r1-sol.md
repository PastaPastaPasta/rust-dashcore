# RP0 round-1 review (Sol)

Range: `node/rp0-rpc-core` `f4046b5f5..bf2b565d2` (six signed commits), detached checkout at `bf2b565d2`.
Reference: Dash Core `v24.0.0-rc.3`.

## Findings

### major

possible security note: see manager

### minor — signed HTTP versions are not byte-compatible

- Location: `dashd-rpc/src/http/parse.rs:399-410`.
- Core's `sscanf("HTTP/%d.%d%c")` accepts a signed negative component. The Rust parser stores both components as `u32`, so `HTTP/1.-1` and `HTTP/-1.1` are rejected as a 400 error page. A v24 dashd request with either form reaches JSON-RPC and returns 405, preserving the signed version in the status line (`HTTP/1.-1 405 ...` / `HTTP/-1.1 405 ...`).
- Suggested fix: preserve Core's signed parsing/rendering behavior (or add an explicit compatibility branch), and add both forms to DIFF-RPC. These are malformed client inputs, but the range claims raw HTTP parity.

## Review checks

- Authentication: constant-time compare, rpcauth HMAC, cookie generation with owner-only mode, cookie cleanup after failed auth initialization, rpcuser/rpcpassword, and whitelist parsing were traced against Core. The auth and whitelist mutation checks failed as expected when the comparison/whitelist predicates were inverted, then the checkout was restored.
- Whitelist checks cover singleton calls, named/composite calls, every batch member, non-object batch members, and the default-deny profile. The integration suite passed.
- HTTP limits and lifecycle: header/body/chunk parsing, queue depth, negative timeout behavior (`0`/`-1` no timeout and below `-1` immediate close), panic recovery, bind/allow behavior, warmup, `/wallet/`, and shutdown ordering were exercised. The panic worker survives, and the long-call/stop hook passes.
- DIFF-RPC: the pinned v24 binary and rdashd matched all 46 committed raw cases after masking only `Date`. Additional probes matched for HTTP/0.1, HTTP/2.0 rejection, truncated bodies, transfer-encoding, duplicate content length, chunk extensions/malformed chunks, absolute targets, malformed auth, and folded headers. The two negative-version probes are the minor finding above.
- D7/resource probe: possible security note: see manager
- Ledger/status: STATUS records RP0 as complete with ledger rows 137 and 319–321, bench inputs unchanged, and the requested re-homes (`rpc_coinjoin` → WL6, `rpc_net`/`rpc_setban` → P2a/RP1).

## Tests

- `cargo test -p dashd-rpc --all-features -- --test-threads=4`: **54 passed**.
- `cargo test -p rdashd --all-features --test rpc -- --test-threads=1`: **14 passed**.
- `cargo test -p rdashd --all-features --test hooks -- --test-threads=1`: **5 passed**.
- `DASHD_PATH=.../dashcore-24.0.0-rc.3/bin/dashd cargo test -p rdashd --all-features --test rpc_diff -- --nocapture --test-threads=1`: **46/46 raw cases equal**.

VERDICT: CHANGES-REQUIRED
