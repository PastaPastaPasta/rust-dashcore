ACCEPT-WITH-NITS

# Findings

## Major

None.

## Minor

### m1. Synchronous ECONNREFUSED logs appear after cached-key loading in Rust

- **Rust:** `dashd-net/src/torcontrol.rs:894-898, 908-919`
- **Core:** `src/torcontrol.cpp:122-136, 324-334`
- **Problem:** On a platform where a nonblocking connect to a closed control port returns `ECONNREFUSED` immediately, libevent invokes Core's error callback synchronously from `bufferevent_socket_connect`. Core therefore logs `Error connecting to Tor control socket` and schedules reconnect before the constructor reads and logs the cached private key. Rust starts the connect, constructs `TorController` (which reads the key), and only then handles `Attempt::Started(Err)` and logs the error/reconnect event.
- **Failing scenario:** Put a valid `onion_v3_private_key` in the datadir and run against a closed control port on macOS/BSD (or another platform with immediate refusal). Core's log order is error/reconnect, then `Reading cached private key`; rdashd logs `Reading cached private key` first. The existing refusal differential uses a fresh datadir and Linux's delayed refusal, so it does not expose this ordering difference.

### m2. The node-level `-onlynet` Tor reachability arm has no behavioral test

- **Rust:** `dashd-node/src/p2p/tor.rs:34-41, 101-104`
- **Core:** `src/torcontrol.cpp:397-411`
- **Problem:** The controller tests pass a fake `onlynet_allows` boolean, and the process differential never passes `-onlynet`; no test exercises `NodeTorFiles::onion_allowed_by_onlynet` or the `-noonlynet`/`tor` parsing path. A mutant that always returns `true`, or ignores whether `-onlynet` was set, survives the current suites.
- **Failing scenario:** Run with `-onlynet=ipv4` and a Tor mock that answers `GETINFO`. Core installs the onion proxy but keeps Onion unreachable, so `getnetworkinfo` reports `reachable=false` and `AddLocal` does not advertise the onion service. The surviving mutant marks Onion reachable and advertises it without any test failure.

## Nit

### n1. `-proxy` override and `-noonion` are absent from the rc.4 differential matrix

- **Rust:** `dashd-node/src/p2p/tor.rs:94-99`; `dashd-net/src/torcontrol.rs:493-507`; `rdashd/tests/tor_diff.rs:260-301`
- **Core:** `src/torcontrol.cpp:454-467`; `src/init.cpp:1932-1973`
- **Problem:** The implementation follows Core, but `rdashd/tests/tor_diff.rs` only compares an explicit `-onion` proxy. It does not pin that automatic `GETINFO` replaces an inherited `-proxy` when `-listen=1`, or that `-noonion` suppresses `GETINFO` while still issuing `ADD_ONION` and leaving Onion unreachable.
- **Failing scenario:** A mutant that uses `is_arg_set("-onion")` instead of Core's `GetArg("-onion", "") == ""` sends the wrong command transcript for `-onion=`/`-noonion`, but all current differential cases still pass.

### n2. The immediate-refusal connect branch is not exercised on this Linux host

- **Rust:** `dashd-net/src/torcontrol.rs:831-839`
- **Core:** `src/torcontrol.cpp:168-173` and `src/torcontrol.cpp:122-136`
- **Problem:** `a_refused_control_port_matches_dashd_v24` reaches the delayed error-event path here: Linux loopback returns `EINPROGRESS`, then the writable event reports refusal. It does not execute Rust's `ConnectionRefused` arm or Core's synchronous libevent error callback.
- **Failing scenario:** A regression that treats immediate `ECONNREFUSED` as `Attempt::Failed` would pass the current Linux tests while omitting the error-event/reconnect path on macOS/BSD. Factor the errno classification into a testable function or add a platform-specific test.

### n3. SAFECOOKIE unit expected hashes are computed by the implementation under test

- **Rust:** `dashd-net/src/torcontrol_tests.rs:290-317, 325-329`
- **Core:** `src/torcontrol.cpp:473-498, 519-526`
- **Problem:** The unit test computes both expected hashes with the same `compute_response` used by the controller. A mutant that swaps nonce order, swaps the two HMAC keys, or changes both client and server calculations consistently survives the unit test.
- **Failing scenario:** Such a mutant would send an authentication hash Tor rejects; only the independent mock in `tor_diff` catches the nonce-order/key errors. Add a fixed known HMAC vector or an independently implemented test helper.

### n4. Parser boundary cases leave surviving mutants

- **Rust:** `dashd-net/src/torcontrol.rs:345-355, 984`; `dashd-net/src/torcontrol_tests.rs:458-481, 524-540`
- **Core:** `src/torcontrol.cpp:89-116`; `src/util/strencodings.h:172-190`; `src/util/strencodings.cpp:81-97`
- **Problem:** The current tests do not pin several Core quirks: a final `+50` line must produce code 0, `600` is asynchronous while `599` is synchronous, exactly 100,000 unfinished bytes are retained, and vertical-tab is accepted as hex whitespace. Mutants for each boundary survive the existing unit and integration suites.
- **Failing scenario:** A Tor reply containing `+50`, `600`, or a 100,000-byte partial line would be classified or disconnected differently by those mutants, but no current assertion observes the difference. Add direct assertions for the reply code/handler, async threshold, length boundary, and `parse_hex(b"00\x0bff")`.

### n5. A lone quote in the SOCKS listener list is untested

- **Rust:** `dashd-net/src/torcontrol.rs:520-533`
- **Core:** `src/torcontrol.cpp:357-367`
- **Problem:** The `port.len() >= 2` guard is correct, but no test sends a one-byte quoted token. A mutant that slices away the first and last byte without the guard would panic the controller task.
- **Failing scenario:** Tor replies with `250-net/listeners/socks=" 127.0.0.1:9250`. Core ignores the malformed one-byte token and selects the valid localhost endpoint; the mutant panics or loses the proxy update. Add this token to `the_onion_proxy_from_getinfo`.

### n6. `write_private` does not report a close error

- **Rust:** `dashd-node/src/p2p/tor.rs:72-79`
- **Core:** `src/util/readwritefile.cpp:37-49`
- **Problem:** Rust returns the result of `write_all` and drops the file, so an error reported by `close` is ignored. Core checks `fclose` and reports `WriteBinaryFile` failure.
- **Failing scenario:** On a filesystem that accepts the write but reports a delayed flush error from `close`, Core logs `tor: Error writing service private key ...` while rdashd logs the cache write as successful. Check `sync_all`/close status before returning success if exact error behavior is required.
