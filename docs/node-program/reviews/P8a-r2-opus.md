ACCEPT-WITH-NITS

# P8a r2 review (Opus), tree 50abc7a04

Scope: all of P8a (`git log --first-parent fd0da3b67..50abc7a04`, 13 commits including the Sol r1
fix round), against Dash Core v24.0.0-rc.4 `375c64d2317`. I found no divergence in accepted or
rejected inputs, wire bytes, RPC output or init errors. Every finding below is a test gap (a
mutant that survives) or a documentation nit.

## Major

None.

## Minor

None.

## Nit

### N1. A SOCKS5 domain-name bound address of the wrong length passes every test

- Rust: dash-p2p/src/proxy.rs:337-345 (the `ATYP_DOMAINNAME` arm, `let n = usize::from(ret3[0])`)
- Core: src/netbase.cpp:481-490
- Problem: the code is right: it reads one length byte, then `n` bytes, then the port. But the
  only successful domain-name reply in the tests (`randomized_credentials_use_rfc1929`: `03 03
  'a' 'b' 'c' 00 01`) checks only the boolean result. It never checks where the stream is left.
  The empty-name case (`request_failures`, `03 00 00 00`) can't tell `n` from 0.
- Failing scenario: env-gated mutant `n = 0` (A6MUT=1 in the reflink copy). It **survives**
  `cargo test -p dash-p2p --lib` (155 passed). `proxy_diff`'s mock always answers REP 5, so it
  never reaches this read. In production, a proxy that answers with a domain-name bound address
  (several non-Tor SOCKS5 servers do) would leave `abc`'s tail and the port in front of the
  peer's first message. The P2P stream would then be misframed, where Core reads it cleanly.
- Fix: the same pattern as `an_ipv6_reply_is_read_whole`. Reply `05 00 00 03 03 'a' 'b' 'c'
  <port>` followed by `PEER`, and assert the stream is left at `PEER`. An IPv4-reply version of
  this test would also pin the 4-byte read on the success path; the truncated-reply cases
  already pin it from the failure side.

### N2. The UNIX-socket proxy's `proxy_connection_failed` isn't tested

- Rust: dash-p2p/src/netcore.rs:1175-1177 (`*proxy_failed = true` when `connect_unix` fails)
- Core: src/netbase.cpp:737-742 (`proxy.Connect()` → `proxy_connection_failed = true`), net.cpp:474-478
- Problem: `proxy_failures_and_the_attempt_rule` covers the rule for a TCP proxy only.
  `unix_socket_proxy` covers only a reachable socket.
- Failing scenario: env-gated mutant that drops the assignment in the UNIX branch (A6MUT=2). It
  **survives** the dash-p2p lib suite (155 passed), and no rdashd test has a dead `unix:` proxy.
  With that mutant, `-proxy=unix:/missing.sock` would record an AddrMan `Attempt` (and count a
  failure) for every peer the dial loop picks. Core records none. After a few rounds that changes
  `IsTerrible` and `GetChance`, and so which peers are chosen once the proxy comes back.
- Fix: in `proxy_failures_and_the_attempt_rule`, add a case with `ProxyAddr::Unix` pointing at a
  path nothing listens on, and assert `last_try != NOW`. While there, also assert that it takes no
  `-proxyrandomize` counter value (the `auth()` call order in that branch).

### N3. The CJDNS reachability passed to the core's `-whitelist` is untested

- Rust: dashd-node/src/p2p/mod.rs:166 (`whitelist(args, cjdns_reachable(args)?)`), :176-180
- Core: src/init.cpp:2736-2741 (TryParse → LookupSubNet → `MaybeFlipIPv6toCJDNS`, netbase.cpp:773)
  at step 12, after init.cpp:1880-1887
- Problem: P8a changed this argument from a constant `false` to `cjdns_reachable(args)`. The logic
  is right: `IsArgSet("-cjdnsreachable")`, and `-onlynet` absent or naming `cjdns`, which is what
  step 6 leaves in `g_reachable_nets`. But nothing checks the subnets the core actually holds.
  The step-12 reparse (`mod.rs:687`) is only used for its error.
- Failing scenario: env-gated mutant that forces the argument to `false` (A6MUT=3). It
  **survives** `cargo test -p dashd-node --lib` (156 passed, 3 ignored). With
  `-cjdnsreachable -whitelist=noban@fc00::/8`, Core flips the subnet to CJDNS. An inbound peer
  from fc00::1, flipped to CJDNS on accept, gets `noban` in Core but none under the mutant, which
  changes its eviction protection, banning and getpeerinfo `permissions`.
- Fix: a `net_config` unit test with `-cjdnsreachable -whitelist=fc00::/8` asserting the stored
  subnet's network is CJDNS. Add a negative control without `-cjdnsreachable` (IPv6), and one with
  `-onlynet=ipv4` (IPv6, since CJDNS is then unreachable).

### N4. D10's evidence counts are stale (I confirm the validator's deferred nit)

- Rust: docs/node-program/designs/P8-notes.md:53 (D10)
- Problem: D10 says `proxy_tests` has 14 tests, `dashd_v24` has 15 more init cases, and
  `getnetworkinfo_matches_dashd_v24` has 8 more configurations. After the fix round the tree has
  16 `proxy::tests`, 21 new `dashd_v24` cases (13 + the 6 `localhost`/control cases + the 2
  step-12 cases) and 9 new configurations (the array went from 8 to 17). D12b describes the
  additions but never corrects the counts.
- Fix: update D10's three numbers, or have D10 point to D12b for the totals.

### N5. Init drops the setters' bool with no marker (I confirm the validator's deferred nit)

- Rust: dashd-node/src/p2p/mod.rs:549, :551, :569
- Core: src/init.cpp:1925-1928, :1961 (Core ignores `SetProxy`'s and `SetNameProxy`'s return too)
- Problem: the behaviour is the same as Core's, and the value can't be false, because `proxy_for`
  and the `unix:` check have already validated the proxy. But since M2 made `set_proxy` and
  `set_name_proxy` return the validity bool, these three call sites drop it silently. A future
  edit to `proxy_for` that let an invalid proxy through would then fail quietly: the entry would
  stay unset with no init error.
- Fix: `let _ = …;` with a `// Core ignores SetProxy's result; proxy_for has validated it`
  comment, or a `debug_assert!(core.set_proxy(…))`.

## Checked and correct

- **SOCKS5 bytes** (proxy.rs:234-360 vs netbase.cpp:370-507): the method offer `05 01 00` or
  `05 02 00 02`; the RFC 1929 message `01 ulen user plen pass`, length-checked after the method
  reply as Core does; `CONNECT` is always `ATYP` 03 with `ToStringAddr` text and a big-endian port;
  `Hostname too long` comes before any send; USER_PASS without credentials offered is
  "wrong authentication method 02".
- **SOCKS5 replies**: VER, REP and RSV checks in Core's order; IPv4 4 bytes, IPv6 16 bytes,
  domain 1+n bytes, then a 2-byte port; an unknown ATYP is malformed; the `REP` texts
  (`Socks5ErrorString`, 0x07 = "protocol error"); a timeout on the request reply is silent; log
  levels (LogPrintf → info, LogError → error, NET/PROXY → debug targets).
- **`InterruptibleRecv`**: a per-read 20 s window; waits of at most `MAX_WAIT_FOR_IO`; the
  interrupt is checked after every read or wait, including a successful one; a 0-byte read is
  OK; nothing is over-read (reads are bounded by `buf[got..]`, and tokio has no user-space
  buffer).
- **`ConnectThroughProxy` / `Proxy::Connect`** (netcore.rs:1139-1223 vs netbase.cpp:652-682,
  :732-758): the proxy is dialled as a manual connection for logging; the UNIX path is stripped
  of `unix:` and logged as "connect() to %s failed: %s"; the credential counter advances only
  after the proxy connects; credentials are `"%i"` text used as both user and password; a UNIX
  socket's `addr_bind` is invalid, so getpeerinfo omits `addrbind` (rpc.rs:429), as Core's
  `GetBindAddress` + `IsValid` does.
- **`ConnectNode`** (netcore.rs:1037-1080 vs net.cpp:401-488): the proxy is chosen by
  `GetNetwork()` (loopback and RFC 1918 without `-allowprivatenet` are `NET_UNROUTABLE`, so they
  dial directly); `Attempt` is skipped only when the proxy itself fails; with a name proxy, names
  are looked up only numerically (`fNameLookup && !HaveNameProxy()`); an empty result goes to the
  name proxy with `SplitHostPort(dest, GetDefaultPort(dest))` and no `Attempt`, and the peer keeps
  the invalid `CAddress` plus the name; an Onion address without an onion proxy is Core's failed
  `CreateSock`; a `-proxy=<x>.onion` proxy fails `CreateSock` and sets `proxy_failed`, as Core does.
- **DNS seeds** under a name proxy: `AddAddrFetch(seed)` comes before the `SetInternal` check, as
  at net.cpp:2760-2762, and is equal to dashd byte for byte (`the_dns_seed_goes_through_the_name_proxy`).
- **`Proxy::IsValid`** (nb.h:73-77): `CService::IsValid` is the address only (port 0 is
  rejected earlier by `check_socket_args`); `IsUnixSocketPath` is `size + 1 <= sizeof sun_path`
  (108 on Linux, 104 on the BSDs and macOS). The setters refuse an invalid proxy and keep the
  previous entry. `IsProxy` matches `::` for unset and UNIX entries across all `NET_MAX`
  entries, as Core's `CNetAddr` comparison of the default `CService` does.
- **Step 6** (mod.rs:487-591 vs init.cpp:1870-1981): the order is `-onlynet` (RemoveAll, then
  Add, using `ParseNetwork` with the `tor` warning), the CJDNS error and removal, the explicit
  `-dnsseed=1` check, `-proxy` (`""` and `"0"` mean none, then `unix:` or `Lookup(…, 9050,
  fNameLookup)` → IPv4, IPv6, CJDNS and the name proxy, plus the onion default),
  `onlynet_used_with_onion` taken after `-proxy`, `-onion` (`0` clears it, with its error), the
  onion proxy or the `-listenonion` error plus Onion's removal, then `-externalip` at
  `GetListenPort()` with `LOCAL_MANUAL`. All error texts are verbatim. I2P is still reachable
  during `-externalip`, as in Core, so `-externalip=<i2p>` is added in both.
- **Step 12** (mod.rs:688-708 vs init.cpp:2746-2775): the `-dnsseed` line checks
  `IsArgSet(-connect)`, `IsArgSet(-dnsseed)`, `GetBoolArg` and `IsArgSet(-proxy)`, in that order,
  after the whitelist; then the `-onlynet=i2p` error and I2P's removal before the core starts.
- **`-cjdnsreachable`**: tested with `IsArgSet` (`=0` still counts); `-noonlynet` gives no
  reachable networks, as with Core's `RemoveAll` plus an empty `GetArgs`.
- **`-dns`**: `fNameLookup` is read at `Net::new`, which is harmless because Core uses it nowhere
  before init.cpp:1901. It applies to `-proxy`, `-onion` and `-externalip` lookups and to
  `ConnectNode`, and DNS seeds always look up.
- **`-timeout`**: one `connect_timeout_ms` (`as i32`, then `<= 0` → 5000) feeds both `Flags` and
  `NetConfig` (Sol M1 closed); it is used for TCP, the proxy dial and UNIX connects.
- **getnetworkinfo `networks`**: five networks in index order; `limited` and `reachable` come from
  runtime state; `proxy` is `ToStringAddrPort` (`[::1]:19050`) or the `unix:` path, else `""`;
  `proxy_randomize_credentials` is false without a proxy (Core's default `Proxy`).
- **`-bind`/`-whitebind` tags and `-discover`** (BL-327, consumed unchanged): `rfind('=')`, only
  `onion`, `Lookup(…, OnionServiceTargetPort, false)`, "Cannot resolve -bind address";
  `Discover` only with `bind_on_any`, at `LOCAL_IF` on `GetListenPort`.
- **Known and ruled items** (DEC-528/BL-436, BL-441) were not re-raised. D6 and D7 are
  documented and not peer-observable.

## Commands run

All in `/work/scratch/p8a-r2-opus/tree` with `CARGO_TARGET_DIR=/work/scratch/p8a-r2-opus/target`, `-j8`:

    cargo test -p dash-p2p -p dashd-net -p dashd-node --lib      # 175 + 13 + 156 passed (3 ignored)
    cargo test -p dash-p2p --lib proxy                           # 24 passed
    cargo test -p rdashd --test proxy_diff --test dashd_v24      # 3 + 8 passed (rc.4 dashd found in ~/.rust-dashcore-test)
    cargo test -p rdashd --test rpc_diff getnetworkinfo          # 1 passed

Mutants were env-gated (`A6MUT=n`) in the reflink copy `/work/scratch/p8a-r2-opus/mut` (target
`mut-target`). The live tree was never patched. A6MUT=0 is the baseline. 1 (N1), 2 (N2) and 3 (N3)
all survived their crate's full lib suite. No connections were made other than to loopback mock
proxies and the test nodes' own ports below 32768.
