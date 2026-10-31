# P2a-net round 1 — Sol review

Reviewed branch `node/p2a-net`, pinned head `ae664b818e2ce32373ba2f1e831fbacd72a0856a`; SB6 `742570e55` and P2a-core `77a5341a3` are the dependency heads supplied by the brief.

Scope: P2a rev3 phases D/F, SB0 rev6.1 split, PLAN P2a-net, and implementation D1–D15. The branch is read-only. Tests use `nice -n 10`, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=`, and mode-0700 `TMPDIR=/work/scratch/p2a-net-r1-sol/tmp`. The intended network scope is loopback and simulation; the baseline suite was run before the loopback test isolation gap below was recognized, and its actual egress was not traced. PLAN §7 is handled by codenames only; no private directory is read.

## Findings

### R1-M1 — major: `stop_threads` cannot drain an in-flight periodic file dump

**Location:** `dashd-net/src/connman.rs:151-160,178-193`; composed with `dash-p2p/src/netcore.rs:965-1004` and `dashd-net/src/store.rs:33-38`.

`interrupt()` aborts and discards the Tokio task handles, while `stop_threads()` joins only the OS dial threads. Once the periodic task has launched `spawn_blocking(core.dump)`, aborting its parent does not cancel or join that blocking write. The documented host sequence (`interrupt → stop_threads → stop processor → core.shutdown`) can therefore finish its final dump while an older periodic snapshot remains in flight; that older snapshot can subsequently rename over the final `peers.dat`. There is no remaining handle through which the host can drain it on a shared runtime. Aborted listener tasks are also discarded without waiting for their teardown.

**Executed proof:** scratch-only `periodic_dump_outlives_stop_and_overwrites_final_dump` blocks the periodic peers save, adds an address, runs the documented stop and final shutdown, then releases the old save. It observed `writes at stop=3`, `after releasing old dump=4`, and a changed final image. This is a lifecycle failure, not a torn-file-format failure: atomic rename does not preserve snapshot order. Core's socket threads are joined in `net.cpp:4168-4185`; its dump task belongs to the host scheduler, which `init.cpp:354` stops separately. Do not infer that Core's `StopNodes` alone drains that scheduler; this implementation owns the task but offers no equivalent drain.

**Fix:** retain and await the accept/scheduler handles and the active blocking dump before completing the network stop/final flush. Use cooperative scheduler cancellation and an explicit join, not abort-and-forget. Add a synchronized slow-store test covering a dump already in flight at shutdown and immediate listener-port reuse.

### R1-M2 — major: a normal peer removal shortens Core's connection-driver sleeps

**Location:** `dashd-net/src/driver.rs:85-86` calling `dash-p2p/src/dial.rs:262-293`; wake producers at `dash-p2p/src/netcore.rs:939-944,1119`.

`dial_wait(Some(duration))` treats any `dial_wake` notification as successful completion of the whole sleep. That notification is also sent on peer removal and `setnetworkactive`, not just interrupt. The driver consequently skips the remainder of its 500 ms opencon sleep, 60 s addcon retry, feeler jitter, or 11 s DNS-seed wait. The DNS state machine has already deducted the full requested wait before the notification, so repeated early wakes also advance its logical wait budget faster than real time. Core's `CThreadInterrupt::sleep_for` (`util/threadinterrupt.cpp:32-35`) waits for the deadline unless its interrupt flag is set; peer deletion releases grants but does not finish these sleeps.

**Executed proof:** scratch-only `peer_removal_finishes_an_eleven_second_sleep` closes an in-memory peer 150 ms into a DNS-loop sleep. The requested 11 s wait returned successfully after 244 ms with `interrupted=false`. No real-network connections were used. `SimExec::wait(Some(d))` (`dashd-net/src/sim.rs:248-253`) simply advances the virtual clock by all of `d`, so the advertised driver/sim tests cannot detect this runtime divergence.

**Fix:** ask SB6 for distinct interrupt and state/slot notifications, or preserve an absolute monotonic deadline across non-interrupt wakes. All fixed-duration sleeps must retain Core's duration. Add a runtime test that removes a peer during an addcon/DNS sleep and checks both elapsed time and the next plan.

### R1-M3 — major: `-dns=0` also disables the independent DNS-seed path

**Location:** `dashd-net/src/resolver.rs:15-17,46-54`, `dashd-net/src/driver.rs:89-90`, and `dash-p2p/src/dial.rs:946-957`.

D12 instructs the host to construct `SystemResolver.allow_lookup` from `-dns`, and the same resolver serves both named connection targets and DNS seeds. With `-dns=0 -dnsseed=1`, `resolve_seed` asks that resolver for `x1.<seed>`; it gets an empty answer without a lookup, queues the bare seed as ADDR_FETCH, and that name also fails. A fresh node falls through to fixed seeds instead of learning the DNS-seed addresses. Core deliberately calls `LookupHost(host, 32, true)` in `ThreadDNSAddressSeed` (`net.cpp:2775`); `-dns` controls `fNameLookup` in `ConnectNode`, not this explicit-true seed lookup. `init.cpp` does not soft-set `-dnsseed=0` merely because `-dns=0`.

**Fix:** extend SB6's resolver interface to carry lookup permission per call, or provide separate name-target and seed resolvers. Keep DNS seeds enabled when explicitly/default enabled regardless of `-dns`. Verify the combined options with a recording resolver, without public DNS.

### R1-M4 — major: the “loopback” seednode test permits real-network dialing

**Location:** `dashd-net/tests/loopback.rs:75-100,130-151,259-279`; automatic threads started at `dashd-net/src/connman.rs:137-149`.

`seednode_addr_fetch` inserts 100 routable IPv4 addresses with `NODE_NETWORK` services into the seed provider's live AddrMan. Both nodes run the normal automatic opencon driver; the helper disables DNS seeding, but does not restrict dialing to loopback. The provider can dial its fixtures and the fetching node can dial learned fixtures. Port-0 loopback listeners do not constrain outbound destinations. This makes the supposed loopback suite capable of contacting real peers and lets external routing/timing affect a local test.

**Executed proof:** reran only `seednode_addr_fetch` under `sudo unshare --net` with loopback enabled and no external routes, recording `connect` syscalls. It passed in 4.15 s while attempting `41.1.1.1:9999`, `48.1.1.1:9999`, and `35.3.1.1:9999`; all three returned `ENETUNREACH` inside the isolated namespace. The original 33-test baseline was run before this isolation defect was recognized and without syscall tracing; whether it attempted external connections is unknown. The isolated reproduction made no connections to real networks. Trace: `/work/scratch/p2a-net-r1-sol/loopback-seednode-connect.trace`.

**Fix:** enforce a loopback-only transport boundary in the test harness, or run these tests in a loopback-only network namespace in both local and CI invocations. Preserve the routable AddrMan fixtures required by the relay/intake test while preventing them from becoming external connections. Add an assertion or harness check that rejects any non-loopback destination.

### R1-m1 — minor: numeric resolution with DNS disabled is narrower than Core's

**Location:** `dashd-net/src/resolver.rs:46-50`.

Rust's strict `IpAddr` parser is not Core's `getaddrinfo(AI_NUMERICHOST)`. With `-dns=0`, Core accepts numeric loopback spellings `127.1` and `0x7f000001`, whereas this resolver rejects them. This affects explicit `-connect`/`-addnode` targets without making any DNS query. The scratch resolver probe returned no addresses for either; the same machine's `getaddrinfo` with `AI_NUMERICHOST`, `SOCK_STREAM`, and `IPPROTO_TCP` returned `127.0.0.1` for both.

**Fix:** use a safe system-resolver adapter with Core's actual numeric-only mode, rather than classifying every failed strict parse as a hostname. Cover these alternate numeric spellings and ordinary names in a no-DNS test. This is separate from R1-M3's per-call lookup-policy problem.

### R1-m2 — minor: unused dial grants survive the documented stop sequence

**Location:** `dash-p2p/src/dial.rs:248-250,783-790,825-830`; composed with `dashd-net/src/driver.rs:62-64` and `dash-p2p/src/netcore.rs:974-984`.

The sans-io loop stores grants in `DialState`, so exiting the OS driver on interrupt does not destroy them. For example, addcon acquires a grant even with no added nodes, then stores it through its two-second sleep. Interrupt makes the next tick return `Done`; neither that path nor `shutdown` clears the saved grant. The executed `stop_keeps_the_addcon_loops_unused_grant` probe acquired only 7 of 8 addnode grants after interrupt and shutdown, with zero peers. Pending grants and a planned feeler's `OpenRequest` have the same ownership problem. Core's grant is a thread-local RAII variable that is destroyed when the loop exits.

**Fix:** have SB6 clear owned/pending dial plans and grants after the drivers stop, without racing a live driver, and test that both semaphores return to full capacity on shutdown. This is an inherited dependency issue; P2a-net should request the API fix rather than modify SB6 privately.

### R1-m3 — minor: dump cadence measures from a fixed tick, not from completion

**Location:** `dashd-net/src/connman.rs:151-159` (D9).

Core's `CScheduler::Repeat` runs `f()` and only then schedules another full interval (`scheduler.cpp:102-111`). Tokio `interval_at` maintains a sequence of tick deadlines; `MissedTickBehavior::Delay` only repairs an already-missed tick, and does not rebase every tick on completion of the preceding dump. A dump lasting ten minutes can be followed five minutes later by the next dump here, while Core waits fifteen minutes after it finishes. The comment claiming Core rescheduling is therefore stronger than the implementation.

**Fix:** use a cancellable sleep of the full interval after each awaited dump, or explicitly reset the next deadline on completion. A recording slow store can verify the gap independently of disk load.

### R1-m4 — minor: the approved directory-sync persistence step is missing

**Location:** `dashd-net/src/store.rs:33-38` (D7); approved P2a §4.1 `peers.dat` write contract.

The implementation syncs the temporary file and renames it, then returns without syncing the containing directory. This follows Core's syscall sequence, but P2a §4.1 explicitly adds directory fsync to make the rename durable; D7 does not explain withdrawing that approved requirement. Normal round-trip and inode tests establish replacement, not persistence of the directory entry after power loss.

**Fix:** sync the containing directory after successful replacement on supported platforms, or obtain an explicit manager disposition changing the approved persistence contract. This does not alter the Core-compatible image bytes.

## Core comparison and coverage assessment

- **Connection selection:** compared SB6's ladder and selection to Core `ThreadOpenConnections`: anchor priority and pop-from-back semantics, 8 full-relay then 2 block-relay peers, stale-tip extra before periodic extra block-relay before feelers, preferred-network selection and onion-only fallback, netgroup exclusions, regtest/devnet's skipped *accounting* as well as group rule, 100 attempts and the 30/50 retry/port thresholds, desirable services, count-failure threshold, and v2 service intersection. The inspected selection logic matches these rules. The runtime wait divergence is R1-M2.
- **Manual/seed drivers:** `-connect` uses MANUAL without grants and bypasses automatic addr-fetch/fixed-seed work; `-connect=0` returns `Done`. Addcon uses its separate grants and preserves the snapshot/500 ms/regrant/60-or-2-second sequence. DNS steps implement empty/forced immediate seeding, 11 s versus 5 min waits in 11 s pieces, two successfully connected non-probe full-outbound peers as the early-stop condition, inactivity waits, 32 answers, source and 3–7-day ages. Fixed seeds are filtered to reachable networks with empty AddrMan data and added immediately only when other configured sources are absent, otherwise after the strict 60 s threshold. Findings concern the actual wait and resolver adapter, not the abstract ladder.
- **Startup and RPCs:** A1 holds an outbound grant for `addconnection`, including failed opens; onetry has none as Core. A2 leaves anchors untouched on fatal bind failure and calls `start_addresses` before spawning accepts. Explicit bind/whitebind/onion failures and the default IPv4 bind are fatal; only default IPv6 failure is tolerated. NodeStats maps the exposed Core fields and skips peers marked for disconnect. The test configs are directly supplied `NetConfig`s; future ND1 argument assembly must still reproduce Core's derived per-type limits and option soft-sets.
- **Relay:** compared `RelayAddress`'s raw service hash, 24-hour address-offset rotation and top-one/two SipHash targets, compatibility and source exclusions, the 1000-element replacement queue, strict trickle/local timer comparisons, duplicate filtering, stored `our_services`, IBD condition, inbound-only/once-per-connection getaddr, network/local-bind/port cache key, 21 h + 0–6 h expiration, and `addr` permission bypass. No additional mismatch was found in these relay functions by source comparison. SB6 intake matches the token-before-filter rule, timestamp refill/reset, processed/rate-limited counts, strict fresh-time threshold, getaddr window, and addr-fetch completion. This source comparison is not a substitute for D14's eventual differential campaigns.
- **Files:** FileStore writes SB6's images without modifying the encoding. Examined the peers V4/compat header and CAddress disk form, magic/payload/SHA256d envelope and parse-before-checksum error order; anchors use the same envelope with a disk-v2 address vector. Start deletes anchors after a read, including failed decode; shutdown writes peers before anchors while peers still exist and then disconnects them. The non-security byte-format checks do not reveal an additional incompatibility. Atomic replacement, cleanup and `.bak` handling pass normal tests; R1-M1 and R1-m4 concern lifecycle/durability, not decoding. File exchange with a live Core process and power-loss tests were not run.
- **Resolver/binds:** empty/NUL and internal results are filtered in `SystemResolver`; standard IP literals and bracketed IPv6 work; overlay name routing is deferred to P8. CJDNS binds convert back to their underlying IPv6 sockaddr and match Core's special handling of `IPV6_V6ONLY`. The D12 `AI_ADDRCONFIG` omission remains a declared parity limitation (it can retain families Core would filter); the missing IPv6 scope id is the supplied backlog item and is not scored here. D15's 50 ms accept-error backoff is a deliberate runtime-scheduling departure, not proven exact equivalence to Core under persistent accept errors. No fault-injection bind/CJDNS host or non-Linux build was available.
- **Loopback tests:** use actual runtime threads, TCP, processor, relay and file store; bind port 0 avoids port-probe races. They verify manual connection/handshake, addconnection capacity, addr-fetch intake/disconnect, a feeler's received version before disappearance, onetry duplication, fatal bind files and anchor reconnect. They do not prove external Core interop, v2 driver fallback, all bind-failure combinations, bind-before-accept timing under a slowed store, in-flight dump drainage, or the specified connect-to-version latency bound. The seednode case actually injects routable NODE_NETWORK addresses while automatic dialing remains enabled; the isolated syscall reproduction confirms R1-M4. Actual egress during the original baseline run was not traced.
- **Sim/model tests:** sim_dial exercises the real `dial_tick` and shared `driver::step` dispatch, but substitutes its own open checks and virtual waits and has no processor. `regtest_churn` manually closes/refills peers; it does not execute P2a-core's 45 s eviction. `extra_peers` permits the final extra slot to be either block-relay or feeler, so it does not independently prove periodic extra block-relay creation. The six relay scenarios establish fanout, eligibility, BIP155 format, self-announcement, basic cache sharing/permission bypass and outbound-getaddr ignoring; they do not assert exact target hashes, timer boundaries, cache expiration/network/port partitioning, queue overwrite, or a per-peer service override differing from config.
- **`connman_sched`:** meaningful as a bounded invariant checker over the actual opencon state machine, rather than a Core differential oracle or a full concurrency model. It checks duplicates/netgroups, automatic-type count limits and unknown/unreachable services; changing flags, clock, closes and held feelers gives useful state transitions. `broken_tables_are_reported` is a useful checker sanity test. Its grants assertion counts connection types, not actual held grants; scripts use only IPv4, no anchors/seeds/manual peers, no processor and no runtime waits. It cannot establish scheduler timing, shutdown/grant cleanup, DNS, relay, or equality of Core transcripts. The supplied 11,436-execution smoke is consistent with this limited role; the local baseline additionally passed its scripted/checker tests. `fuzzctl check` passed (43 targets). No new libFuzzer campaign was run and no existing private corpus was accessed.

## D14 disposition

**The reasoning is sound.** At this head the oracle FFI advertises ABI 7, not the required session ABI 9. SB6 still has a per-core advancing RNG and no per-site draw source, and its documented D32 keeps the session node-leg swap as follow-up work. Pinning only a shuffle seed cannot align ConnMan SipHash seeds, the AddrMan key/permutation/draws and the independently seeded contexts used by Core's overwrite/timer/Bloom sites. An ordinary byte-for-byte campaign before those dependencies land would compare unrelated random realizations. The manager should keep A3, D32, O1c and the three 24 CPU-hour campaigns (plus the joint multi-relay gate) explicitly open; this review neither treats D14 as a defect nor claims the source/sim checks satisfy those future campaigns. **Sol gate failure here is due to R1-M1–M4, independently of D14.**

## Evidence and limitations

- Common reviewer contract, relevant design sections, implementation notes, and `p2a-core-pin-diff` memory read.
- D14's deferred campaigns are assessed as a dependency/evidence limitation, not a defect, per the review brief.
- Core tag resolves to commit `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`; extracted `net.cpp` SHA256 equals the tag's file (`6e94b520d79a0c115c57f4bddbf798239a689b81bbd9f9a84e8398d3e41c1c97`).
- Local `cargo test -p dashd-net --all-features --locked` passed: lib 7, fixed_seeds 2, loopback 7, relay 6, sim_dial 11 (33/33). The initial Cargo package-cache lock cleared normally.
- Verified A1's nonblocking outbound grant and A2's `InitBinds → start_addresses → accept` order in the pinned head. Baseline bind-failure and anchors-round-trip tests pass.
- Four scratch-only probes in `/work/scratch/p2a-net-r1-sol/probes.rs` all reproduced their expected observations: an old periodic dump replaced the final image after stop, an 11 s sleep ended on peer removal, no-DNS alternate numeric spellings were rejected, and one unused addcon grant remained held after shutdown. They link the reviewed branch's built artifacts; source on the reviewed branch was not edited.
- `cargo test -p dash-p2p --test addr_ping --all-features --locked`: 10/10 passed.
- `cargo test -p dash-p2p --lib addrman::tests --all-features --locked`: 37/37 passed, including Core golden trace, load-error classes and anchors serialization.
- `cargo clippy -p dashd-net --all-features --all-targets --locked -- -D warnings` and `cargo fmt --all --check`: passed.
- `python3 -I node-fuzz/fuzzctl.py check`: passed, 43 targets. The supplied libFuzzer smoke was not independently rerun.
- The isolated, syscall-traced rerun of `seednode_addr_fetch` passed 1/1 and demonstrated the external-dial attempts described in R1-M4. Its private namespace had only loopback and no external routes.
- Core's tagged fixed-seed header hash matches the fixture/provenance (`b9199ffaabcf225dbefa68ba496168ed2e8bb9903746a8c388d0019775c1a124`).
- Reviewed HEAD remained `ae664b818e2ce32373ba2f1e831fbacd72a0856a` and its worktree remained clean. This report is an uncommitted file in the integration worktree.
- possible security issue, ask the manager.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
