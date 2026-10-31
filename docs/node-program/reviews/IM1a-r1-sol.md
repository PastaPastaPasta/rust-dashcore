# IM1a round 1 — Sol

Reviewed **e8cdec55 → f979ed80** (five commits), branch `node/im1a-core-format-detect`, read-only in `/work/home/agent/workspace/rdc-wt/im1a`. This review includes `anchors.dat`, the shared `read_tx` reservation change, fixtures, capture script, detection, and all six new fuzz targets. Followed `briefs/REVIEW-COMMON.md`, PLAN §4 IM1a, PLAN §7 handling, and the code-review-validator workflow. No commits, pushes, GitHub comments, or edits to the reviewed branch.

**Verdict: CHANGES-REQUIRED. Sol gate satisfied: no.**

The captured ordinary files parse correctly, and the allocation/count hardening is useful. The filesystem check still has a reproducible blocking race. Several declared deviations discard information or change Core's import results without imposing a safety bound. These need correction before the next review gate.

All Core references below are to **v24.0.0-rc.3**, peeled commit `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`, inspected from a separate archive rather than the moving checkout. The installed binary's SHA256 matches the provenance. Core repros ran in six private, fresh regtest datadirs and were stopped afterward; no existing node was queried or changed.

## Findings

### F1 — major: the regular-file check does not constrain the opened file

**Location:** `dash-coreimport/src/netstate/mod.rs:58` and `:64`.

`metadata(path)` follows symlinks and checks one pathname lookup; `File::open(path)` resolves the path again. A datadir entry or symlink target replaced between these calls can become a FIFO. The later blocking open never reaches `take(limit + 1)`, so the input cap cannot protect startup. Replacement with a device also bypasses the stated regular-file refusal. Checking the descriptor only after an ordinary blocking open is insufficient for the FIFO case.

**Reproduction:** a scratch preload hook replaces a real three-byte regular file with a FIFO immediately before the actual `open64`. The unchanged library first successfully executes `statx(... S_IFREG ...)`, then blocks in `openat(... O_RDONLY|O_CLOEXEC ...)`; the supervised probe exceeds its two-second deadline. Without the injected interleaving it returns `abc`. This schedules a real path replacement; it does not falsify the metadata result. Evidence: `/work/scratch/im1a-r1-sol/race.strace` and `race.c`.

**Fix:** open with a platform-appropriate strategy that cannot block on a substituted FIFO (on Unix, `O_NONBLOCK`), then inspect **that descriptor's** metadata and reject non-regular files before reading. Specify the symlink policy. Following a symlink to a regular file matches Core's ordinary `fopen` behavior and need not be prohibited; refusing symlinks would be a separately documented filesystem safety restriction. Add an actual FIFO/replacement regression, not just the existing directory test.

### F2 — major: the JSON representation changes first-key and object traversal semantics

**Location:** `dash-coreimport/src/netstate/json.rs:36`; `dash-coreimport/src/netstate/banlist.rs:155` and `:166`.

Only the outer object uses the custom visitor. Every nested object is parsed into `serde_json::Value`, which keeps the last repeated key. Thus an entry containing `"banned_until":4102444800,"banned_until":2` supplies `2` here, while Core uses `4102444800` and retains the ban. Nested settings are affected too: `{"a":{"x":1,"x":2}}` exposes `x=2` here and `x=1` through Core's UniValue lookup. Top-level duplicates must continue to be rejected, as `ReadSettings` does.

There is another consequence of this representation: without `preserve_order`, nested object values are traversed in sorted-key order. Core accepts an object in `banned_nets` and traverses its values in **file order**. For two entries for the same subnet under keys `z` then `a`, Core applies the `a` entry last; this reader emits `a` then `z`. Core's subsequent `insert_or_assign` makes the order observable in the final expiration time. The regtest probe retained the last textual expiration, `4102444800`.

**Core basis:** `src/util/settings.cpp:114` (outer duplicate rejection); `src/univalue/lib/univalue.cpp:156` and `:233` (first matching key); `src/univalue/lib/univalue_get.cpp:53` (`getValues` returns the stored sequence); `src/net_types.cpp:60` and `:72` (ordered traversal, then `insert_or_assign`). A harness compiled with the pinned UniValue sources independently prints first-key values `2` and `1` for the short probes, and traversal order `2,9`.

**Fix:** preserve Core's nested first-key lookup and the order needed by the banlist object form. A recursive visitor can keep the first value while still fully consuming/validating later occurrences; preserve insertion order for object traversal. Add duplicate-field, nested-settings, and reversed object-key-order regressions. Merely enabling `preserve_order` does not fix last-key-wins.

### F3 — major: a skipped invalid ban can cause valid bans to be discarded

**Location:** `dash-coreimport/src/netstate/banlist.rs:182`–`:186`.

An entry with `version=1`, `address="invalid"`, and non-integer timestamps is dropped by Core **before** timestamp extraction. This reader instead returns an error, losing a valid subsequent entry as well. The fresh Core node successfully retained a following `1.2.3.4/32` ban through startup; the Rust probe returns `Json` for the invalid entry's `ban_created`.

This is not a resource or recursion bound. It changes a recoverable ignored entry into a whole-file failure. Core's `BanMan::LoadBanlist` recreates the ban map on read failure, so using that outcome at the loader boundary would remove otherwise valid bans.

**Core basis:** `src/net_types.cpp:67`–`:72` validates the subnet and skips it before constructing `CBanEntry`; `src/net_types.cpp:14` then reads the timestamps; `src/banman.cpp:32`–`:50` distinguishes successful loading from map recreation.

**Fix:** retain Core's validation order. Distinguish an invalid subnet from a supported special address awaiting the owning ban manager; do not drop every `parsed: None` entry, since the captured onion ban is valid. Either validate Core-supported address classes before timestamps or expose a raw/two-stage entry API that lets the owner do that validation before extracting timestamps. Regress with an invalid-address/bad-timestamp entry followed by a valid ban.

### F4 — major: the mempool API cannot recover the prefix Core already imports

**Location:** `dash-coreimport/src/netstate/mempool.rs:91`–`:103`.

Every later decode error returns only `FormatError` and drops the complete earlier records. Core applies each complete transaction record's fee delta and attempts admission before decoding the next one. A later corrupt transaction or tail does not roll back those effects.

**Independent Core repro:** a version-1 dump with one completely decoded record carrying delta `7`, followed by a corrupt second record, logs a deserialize failure. On clean shutdown Core writes the earlier delta back to its new mempool dump. The same Rust input returns only `Corrupt`, with no usable prefix. The repro uses an expired, empty transaction to isolate the unconditional fee-delta effect; it does not claim that such a transaction passes admission.

**Core basis:** `src/kernel/mempool_persist.cpp:79`–`:109` imports record by record; `:126` catches a later failure without clearing the pool or deltas. This is observable import behavior, not a speculative future requirement.

**Fix:** keep the strict full-file parser for validation/sniffing if useful, but add an iterator or recoverable parse outcome carrying complete records and a terminal error. Model the tail phases too: incomplete `mapDeltas` is not applied, but a complete map is applied even if the subsequent unbroadcast set fails. Do not return an incomplete transaction record. Add prefix/tail recovery tests against Core, including retained prior fee deltas. This can remain a decoder API without performing transaction admission or mutating a pool.

### F5 — major: mempool map/set decoding loses Core's duplicate handling

**Location:** `dash-coreimport/src/netstate/mempool.rs:57` and `:67`.

`mapDeltas` and the unbroadcast set are returned as vectors with repeated txids. A natural loader iterating the deltas would apply both values, whereas Core's map deserializer keeps the first value for a duplicate key. For repeated deltas `7` and `11`, Core's new dump contains one delta of `7`; the Rust decoder returns both. The unbroadcast collection likewise needs set semantics.

**Core basis:** `src/serialize.h:1232`–`:1242` uses `map.insert` (first insertion wins); `:1282`–`:1291` uses set insertion. `src/kernel/mempool_persist.cpp:111`–`:125` applies those decoded containers. The independent Core startup/shutdown repro confirms the first delta wins.

**Fix:** consume every encoded item, but deduplicate `mapDeltas` with first-value-wins and unbroadcast txids as a set. If their vectors intentionally represent raw serialized records, add and use an explicitly Core-semantic conversion API and correct the current map/set documentation before handing them to loaders.

Do **not** indiscriminately deduplicate the main transaction-record list: Core calls `PrioritiseTransaction` before testing whether a transaction is already present. The separate Core repro with repeated transaction records carrying `7` and `11` retains delta `18`. Admission uniqueness and per-record effects belong to the mempool owner.

### F6 — major: settings JSON numbers are rejected or rounded before Core would interpret them

**Location:** `dash-coreimport/src/netstate/json.rs:36`; `dash-coreimport/Cargo.toml:22`.

The default `serde_json::Value` number representation is narrower than UniValue's stored numeric literal. `settings::parse(b"{\"n\":1e400}")` rejects the otherwise valid object as “number out of range”; pinned UniValue accepts it and preserves `1e400`. `18446744073709551617` is accepted here but rounded to `1.8446744073709552e19`; Core preserves the original integer text. In a settings file containing ordinary wallet settings and an unused numeric key, eager numeric rejection can prevent the entire file from loading. This is additional to the declared ten deviations.

**Core basis:** `src/util/settings.cpp:95`–`:124` validates JSON/object shape and outer key uniqueness without converting every number; UniValue stores numeric token text (`univalue_read.cpp:417`) and checks numeric ranges only on a typed `getInt`/`get_real` call. `util/system.cpp:477`–`:483` explicitly ignores unknown settings without interpreting their values. Both examples were reproduced with the pinned UniValue C++ harness and the unchanged Rust reader; logs are `univalue-probe.log` and `probe.log` under the scratch evidence directory.

**Fix:** enable a lossless numeric representation (for example `serde_json`'s `arbitrary_precision` feature) and retain the literal through the settings decoder. Numeric conversion/range checks belong where a particular setting requests a numeric type. The existing file/depth caps already bound storage; silently rounding or rejecting valid numeric tokens is not the stated hostile-input bound. Add both examples as regressions.

### F7 — minor: `sniff`'s JSON heuristic is documented as format validation

**Location:** `dash-coreimport/src/netstate/detect.rs:170` and `:181`.

`sniff(b"{\"banned_nets\":42}")` returns `BanlistJson`, although `banlist::parse` rejects that input. A perfectly valid settings object can also contain this key. For binary formats the implementation really does call the full parser; JSON is classified by object syntax and key presence alone. Therefore the assertions that the whole format parses and corrupt instances yield `Unrecognized` are too strong.

**Fix:** document JSON classification as a structural hint and explicitly require the named reader, or strengthen validation and define the ambiguous JSON fallback. Keep `detect_named` clearly separated from full validation. Add the wrong-shaped `banned_nets` case and a settings object containing that key. This is a detection contract issue, not an observed ordinary-vector misclassification.

### F8 — minor: the test claims overstate truncation and hardening coverage

**Location:** `dash-coreimport/tests/netstate.rs:808` and `:1020`.

`assert_every_prefix_fails` advances by `len / 400`; large peers and fee-estimate files get sampled prefixes, not every prefix. The checksummed-file bit-flip test selects three bits per byte, despite its comment saying every single-bit flip. These are useful tests, but the stated coverage should be corrected or supplemented with exhaustive small constructed vectors.

The new huge-mempool-count regression also has one extra `0xff` after the eight count bytes. Its first transaction therefore starts with `ff ff ff ff`, then an over-range input count; it fails before reaching the advertised huge **output** count. Fix the encoding and verify the path it exercises. A sparse or modest backing buffer and allocation instrumentation can test the 4096 reservation clamp without allocating a gigabyte.

## Disposition of the ten declared deviations

Here **(a)** is an accepted deliberate safety bound, **(b)** needs a fidelity fix now, and **(c)** is a documented decoder/owner responsibility or evidence limitation.

| # | Verdict | Pinned Core basis and required disposition |
|---|---|---|
| 1. NaN decay rejected | **(a), accept** | `policy/fees.cpp:425`–`:428` uses comparisons that do not reject NaN. Refusing a non-finite decay is a narrow deliberate validation bound protecting the downstream estimator. Document it and retain the regression. This observation is limited to the specified compatibility difference. |
| 2. Depth 128 and file caps | **(a), accept** | UniValue's `univalue_read.cpp:20` sets depth 512; `util/settings.cpp:95` reads the entire file without these caps. The Rust depth/file limits directly bound hostile resource use. Reservation clamping changes allocation strategy, not accepted valid counts. Keep the exact 1 MiB/16 MiB/64 MiB/1 GiB/1 MiB limits discoverable for callers. |
| 3. Bad timestamp fails whole file | **(b), F3** | Failure for a bad timestamp on a **valid** subnet already matches Core. The actual difference is eager timestamp validation on an address Core skips; fix that ordering rather than weakening valid-entry numeric checks. |
| 4. Last repeated JSON key wins | **(b), F2** | Nested UniValue lookup is first-key-wins. Outer duplicates must still error. Preserve order for the accepted object-form banlist as well. Neither difference is a safety bound. |
| 5. Onion/I2P bans kept as text | **(c), accept at the decoder boundary** | `net_types.cpp:67` uses `LookupSubNet`; `netaddress.cpp:997`–`:1019` accepts single-host onion/I2P/CJDNS subnets. Keeping the exact text is information-preserving. The owning ban manager must resolve these classes and must not interpret `None` as permission to omit them. The broader special-IPv6 text handling needs the same explicit owner contract. |
| 6. Bucket ordering not validated | **(c), matches Core here** | `policy/fees.cpp:1005`–`:1028` copies fee boundaries and constructs `bucketMap` without an ordering test; the reader's omission is not an additional deviation. For addrman bucket placement, `addrman.cpp:330`–`:397` rebuilds/prunes state rather than validating serialized ordering in this decoder. Keep placement/pruning with the owner. |
| 7. peers formats 0–2 model-only | **(c), evidence limitation** | `addrman.cpp:244`–`:275` enables BIP155 from 3 and bucket XOR from 1; `:346` reads the asmap checksum from 2. The port matches those branches, and v24 writes only format 4. Label modeled legacy vectors accurately; add a pinned-Core legacy-layout deserialize harness when claiming independent legacy conformance. Do not describe these vectors as captured legacy files. |
| 8. `CheckAddrman` deferred | **(c), decoder/owner split** | `addrman.cpp:304`–`:405` performs validity filtering, collision handling, reference limiting, rebucketing, pruning and finally `CheckAddrman`. Those require the owning manager and netgroup/asmap state. Raw decoding may land first, but neither checksum success nor `bucketing_restorable` certifies a safe usable manager. The future owner must run this complete reconstruction/check path before exposing state. |
| 9. Mempool all-or-nothing | **(b), F4** | `kernel/mempool_persist.cpp:79`–`:126` preserves already applied effects. The existing API discards the prefix and cannot support that behavior without another decoder. Add recoverable decoding now; keep full parsing strict for sniffing. |
| 10. Duplicate txids kept | **(b) for map/set, (c) for transaction records; F5** | `serialize.h:1232` and `:1282` deduplicate containers. Main transaction records are different: `mempool_persist.cpp:85` applies each delta before admission. Preserve that sequence and let the owner enforce pool uniqueness. A blanket first-txid-wins filter would introduce a new mismatch. |

## Provenance, detection and security assessment

- Verified the peeled Core tag and the installed binary hash `e204e3163185b1bffcd3bf3532c0957d8cc11019eaf8f3dbc3b2fc82a7ab5bb4`. All **13** hashes in `PROVENANCE.md` match the files, including RPC ground truth; all **13** fuzz seeds equal their corresponding captured fixtures. The capture script drives Core's own test framework, collects RPC ground truth before stop, and copies the files after clean shutdown. It does not use these Rust readers to construct expected values.
- The capture's ordinary format-4 peers, BIP155 anchors, version-1 unobfuscated mempool, fee version words, wallets and ban fields agree with source and the passing fixture tests. The tests compare the relevant addrman rows, sources/ports, timestamps, bans, mempool ids/deltas/unbroadcast, and selected fee metadata. Fee tests do not independently establish every statistic or the complete fee-estimation algorithm, which is outside this reader.
- All twelve captured files are correctly detected by both entry points. The whitespace-led binary regression is valid and the fix falls through to the fee parser. Unsupported binary versions are intentionally reportable by name even when full parsing refuses them. Header detection does not certify the checksum or payload. JSON syntax alone cannot uniquely identify a settings file versus a banlist; see F7.
- Binary cursors maintain valid positions and check extents before slicing; compact-size conversions and bounded count products inspected here do not expose an additional overflow or panic. Address lengths are capped at 512, fee dimensions at 1000 buckets/1008 confirmations, peers entry counts at Core's limits, and initial reservations at 4096. Count loops consume input on every iteration and fail on missing data. JSON's recursion limit also bounds partial-tree destruction. No additional attacker-driven unbounded recursion/allocation was identified in the in-memory readers. This is not a proof that their largest permitted input fits every host's memory budget.
- `read_file_capped`'s saturation fix prevents the former `limit + 1` overflow, but its path check does not eliminate special-file races (F1). Pure-byte fuzzing does not exercise filesystem handling or symlink replacement.

## Independent validation

All review builds used `RUSTC_WRAPPER=` and `CARGO_BUILD_JOBS=12`; temporary test data used `/dev/shm/im1a-r1-sol-tmp` with mode **0700**. Cargo tests/clippy used `/work/scratch/im1a-r1-sol/target`, separate from the worktree's target. No reviewed source was changed.

- `cargo test -p dash-coreimport --all-features`: **PASS** — 31 library unit tests and 40 integration tests, including all 31 netstate integration tests; 0 failures, 0 ignored. The fixture/truncation/corruption suite completed locally.
- `cargo clippy -p dash-coreimport --all-features --all-targets -- -D warnings`: **PASS**.
- `cargo fmt --all --check`: **PASS**.
- `python3 -I node-fuzz/fuzzctl.py selftest`: **PASS**, 50 tests. Independently confirms f979ed80 fixes the registry-copy failure.
- `python3 -I node-fuzz/fuzzctl.py check`: **PASS**, 15 targets and 20 smoke/farm legs registered.
- Six short instrumented fuzz legs: **PASS**, all six budgets met; **1,595,097 executions**, **0 crashes**, **0 divergences**, **0 sink failures**. Used the unmodified shared `run.sh` from an exact `git archive f979ed80` scratch copy, its own `node-fuzz/target`, an isolated mode-0700 sink, pin `v24.0`, `--cpu-hours 0.0056 --jobs 1`. Reused a prebuilt RocksDB native archive from the identical dependency/profile to avoid rebuilding an unrelated storage dependency; Rust readers and targets were freshly compiled/instrumented. The runner has a 60-second minimum segment, so actual execution can exceed the requested ~20 CPU seconds per target. An initial build was stopped to reuse the native library. Earlier scratch runs were superseded after noticing the runner overrides `TMPDIR`; the final six counted runs use an isolated sink under `/dev/shm/im1a-r1-sol-tmp/fuzz-private`, so their actual subprocess `TMPDIR` is also in mode-0700 `/dev/shm` directories. Superseded runs do not contribute to the final budgets. This is a focused six-target check, not a rerun of the claimed entire 20-leg smoke gate.
- Scratch Rust probes, a pinned UniValue C++ harness, syscall-traced replacement test, and six fresh Core startup/shutdown probes confirm the findings. Non-fuzz evidence/logs are under `/work/scratch/im1a-r1-sol/`; fuzz findings remain under the isolated runner sink and are reported only in aggregate as required by PLAN §7.


| Fuzz target | Executions | Measured CPU hours | Result |
|---|---:|---:|---|
| `coreimport_anchors_dat` | 255,861 | 0.0064 | PASS |
| `coreimport_banlist_json` | 292,105 | 0.0073 | PASS |
| `coreimport_fee_estimates_dat` | 40,179 | 0.0104 | PASS |
| `coreimport_mempool_dat` | 388,661 | 0.0091 | PASS |
| `coreimport_peers_dat` | 207,848 | 0.0063 | PASS |
| `coreimport_settings_json` | 410,443 | 0.0102 | PASS |

Aggregate verdicts: `/work/scratch/im1a-r1-sol/fuzz-summary.json`; isolated run evidence retained in mode-0700 `/work/scratch/im1a-r1-sol/fuzz-evidence/`. The original `/dev/shm/im1a-r1-sol-tmp` tree was removed after all runs and Core probes stopped. Runner revision metadata is `-` because the source is a git archive; the archive was created explicitly from f979ed80.

The fuzz targets do reach the byte readers and shared transaction decoder, and repaired magic/checksum variants help keep successful envelope paths reachable. They check deterministic output, dimensions, extents and consistency between detection entry points; they **do not compare against Core's parsing/import semantics**. `sniff_agrees` only compares `sniff` with `detect_named`, which would both accept the wrong-shaped JSON in F7. Valid seeds improve coverage, but default mutation lengths are far below most file-size caps; the relevant caps/huge-count cases still need targeted tests. Byte targets cannot validate F1. A clean short fuzz result therefore does not close the semantic or filesystem findings.

## Handoff

Fix F1–F6 and add the focused regressions. Address F7–F8's detection/coverage contracts. The documented decoder/owner boundaries for special bans and addrman are acceptable for this reader-only change, but must remain explicit acceptance requirements when their owning loaders land. No change to the owning addrman, fee estimator, or transaction-admission engine is requested in this review.

**Verdict: CHANGES-REQUIRED.**

**Sol gate satisfied: no.**
