# P1 — Sol review, round 1

**Tier:** rev Sol→O-h. **Range:** `2bf18991` → `a6af9505` (13 commits), `node/p1-p2p-transport`. Reviewed the clean P1 worktree without modifying it. Experiments live under `/work/scratch/p1-r1-sol`; this report is intentionally uncommitted. PLAN §7 handling followed: codenames only; no private material inspected.

## Findings

### Major F1 — V2 packet reception reallocates on every small fragment

**Location:** `dash-p2p-transport/src/v2.rs:345–358`, especially line 358.

`reserve_exact` takes an additional amount relative to the current **length**, not the unused capacity. Calling `reserve_exact(min(max_read, input.len() + 256 KiB))` on every iteration therefore raises the desired capacity on every fragment while more than 256 KiB of the packet remains. Core's `V2Transport::ReceivedBytes` (`v24.0.0-rc.3:src/net.cpp:1424–1441`) first checks whether `size + min(input.size(), max_read) > capacity`, and reserves only then. That outer check was lost in the port.

A peer can complete the key exchange, announce a legal large VERSION or APP packet, and supply its body in small fragments. No PoW, application handshake, or authenticated completed body is required to drive these allocations. Memory remains bounded, but the allocator work scales with the number of fragments; when reallocation moves the buffer, copying becomes quadratic in packet size. This also gives an active timing probe a transport-specific distinction from dashd. The current frame benchmarks use at most 65,536-byte payloads and whole-frame delivery, so they do not exercise this growth path.

An external Rust program using only the public transport API, with a counting global allocator, reproduced:

- Legal 512 KiB payload, one-byte fragments: **262,162 reallocations**, with **103,089,045,711 bytes** of old allocations subject to copying.
- Legal 3 MiB payload, one-byte fragments: **2,883,602 reallocations**, with **4,913,506,156,751 bytes** of old allocations subject to copying.
- Legal 3 MiB payload, 64-byte fragments: **45,057 reallocations**.

On this host glibc usually extended these buffers in place. To test the permitted moving-allocation case, the same program also used a valid allocator that allocates a replacement, copies, and frees on every realloc: the 512 KiB case copied the full 103 GB and took about **9.09 seconds** (64-byte fragments: 1.61 GB, about 124 ms). These are allocator-behavior experiments, not a claim that the normal host heap always moves every allocation.

**Suggested fix:** restore Core's capacity check around the reserve logic, retaining the early return for APP_READY, and add a resource regression test for legal large packets fed in one-byte and small fixed-size fragments. Check allocation/reallocation counts, not just payload equality. Include the VERSION state as well as APP. The scratch-only control with that guard reduced the 3 MiB, one-byte case to **13 reallocations**; the 512 KiB moving-allocator case to **3 reallocations**, **786,444 copied bytes**, and about **7.27 ms**. The original and control both delivered the correct payload.

### Minor F2 — Oversize V1 errors print unsanitized peer command bytes

**Location:** `dash-p2p-transport/src/message.rs:134` (invoked by `src/v1.rs:107`).

The command has not been validated when an oversized header is rejected. `Display` interpolates `CommandString` directly, so an attacker-controlled newline or escape sequence can enter a caller's log. A 24-byte regtest header with command `ping\nFORGED!` and length 3,145,729 produced the escaped string `"Header error: Size too large (ping\nFORGED!, 3145729 bytes)"`. Core's corresponding `readHeader` path (`net.cpp:739`) calls `SanitizeString(hdr.GetCommand())`.

**Suggested fix:** sanitize the raw command up to its first NUL using Core's default allowed-character set before formatting this error. Preserve the raw command in the structured error if useful. Add a formatting regression for newline, ESC, and non-ASCII command bytes. This is a diagnostic mismatch and log-injection concern; the wire disconnect decision is correct.

### Minor F3 — Fuzzing does not exercise hostile authenticated V2 packet states

**Location:** `node-fuzz/src/models/p2p_transport_model.rs:32–35,185–237,279–367`.

The paired mode reaches normal encrypted handshake/APP traffic and rekeys, but only transfers packets emitted by another instance of the same transport, with payloads capped at 4,096 bytes. It cannot create decoys, nonempty transport-version packets, invalid encrypted message types, oversized encrypted descriptors, incorrect tags/AAD, reserved header bits, or large-fragmentation allocation pressure. The raw-responder mode can exercise detection, key reception, and garbage failure, but cannot reliably synthesize the derived terminator and authentication tag needed to enter adversarial VERSION/APP cases. Successful round trips also do not independently check V2 cryptographic correctness; the golden vectors do provide that separate check.

**Suggested fix:** add a scripted bare `Bip324Cipher` peer whose keys and inputs come from the fuzz input, as the deterministic `Tester` already does. Let the script construct and mutate descriptors, tags, AAD, decoys, version contents and types, and deliberately stop at decision boundaries. Add independent expected consume/drop/disconnect outcomes and bounded allocation/work invariants. Keep fast small-payload runs, with targeted large-length/fragmentation cases that reach F1's threshold. The clean smoke run must not be treated as coverage of these hostile packet paths. The existing deterministic unit tests cover many of them, which is why this coverage gap is minor rather than a separate implementation blocker.

## Scope assessment

- V1: 24-byte header consumption; magic before size; 3 MiB limit; checksum before command rejection; dropped-message recovery and raw sizes match Core. No peer-reachable panic found under the documented caller contract.
- V2: responder detection at at most 16 bytes; mismatch/send behavior; the wrong-network V1 error's input-step dependence; 64-byte key boundary; 4,095-byte garbage limit plus 16-byte terminator; first matching terminator; and length-before-body decisions match Core. The garbage loop compares only the last 16 bytes at each step and reserves a bounded buffer; no quadratic terminator search found.
- Packet handling: VERSION contents ignored; decoys keep the receive state; the first authenticated packet, including a decoy, consumes the garbage AAD; failed authentication is terminal; APP_READY blocks further consumption; reserved short-ID slots give the empty type and out-of-range IDs are dropped. Receive contents limit is 3 MiB + 13, including for short types, as Core specifies.
- Crypto: initiator/responder ElligatorSwift ordering matches `CKey::ComputeBIP324ECDHSecret`; the default secp256k1 BIP324 hash is used. Dash magic is appended to `bitcoin_v2_shared_secret`; all six HKDF labels match. Length-cipher rekeys consume the next stream bytes; packet-cipher rekeys use nonce `{0xffffffff, rekey_counter}`, block 1. Both rotate after 224 calls, including failed decrypts for the packet counter. All 7 BIP324, 3 FSChaCha20 and 2 FSChaCha20Poly1305 golden tuples were independently compared with the pinned Core source, including C++ concatenated strings. The 33 Bitcoin and 41 Dash short-ID table entries also match the pinned source after resolving `MAKE_MSG` names.
- Send behavior/accounting: send eligibility after key exchange and after draining handshake bytes, `more`, message labels, version-gated short IDs, 24-byte reconnect threshold, partial-write charge retention, and V2's logical libstdc++ capacity growth match Core. Queue charge is `56 + MallocUsage(payload.capacity())` on 64-bit; V2's transport charge is `24 + MallocUsage(logical_capacity)`. This is explicitly logical accounting, not the Rust struct's layout.
- API: the sans-io design fits SB6's driver. SB6 must loop on unconsumed bytes, extract completed messages, close on `TransportError`, pass the common protocol version, update pause state after writes, and poll `should_reconnect_v1` on outbound close. It also owns socket wakeups, TCP_NODELAY/MSG_MORE or equivalent coalescing, timeouts and reconnection. Those socket-level timing and segment-boundary properties cannot be certified by this crate's interop tests alone and remain SB6/O13 work. The local type splitter preserves Core's reserved slots and is justified; the custom cipher composition is supported by vectors and interop; optimization profiles and the documented best-effort key wipe are acceptable deviations.

## Validation

Every reviewer build used `RUSTC_WRAPPER=` and 12 Cargo build jobs. Main tests used `--target-dir /work/scratch/p1-r1-sol/target`; fuzzing used an isolated archive of `a6af9505` and its own target directory. Temporary build/regtest data used mode-700 `/dev/shm/p1-r1-sol-tmp`; the runner's per-run temporary directory was directed there as well. No shared chain instances were touched.

- `eval $(python3 contrib/setup-dashd.py)` followed by `cargo test --locked -p dash-p2p-transport --all-features --target-dir /work/scratch/p1-r1-sol/target -- --nocapture`: **38 passed, 0 failed**. The interop selector used the cached **v24.0.0-rc.3**, and the live RPC version was **240000**. Both V2 directions, session IDs, short-ID ping/pong, V1 detection and outbound V1-only reconnect passed. The 1,000-sample KS statistics were two-sample **0.0310**, dashd/uniform **0.0251**, and Rust/uniform **0.0211**; all within the suite's limits.
- Setup repeated, then `DASHD_V24_PATH="$DASHD_PATH"` to explicitly select setup's **23.1.0**: **all 5 interop tests passed**, RPC version **230100**. Its 1,000-sample KS statistics were **0.0330 / 0.0231 / 0.0405**.
- `cargo clippy --locked -p dash-p2p-transport --all-features --all-targets --target-dir /work/scratch/p1-r1-sol/target -- -D warnings`: **passed**. `cargo fmt --check -p dash-p2p-transport`: **passed**.
- Packaged v24 `test_dash --run_test=bip324_tests,crypto_tests --log_level=message`: **19 cases passed**, no errors.
- Independent vector/table extraction comparisons: **passed**, as detailed above.
- Public-API allocation and error-format probes: **F1 and F2 reproduced**. The capacity-guarded scratch control also passed all **17 V2 behavior tests**. Probe source and outputs: `repo/dash-p2p-transport/examples/review_alloc.rs`, `repo/dash-p2p-transport/examples/review_error.rs`, `alloc-original.log`, `alloc-fragmented.log`, `alloc-control.log`, `alloc-control-fragmented.log`, `error-display.log`, and `control-v2-tests.log` under the scratch directory. The reviewed branch contains none of these experiments.
- Isolated-snapshot `node-fuzz/run.sh p2p_transport_model --pin v24.0 --cpu-hours 0.017 --jobs 1 --run-tag 0000000000000012`: **passed**, **136,393 executions**, **0 divergences / crashes / sink failures / refusals**, **0.0216 CPU hours** (about 78 CPU seconds), one segment. The runner reported revision `-` because the snapshot is a git archive; the transport and model sources were separately byte-compared with the unchanged `a6af9505` worktree. `fuzz-summary.log` and the run's `verdict.json` are retained in scratch. The resulting corpus's largest input was 315 bytes; the long paired rekey script was therefore replayed separately below. F3 explains the hostile-packet coverage limitations.
- Additional `cargo test --release --lib models::p2p_transport_model::tests` with fuzz instrumentation **could not link** the libFuzzer callback `rust_fuzzer_test_input`. Through the supported runner, **17 public input scenarios passed replay**, including the source unit tests' edge/detection/V1 streams and long paired rekey script, plus a second fragmented paired script. No divergence, crash, sink failure or refusal occurred. This validates those model invocations, rather than claiming the direct unit-test command passed. `model-unit-tests.log`, `model-replay-summary.log` and the copied replay tree are retained in scratch.
- `TMPDIR=/dev/shm/p1-r1-sol-tmp RUSTC_WRAPPER= CARGO_BUILD_JOBS=12 contrib/node/selftest-checks.sh`: **all 104 cases passed**, including the corrected re-pin/audit-stamp fixture. Its temporary worktree was removed by the script. All reviewer-started dashd instances exited, and the reviewer-owned `/dev/shm` tree was removed after validation.

The claimed complete workspace 3169/0 run and full integration gate were not independently rerun; the reviewer ran the focused crate suite and relevant selftest. Successful interop proves supported live transcripts, not exhaustive socket timing/fingerprinting equivalence.


## Verdict

**CHANGES-REQUIRED.** F1 must be fixed and the allocation regression covered before this critical transport passes the Sol gate. F2 and F3 are smaller follow-ups with concrete fixes above.

**Sol gate satisfied: no.**
