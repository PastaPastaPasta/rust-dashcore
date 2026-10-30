# SB6 round 2 — narrow Sol review

## Scope and pins

Reviewed fix-r1 on `node/sb6-dash-p2p` at **ccb936356eabf6366ab89a7c94503e17f5c901a4**, in the detached worktree `/work/scratch/sb6-r2-sol/wt`. The review range was `742570e55..ccb936356`, directed at prior Sol findings F1–F4, DEC-178, and D35–D37. Oracle-linked validation used detached test base **87a14e6a1c3824188afbe9213b22a148d7c71d16** at `/work/scratch/sb6-r2-sol/tb`. Its `dash-p2p` and the two decode matrix/model source files are identical to the reviewed head.

Contract: `briefs/REVIEW-COMMON.md` in node-program, with this task's disk-only TMPDIR and security-report instructions taking precedence. Core sources were read from `v24.0.0-rc.3`, **da567d7dba945143eee4d2ce1621c21b32089989**, through tag blobs and a scratch archive. No branch was edited, committed or pushed; no GitHub action or private-material access occurred. The `node/sb6-rng` follow-up, D38/D39, its test hooks and its P2a-net API changes were not reviewed.

## Prior findings

### F1 — CLOSED: atomic ban snapshot and ordered saves

`dash-p2p/src/ban.rs:146`, `dash-p2p/src/netcore.rs:661`.

`BanList::take_dump` sweeps, tests/clears dirty, and copies the entries under the same `inner` lock. A mutation cannot fall between the snapshot and dirty clear. A subsequent mutation takes that lock and sets dirty again, so a save in progress cannot acknowledge a change it did not capture. `dump_bans` holds the per-core dump mutex across snapshot, serialization, the synchronous store save, and dirty restoration on failure. Immediate ban/unban/clear, periodic `dump`, and shutdown all use that method; an older save cannot complete after a newer one. This matches Core's `BanMan::DumpBanlist` locking mechanism (`banman.cpp:47`).

Validation: the atomic-snapshot unit test and all three `tests/bans.rs` regressions passed: reverse-order save opportunity, mutation after snapshot, and failed-save retry. This closes both original races, including periodic dump versus StopNodes' dump, rather than merely changing the original two-ban outcome.

### F2 — CLOSED within the agreed SB6 decode scope

`dash-p2p/src/policy.rs:866`, `:885`, `:943`; `node-fuzz/src/models/p2p_decode_matrix.rs:777`, `:912`, `:960`, `:973`; `node-fuzz/src/models/p2p_decode_model.rs:57`.

The partition is complete and disjoint. A source-derived audit against the actual policy table and pinned base-sdk `stub` declarations found:

- **59 payload-reading rows = 10 oracle-judged + 32 matrix-judged + 17 raw processor-owned rows.**
- **74 total rows = those 59 + 15 rows that read nothing.**
- Thus the stated **32 out of scope = 17 raw + 15 no-read** includes rows outside the 59 payload-reading denominator. Adding 10 + 32 + 32 gives 74, not another partition of 59. D37's command lists themselves are correct; no payload-reading row is unclassified or counted twice.

The ten oracle-judged node reads are `clsig`, `isdlock`, `qsigrec`, `qfcommit`, `mnauth`, `spork`, `platformban`, `qgetdata`, `qdata`, and `getqrinfo`. The test base's overlay Rust leg calls `policy::decode`. Its strict-flag exception contains only `smlentry`, `mnlistdiff`, and `qrinfo`; `getqrinfo` cannot escape a Core-accept/boundary-reject disagreement that way. The other four overlay targets cover two no-read node replies and their embedded types, so 14 overlay targets and 10 SB6 reading rows are consistent.

For the 32 matrix rows I checked the field lists and custom reads against the pinned serializers/read sites: Core bools, version's optional tail, address encodings and unrestricted service CompactSize, locators, inventory, header counts and ignored tx counts, bloom bounds, governance layouts, compact-block/differential indexes, and fixed request/control fields. Embedded transactions and blocks explicitly delegate to the separately Core-judged consensus decoders. The matrix compares acceptance and consumed length; it distinguishes handler count returns from stream throws. `VersionTail` is accepted as a tail exception only for `version`. `LENIENT` is consulted after that verdict check, for re-encoding; adding a command to it cannot excuse a false rejection.

The boundary corrections are mechanisms shared by whole-message `decode` and typed `read`, through `read_as_core`:

- `sendcmpct` and `getqrinfo`: any nonzero bool becomes true. The latter's position is derived from a canonical CompactSize and checked offset arithmetic, covering multi-byte counts and truncation, rather than a fixed repro offset.
- `govobjvote`: signature count is bounded at 96 before decoding its bytes, and the completed signature must have size 65 or 96, matching the network serializer in `governance/vote.h:193`.
- `govsync`: the vData limit is checked and all nine bytes of the filter's subsequent three fields must exist and are consumed, matching `common/bloom.h:78`.
- `headers2`: only the strict-uncompress `InvalidValue` path triggers reconstruction. It follows Core's first-header defaults, seven-entry version list, duplicate-version insertion, valid-offset splice, out-of-list zero version, derived previous hash, wrapping timestamp delta and inherited bits. Original consumed length is retained. The independent matrix also reconstructs values, so acceptance alone cannot hide a wrong header.

The generic typed reader maps all five affected types through the same corrections, advances the original cursor only on success, and omits whole-message transport checks at the typed read point as documented. It does not add a second set of per-command correction implementations.

Validation: exact matrix/model source tests passed **9/9**, including 320,000 matrix comparisons (32 rows × five versions × 2,000 payloads), 29,600 model inputs, malformed counts, all listed bool fields, and the headers2 value tests. The five-category headers2 coverage assertions all exceeded 150. Test-base `l1_overlay` passed **5/5**, including 445 BS8 corpus cases, 37 O4 fixtures and deterministic mutations over all 14 commands, using the pinned archive identified below. SB6's typed-read boundary tests also passed. The committed tests exercise both legacy/basic contexts where applicable; this review did not add a new exhaustive BLS campaign.

### F3 — CLOSED: release-libstdc++ eviction ties

`dash-p2p/src/stdsort.rs:73`, `:106`, `:222`, `:266`; `dash-p2p/src/inbound.rs:57`, `:87`, `:97`, `:110`, `:126`.

The sort port uses the exact 16-element insertion cutoff, floor-log2(n) × 2 depth budget, median of first+1/middle/last−1 moved to first, unguarded partition, right-partition recursion followed by left iteration, and partial-sort heap fallback. Final insertion sort and heap child selection preserve libstdc++'s comparison direction and order. Moving the temporary by swaps keeps the comparison operands at the same logical values. Core's eviction comparator fall-throughs, bool directions, network/local preference and reverse connection-time comparisons match; the separate four-network stable sort remains stable. Tail removals preserve survivor order.

I compiled fresh reference binaries with **GCC 13.3.0**, including Core's actual tagged `node/eviction.cpp`. Fresh outputs were byte-identical to both checked-in traces:

- **1,749 sort cases**, comparator-call count, call-sequence hash and final-order hash; the Rust comparison passed, including the asserted heap-fallback cases. Trace SHA-256 `1977fb40c80a75cb15b1b4bec4f87052895d8d9f8e424fd2fb916b1aca2c948b`.
- **1,060 eviction cases**, selected peer plus ratio-protection survivor order. Trace SHA-256 `daccfa63cb3d9d9aaf27bd549cb93019c9b60e42caee855b354c3015829a24bd`; the exact-head Rust trace test passed.

D36 correctly pins observable tie order to the release compiler (`contrib/guix/manifest.scm:96`). It does not claim libc++ or every future libstdc++ has the same tie order. A stable or arbitrary Rust unstable sort is no longer on the candidate path.

### F4 — CLOSED: completion handles survive shutdown cancellation

`dash-p2p/src/netcore.rs:890`, `:957`, `:1010`, `:1220`, `:1244`, `:1302`.

Shutdown calls serialize through an async mutex. Interrupt stops new table insertion and stops the sweep at its loop boundary. Shutdown joins that sweep before dumping or removing the remaining peers, so a sweep-owned removal/finalizer cannot be bypassed by an empty table. The table write lock orders successful attachment, task registration and shutdown's table take. Every reader/writer is tracked in a JoinSet; shutdown aborts and joins the remaining tasks, then waits for in-flight attachment cleanup. Removal releases socket/grant/count state and invokes finalization once.

`Taken` retains the sweep handle or JoinSet in the awaiting future and restores it to its slot when that future is dropped. Its declaration/drop order restores the tasks before the shutdown mutex is released. An interrupted attachment cannot insert or spawn new tasks into a set another shutdown has taken. Therefore the next shutdown still has the completion handles it must await; cancellation cannot turn a pending join into an empty-slot success.

Validation: all three committed shutdown tests passed, covering a blocked sweep finalizer, a concurrent second shutdown, an attachment held in initialize, and cancellation while joining the sweep. I additionally paused a reader in a scratch socket callback, cancelled shutdown while it awaited the I/O JoinSet, and invoked shutdown again. The second call remained pending until the reader was released, then returned with one finalization and no task-owned peer handle. This independently covers cancellation at the other `Taken` await. The barrier models scheduling preemption; it is not a production blocking socket implementation.

The completion guarantee assumes D31's host shutdown order: stop/join the processor and connection-producing dial/accept loops. An attachment newly started after shutdown can still run initialize/finalize on its caller; D31 explicitly excludes that usage from the completion promise. Cancellation requires awaiting another shutdown before destroying processor state.

## New findings by severity

No new blocker, major, minor or nit finding established in the scoped fix. F1–F4 are closed.

## D35 and recorded residuals

- **D35:** the named standalone coverage exists and passed. `dial::tests::inactivity_check` now exercises Detecting/V2, all reason strings, initial timeout and exact 20-minute boundary. The teardown interleaving is forced by `tests/shutdown.rs`. Inbound eviction has both the Core invariant tests and the new differential trace. Stale-tip/extra-outbound eviction remains processor-owned; Core-on-Core session legs do not prove Rust processor behavior.
- **P2a-core `read_headers2`:** acceptable as an explicitly scoped downstream integration residual, not proof that every eventual node read uses SB6. Its existing reader separately performs the same lenient reconstruction. The follow-up should route it through the shared boundary or judge its values with the same matrix while retaining its hash-once behavior. This review approves the SB6 boundary/model; it does not approve or gate that P2a-core reader.
- **GovSync's missing fields:** the boundary now checks and consumes the full wire filter, but base-sdk's `GovSync` retains only its data vector. The original payload's `payload[consumed - 9..consumed]` correctly identifies nHashFuncs, nTweak and nFlags after a successful read, including nonempty filters and ignored trailing bytes. This is a documented API limitation, not another acceptance gap. A processor must preserve the original payload and use those fields for its post-read bloom policy. No complete governance processor is validated here.
- D37 honestly judges matrix verdicts and consumed bytes, not every Core exception text. The listed jointly-rejected text differences and a future base-message oracle are separate follow-ups; neither is used to excuse a Core-accepted alternate in these gates.

## Validation and limits

All checks used `nice -n 10`, `CARGO_BUILD_JOBS=8`, empty `RUSTC_WRAPPER`, and disk TMPDIR `/work/scratch/sb6-r2-sol/tmp` (0700). Worktrees and the standalone harness used their own target directories. The test sink was the review-created 0700 fixture directory `/work/scratch/sb6-r2-sol/fixtures`, never the real embargo sink.

- Exact head: `cargo test -p dash-p2p -p dash-clock --all-features -- --test-threads=4` — **140 dash-p2p + 2 dash-clock tests passed**, zero failures.
- Exact head: `cargo clippy -p dash-p2p -p dash-clock --all-features --all-targets -- -D warnings`, `cargo fmt --all -- --check`, and `git diff --check 742570e55..HEAD` — passed.
- Read-only scratch harness importing the exact matrix/model files — **9 tests passed**. Its `node_l1::Divergence` substitute carries only the field name; it does not substitute a decoder or a verdict. This avoids building unrelated RocksDB/fuzz-runner code. The harness used the host default Rust 1.99.0; worktree tests/clippy used repository-pinned Rust 1.98.1. Harness-only unused-code warnings are a consequence of importing private modules without the normal model dispatcher.
- Test base: `cargo test -p node-l1 --features oracle --test l1_overlay --test l1_addrman -- --test-threads=4` — **5 + 2 passed**. The initial default-cache attempt failed to link because its requested archive was absent there. The successful rerun used `/work/scratch/o1c/oracle-cache`, with `DASH_ORACLE_ALLOW_UNPINNED` unset: archive commit **6efa24f2152d39e7d88da02d04d627cec7eb79db**, verified SHA-256 **fb57f72733dbba7e60515edb3eec3715e2c4b36b6ef0d8f846555e4672987834**, matching test-base pins.txt. This was a pinned run, unlike r1's development-archive smoke.
- Fresh GCC reference regeneration and the additional cancelled-I/O probe — passed as described above.
- I inspected the supplied gate/rerun logs: the gate records scanner_parity and three dash-spv dashd_sync failures; solo reruns show scanner_parity **2/2** and dashd_sync **37/37** green. I did not repeat the full reported 3,682-pass gate or independently attribute those failures to BL-85/BL-20. The scoped checks reproduced no failure.

No new libFuzzer run, plateau campaign, complete Rust-vs-Core session comparison, full workspace gate, cross-platform binary build, or new hostile-input campaign was performed. This is the requested narrow r2 closure review, not a review of the next RNG series or every consumer's integration.

Scratch evidence: `/work/scratch/sb6-r2-sol/logs/{p2p-tests,matrix-tests,oracle-tests-pinned,clippy,fmt,stdsort-fresh,shutdown-probe,classification}.log`, `shutdown_probe.rs`, `stdsort_fresh.rs`, the fresh reference binaries/traces, `core/`, and `matrix-harness/`. Both detached worktrees remained clean at their exact pins. This report is uncommitted.

**Verdict:** APPROVE

Sol gate satisfied: yes
