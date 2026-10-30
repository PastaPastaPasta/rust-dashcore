# O1c independent Sol review, round 1

Status: review complete. Five major findings require changes; one minor finding qualifies the disabled-hook claim.

Scope: Core `node/o1c-oracle-v24.0` at `6efa24f215`, delta from `9c6dd45e0b`; rust-dashcore `node/o1c-oracle` at `17c4fbe62`, delta from `9da1e93bf`. This review follows REVIEW-COMMON.md and the O1c brief. Neither branch is modified. No private material is read; PLAN §7 uses codenames only.

Validation uses `nice -n 10`, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=`, and a mode-0700 `TMPDIR=/work/scratch/o1c-r1-sol/tmp`. The implementer's `/work/scratch/o1c/oracle-cache` is consumed read-only. The task's explicit disk-TMPDIR requirement takes precedence over REVIEW-COMMON's /dev/shm suggestion.

## Findings

### F1 — major: the session ignores its advertised local services

Core `src/test/oracle/oracle_session.cpp:228,270,328`; `src/test/util/setup_common.cpp:428`; `src/net.h:1224,1261`.

`oracle_peer_session_new` accepts `our_services` but never uses it. `TestingSetup` initializes ConnMan with default `Options.nLocalServices = NODE_NONE`; `add_peer` then passes `connman.GetLocalServices()` to the real `InitializeNode`. A caller requesting NODE_NETWORK|NODE_BLOOM (5), NODE_NETWORK_LIMITED (1024), or NODE_HEADERS_COMPRESSED (2048) gets a node advertising 0. This changes version bytes and every handler gated on `Peer::m_our_services`, including bloom/mempool/filter and limited-network block serving. Rust-vs-oracle parity would therefore compare against the wrong configured node.

Independently confirmed against the pinned plain archive by `/work/scratch/o1c-r1-sol/services-probe.cpp`; output saved in `evidence/services-probe.log`: requested 0/5/1024/2048 all advertised 0. The existing handshake smoke checks the peer's services, but not ours.

Suggested fix: initialize ConnMan's options with the supplied service mask before peers exist and retain its message processor/options; test version's local-services word with distinct masks and a service-gated handler. Re-pin both flavors after the Core change.

### F2 — major: allocation exceptions escape the new C ABI entries

Core `src/test/oracle/oracle_session.cpp:241,337`; `src/test/oracle/oracle_internal.h:38`; `src/test/oracle/oracle_addrman.cpp:353`; `src/test/oracle/oracle.cpp:1324`. Rust declares these functions `extern "C"` in `dash-oracle-sys/src/ffi.rs`.

The session allocation in `new` and the command-string allocation in `deliver` occur before any exception guard. In the AddrMan entries, conversion of a large capturing lambda into the new helper's `std::function<int()>` also allocates before entering `Guarded`. A failed allocation thus throws across the C ABI instead of returning ORACLE_E_EXCEPTION. The Rust declarations permit no foreign unwind.

Independently confirmed with first-allocation fault injection against the pinned archive: `addrman_new`, `peer_session_new`, and `peer_session_deliver` each let `std::bad_alloc` escape to a C++ caller. Probe `/work/scratch/o1c-r1-sol/abi-alloc-probe.cpp`; log `evidence/abi-alloc-probe.log`. The long-command deliver case allocates even before rejecting the >12-byte command.

Suggested fix: put the entire entry body, including construction and validation allocations, inside an allocation-free exception boundary (e.g. a templated Guarded in the header or a top-level try/catch); retain cleanup of partially constructed sessions. Extend the existing allocation-fault coverage to these entries. Also audit DrawLog's allocating `noexcept` callbacks, which can terminate on allocation failure rather than reach the guard.

### F3 — major: omitted maintenance paths leave required differential scenarios unreachable

Core `src/test/oracle/oracle_session.cpp:381`; `src/test/oracle/oracle.h:865`; design `docs/node-program/designs/P2a-outbound.md:1459,1506`.

M3 is a scope gap, not a substitute for the scheduled behavior. The session never starts or explicitly drives `CheckForStaleTipAndEvictPeers`, so it cannot test `EvictExtraOutboundPeers`, stale-tip flags, or the transition enabling extra block-relay peers. P2a §6.2 explicitly includes extra-outbound eviction in the multi-peer headers gate. For example, after nine unprotected full-relay peers establish connections and serve no blocks, advancing the mock clock through a 45-second maintenance interval cannot invoke Core's extra-peer selection/removal (even while the peers remain below their ping timeout). Two implementations omitting that path would agree. `InactivityCheck` is also absent: a silent pre-version/pre-verack peer cannot exercise the handshake/send/receive timeout disconnects.

Suggested fix: add a deterministic maintenance/tick operation that calls Core's actual maintenance and inactivity functions under the session seam, with production connection-limit options and a defined cadence/order. Add an extra-peer-eviction scenario and a silent-handshake timeout scenario. If inactivity is intentionally owned by P1/SB6 instead, record an explicit manager-approved coverage transfer; the specified extra-outbound eviction still needs an oracle driver before this scope is complete. Ping and chain-sync timeouts in SendMessages remain real and reachable; this finding does not conflate them with scheduled eviction.

### F4 — major: the session cannot cover the required send-backpressure boundary cases

Core `src/test/oracle/oracle_session.cpp:375,398`; `src/test/util/setup_common.cpp:427`; `src/net.h:1234`; design `docs/node-program/designs/P2a-outbound.md:1505`.

D12 clears the send buffer and `fPauseSend` before every deliver and every pass. TestingSetup's ConnMan options also leave `nSendBufferMaxSize = 0`. There is no ABI operation to withhold reads, preserve a partial write, or set/query the retained-memory charge. Consequently the required §6.2 `diff_p2p_multi_handshake` G3 cases (many small pongs, one partly sent large payload, V1/V2 charge boundaries) cannot reach production's persistent pause/defer behavior. A Rust implementation that drains or resets too eagerly could pass this differential. The zero cap can also pause `ProcessGetData` after the first pushed reply within a pass (`net.cpp:4888`, `net_processing.cpp:2965,3090`), so the eager flush does not make the within-pass behavior equivalent to a real promptly reading peer with a nonzero send buffer.

Suggested fix: keep the current eager-read mode for basic smoke, but provide a controlled send-drain/partial-write mode with production buffer limits and observable charge/pause state. Drive the real Core transport/ConnMan accounting for the required boundaries, or obtain an explicit manager-approved transfer to an equally strong oracle-backed gate.

The zero-cap consequence is also observed in `/work/scratch/o1c-r1-sol/backpressure-probe.cpp`: after connecting the valid three-block chain, one `getdata` requesting two small known blocks produces one reply on the first pass and the other on the next pass. Log `evidence/backpressure-probe.log`. With sufficient nonzero capacity and no I/O during that processing call, Core's loop has no corresponding reason to pause after that first small reply.

### F5 — minor: the disabled observer is behaviorally dormant, but not zero-cost

Core `src/addrman.cpp:1328`; `src/timedata.cpp:49`; `src/test/oracle/observer.h:36`.

D20's claimed absence of timing change is false as written. The disabled path still reads thread-local `g_oracle_observer`, tests it, and branches. The pinned plain archive's `AddrMan::Add` contains a `mov %fs:...`, `R_X86_64_TPOFF32 g_oracle_observer`, `test`, and `je` after the real Add call (saved in `evidence/addrman-add-disassembly.txt`). The source has no compile-time exclusion for these checks, and `OraclePeerSnapshot` is also compiled unconditionally. In this head, observer.h is included by addrman.cpp and timedata.cpp; net_processing.cpp adds the snapshot/friend access rather than that include. I found no off-mode mutation of Core's logic from these observer sites; the concrete counterexample is extra executed instructions and changed code layout, not a measured wall-clock slowdown.

Suggested fix: compile the observer/snapshot hooks only for an explicit oracle build flag and verify that off-mode objects omit their instructions/symbols. At minimum amend the claim to bounded instruction overhead and retain the manager's local-oracle-only restriction. The branch is a DEC-16 local pin, the build script packages it as a test archive, and dash-oracle-sys is publish=false/test-only; this is evidence of intended confinement, not an enforced ban on building a dashd from the patched branch.

### F6 — major: a real reorg makes an unseeded, unlogged scheduler-thread draw

Core `src/test/oracle/oracle_session.cpp:143,404`; `src/test/oracle/oracle.h:791`; `src/net_processing.cpp:2217,2228`; `src/common/bloom.cpp:385`; `src/validationinterface.cpp:241`. PLAN O1c requires every draw logged.

DrawScope is thread-local and installed only on the ABI calling thread. Draining the validation queue waits for work but does not propagate the seam to Core's scheduler. On a reorg, the scheduler executes `PeerManagerImpl::BlockDisconnected`, resets `m_recent_confirmed_transactions`, and `CRollingBloomFilter::reset()` calls `GetRand<unsigned int>()` with no hook. That salt comes from the process RNG and is missing from the session draw log. Later inventory/request decisions consult this probabilistic filter, so equal logged draws do not guarantee identical internal state or future behavior.

Independently exercised against the pinned archive with valid mined regtest blocks: start with an A1 fixture, handshake, deliver an alternate B1–B3 chain through the real headers/block handlers, and end at height 3. GDB stopped at the reorg's filter reset on the scheduler thread with `g_oracle_rng_hook == nullptr`; stack: reset → PeerManagerImpl::BlockDisconnected → CMainSignals queue → SingleThreadedSchedulerClient → CScheduler. Probe `/work/scratch/o1c-r1-sol/reorg-probe.cpp`, script `reorg.gdb`, logs `evidence/reorg-probe.log` and `evidence/reorg-rng-gdb.log`. This proves a missing draw; it does not claim a sampled byte-level divergence from a bloom false positive.

Suggested fix: drive validation callbacks on a deterministic, seam-installed executor, or propagate the session seam to the scheduler with serialized access and a defined ordering relative to the ABI thread. Simply taking a mutex around the draw log is insufficient to make draw order deterministic. Add a reorg replay test that also asserts the filter reset's draw is present. The header's disclosure of off-thread draws is useful, but does not satisfy the promised complete RNG seam.

## Evidence and scope judgments

Independent pinned-archive smoke: addrman 10/10, peer_session 8/8, smoke 18/18; cross-process seeded replay and forced buffer retry passed. Log: `/work/scratch/o1c-r1-sol/evidence/oracle-tests.log`.

Independent `node-l1 --features oracle --test p2p` passed its six seeded scripts per target; `fuzzctl.py check` passed (44 targets). These are Core-vs-Core replay checks, not Rust-vs-Core implementation parity (D18 is explicitly marked as an SB6 hand-over).

Both installed archives hash to pins.txt: plain `fb57f72733dbba7e60515edb3eec3715e2c4b36b6ef0d8f846555e4672987834`, fuzz `e75eb341d73570e238439e4ad88be2c2c0390c0f0da5ab7124390f7e38f439ec`. Read the two-root logs and check-oracle-repro.sh: separate clean trees/depends builds with different root lengths, shared downloads only, matching archive hashes, successful installation. No Core rebuild was needed. Original successful roots were deleted by the script; this review independently verifies the installed bytes and retained evidence, not a third reproduction.

Semantic comparison against `9da1e93bf` confirms all 46 SB4 vectors keep every verdict, state and input field; only core_commit, binary/archive hashes and two instructions_u values differ. CS3's 118 rows are identical; only its provenance string changes. Script `/work/scratch/o1c-r1-sol/collateral.py`, log `evidence/collateral.log`. The retained gate.log reports 3579 passed / 0 failed, and gate-last-green names the requested rust head; that full gate was not independently rerun.

Independent SB4 pin/input checks and smoke regeneration passed (2/2, five byte-exact regenerations, eight schedule variants for each selected ChainLock case); CS3 recorded/oracle and Rust/Core checks passed (2/2). `cargo clippy --locked -p dash-oracle-sys -p node-l1 --all-targets --features oracle -- -D warnings` and `cargo fmt --all --check` passed. Full SB4 `regen.py --check` exited 0: 44 exact matches, two tolerance-checked benchmarks, and eight alternate delivery schedules for each of the 28 eligible ChainLock cases. Log: `/work/scratch/o1c-r1-sol/evidence/sb4-full-check.log`.

## Fidelity, ABI and deviations

AddrMan's operations call the real public Core mutators/selectors; bucket/chance/netgroup use real AddrInfo and NetGroupManager methods; asmap bits are decoded LSB-first and passed through Core's SanityCheckASMap. Canonical state reads Core's own entries, bucket tables, vRandom, collision set, counts and RNG position, without replacing their behavior. DUMP/LOAD reproduce the in-memory SerializeDB/DeserializeDB byte layout and Core's exception classes. They deliberately omit filesystem I/O, file-not-found/rename errors and temporary-filename draws, and document the different stream error text and reset/backup post-load dump; these are domains to keep separate from AddrMan parity, not proof of filesystem startup parity.

Peer handshake, ping/pong, addr-size misbehavior, version rejection/removal, fixture acceptance, header/block handling and reorg run the actual Core code. The functional reorg probe provides evidence beyond the implementer's session smoke. Session peer teardown happens after the NodesSnapshot is released, invokes real FinalizeNode, and retains the pre-removal snapshot. Logger/capture restoration, argument/time-data reset and the repaired IBD flag initialization were inspected. Equal-seed sessions and handles replay in the tested ordinary-message domain; F6 prevents a claim of complete isolation from process randomness for the full block/reorg domain.

The normal buffer-retry rule is sound: distinct opcode/time or canonical tags identify AddrMan calls; peer ids identify outcomes; successful collection clears the cached output and other valid operations discard it. Invalid out/out_len is rejected before mutation/draining. Rust owns both opaque handles through NonNull and Drop; AddrMan is Send but not Sync, and PeerSession's lifetime-held MutexGuard keeps it on its calling thread and holds network selection. Inputs/CStrings/markers live through synchronous calls, and repr(C) substitution fields match the C declaration. The Rust buffer helper retries once with the exact reported size, while PendingOutput prevents that retry from re-running mutations or draining logs. Independent raw-C probe `pending-probe.cpp` additionally compared ADD, SELECT, DUMP, LOAD and canonical output with three successive undersized retries and invalid-output preflight against an operation-once twin; outputs and subsequent state matched. Allocation-failure safety remains F2.

Deviations 1–6 were accepted by the manager and are not reopened. Judgments on 7–20:

| Deviation | Judgment |
| --- | --- |
| 7 ABI 9 | Accept. FFI constant/header/pinned archive agree; old pins are rejected by archive hashes. A correction to Core requires new hashes. |
| 8 serialized add_peer address | Accept. Addresses are real Core CService values; remote version/services still enter through the actual handshake. Distinct addresses per session are an explicit restriction. |
| 9 structured substitutions | Accept. Bounds are checked before queueing; the ping marker smoke exercises capture and use. |
| 10 deliver queues, pass processes | Accept. A script controls message/pass ordering and exercises the real transport receive and message handler. |
| 11 no masternode throttle | Accept for ordinary peers, whose m_masternode_connection is false. This oracle cannot claim coverage of the 100 ms throttle, masternode connection flags or quorum-priority interleaving; later specialized gates must cover those domains. |
| 12 eager buffer flush | Changes required: F4. |
| 13 restart-style fixture start | Accept as a documented restart fixture. AppInitMain sets best height after chain load; absent historical BlockConnected signals explains the initial zero tip-update time. This does not claim fresh-genesis notification parity. |
| 14 validation boundary drains | Accept the explicit boundary ordering for scripted comparisons, but it does not complete the RNG seam: F6. |
| 15 seed0 then seed1 | Accept a specified deterministic seed assignment in the shim. Production's random constructor-argument evaluation order is not an externally fixed seeded protocol. |
| 16 stateful-once PendingOutput | Accept in the normal buffer-error domain; independently validated above. |
| 17 hook-mode test-util changes | Accept. Fresh test seed per session removes the cached-seed dependency; temp paths use pid/counter/time to avoid consuming an unnumbered global FastRandomContext. These names are not oracle outcome fields. |
| 18 Core-on-same-seed node leg | Accept as an explicitly labeled bootstrap/hand-over to SB6, not a parity gate for a Rust implementation. The registry and source both state this limitation. |
| 19 last_getheaders in microseconds | Accept. Snapshot converts the time-since-epoch to microseconds and Rust reads i64 consistently. |
| 20 production-file observer hook | F5: behaviorally dormant when null; demonstrably not zero-instruction-cost. Local/test-only confinement is supported by DEC-09/16, untracked-upstream local branches and the current dependency manifests. It is not a compile-time shipping exclusion. |

M3's maintenance omissions are F3. Scheduled extra-peer eviction is explicitly required by P2a and cannot be waived by a header comment. Inactivity and masternode-only behaviors may be split into an explicitly approved later gate, but cannot be counted as coverage here.

Final verification: both reviewed worktrees remain clean at the requested heads. This report is untracked and uncommitted in the integration worktree. No Core archives were rebuilt and no GitHub messages were posted.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
