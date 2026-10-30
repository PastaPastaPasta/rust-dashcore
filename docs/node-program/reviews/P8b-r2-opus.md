ACCEPT-WITH-NITS

# P8b r2 review (Opus): round-1 fixes, 8e07509c1..e6217e566

Tree: /work/scratch/p8b-r2-review/wt at 160617313 (detached, read only, `git status` clean after the
review). Core: dash 375c64d2317 (v24.0.0-rc.4). libevent 2.1.12 was read from
`depends/sources/libevent-2.1.12-stable.tar.gz` with `tar -xzOf`. Abbreviations: tc =
src/torcontrol.cpp, init = src/init.cpp, se = src/util/strencodings.h.

## Runs

These used CARGO_TARGET_DIR=/work/scratch/p8b-r2-review/target, TMPDIR=/work/tmp and load
average 30–40.

- `cargo test -p dashd-net torcontrol`: 21 lib tests passed. This filter skips every test in
  `tests/torcontrol.rs`, whose names don't contain "torcontrol".
- `cargo test -p dashd-net --test torcontrol`: 5 passed, including
  `a_line_of_the_maximum_length_is_kept`.
- `cargo test -p dash-coreimport --no-default-features --features keys --test keyfiles`: 8
  passed.
- `cargo test -p dashd-node onion_allowed_by_onlynet`: 1 passed.
- `cargo test -p rdashd --test tor_diff`, after `setup-dashd.py`, against the cached rc.4 dashd:
  3 passed in 28.75 s. That includes `tor_control_matches_dashd_v24` with all 11 cases.

D17's counts (21 and 5) are correct.

## Round-1 findings

| r1 finding | Fix | Verdict |
|---|---|---|
| Sol m2 / Opus m1: `-onlynet` arm | `NodeTorFiles::new` factored out (tor.rs:32-40). Unit test tor.rs:125-136 covers unset, `-noonlynet`, `ipv4`, and `ipv4`+`onion`. tor_diff adds `onlynet-ipv4` and `onlynet-onion` and asserts dashd's `reachable` per case, and that `proxy` is Tor's SOCKS port for `onlynet-ipv4`. | Fixed. `-noonlynet` gives `IsArgSet` true with no values, so onion is not allowed (tc:399-403). An always-true mutant now fails both the unit test and tor_diff (`onlynet-ipv4` must be `reachable:false`). For the `tor` alias, see n1. |
| Sol n1 / Opus m2: `-proxy -listen=1`, `-noonion`, `-onion=` | Three tor_diff cases, compared with dashd: sessions, logs, the whole onion `networks` object, local addresses and the key file. `noonion` is asserted unreachable, and `proxy` is asserted to end on 127.0.0.1:19050. | Fixed. This matches init:1937-1973: `-noonion` gives `GetArg` "0", so the proxy is cleared and `Remove(NET_ONION)`; there is no GETINFO (tc:456), ADD_ONION is still sent, and `AddLocal` refuses the address. `-onion=` gives `GetArg` "", so GETINFO is sent. A `!is_arg_set("-onion")` mutant now fails `onion-empty`. |
| Sol n3 / Opus n6: self-referential SAFECOOKIE | `safecookie_hashes_are_a_fixed_vector` (torcontrol_tests.rs:626). | Fixed and independent. I recomputed both literals with Python `hmac` (`-I`, constants copied from tc:50-52), and they match. The nonce-swapped variant gives `9633fc5a…`, so M6 (nonce order) dies. A swap of the keys at the call sites (torcontrol.rs:478/:488) still dies in `safecookie_authentication`, because its server hash uses SERVERKEY and the controller would check it with CLIENTKEY. |
| Sol n4 / Opus n1: parser boundaries | `reply_code` is factored out (torcontrol.rs:168). New tests: `reply_codes_and_the_async_threshold` (`+50`→0, `-12`, `2a0`, 599 sync and 600 async through the handler queue), `\v`/`\f` in `parse_hex_is_cores`, `the_last_auth_line_wins`, `the_service_id_outlives_its_reply`, and `a_line_of_the_maximum_length_is_kept`. | Fixed. `reply_code` is unchanged in behaviour: it takes the first three bytes, then `starts_with('+')` replaces `s[0] != b'+'`, and every caller has `len ≥ 4`. It equals `ToIntegral<int>` (se:180-190): `from_chars` refuses a leading space or `+` and any trailing byte, and so does `i32::from_str` once `+` is filtered. 599 runs PROTOCOLINFO's handler and pops it; 600 leaves it, so the next reply authenticates (tc:97-108). The test pins exactly that. The last AUTH line wins as at tc:550-552. `service_id` survives as at tc:418-433, with the write before `AddLocal` (tc:435-440). For how strong the 100,000-byte test is, see n3. |
| Sol n5 / Opus n2: lone `"` | Two rows in `the_onion_proxy_from_getinfo` (torcontrol_tests.rs:409-412). | Fixed. Core keeps `"` as `socks_location` (tc:359 needs `size() >= 2`). `LookupNumeric("\"")` is invalid, so Core falls back to 127.0.0.1:9050 (tc:384-390), and a later 127.0.0.1 token replaces it. The test comment says this, which is more precise than Sol's "ignores". A mutant without the guard slices `[1..0]` and panics on both rows. |
| Sol n2 / Opus n3: errno classifier | `connect_start` is pure (torcontrol.rs:868). `connect_errors_are_classified_as_libevent_does` (torcontrol_tests.rs:554). | Fixed. It matches libevent: `evutil_socket_connect_` (evutil.c:556-562) returns 0 for `EVUTIL_ERR_CONNECT_RETRIABLE`, 2 for `_REFUSED` and -1 otherwise. On Unix those macros are `EINTR‖EINPROGRESS` and `ECONNREFUSED` (util-internal.h:122-130). On Windows they are `WSAEWOULDBLOCK‖WSAEINTR‖WSAEINPROGRESS‖WSAEINVAL` and `WSAECONNREFUSED` (util-internal.h:142-152). Rust tests in-progress first, then `ErrorKind::ConnectionRefused`, which std maps from exactly `ECONNREFUSED`/`WSAECONNREFUSED`. The two sets are disjoint, so the reordering against the old `start_connect` changes nothing. The test covers EINPROGRESS, EINTR, ECONNREFUSED, and four failure errnos including ENETUNREACH and EACCES. EAGAIN is untested, but Rust and libevent agree on it (Failed and -1). |
| Sol n6: unchecked close | D21. | The decline is accurate (below). |
| Opus n4, n5: D15 mode, D16 logging | D15 and D16 reworded. | Fixed. Both now say what Core does. |
| r1 L1–L3: key tests keys-only | e28f61844. | Fixed. The key and conf section of the old `flatfiles.rs` (8 tests) moved verbatim into `keyfiles.rs` with its own `data()` helper, apart from the header. It runs and passes under `--no-default-features --features keys`. `flatfiles` keeps `required-features = ["full"]`. The intra-doc links to cfg'd-out items became code spans. |

## DEC-556 (Sol m1 declined): the conclusion holds

Here is the libevent 2.1.12 path for an immediate refusal:

1. `bufferevent_socket_connect` gets r == 2 from `evutil_socket_connect_` and goes to the `else`
   branch.
2. The `else` branch sets `result = 0` and calls
   `bufferevent_run_eventcb_(bev, BEV_EVENT_ERROR, BEV_OPT_DEFER_CALLBACKS)`
   (bufferevent_sock.c:439-443).
3. `bufferevent_run_eventcb_` tests `(p->options|options) & BEV_OPT_DEFER_CALLBACKS`
   (bufferevent.c:291-294). The flag is passed as an argument, so the call is deferred even
   though Core creates the bufferevent with only `BEV_OPT_CLOSE_ON_FREE` (tc:159).

So `Connect` returns true and logs nothing (tc:169). The callback runs only from
`event_base_dispatch`, which `TorControlThread` calls after the `TorController` constructor
returns (tc:665-667). By then the constructor has read the key and logged "Reading cached
private key" (tc:328-333). rdashd's order is the same. D20's citations are correct.

## D21: accurate

`WriteBinaryFile` fails on a short `fwrite` or a non-zero `fclose` (readwritefile.cpp:37-49).

- A key is a few hundred bytes, so it sits in stdio's buffer. On a full disk the error surfaces
  in `fclose`'s flush. rdashd's unbuffered `write_all` (tor.rs:88) gets the same ENOSPC from
  `write(2)` and logs the same "Error writing service private key" line.
- The only remaining case is an error that `close(2)` alone reports, such as NFS deferred write
  errors. std's `Drop for File` ignores those, as D21 says.
- The claim "needs `unsafe` or a new dependency" is true: std has no stable checked close.
  dashd-node already calls `libc` under `unsafe` for the datadir lock (datadir.rs:180-186), so
  "not worth `unsafe`" is a judgement, not a constraint. That is fine as written.

## New regressions

None found. The refactors (`reply_code`, `connect_start`, `NodeTorFiles::new`) are equivalent to
the code they replace, and the doc-link changes in dash-coreimport only touch docs.

## Major

None.

## Minor

None.

## Nit

### n1. The `-onlynet=tor` alias is still untested on the controller's path

- **Rust:** dashd-node/src/p2p/tor.rs:44-50 (`parse_network` per GETINFO reply) and the test
  table at :127-132.
- **Core:** tc:397-403 with netbase.cpp:104-107 (`ParseNetwork("tor")` logs the deprecation
  warning and returns `NET_ONION`).
- **Problem:** Opus r1 m1 suggested `-onlynet=tor`; the fix added `-onlynet=onion`. D14 says
  "-onlynet is parsed again at each GETINFO reply, with ParseNetwork's `tor` line" and calls each
  quirk "pinned by a unit test or tor_diff". Neither covers it:
  - The unit table has no `tor` row.
  - A `-onlynet=tor` tor_diff case would compare reachability, but `tor_lines` keeps only `tor:`
    and `[tor]` lines, so it would drop the `Warning: net name 'tor'…` line that Core logs again
    at each GETINFO reply.
- **Failing scenario:** Make `onion_allowed_by_onlynet` compare the raw string with `"onion"`,
  or call a non-logging parser. With `-onlynet=tor`, Core marks Onion reachable and logs the
  warning once more after GETINFO. The mutant leaves Onion unreachable, or drops the extra line.
  No current test fails either way.
- **Fix:** Add `(&["-onlynet=tor"][..], true)` to the unit table, and assert the logger saw the
  warning line. Alternatively, reword D14.

### n2. tor_diff's module doc lists only the original cases

- **Rust:** rdashd/tests/tor_diff.rs:1-6.
- **Core:** n/a.
- **Problem:** The header still lists the six round-1 cases. The five new ones (`onlynet-ipv4`,
  `onlynet-onion`, `proxy`, `noonion`, `onion-empty`) are named only in D17 and in comments in
  the table.
- **Failing scenario:** None at runtime. A reader of the file underestimates what is compared
  with dashd.

### n3. The 100,000-byte boundary test kills the `>=` mutant only if the 500 ms sleep wins

- **Rust:** dashd-net/tests/torcontrol.rs:233-236. The check is in the task's read loop
  (`MAX_LINE_LENGTH`, Core tc:116).
- **Problem:** The test can only distinguish `>` from `>=` if the controller has buffered all
  100,000 bytes before the CRLF arrives. Under heavy load, the reads may lag past the 500 ms
  sleep and take the CRLF in the same batch. The buffer left after `take_lines` is then small,
  and the mutant passes that run. The correct code passes either way, so the test cannot flake.
  It can only lose mutant sensitivity.
- **Failing scenario:** At load average 150 or more, an `M3` mutant can pass
  `a_line_of_the_maximum_length_is_kept` intermittently.
- **Fix:** Optional. A sans-io unit test that feeds exactly 100,000 unterminated bytes to the
  buffer check would make it deterministic, if the check can be reached without the socket.

## Not run

I did not run a mutation pass this round. My verdicts that the r1 mutants (M1–M8, M11, the
`-onlynet` always-true mutant, and the `-onion` `is_arg_set` mutant) are now killed come from
reading the new assertions against each mutant, as described in the table above.
