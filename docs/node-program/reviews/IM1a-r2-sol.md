# IM1a round 2 — Sol (+SEC)

Reviewed **f979ed80aabdb5a9ab52fb5a709f5dc5caf82777 → 9974813c6fb4f40654f21754efb0644afae5cd61**, the single DEC-137 fix commit on `node/im1a-core-format-detect`, in `/work/home/agent/workspace/rdc-wt/im1a`. This is the requested narrow r2 confirmation of F1–F8, including the new JSON/subnet ports and their security boundaries. Followed `briefs/REVIEW-COMMON.md`, PLAN §7 handling, and the code-review-validator workflow. The reviewed branch was not edited, committed, pushed, or commented on. This report is uncommitted in the integration worktree.

**Verdict: CHANGES-REQUIRED.** The original F1–F8 are closed in the readers and their focused regressions. One remaining inconsistency in the changed validation target prevents the Sol gate: the fee-estimate fuzz model rejects the NaN decay that the corrected reader and Core accept. The memory table also overstates its measured amplification as a worst-case ceiling.

All Core citations below refer to **v24.0.0-rc.3**, peeled commit **0364ed626ec2eaa518bf1ae4a0566ce638c7fe18**. Source was independently archived from that tag into `/work/scratch/im1a-r2-sol/core-v24`; no moving Core checkout was used as the reference. The executable used for nine fresh Core probes has the provenance SHA256 **e204e3163185b1bffcd3bf3532c0957d8cc11019eaf8f3dbc3b2fc82a7ab5bb4**. Numeric-host comparisons exercised the installed **glibc 2.43** through `getaddrinfo` with Core's `AF_UNSPEC`, `SOCK_STREAM`, `IPPROTO_TCP`, and `AI_NUMERICHOST` arguments.

## Findings, by severity

### R2-M1 — major: the fee-estimate model still rejects a Core-accepted NaN decay

**Location:** `node-fuzz/src/models/coreimport_netstate.rs:360`.

The reader now correctly uses `decay <= 0.0 || decay >= 1.0`, matching `src/policy/fees.cpp:425`–`:428`. Both comparisons are false for NaN. The model's statistics check still uses `!(s.decay > 0.0 && s.decay < 1.0)`, which is true for NaN, and returns `Divergence { field: "fee_estimates.dat stats shape" }` for a valid parse. The earlier determinism check already accommodates NaN; execution reaches this inconsistent shape check.

**Independent reproduction:** change only the first decay word of the captured populated `fee_estimates.dat` to a quiet NaN. The Rust reader succeeds. A fresh pinned Core node starts, loads the file, and writes the NaN decay back on clean shutdown. Compiling the exact current `fee_estimates_dat` function in a scratch probe produces:

```text
ordinary=Ok(())
NaN=Err(Divergence { field: "fee_estimates.dat stats shape" })
```

Evidence: `/work/scratch/im1a-r2-sol/fee-model-probe.rs`, `fee-model-probe.log`, `core-probes.json`, and `fee-nan-debug.log`. The scratch probe substitutes the unused release parameter and supplies the detection-consistency helper; the statistics function body is extracted from the reviewed source. This is a direct model reproduction, not a claimed full libFuzzer run.

**Impact:** mutations to an accepted decay produce false findings in the registered safety-validation target. `Harness::run` records these findings and ultimately stops at its divergence limit (`node-fuzz/src/lib.rs:167`). Passing a short smoke run does not establish that this target's invariant matches the newly accepted inputs.

**Fix:** make the model's decay predicate match Core's comparisons and add a regression that invokes the model with NaN in each horizon. Update `ConfirmStats::decay`'s finite-range-only documentation at `dash-coreimport/src/netstate/fee_estimates.rs:33` while making that correction. Retain the reader's NaN acceptance.

### R2-m1 — minor: the JSON amplification documentation is not a worst-case bound

**Locations:** `dash-coreimport/src/netstate/mod.rs:38` and `dash-coreimport/src/netstate/json.rs:27`.

The table says the worst shape takes up to 65 times the input and gives a 1 GiB peak for 16 MiB. The constant's documentation still says approximately 25 times. Neither is a general peak-memory ceiling.

**Independent reproduction:** parse a valid banlist containing an empty `banned_nets` array and an unused `extra` array of repeated single-child array chains. Four nested arrays reproduce the documented measurement: **16,777,208 input bytes, 1,065,648 KiB peak RSS**, about **65.04×**. With **510** nested arrays per item, still within Core's 512-container limit including the root object and outer array, the reader succeeds with **16,777,180 input bytes, 1,197,272 KiB peak RSS**, about **73.08×** (1.14 GiB). Measurements used separate processes and `/usr/bin/time`; RSS includes the input and process overhead. Evidence: `amplify.rs` and `amplification-depth{4,510}.log` in the scratch directory.

**Fix:** label these as measurements of specific shapes on this allocator, record the deeper case, and remove the stale 25× figure. If a peak-memory ceiling is intended, establish it separately; the input cap alone is not that ceiling. The actual allocation remains bounded by input size and depth, so this finding does not require changing the accepted file-size cap.

## F1–F8 confirmation

| R1 finding | Disposition and independent evidence | Pinned Core basis |
|---|---|---|
| **F1 — file replacement** | **Closed on the reviewed Linux path.** Recompiled and replayed the original preload interleaving: a regular file becomes a FIFO immediately before `open64`. It now returns `InvalidInput` within the two-second supervisor deadline. Trace: `O_RDONLY\|O_NOCTTY\|O_NONBLOCK\|O_CLOEXEC`, followed by descriptor-based `statx(... AT_EMPTY_PATH ...)` identifying `S_IFIFO`. A regular-file trace confirms `F_GETFL` and successful `F_SETFL` clearing `O_NONBLOCK`. Device, symlink, directory, cap, and maximal-limit regressions pass. | Core's file helpers ordinarily follow symlinks through `fopen`; descriptor/type refusal is the explicit DEC-137 safety restriction. No prior pathname type check remains. |
| **F2 — first key and object order** | **Closed.** The original repeated `banned_until` reads the first value; nested `x` reads `1` while retaining both members; object-form bans apply `z` then `a` in textual order. Fresh Core probes confirm first-field lookup and last-entry replacement. | `univalue/lib/univalue.cpp:156` (`findKey`); `univalue_get.cpp` (`getValues`); `net_types.cpp:60`–`:72`. |
| **F3 — ban check order** | **Closed.** The invalid-address/bad-timestamp r1 input drops that entry and retains the following valid ban. A fresh Core startup retains the same valid ban. Subnet validity is decided before timestamps, and unknown versions are dropped before address access. | `net_types.cpp:60`–`:72`; `netbase.cpp:760`–`:793` (`LookupSubNet`); `netaddress.cpp` subnet constructors and special-address parsers. |
| **F4 — retained mempool prefix** | **Closed.** `load` returns the complete first record and delta `7` when the second record is corrupt; strict `parse` still errors. Independent cuts through the map retain prior records but apply no partial map; cuts through the set retain the complete map but apply no partial set. Fresh Core startup/shutdown probes retain delta `7` after both transaction and set failures. | `kernel/mempool_persist.cpp:79`–`:126`: record effects, then the complete map, then the complete set, with no rollback on the later catch. |
| **F5 — duplicate map/set values** | **Closed.** The original repeated map values `7,11` yield one value `7`; repeated unbroadcast ids yield one set member. Main transaction records remain repeated and ordered. Fresh Core probes confirm the map's `7` and repeated record effects totaling `18`. | `serialize.h:1232`–`:1242` (`map.insert`), `:1282`–`:1291` (set insertion); `mempool_persist.cpp:85` applies each record delta before admission. |
| **F6 — numeric literals** | **Closed.** `1e400` and `18446744073709551617` remain exact text. A fresh Core node accepts both as unused settings. Typed integer accessors enforce whole-token integer syntax and the requested range. | `univalue_read.cpp` number-token storage; `univalue.h:137`–`:145` (`getInt`, `from_chars`); `util/settings.cpp:95`–`:124` outer-object/key validation. |
| **F7 — detection contract** | **Closed.** `sniff` explicitly documents JSON as a structural hint, including `{"banned_nets":42}` and settings/banlist ambiguity. The named reader remains authoritative. Both focused cases pass. | Core selects these readers by their known datadir filenames; JSON object syntax does not independently identify their purpose. |
| **F8 — coverage and huge-count encoding** | **Closed.** Tests explicitly document exhaustive prefixes through 64 KiB and sampled larger files; bit-flip coverage states its selected positions, with all eight positions exercised for the small anchors vector. The huge-output-count vector now encodes zero inputs and `2^25` outputs correctly; the independent probe confirms an empty retained prefix and a 4,096-record initial reservation. NaN reader acceptance also agrees with a fresh Core load/write probe. | Core `ReadCompactSize`/`MAX_SIZE`; `policy/fees.cpp:425`–`:428`. The remaining model inconsistency is separately reported as R2-M1. |

## New ports and security assessment

`json.rs` preserves the complete member sequence, first-key lookup, and numeric token spelling. Its UTF-8 filter follows the pinned UniValue filter, including overlong normalization, surrogate joining, and byte strings for accepted code points above U+10FFFF. An independently compiled pinned-UniValue harness and the actual Rust reader agreed on **41,079** documents, including **10,141** accepted documents: UniValue's JSON fixtures, exhaustive short token combinations, duplicate members, numeric boundaries, byte-string/escape mutations, generated trees, and depth-limit cases. Acceptance and the complete ordered value tree were compared, not just successful parsing. **Zero mismatches.**

`subnet.rs` follows the pinned `LookupHost`/`LookupSubNet` split, last slash, bracket removal, `ParseUInt8` fallback, contiguous masks, IPv4-mapped addresses, internal/TORv2 legacy prefixes, onion checksum/base32 handling, I2P handling, and CJDNS option. **29,426** fresh numeric-host cases were compared to glibc, with **13,391** accepted cases and **zero mismatches**. These included decimal/octal/hex IPv4 forms and range boundaries, bare `0x`, IPv6 syntax, mapped IPv4, brackets, numeric scope limits, and existing/nonexistent interface names. Both `Subnet` enum order and field order reproduce Core's network/address/mask ordering; ban entries now emerge in `banmap_t` order.

No new input-driven panic, unchecked count-sized reservation, or unbounded recursion/allocation was identified in the new ports. JSON size is checked before parsing, recursion stops at 512 open containers, and partial-tree destruction passed the deep malformed cases. Numeric accumulators stop at their checked limits. Subnet work/storage is proportional to the supplied text; fixed-size SHA3 state and mask processing do not depend on encoded counts. The only new subnet FFI call receives a live NUL-terminated `CString`. Files are read only after the opened descriptor is confirmed regular; the FIFO replacement and `/dev/zero` checks establish the relevant Linux filesystem regression closures. This assessment does not turn a finite input cap into a small RSS guarantee; see R2-m1.

## Bounds, deviations, and host inputs

- **Accepted explicit safety restrictions:** the 16 MiB peers, 1 MiB anchors, 1 GiB mempool, 64 MiB fee, 16 MiB banlist, and 1 MiB settings input caps; 4,096-element initial reservations further limited by available bytes; and descriptor-based refusal of non-regular files. These are the DEC-137 bounds. Larger legitimate mempool dumps are explicitly refused as a whole, whereas Core streams/applies their prefix. The table makes this consequence clear.
- **Core limits, not added differences:** JSON depth is now Core's 512; compact-size, fee dimensions, addrman counts, and BIP155 address limits retain the Core checks. NaN rejection and nested last-key behavior have been removed. Bucket-order omission matches this Core reader rather than introducing a new validation rule.
- **Accepted host inputs for this Linux/glibc target:** named scopes depend on interfaces present when importing. Existing names work only for `fe80::/10`, `ff?1::`, and `ff?2::`; global/other multicast names fail. Numeric scopes require unsigned decimal digits fitting 32 bits, with no sign or whitespace. Scope is excluded from subnet identity, matching Core. `-cjdnsreachable` is supplied explicitly through `parse_with`, with the Core default off. No DNS lookup is performed.
- **Accepted decoder/owner responsibilities:** addrman rebucketing, filtering, pruning, and `CheckAddrman`, plus mempool transaction admission and pool membership checks. Special bans now have typed subnet identities, so the former raw-text/`None` ambiguity is gone. Legacy peers layouts remain modeled evidence, not captured legacy files.
- **Platform evidence limit:** this review establishes Linux/glibc behavior. Non-Unix text-mode translation and other platforms' native resolver/interface behavior were not validated. A future platform loader must supply that platform's Core-equivalent input handling; the documentation is not evidence of universal cross-platform fidelity.

## Independent validation

Cargo builds/tests used **`RUSTC_WRAPPER=`**, **`CARGO_BUILD_JOBS=12`**, and **`TMPDIR=/dev/shm/im1a-r2-sol-tmp`**, mode **0700**. Cargo used the reviewed worktree's own `target/`; no shared `CARGO_TARGET_DIR` was set. Supplemental compilations also used that temporary directory, with `RUSTC_WRAPPER=` for Rust compilations. Scratch sources, executables, and logs are under `/work/scratch/im1a-r2-sol`, mode 0700. All nine private Core nodes were stopped, and the temporary `/dev/shm` tree was deleted afterward.

- `cargo test -p dash-coreimport --all-features`: **PASS**, **36 unit + 54 integration tests**, including **45 netstate** tests; zero failures/ignored tests.
- `cargo clippy -p dash-coreimport --all-features --all-targets -- -D warnings`: **PASS**.
- `cargo fmt --all --check` and fix-range `git diff --check`: **PASS**.
- Original r1 Rust probe recompiled against the reviewed crate; separate assertion probe covers the new recovery API, map/set phases, duplicates, numeric text, detection, huge output count, NaN, and file bounds: **PASS**. Strict mempool parsing intentionally still errors on corruption; the newly tested `load` API supplies the prefix.
- Original scheduled FIFO replacement, with syscall trace and two-second deadline: **PASS**. Regular-file trace confirms nonblocking-flag clearing.
- Pinned UniValue and live glibc comparisons: **PASS**, counts above.
- Nine fresh Core startup/shutdown probes: **PASS** — invalid-ban skipping, object ban order, first repeated field, settings numbers, damaged mempool prefix, first map value, repeated transaction deltas, complete map before set failure, and NaN fee decay.
- `python3 -I node-fuzz/fuzzctl.py check`: **PASS**, 15 targets/20 smoke legs/20 farm legs registered. `selftest`: **PASS**, 50 tests. These validate runner/registry behavior, not the fee model's NaN invariant.
- Exact fee model function probe: **CONFIRMED R2-M1**. No new six-target instrumented smoke campaign was claimed or run for this narrow confirm.

The implementer's unrepeated validator pass is not treated as approval. This independent four-pass review verifies the final commit and supplies concrete evidence for the remaining findings.

Fix R2-M1 and correct the memory documentation, then run the targeted model regression and affected checks. No reader semantic redesign is requested.

**Verdict: CHANGES-REQUIRED.**

Sol gate satisfied: no
