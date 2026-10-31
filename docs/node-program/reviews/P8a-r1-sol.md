ACCEPT-WITH-NITS

## Blocker

None.

## Major

None.

## Minor

### M1. Flags keeps an un-narrowed -timeout value

- Rust: dashd-node/src/init/flags.rs:105-107
- Core: src/init.cpp:1365-1368
- Problem: Core assigns GetIntArg to the int nConnectTimeout field before applying the positive check. The Rust internal_flags path checks the raw i64 and stores it, while the actual NetConfig path separately narrows it in dashd-node/src/p2p/mod.rs:170-181. This leaves the exported Flags.connect_timeout_ms state different from Core for values outside the signed 32-bit range.
- Failing scenario: Start with -timeout=2147483648. Core's narrowed int is negative and falls back to 5000 ms; internal_flags reports 2147483648 in Flags.connect_timeout_ms. The live socket timeout currently remains correct because Net::new uses the separate helper, so this is an internal-state/API parity issue rather than a currently observed wire failure.
- Fix: Centralize the Core-compatible i64 -> i32 -> positive/default conversion and use it both for Flags.connect_timeout_ms and NetConfig.connect_timeout; add an overflow boundary assertion to the flags tests.

### M2. Public proxy-table setters do not enforce Core's validity contract

- Rust: dash-p2p/src/proxy.rs:87-98 (also exposed through dash-p2p/src/netcore.rs:934-942)
- Core: src/netbase.cpp:684-708
- Problem: Core's SetProxy and SetNameProxy reject an invalid Proxy and leave the table unchanged. Rust's public setters unconditionally store the value. The CLI path validates its lookup result first, so the normal P8a startup path is unaffected, but library callers can create proxy state Core would never expose.
- Failing scenario: A caller invokes NetCore::set_proxy(Network::Ipv4, Proxy { addr: ProxyAddr::Service(Service::default()), .. }) (or supplies an overlong unix: path). Core returns false and retains the previous entry; Rust stores Some and later proxy(), is_proxy(), or getnetworkinfo can observe the invalid entry.
- Fix: Add Proxy::is_valid matching Core's service/UNIX checks, make the setters return success/failure (or keep them private), and reject invalid values before storing them.

## Nit

### N1. IPv6 SOCKS5 reply bytes are not exercised

- Rust: production handling dash-p2p/src/proxy.rs:314-318; tests dash-p2p/src/proxy_tests.rs:101-112 and dash-p2p/src/proxy_open_tests.rs:81-112
- Core: src/netbase.cpp:476-480
- Problem: The production branch correctly consumes 16 bytes for ATYP_IPV6, but the unit test named ipv6_destination_goes_as_text only uses an IPv6-looking destination string and sends an IPv4 reply. The open-test mock also always emits an IPv4 reply. A mutant that reads only four bytes for an IPv6 reply would pass the current suite.
- Failing scenario: A SOCKS5 proxy returns 05 00 00 04, a 16-byte bind address, and a two-byte port. The current tests never send this transcript, so a four-byte read mutant remains green.
- Fix: Add a scripted local mock (no host IPv6 required) that emits an IPv6 reply with all 16 address bytes and the port, then assert the connection succeeds and the transcript is consumed.

### N2. The process tests do not prove the successful numeric-only -dns=0 path

- Rust: resolver/init path dashd-node/src/p2p/mod.rs:538-547, 590-597; current process cases rdashd/tests/dashd_v24.rs:81-95
- Core: numeric-only lookup uses AI_NUMERICHOST in src/netbase.cpp:44-74; the option call sites are src/init.cpp:1900-1916, 1947-1953, 1975-1980
- Problem: Existing -dns=0 process cases use deliberately malformed names (abc..abc, xyz..xyz, foo) and therefore fail even if a mutant accidentally enables DNS. The unit resolver test covers the boolean passed to the resolver, but the end-to-end init tests do not distinguish numeric-only lookup from a real DNS lookup.
- Failing scenario: Mutate the init path to pass allow_lookup=true. Existing malformed-name cases still fail; a valid-looking hostname such as localhost would be accepted by the mutant but must be rejected by Core under -dns=0.
- Fix: Add deterministic init cases such as -dns=0 -proxy=localhost, -dns=0 -onion=localhost, and -dns=0 -externalip=localhost, asserting Core's invalid-address errors. Keep numeric literals as positive controls.

## Checked and correct

- SOCKS5 method negotiation, USER_PASS framing, always-domain CONNECT requests, reply parsing, all SOCKS5 error strings/log branches, per-read 20-second windows, and interrupt handling match Core.
- The -proxyrandomize credential counter advances only after the proxy TCP/UNIX connection succeeds, matching Core's netbase.cpp:737-748; a down proxy does not consume a credential.
- Per-network proxy and name-proxy assignment, -onion override, -onion=0/-noonion, -proxy=0/-noproxy, -onlynet, -cjdnsreachable's IsArgSet behavior, -externalip, -dns, and the live connection timeout path match Core.
- Reachability state and getnetworkinfo's five-network ordering, limited/reachable, proxy text/randomize fields, and localaddresses match the v24 reference in the differential RPC tests.
- Declared D6 (Tokio cannot distinguish immediate connect failure from post-wait failure, so both use Core's “failed after wait” text) and D7 (per-NetCore credential counter, non-reset interrupt flag, and write_all) are documented implementation details with no peer-visible effect.
- No real Tor/I2P/public proxy was used; all proxy checks use local mocks.

Commands run:

    git -C ~/workspace/rdc-wt/p8 diff 497d3ca11..81795bc90
    cargo test -p dash-p2p --all-features
    cargo test -p dashd-node --all-features
    cargo test -p rdashd --test proxy_diff
    cargo test -p rdashd --test dashd_v24 --test rpc_diff

All commands used CARGO_TARGET_DIR=/work/scratch/p8a-r1-sol/target, CARGO_INCREMENTAL=0, and at most 8 jobs; the suites passed (dashd_v24 8, rpc_diff 6, proxy_diff 3, plus the crate suites).
