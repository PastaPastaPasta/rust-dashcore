# SB6 round 4 — narrow Sol review

## Scope and pins

Reviewed **d2dc47d6e1e5149dd3752a201d6e0b23cfc338af..5516ee24298505f5044de457b1a7206607ef56f8**, detached at `/work/scratch/sb6-r4-sol/wt`. Both named branch refs resolve to that head. The supplied range contains **nine commits**, rather than the brief's ten; all nine signatures verify as `G`. The exact hashes, rather than the stated count, define this review.

Detached test bases: r1 **32dce97f80164ea03e8402d65776d99daec38417** (`tb-r1`, pinned ABI 9), and r2 **14b7578cb8ac9098f0764ae5c221410875d88a5d** (`tb-r2`, development ABI 10). Their `dash-p2p`, `dash-p2p-transport`, `dash-clock`, and reviewed decode model/matrix files are identical to the target head. D42's node-l1 additions are reviewed on r2, where they actually live.

Contract: `briefs/REVIEW-COMMON.md`, DEC-178, and SB6 D35–D42, with the task's disk TMPDIR and security handling taking precedence. This is a closure review of the supplied follow-ups, not a reopening of the r2/r3 baseline. Core references are **v24.0.0-rc.3**, **da567d7dba945143eee4d2ce1621c21b32089989**; the six relevant archived source files used by the prior probes were checked byte-for-byte against that tag.

## Closure status

- **R3-M1 / D38: closed.** Startup keeps the constructed AddrMan's RNG, uses the file's key, and draws a replacement only on Core's recovery branches. Fatal load and backup errors also follow Core's draw order.
- **R3-m1: closed.** The seven-version and eighth-insertion cases now have effective policy and independent matrix regressions. The six-entry mutant fails both.
- **R3-n1: closed.** Error cursor rollback and retained hook effects are documented, asserted, and checked by the model. This deliberately remains an adapter convention, not Core failed-read cursor parity.
- **R3-n2: closed.** The original `Throw` API row is restored, and f1e6590d7 has its own typed-result migration row (`SB6-dash-p2p-notes.md:80–81`).
- **Opus nit 5: closed.** D38 distinguishes checked GCC/Linux Clang behavior and explicitly leaves Apple Clang/macOS unchecked. The prior broad Clang claim is gone.
- **D40/D41: closed.** The copy is coherent under the writers' lock order; the linger hook reads the sweep's actual deadline. Both methods and `QueuedSends` are unavailable without test/test-utils cfg.
- **D42: implementation verified.** Controlled, Tick and Drain reach the oracle with the supplied arguments. Decoder preservation is independently reproduced. One nonblocking regression-strength finding remains below.
- **BL-61: closed.** `BothAccept` is the correct healthy baseline under DEC-178. Allocation failures still become oracle errors.

## New findings by severity

### R4-m1 — minor, nonblocking: D42's regression accepts ignored Tick and Drain operations

**Location:** r2 `node-l1/tests/p2p.rs:134–154`, particularly the outcome-count interval and replay assertions at `:151–154`; operations under test are `node-l1/src/p2p_session.rs:116–123`.

**Scenario:** a later regression removes `session.tick(*t, *what)?`, or replaces `session.drain(... )` with a successful no-op, while still collecting snapshots. The test continues to pass: counting snapshots and comparing a run with its own replay do not establish timer or drain effects, and Controlled still differs from Eager without either operation.

**Evidence:** standalone copies of the exact r2 `core_session` function, retaining the real Session types and oracle, were mutated independently. `core_leg_runs_ticks_drains_and_controlled_peers` passed with **no Tick call** and with **no Drain call**. The claimed forced-Eager mutant does fail. Results and sources are in `/work/scratch/sb6-r4-sol/session-mutation-results.json`, `session_mutations.py`, and `session-{eager,no_tick,no_drain}-test.log`.

**Fix:** add deterministic assertions for the clock/removal effects of Tick and for a Controlled peer's bounded write progress after Drain, including a partial drain. An alternative is a transcript comparison with a separately driven raw oracle session. The scratch `session_forwarding.rs` demonstrates the latter: it passes at the unmodified head and rejects both no-op mutants. Persist a regression of this strength; avoid relying solely on output length and replay.

This is a test gap, not a defect in the current dispatch. The current implementation passed the separate direct-oracle comparison, so this suggestion does not block the scoped Sol gate. No new blocker or major finding was identified.

## Mechanism and focus rulings

### AddrMan startup and recovery

`AddrMan::unserialize` (`addrman/mod.rs:428–443`) holds the existing object's lock and asserts the same empty-random-table precondition as Core. Its temporary deterministic `Impl` draws neither a source seed nor a key. The existing serializer reads the saved key and table and runs unconditional `check_addrman()` (`ser.rs:491`) before the trailing checksum (`:528`). This matches Core's `Unserialize`/`DeserializeDB` ordering, including a consistency failure taking precedence over a checksum failure. Neither consistency check consumes RNG state.

Only after a successful read does it swap the complete original `FastRandom` into the loaded table. The swap preserves keystream position and the bit buffer; it does not reconstruct or skip ahead in the context. `NetCore::new` now calls this method (`netcore.rs:487`). Its successful `size()` rolls `Check()` twice on that retained context, as Core's logged `Size()` does.

The transactional table rollback on errors differs from Core's potentially partially modified object, but startup immediately replaces or discards that object. It does not change observable startup state or draws. `deserialize` deliberately retains its separately documented fresh-context replacement semantics.

Recovery is precisely Core's constructor replacement: `clear()` constructs a fresh Impl with the same source and policies, drawing the replacement key on its new context. An unsupported image backs up **before** that replacement; a backup failure exits without drawing it. Ordinary magic/checksum/truncation/compat errors terminate startup without replacement. A null saved key produces `Inconsistent` before checksum verification, triggering one replacement.

The unchanged r3 probes linked to fresh target-head libraries now pass **11/11**. Their readable startup reports **4 draws**, randomizer **5482090021177250669**, matching Core-ordered seeds; unsupported startup reports **5 draws**. The shipped nonempty-image tests pass at check ratio 3, checking saved keys/addresses and context state before and after later Size calls. Two extra scratch tests also pass: fatal errors above each take only **one** constructor draw, and successful/error reads preserve an already-used bit buffer without drawing from the source. Evidence: `r3-rng-probes.log`, `load-probe.log`, `load_extra.rs`.

### headers2

The production reader's reconstruction is unchanged by this range. The new tests cover short count, a cut eleventh derived header, TooMany, count throws, successful consumption, and a trailing byte after a zero count. Errors retain the original payload and already-executed hook effects. The model's error/error arm now rejects cursor movement (`p2p_decode_model.rs:214`).

The policy test (`policy.rs:1858`) and the matrix test (`p2p_decode_matrix.rs:1517`) pin versions 100 and 101 respectively after seven/eight insertions followed by offset 7. The matrix reconstructs independently and compares actual header values with both boundary and dedicated reader. Changing only `H2_MAX_VERSIONS` to 6 fails **both** tests.

The r3 independent Core probe was rerun against freshly built reviewed Rust libraries on all **2,366 cases**, plus the four error-cursor cases and the minimal seventh-version case: **zero differences in verdict, successful values/consumption, or hook count**. The main set has 1,328 deliberate failed-read cursor differences; the four explicit error probes retain all four documented differences. This remains the approved transactional adapter contract. The probe uses an identical counted noncryptographic hash on both legs; the shipped actual-X11 tests also passed. D37 and the API migration row now correctly qualify fallback hashing and explicit previous-hash batches.

### Peer hooks

`queued_sends` (`conn.rs:1041`) takes send then transport, matching `push_msg`, `send_data`, and `send_queue_empty` (`:819`, `:849`, `:915`). Receive processing releases transport before taking its queue lock. The sweep releases its linger guard before asking whether sends are empty (`netcore.rs:1206`); its transport-only reconnect query introduces no inverse send acquisition. The added hooks introduce no lock-order cycle with those paths.

The exact queue charge assertion is sound: when the oldest message remains queued, V1 charges its idle buffers; when the writer has moved it into the transport, the transport charges that message. The passing send test covers both permissible snapshots and the return to idle after draining. The linger test bounds the actual stored deadline by the mark/observation times.

Both methods are inside the cfg-gated impl at `conn.rs:1020`, and the result type is independently gated at `:1060`. A fresh production build succeeds; separate downstream probes for each method fail with E0599 and the type probe fails with E0425. There is no unconditional public escape through these additions.

### Sessions and BL-61

The Core leg passes each peer's SendMode to `add_peer_with`, translates script peer/substitution indices to returned NodeIds, forwards Tick's time/mask and Drain's u64 bound, and collects each peer plus the global snapshot only after a step runs. Invalid indices fail; skippable removed-peer deliveries/drains omit snapshots; pass/tick errors propagate. This matches the ABI 10 shim's dispatch.

The separate raw-oracle probe runs maintenance and inactivity ticks plus Controlled drain limits **0, 1, 23, 1 MiB**. Its actual writes are **0, 1, 23, 119 bytes**; Eager has already written and drains zero. For each mode, all **16 snapshots** match `core_session` exactly. Invalid tick mask and invalid peer index are also refused. Evidence: `session-forward.log` and `session_forwarding.rs`.

The old r1 decoder was independently compiled alongside the new r2 decoder. All **512 inputs × 3 targets** render identically per input, reproducing length **2,860,783** and FNV **05b4d61b4ac8cd33**; services/mocktime, Eager modes, and Deliver/Pass-only output are also checked. The source diff preserves input consumption and vocabulary selection. The claimed 568-entry farm-corpus comparison was not rerun because this review did not access the real sink.

BL-61's 34-byte getqrinfo reads an empty hash vector, zero request hash, and `extraShare = 2`. Core's bool deserializer treats any nonzero byte as true (`serialize.h:239`); the boundary normalizes that field and both sides reencode it as 1. DEC-178 requires acceptance agreement here, and the StrictFlag allow-list excludes getqrinfo. The healthy `BothAccept` and injected allocation-failure assertions pass on **both** pinned ABI 9 and development ABI 10.

## Validation and limits

Commands used `nice -n 10`, `CARGO_BUILD_JOBS=8`, empty `RUSTC_WRAPPER`, and `/work/scratch/sb6-r4-sol/tmp` at 0700. Each checkout/build configuration used its own target directory. Tracked files in all three detached worktrees remained clean; this report and standalone probes/mutation copies are uncommitted. No code-under-review edits, commits, pushes, GitHub actions, private-material access, real-sink access, process-pattern kills, or access to `/work/chains` or `/work/bench` occurred.

- Exact head `cargo test -p dash-p2p --all-features`: **160 passed**, zero failures.
- Exact head `cargo build -p dash-p2p --no-default-features`: passed, followed by the three expected production compile rejections.
- Exact head `cargo clippy -p dash-p2p --all-features --all-targets -- -D warnings`, workspace format check, standalone node-fuzz format check, and range whitespace check: passed.
- r1 `cargo test --manifest-path node-fuzz/Cargo.toml --lib p2p_decode`: **10/10 passed**; these are model/matrix tests, not an oracle session comparison. All reviewed Rust sources were built fresh. The unchanged RocksDB native library was reused read-only from r3's documented cache; these tests do not exercise it.
- r1 pinned `node-l1` p2p and BL-61 tests: **1 + 1 passed**, unpinned override unset. Verified archive **6efa24f2152d39e7d88da02d04d627cec7eb79db**, SHA-256 **fb57f72733dbba7e60515edb3eec3715e2c4b36b6ef0d8f846555e4672987834**, from `/work/scratch/o1c/oracle-cache`.
- r2 `node-l1` AddrMan, BL-61, and p2p tests: **2 + 1 + 3 passed**. Its targeted clippy check also passed. Archive **d694a47043d90726b30b7ca4994d85fc5e483d5d**, SHA-256 **9ac1454c55b5ad24af3ee3a6f697be8b0698f12df2878afcc9c3b85ab24a8000**, with the explicitly requested unpinned override. ABI 10 evidence is development evidence, not certification of a pinned release archive.
- Independent r3 probes, additional load/forwarding/decoder checks, and the mutation results are recorded above. Logs and sources are retained under `/work/scratch/sb6-r4-sol/`.

The supplied GATE EXIT 1/BL-20 report and 17-target smoke were not rerun. No new libFuzzer campaign, plateau certification, full workspace gate, cross-platform build, or Rust-node-versus-Core processor comparison was performed. D32 still uses Core on the node leg until P2a-core integrates; this review verifies SB6's boundary and session adapter, not that downstream processor. D35/D36/D39 behavior outside this range is carried forward from the prior review.

**Verdict:** APPROVE-WITH-NITS

Sol gate satisfied: yes
