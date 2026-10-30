ACCEPT-WITH-NITS

# P8b r1 review (Opus): Tor control, 81795bc90..1b35b38e9

Tree: /work/scratch/p8b-review/wt at 1b35b38e9, read only. Core: dash 375c64d2317 (v24.0.0-rc.4),
`src/torcontrol.cpp` (tc), `src/init.cpp` (init), util/strencodings.{h,cpp} (se).

## What I checked

I compared the whole of `dashd-net/src/torcontrol.rs` line by line with tc:66-713, then
`dashd-node/src/p2p/{tor.rs,mod.rs}` with init:290, :352, :1903-1975 and :2710-2728.

**Reply parsing.**
- `SplitTorReplyLine` and `ParseTorReplyMapping` are equal, including the quoted-string escapes,
  the octal 4–7 rule, the `OptArguments` break, and the empty map on an unterminated pair.
- The reply code follows `ToIntegral` (`from_chars` takes `-`, not `+`, se.h:181-190).
- Short lines are skipped, `-` and `+` lines gather into one reply, the final line's code wins,
  and replies of 600 or above go to no handler.
- Lines split as `EVBUFFER_EOL_CRLF` splits them, including a lone `\r` and `\r\r\n`.
- `MAX_LINE_LENGTH` is checked after the complete lines are taken, and each read is 4096 bytes,
  which matches libevent's `EVBUFFER_MAX_READ`.

**Authentication.**
- The selection order is the same: password wins, then NULL, then SAFECOOKIE, then the
  HASHEDPASSWORD-only message, then "No supported authentication method" (tc:572-605).
- Only `"` is escaped in the password.
- The cookie is read up to 32 bytes and must be exactly 32. A longer file is cut and used, as
  `ReadBinaryFile(path, 32)` does. A directory or a missing file is "could not be opened".
- The client nonce is 32 random bytes.
- The HMAC inputs are the cookie, the client nonce and the server nonce, under the correct keys
  for each direction.
- `ParseHex` follows `TryParseHex`: `IsSpace` includes `\v`, and an odd length or a space inside
  a byte pair gives an empty result.
- A server nonce that is not 32 bytes is refused before the hash is checked, and the server hash
  is compared as bytes.

**ADD_ONION.**
- The default port is the virtual port and the target is `ToStringAddrPort`.
- `NEW:ED25519-V3` is sent only when no key is cached.
- `ServiceID` and `PrivateKey` persist across sessions as Core's members do. `ServiceID` is the
  service's address and stays one; `PrivateKey` is the key.
- The key file is rewritten after every successful ADD_ONION, and the write's failure is logged
  without blocking `AddLocal`.
- An invalid service ID gives `AddLocal([::]:0)`. On disconnect it is not removed, because
  `IsValid` is false. This quirk is kept.

**Lifecycle.**
- The connect happens before the cached key is read.
- A failed lookup or `GetSockAddr` ends the loop (no reconnect is scheduled).
- An immediate `bufferevent_socket_connect` failure leaves the loop waiting for the interrupt.
- ECONNREFUSED and later failures are the error event.
- `connected_cb` resets the backoff. `disconnected_cb` withdraws the service, then waits
  1 s × 1.5ⁿ, computed in `float` and truncated to whole milliseconds.
- `reply_handlers` and the half-gathered reply survive a reconnection.
- A line over `MAX_LINE_LENGTH` frees the connection without the callback, so the loop ends.
- The destructor withdraws the service.
- `InterruptTorControl` logs before the network's interrupt (init:290), and `StopTorControl`
  runs after `CConnman::Stop` (init:352).
- The libevent connect classification matches `evutil_socket_connect_`'s return codes on Unix
  and on Windows (`EVUTIL_ERR_CONNECT_RETRIABLE`/`_REFUSED`).

**Proxy interplay.**
- `ask_socks_port` is `GetArg("-onion","")==""`, so `-noonion` gives "0" and no GETINFO.
- With `-proxy` and `-listen=1`, Tor's SOCKS port overrides `-proxy` as the onion proxy, as
  Core's does.
- The proxy always has `randomize=true`.
- Onion becomes reachable only when `-onlynet` allows it (`!IsArgSet || any ParseNetwork==ONION`;
  `-noonlynet` gives false in both).
- `AddLocal` still checks Onion's reachability, which is P8a's runtime state.
- Step 6 removes Onion and raises the `-onlynet=onion` error only when both the proxy and
  `-listenonion` are absent (init:1960-1974; P8a code unchanged).

**Key file.**
- D15's codec path is byte-equal for every key Core writes. The decoder rejects non-canonical
  padding bits, so the round trip is exact.
- Everything else, including oversize content, falls back to a verbatim read.
- A missing file gives `None`, so there is no "Reading cached private key" line. An empty file
  gives `Some("")`, so the line is logged and NEW is requested. Both match Core.

**Feature split (a9879cc14).** It is coherent. `cargo tree -p rdashd -e normal` is what
asserted_log_lines reads, and the four `full`-only test targets carry `required-features`.

**Runs** (CARGO_TARGET_DIR=/work/scratch/p8b-review/target-opus):
- `cargo test -p dashd-net --lib torcontrol`: 16 passed.
- `cargo test -p dashd-net --test torcontrol`: 4 passed.
- `cargo test -p rdashd --test tor_diff` against the cached rc.4 dashd: 3 passed (`tor_control_matches_dashd_v24`, `a_refused_control_port_matches_dashd_v24`, `an_unreachable_control_port_matches_dashd_v24`), in 19 s at load average 120

**Mutants**: env-gated, one build, in a reflink copy at /work/scratch/p8b-review/mut-opus. The
shared tree was not touched.

| # | Mutant | dashd-net unit | dashd-net integ |
|---|---|---|---|
| M1 | `+50` parsed as code 50 (`'+'` filter dropped) | survives | survives |
| M2 | async threshold `>= 600` → `> 600` | survives | survives |
| M3 | `MAX_LINE_LENGTH` `>` → `>=` | survives | survives |
| M4 | quote stripping without `len() >= 2` (panics on a lone `"`) | survives | survives |
| M5 | `ParseHex` without `\v` as space | survives | survives |
| M6 | HMAC inputs server nonce before client nonce | survives | survives |
| M7 | immediate ECONNREFUSED treated as connect failure | survives | survives |
| M8 | `service_id` not kept across ADD_ONION replies | survives | survives |
| M9 | handlers/message cleared on disconnect | killed | — |
| M10 | password quotes not escaped | killed | — |
| M11 | `METHODS` unioned across AUTH lines (Core: last wins) | survives | survives |
| M12 | immediate connect failure ends the loop without waiting | survives | killed |

Two of these are not gaps in this suite. M6 is self-referential in the unit test, which computes
the expected hashes with the module's own `compute_response`; tor_diff's mock computes them
independently, and tor_diff kills M6: I rebuilt rdashd from the mutant copy and ran
`tor_control_matches_dashd_v24` with `A6MUT=6`, and it failed because the SAFECOOKIE session never
reaches ADD_ONION. M11 also survives tor_diff, since the mock sends one AUTH line. M7 cannot be reached on Linux loopback: a non-blocking connect
to a closed 127.0.0.1 port returns EINPROGRESS (115) here, not ECONNREFUSED. The branch matters
on BSD and macOS, where the refusal is immediate. The other survivors are below.

I found no accepted-vs-rejected divergence, and no wire-byte divergence.

## Major

None.

## Minor

### m1. The node's `-onlynet` arm of `g_reachable_nets.Add(NET_ONION)` has no test

- **Rust:** dashd-node/src/p2p/tor.rs:34-41 (`NodeTorFiles::onion_allowed_by_onlynet`) and
  :103 (`is_arg_set("-onlynet").then(get_args)`).
- **Core:** tc:397-411.
- **Problem:** The only tests of the reachable flag use a fake. Both
  `dashd-net/src/torcontrol_tests.rs` (`FakeHost::onlynet_allows`) and
  `dashd-net/tests/torcontrol.rs:47` (`Files` returns `true`) hard-wire it. No test reaches
  `NodeTorFiles`: none of tor_diff's cases passes `-onlynet`, and dashd-node has no test of the
  type. A mutant `onion_allowed_by_onlynet → true` (or one that ignores `is_arg_set`, which gets
  `-noonlynet` wrong) survives every suite.
- **Failing scenario:**
  1. rdashd runs with `-onlynet=ipv4 -bind=…=onion`, with the defaults `-listen=1 -listenonion=1`,
     against a Tor that answers GETINFO.
  2. Core sets the onion proxy but leaves Onion unreachable. `getnetworkinfo` shows
     `onion.reachable=false`, and the onion service is not a local address, because `AddLocal`
     refuses an unreachable network (net.cpp `AddLocal`).
  3. The mutant makes Onion reachable and advertises the onion address. Nothing would notice.
- **Fix:** Add a tor_diff case with `-onlynet=ipv4` and one with `-onlynet=tor`; the second
  pins `ParseNetwork`'s deprecation line, which tor_lines would need to keep. `getnetworkinfo`
  and the local addresses then pin the arm against rc.4.

### m2. The `-proxy` + `-listen=1` override and `-noonion` are not in the differential

- **Rust:** dashd-node/src/p2p/tor.rs:97 (`ask_socks_port`) and torcontrol.rs:493.
- **Core:** tc:456-458, init:1903-1975.
- **Problem:** The behaviour is right by reading, but two configurations that mix P8a's proxy
  table with Tor control are not compared with dashd:
  - `-proxy=X -listen=1` (no `-onion`): the onion proxy starts as X and Tor's SOCKS port replaces
    it after authentication.
  - `-noonion -listen=1`: no GETINFO, ADD_ONION is still sent, and `AddLocal` refuses the onion
    address because Onion is unreachable.

  tor_diff covers only `-onion=127.0.0.1:19051`. A mutant computing `ask_socks_port` as
  `!is_arg_set("-onion")` fails `-onion=` (empty) and `-noonion` differently from Core, and
  survives.
- **Fix:** Add both configurations to tor_diff's case table; they cost two more runs.

## Nit

### n1. Log-only and boundary survivors (M1, M2, M3, M5, M8, M11): one line each would pin Core's quirks

- **Rust:** torcontrol.rs:345-349, :355, :984, :177, :573-604, :388.
- **Core:** tc:92 with se.h:181-190, tc:97, :116, se.cpp:86, tc:418-426, :550-553.
- **Problem and scenarios:**
  - **M1:** a final line `+50 X` to GETINFO logs "error code 0" in Core and "error code 50" under
    the mutant. The existing `+50 OK` case cannot tell 0 from 50, because both are non-250.
    Assert the code (`TorReply.code`) or the log.
  - **M2:** add a `600 …` line.
  - **M3:** add exactly 100 000 bytes, which must not disconnect, next to 100 001.
  - **M5:** add `parse_hex(b"00\x0bff")`.
  - **M8:** a second ADD_ONION 250 reply without `ServiceID` reuses the earlier ID in Core.
  - **M11:** a PROTOCOLINFO with two AUTH lines uses the last line's methods.

### n2. A lone quote in `net/listeners/socks` is untested (M4)

- **Rust:** torcontrol.rs:525-527.
- **Core:** tc:359.
- **Problem:** Without the `len() >= 2` guard, `port = &port[1..0]` panics, and the controller
  task dies. Today's code is right, but the guard is unpinned.
- **Fix:** Add `250-net/listeners/socks=" 127.0.0.1:9250` to `the_onion_proxy_from_getinfo`.

### n3. The immediate-ECONNREFUSED branch is untested on Linux (M7)

- **Rust:** torcontrol.rs:837.
- **Core:** libevent's `bufferevent_socket_connect` (`r == 2`), called from tc:169.
- **Problem:** D17 says "an immediate refusal is libevent's error event in dashd too". On this
  box a non-blocking loopback connect to a closed port returns EINPROGRESS, so
  `a_refused_control_port_matches_dashd_v24` exercises the delayed path, not this branch. On
  macOS the refusal is immediate, so the branch is the one that runs, and no test reaches it.
- **Fix:** Either reword D17 or add a `cfg(any(target_os="macos", target_os="freebsd"))` test.
  Alternatively, factor the classification into a function over an `io::Error` and unit-test it
  with `ECONNREFUSED`, `EINPROGRESS`, `EINTR` and `ENETUNREACH`.

### n4. D15's mode claim is only true when the file is created

- **Rust:** dashd-node/src/p2p/tor.rs:72-79.
- **Core:** util/readwritefile.cpp `WriteBinaryFile`, and init's `umask(077)` unless `-sysperms`.
- **Problem:** `OpenOptions::mode(0o600)` applies only on creation. A key file already present
  with another mode (for example a Core datadir run with `-sysperms`) keeps that mode after the
  truncating rewrite. Core's own file is also 0600 by default (umask 077), so "Core's follows
  the umask" makes the deviation sound larger than it is.
- **Fix:** Reword D15: "created 0600; an existing file keeps its mode, as with Core's `fopen`".

### n5. D16's "logging under -debug=tor only" is slightly too strong

- **Rust:** torcontrol.rs:776-779, :812-823.
- **Core:** tc:147, :154, :170, :326, :692.
- **Problem:** With nothing on 9051, the refusal and reconnect lines are indeed `-debug=tor`
  only. But "tor: Thread interrupt" is unconditional at every shutdown of a listening node, and
  the lookup and immediate-connect failures are `LogPrintf`. All of this matches Core; only the
  D-row text overstates.

### n6. The unit test's SAFECOOKIE hashes are self-referential

- **Rust:** dashd-net/src/torcontrol_tests.rs:310-311, :328.
- **Problem:** The expected server and client hashes come from the module's own
  `compute_response`, so M6 (input order) and a swapped HMAC key and message both survive the
  unit suite. Only tor_diff, which needs rc.4 dashd, catches them.
- **Fix:** Add one fixed vector, the literal hex of `HMAC-SHA256(serverkey, c0×32 ‖ 11×32 ‖
  22×32)`, so dashd-net's tests stand alone.

## D-rows

- **D13, D14, D17:** accurate. D17's test counts (16 and 4) are correct; for its refusal wording
  see n3.
- **D15:** real. Byte-equal reads and writes, verified against the codec's canonical decoder.
  The only deviation is the creation mode (n4).
- **D16:** real, and Core's default. For its logging text see n5.
- **D18:** real and complete as far as I can see. Non-UTF-8 log bytes are the only parity gap,
  and the zeroing is not Core-observable.
- **D19:** real. dashd-node takes `default-features = false, features = ["keys"]`; the readers
  that need dash-dml (`evodb`, `netstate::governance`, `detect`) and their tests are gated.
  - The guarantee is per invocation: a `cargo build --workspace` still unifies `full` into
    dash-coreimport through node-l1, dash-oracle-sys, fuzz and others (resolver 2).
  - That is fine for the tripwire, which reads `cargo tree -p rdashd -e normal`.
