> Durable copy for node-program reviews/ (DEC-984). A few words may be reworded for the commit gate; the findings and the verdict are unchanged.

# BL-562 r1 correctness review

Reviewed head `d7814ca0cc6afa575d98127a608a1ba0ad65bcdc` against `3807cb2e3`, limited to `rdashd/tests/dashd_v24.rs`. Followed the task brief and `briefs/REVIEW-COMMON.md`. The reviewed worktree remained clean and at the specified head.

## Findings

No blocker, major, minor, or nit findings.

## Correctness checks

- `rdashd/tests/dashd_v24.rs:192`: `$PORT1` and `$PORT2` are allocated lazily once per case. Repeated `$PORT1` occurrences use the same value, and the resolved arguments remain alive across both binaries' sequential runs. The original case string remains the diagnostic label.
- Substitution uses substring replacement: `$PORT10` would match `$PORT1` and become `<port>0`. The helper explicitly supports only `$PORT1` and `$PORT2`, and no current case contains another token. This does not affect the supported cases; extending the token set would require reconsidering token boundaries.
- `rdashd/tests/dashd_v24.rs:226`: assigning the two ports independently fixes RPC default-port use in the `-port=0` cases. Recognizing `-noport` preserves its invalid-zero-port check. The devnet exclusion preserves both the missing-port cases and every argument passed by `an_unopenable_chainstate_fails_before_the_block_index`.
- Checked Core `v24.0.0-rc.5:src/init.cpp` and the relevant pinned rc.4 code: RPC starts before the spork checks; spork checks precede P2P/RPC port validation; port validation precedes user-agent comments. Valid supplemental ports preserve the intended failure order.
- Independently compared rc.4 oracle exit codes and complete stderr before/after supplemental-port assignment for all nine affected table cases: `-rpcport=0`; `-server=0 -rpcport=1 -noport`; server-disabled P2P ports `0`, `x`, and `65536`; the two spork cases with `-port=0`; `-port=0 -uacomment=a/b`; and the port-22/invalid-whitelist case. All were identical. In particular, the user-agent case still fails on port zero, and the port-22 case preserves its warning.
- Independently ran the new onion case against rc.4 with distinct free ports below 32768. It produced the intended RPC-versus-onion P2P bind conflict, followed by “Failed to listen on any port,” rather than an HTTP startup error.
- Remaining literals are safe for these cases: devnet cases fail before binds; ports 29992/29993 are parsed and mentioned in an onion warning before the invalid whitelist fails; the TEST-NET address fails its explicit bind. Core's default onion target is configured before some later errors but is not bound until `CConnman::Start`/`InitBinds`. The only cases reaching that bind stage use explicit ordinary/onion binds, so neither takes the default onion listener.

## Independent validation

Built an unmodified reflink source copy with its own target directory, `CARGO_INCREMENTAL=0`, eight jobs, `nice -n 19`, and Rust/C++ cache wrappers disabled. Before building, five-minute load was 60.57 and free disk space was 222G. Tests used their own mode-0700 directory under `/dev/shm` and the pinned rc.4 oracle; none were skipped.

- `cargo test --locked -p rdashd --test dashd_v24 --no-run`: PASS.
- `cargo fmt -p rdashd --check`: PASS.
- `git diff --check 3807cb2e3 d7814ca0c -- rdashd/tests/dashd_v24.rs`: PASS.
- `init_errors_match_dashd_v24` alone: PASS.
- `an_unopenable_chainstate_fails_before_the_block_index`: PASS.
- `init_errors_match_dashd_v24` with listeners held on both IPv4 and IPv6 loopback for ports 9996/9998/9999, 19796/19798/19799, 19896/19898/19899, 19996/19998/19999, and 29990–29993: PASS.
- Three concurrent copies of `init_errors_match_dashd_v24`, with those listeners still held: PASS, all three exits zero.

The copied test file's SHA-256 matched the reviewed worktree before and after validation: `3df59e7175b807e705071104a8ed23507de9624c3468f7f4b43945388c996c45`. All spawned processes exited and all held sockets were closed. No branch edits, commits, pushes, or posts were made. No prohibited cache, chain, or restricted-notes trees were read.

Verdict: APPROVE
