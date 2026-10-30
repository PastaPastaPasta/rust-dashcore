# SB6 round 3 — narrow Sol review

## Scope and pins

Reviewed only **ccb936356eabf6366ab89a7c94503e17f5c901a4..d2dc47d6e1e5149dd3752a201d6e0b23cfc338af**, the ten follow-up commits on `node/sb6-dash-p2p`; all ten signatures verify as `G`. Round 2's approved implementation was the baseline, not reopened wholesale. The review worktree is detached at `/work/scratch/sb6-r3-sol/wt`. The detached oracle test base at `/work/scratch/sb6-r3-sol/tb` is **eee304d7f44c0740fba2ade3a280dda4fa26fbcd**; its `dash-p2p`, `dash-p2p-transport`, `dash-clock`, and decode model/matrix sources are identical to the reviewed head.

Contract: `briefs/REVIEW-COMMON.md`, with the task's disk TMPDIR and security-note instructions taking precedence. Binding scope: DEC-178, DEC-184, and SB6 D26/D35–D39. Core source reference: tagged **v24.0.0-rc.3**, **da567d7dba945143eee4d2ce1621c21b32089989**, archived into scratch. Read-only review except this uncommitted report and standalone scratch probes/mutation copies. Neither worktree's tracked files were edited; no commits, pushes, GitHub actions, private-material access, or access to `/work/chains` or `/work/bench` occurred.

## Findings by severity

### R3-M1 — major: loading an existing peers.dat adds an extra source draw and replaces AddrMan's RNG

**Location:** `dash-p2p/src/netcore.rs:466`, `:486`; `dash-p2p/src/addrman/mod.rs:282`, `:410`; `dash-p2p/src/addrman/imp.rs:327`. The new source wiring is from d36232b65. The misleading D38 assertion is at `docs/node-program/designs/SB6-dash-p2p-notes.md:44`; its startup regression is `dash-p2p/tests/rand_source.rs:149`.

**Scenario:** restart with a valid, readable peers.dat, `deterministic = false`, and a harness-provided source. `NetCore::new` constructs/seeds AddrMan once. `AddrMan::deserialize` then calls `fresh()`, which constructs another `Impl`, seeds another context from the same source, draws its temporary key, and overwrites that key with the file's key. Successful startup therefore consumes **five 32-byte source draws instead of four**: an extra AddrMan draw before BanMan and the two connection seeds. AddrMan also retains the second context, rather than the constructor's context. With an unsupported file, the speculative deserialization instance plus `clear()` consume **six draws instead of five**. This shifts all subsequent seeded session draws, not just an unused temporary key. Production still uses secure OS randomness, but D38's required per-site structure is not faithful.

**Core evidence:** `addrdb.cpp:197` constructs one AddrMan; `:202` deserializes into that same object. `AddrManImpl::Unserialize` asserts its table is empty (`addrman.cpp:238`) and reads the saved key (`:268`); it neither constructs nor seeds another context. Only the recovery branches construct a replacement (`addrdb.cpp:206`, `:215`, `:223`). The constructor's context was already seeded by its `rand256` (`addrman.cpp:111`). Thus a readable file contributes one AddrMan source draw, followed by BanMan and nSeed1/nSeed0. A recovery contributes two AddrMan source draws. The scratch store draws nothing for filenames, on either accounting path.

**Executable evidence:** two scratch regression tests linked against the freshly built reviewed libraries fail as expected:

- Readable empty peers.dat: actual **5**, expected **4** draws. For stream seed 42 and randomizer id 9, the port returns `1561031207130939510`; the Core-ordered seeds give `5482090021177250669`.
- Unsupported peers.dat: actual **6**, expected **5** draws.

Logs: `/work/scratch/sb6-r3-sol/logs/startup-readable-probe.log` and `startup-unsupported-probe.log`; probe source: `rng_extra.rs`. The existing startup test uses `NoStore`, so it exercises the missing-file branch and misses both cases. The deterministic AddrMan differential also cannot catch this: deterministic construction takes no bytes from the injected source and recreates the same zero-key context.

**Fix:** let startup deserialize transactionally into table state that retains the initial AddrMan context, without constructing a second seeded instance. Keep the fresh-context semantics required by the standalone replacement/load API separately; only Core's actual recovery replacement should reseed. Add startup draw/state tests for readable, unsupported and inconsistent files, including subsequent AddrMan draws and nonzero consistency-check ratios. Do not compensate by discarding or skipping unrelated source bytes. Correct D38's claim once the implementation matches it.

### R3-m1 — minor: the seventh version-list entry has no effective regression

**Location:** `dash-p2p/src/policy.rs:1174`, `:1751`; `node-fuzz/src/models/p2p_decode_matrix.rs:1329`, `:1367`.

**Scenario:** seven explicit versions followed by a reference to offset 7 must restore the oldest retained version. The real implementation is correct, but changing only `H2_MAX_VERSIONS` from 7 to 6 survives **all 24 policy tests and all 9 model/matrix tests**. The current randomized generator has at most eight headers; its passing run did not exercise enough explicit version insertions before that reference.

**Evidence:** the independent Core cases kill this mutation on 23 inputs. A minimal eight-header probe writes versions 100 through 106, then offset 7: Core and the reviewed reader return last version **100**, while the mutant returns **0**. All three accept, consume 607 bytes, and make one hook call, so verdict/consumed/hash-count assertions alone miss it. `logs/seventh-version-case.log` and `logs/model-mutation-h2_window_six.log` retain the proof. This is a test gap, not a defect in the unmodified reconstruction.

**Fix:** add that deterministic sequence to the dedicated reader and model/matrix regressions, plus an eighth insertion followed by offset 7 to pin oldest-entry eviction. Compare actual header versions with the independent matrix; do not rely only on another call through the same lenient reader.

### R3-n1 — nit: document and pin read_headers2's error cursor contract

**Location:** `dash-p2p/src/policy.rs:957`, `:971`, `:1802`; `node-fuzz/src/models/p2p_decode_model.rs:199`.

**Scenario and evidence:** the reader commits `*payload` only on success. Core retains completed reads on both a handler count return and a throw. Fresh exact-serializer probes show:

- Old-version count 2001: Core returns the over-limit outcome after **3** bytes; Rust returns `TooMany` with **0** bytes committed.
- Noncanonical count 1: Core throws after **3** bytes; Rust commits **0**.
- Count exceeding MAX_SIZE: Core throws after **5** bytes; Rust commits **0**.
- Count 8000 with ten complete headers and no eleventh: Core throws after **435** bytes and 9 hashes; Rust commits **0** bytes and also calls the hook 9 times.

This is consistent with the previously approved transactional `policy::read` convention (API row b6a2d1f26), and these outcomes terminate the message. It is **not a scoring/verdict defect** and does not reopen r2. However, the new API's comments do not state this convention, `TooMany` is a handler return rather than a throw, and the tests/model assert consumption only on success. In particular, cursor rollback does not roll back hook side effects.

**Fix:** explicitly document unchanged payload on either error and add cursor/hook assertions for count returns, count throws and partial header throws. Qualify cursor-parity claims. If exact failed-read consumption becomes an integration requirement, expose the actual read position deliberately instead of silently changing the established adapter contract. Evidence: `logs/cursor-probes.log`; Core `net_processing.cpp:5323`, `streams.h:280`.

### R3-n2 — nit: the API ledger attributes the follow-up result type to the original commit

**Location:** `docs/node-program/designs/SB6-dash-p2p-notes.md:51`, `:77`.

The ledger asks for one row per public API commit, but efd769caa's row was rewritten to describe the final `Headers2Error` result. That commit introduced a `Throw` result; f1e6590d7 changed it to the typed over-limit/throw result. A caller migrating between those revisions cannot find the corresponding API transition in the ledger.

**Fix:** preserve the original row's API and add an f1e6590d7 row with the result-type and match-arm migration. The current final-head migration text is otherwise accurate; this is documentation traceability, not a runtime defect.

## Focus rulings

### headers2

The new dedicated reader's **count limit, outcome distinction, reconstruction and hash call sites are correct** apart from the explicitly described cursor convention.

- `headers2_count` (`policy.rs:1002`) uses canonical CompactSize and its MAX_SIZE check, then 2000 before protocol 70235 and 8000 from 70235 (`:414`). These match Core's `GetHeadersLimit` (`net_processing.cpp:1210`, `version.h:50`). `TooMany { count, max }` makes no header read and no hook call, permitting the processor's `Misbehaving(20)` plus return (`net_processing.cpp:5323`). Truncated/noncanonical/over-MAX_SIZE count encodings become `Throw`, matching the stream-exception path. The helper itself does not score peers; correct processor handling remains P2a-core's responsibility.
- `uncompress_headers2` (`policy.rs:1201`) reads each whole wire entry before reconstructing it or calling the hook. It saves even a first compressed header's default-zero version, preserves missing first fields as zero, inserts duplicate explicit versions, retains seven entries, moves a valid referenced entry to the front, leaves an out-of-list reference zero, inherits bits, and wraps signed time deltas modulo 2^32. These match `primitives/block.h:166` and `primitives/block.cpp:37`, `:95`. The version deque is local to each call, so no state crosses messages.
- The dedicated path never tries strict decoding first. Its hook executes exactly once for each **fully read, non-first header whose previous hash is omitted** (`policy.rs:1226`). No hook runs for an incomplete wire entry. The actual-X11 test `headers2_hash_once_on_both_paths` (`:1751`) passed: both first-header encodings produce 7999 hook calls for 8000 consecutively derived headers. Hashing the final header in the processor gives 8000 for those batches. The separate `decode`/`read::<Headers2>` fallback can rehash, as D37 now correctly states.
- The “hash only the last header yourself” integration shortcut applies to the consecutively derived batches in that test. Core also accepts explicit previous hashes in later entries; those entries do not invoke the hook for their predecessor. A processor must hash every returned header not already covered by its hook memo, exactly once. The helper's per-derived-header claim is sound; it does not promise a hash for every returned header.

I compiled a standalone C++ probe using the tag's actual CompactSize, compressed-header and CBloomFilter serializers and verbatim `Uncompress` functions. Only `CBlockHeader::GetHash` was replaced with a counted deterministic hash, supplied identically through Rust's hook. Across **2,366 cases**—all 256 flags, mixed/version-list sequences, time wrap/extremes, truncated entries, count boundaries, two full 8000-header batches, and governance bounds/tails—there were **0 differences in verdict, successful values/consumption, or hook count on success or failure**. Outcomes: 1036 accepted, 1322 throws, 8 handler count returns. Error cursors deliberately differ as R3-n1 records. This probe is independent of base-sdk's parser; it is not a cryptographic X11 comparison. The shipped actual-X11 tests complement it.

### GovSync

`GovSyncRead` (`policy.rs:914`) exposes Core's uint32 `nHashFuncs`, uint32 `nTweak`, and uint8 `nFlags` in little-endian wire order (`:925`, `:942`), after the object hash and bounded vData. This matches `common/bloom.h:50`, `:78`, and `governance/net_governance.cpp:93`. `policy::read::<GovSyncRead>` joins the boundary's governance limit checks (`policy.rs:896`, `:1108`); `gov_rest` correctly locates vData's end whether the underlying reader stopped there or already read the tail (`:1148`). It neither adds nine bytes twice nor consumes trailing bytes. Bounds are checked before promising vector bytes. No deserialization-only restriction was incorrectly added to hash_funcs or flags; post-read bloom-policy checks belong to the processor. The model's tail access now uses checked subtraction and a guarded slice (`p2p_decode_model.rs:228`). No GovSync defect found.

### D38 and production randomness

Apart from **R3-M1**, the reviewed sites follow Core's structure: fresh lazy contexts for GetRand and temporary shuffles; a no-draw range of one; seeding even an empty `fill`; thread-owned contexts for DNS seed shuffle/address times and feeler delay; fresh GetRand per ping retry; and the MNAUTH challenge drawn/stored at `version_for` (`handshake.rs:274`), after attach/processor initialization, as Core's `PushNodeVersion` does (`net_processing.cpp:1573`). AddrMan retains its own context for table operations. BanMan uses GetRand below UINT_MAX rather than unrestricted rand32. The init sequence and store filename seam otherwise follow `init.cpp:1690`, `:1713`, `:1715`.

V2 attach uses `V2Transport::with_rand` (`netcore.rs:987`): valid secret-key draws with `strong = true`, ElligatorSwift entropy, then a separately seeded garbage context (`dash-p2p-transport/src/v2.rs:153`). This matches Core's braced delegating constructor (`net.cpp:1099`) and key rejection loop (`key.cpp:160`). The ordinary NetConfig cannot supply a deterministic/custom source without test-utils: the field is cfg-gated (`netcore.rs:169`), the custom source is private, and its setter is cfg-gated (`:237`). A no-feature build passed, and two external compile probes fail specifically because `deterministic` and `with_rand_source` do not exist. Thus normal production NetCore V2 keys continue to use `OsRand`/getrandom (`rng.rs:29`); the new source seam does not make them predictable. Explicit transport constructors accepting caller-supplied keys/randomness remain public APIs, as before.

The GCC/Clang distinction is real in fresh Linux compiler probes of the matching `make_unique<CConnman>` expression: GCC 15.2 at -O0/-O2 draws seed1 first; Clang 21.1.8 at both levels draws seed0 first. The port correctly targets the selected GCC oracle order. I did not validate an Apple Clang/macOS release artifact, and C++ does not specify this ordinary argument order.

### D39 shutdown and grants

No additional race or grant leak established. `dial_wait` enables its interrupt notification before checking the interrupt (`dial.rs:271`); only the interrupt wakes a fixed sleep, while freed slots wake semaphore waiters. An acquired permit is stored only after `live_dial` checks the interrupt under the dial lock (`:292`, `:317`). If interrupt wins first it is dropped; if storage wins first, shutdown subsequently clears it under that same lock (`:328`). Shutdown joins the only retry producer—the sweep—before clearing reconnections (`netcore.rs:1100`, `:1105`, `:1240`), so no late sweep retry replenishes the queue.

The AddCon unlock around `unconnected_added_nodes` (`dial.rs:832`) does not reintroduce a grant after cleanup: after relocking, its Iterate phase requires the now-cleared grant and Reconnect sees the cleared queue. An in-flight tick can return a terminal sleep rather than Done; the driver then sees `dial_wait == false`. This has no new connection/grant effect. An OpenRequest already returned to a driver remains driver-owned; `open` refuses it after interrupt (`netcore.rs:846`) and drops its grant.

Beyond the shipped tests, scratch probes passed **48 cancellation schedules** for acquired/queued waiters, an external OpenRequest held through shutdown, and **8 queued V1 retry cycles** proving a slot is held by the queue before shutdown and restored afterwards. These are concrete ownership schedules, not an exhaustive concurrency model check. Resolver permission is also correct: named targets use `name_lookup` (`netcore.rs:921`); DNS seeds pass true (`dial.rs:996`), matching Core `net.cpp:405`, `:2775`.

## Rulings on Opus nits 1–5

1. **derived_hashes reuses base-sdk: valid limitation, not a blocker.** It independently counts hook invocations, but shares wire parsing with the reader and cannot prove that parser's Core fidelity. The matrix and the fresh independent C++ probe supply separate evidence. Keep the planned Core base-message oracle follow-up; do not call this counter alone an independent decoder oracle.
2. **Public From<DecodeError>: no actionable defect.** A public trait conversion for the public error is idiomatic and maps only to Throw; it cannot fabricate a TooMany result or bypass the count check. Making the conversion a private helper is optional API surface preference.
3. **No error cursor assertions: accepted as R3-n1.** Actual cursor positions were checked independently. Correct verdicts/hook counts coexist with intentional rollback; document and test that contract.
4. **efd769caa row edited in place: accepted as R3-n2.** Add the follow-up API row; no runtime consequence.
5. **clang/macOS seed order: partially corroborated, still platform-limited.** Linux Clang/GCC probes confirm the compiler distinction. The Apple release claim remains unverified here; soften it to expected/observed compiler behavior or attach an Apple compiler/release-artifact receipt. This does not block the GCC-order implementation.

## Validation and limits

- `cargo test -p dash-p2p -p dash-clock -p dash-p2p-transport --all-features -- --test-threads=4`: **passed**. 154 dash-p2p tests, 44 transport tests (including all 5 real-dashd interop tests), 2 clock tests, and 1 transport doctest: 201 total, zero failures.
- `cargo check -p dash-p2p --lib --no-default-features`: **passed**; production API negative probes both failed for the intended missing field/method. `cargo clippy -p dash-p2p -p dash-p2p-transport --all-features --lib -- -D warnings`: **passed**.
- Exact unmodified decode model/matrix sources in a standalone harness: **9/9 passed**. The harness replaces only the irrelevant divergence-output type; it does not replace the policy/decoder under test.
- Test base `node-l1` with `--features oracle --test l1_overlay --test l1_addrman`: **7/7 passed** (5 overlay, 2 AddrMan). Correct cache: `/work/scratch/o1c/oracle-cache`. Archive validation linked the pinned plain oracle at **6efa24f2152d39e7d88da02d04d627cec7eb79db**, SHA-256 **fb57f72733dbba7e60515edb3eec3715e2c4b36b6ef0d8f846555e4672987834**. The initial attempt used the default cache, which lacks this pin and failed to link; rerunning with the actual cache resolved it without editing the pin or permitting an unpinned archive.
- Independent headers2/GovSync comparison and targeted failure cursors: results above; `logs/independent-parity.json`, `logs/cursor-probes.log`, `core_probe.cpp`, `core_probe`, `rust_probe`, `cases.txt` and `case-labels.txt` retain the evidence.
- Nine RNG/lifecycle probes passed; two additional startup regressions failed on the reviewed implementation and substantiate R3-M1. No source fix was applied.
- Eight new standalone policy mutations were tried. The policy tests catch swapped governance tail fields, zero/double hook calls, missing bit inheritance, and an inclusive count limit; they miss version deduplication, failed MRU promotion, and the six-entry window. The matrix tests catch the first two misses, leaving only **R3-m1**. The independent C++ cases catch all eight. Results: `logs/mutation-results.json` and `logs/model-mutation-results.json`. These are additional checks, not a claim to have repeated the implementer's five D39 mutations.
- `node-fuzz/run.sh p2p_decode_model --pin v24.0 --cpu-hours 0.0167 --jobs 1`: **passed**, 445,455 executions, 60.77 process CPU-seconds, 0 divergences, 0 crashes, 0 sink failures; runner exit 0 and `verdict.json` status `pass`. The pinned fuzz archive's SHA-256 is **e75eb341d73570e238439e4ad88be2c2c0390c0f0da5ab7124390f7e38f439ec**. Evidence: `logs/fuzz-decode-cached.log` and `fixtures/fuzz/p2p_decode_model/runs/20261011T063120Z-v24.0-4056999-536f6c7233000003/verdict.json`. Only the new mode-0700 scratch fixture tree was used; this was a smoke, with no campaign or real sink access. The initial attempt was stopped during native dependency compilation. The completed rerun linked a byte-verified copy of the matching RocksDB 11.8.1 archive from the implementer's same-head test-base build; this model does not exercise RocksDB. Reviewed Rust still built in this review's own target. The cache receipt is `logs/rocksdb-cache-receipt.json`; the interrupted attempt is not counted as a passing run.
- I inspected the supplied full-gate logs. The d2dc47d6e workspace gate exited 1 with the scanner_parity and dashd_sync failures identified in the brief. The scanner's first rerun still failed its other case; the subsequent serial rerun passed 2/2. dashd_sync passed 37/37 in its rerun. I accept the manager's stated BL-85/BL-20 disposition; these are not fresh green-workspace results from this review. I did not rerun the entire workspace gate, mobile builds, MSRV validation, or P2a-core's future integration. Local Rust validation used rustc 1.98.1.
- Test/build work used nice 10, eight Cargo jobs, empty RUSTC_WRAPPER, disk scratch TMPDIR (0700), and separate local target directories. Eight mutation variants exist only as standalone scratch source copies, outside both git worktrees. No source revision or approved r2 finding was changed by these experiments. The report is uncommitted.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
