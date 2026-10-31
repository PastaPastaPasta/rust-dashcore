# BL-451a review r1 (Opus): SP1 Core-oracle differential (E1–E4)

- rust-dashcore `node/bl451-oracle-mnsync` at 1f7b10257 (base 6c833e82f, 4 signed commits)
- Dash Core oracle shim c3e388c24ce (on 375c64d2317): `oracle.h`, `oracle_mnsync.cpp`, Makefile
- Plan: DEC-626 (P1–P4)

**Verdict: CHANGES-REQUIRED.** 1 Major, 2 minor, 3 nits.

The shim is faithful and the P2 seam is shaped correctly; I have no findings on either.
The Major is about differential strength. Half of E4's comparison (whether the node starts
with the file, and the file left after the store) is checked against a model of the lifecycle
written in the harness, not against the node's own load/store code. A mutant that makes the
node refuse to start on every `IncorrectFormat` netfulfilled.dat/mncache.dat passes all of
`l1_mnsync`. The fix is small.

## Findings

### M-1: E4's `loaded` and `file_after` compare Core with a harness model, not with the node

- `node-l1/src/mnsync.rs:714-740` (`node_load`), `:783-786` (mncache `|obj| obj`), and the
  claims at `:9-11` and `node-l1/tests/l1_mnsync.rs:427-429`.
- Core: `flat-database.h:160-175` (`Read`), `:192-205` (`Store`); `netfulfilledman.cpp:15-29`;
  `masternode/meta.cpp:25-36`.

`node_load` gets the node's `loaded` from `LoadError::recreates()`. It gets `after` from
`encode_with_magic(&obj)`, under the hard-coded assumption "the store's own read of the file
gives the same verdict, so it writes" (`:732-733`). The node code that makes those decisions
never runs on the compared bytes:

- `flatdb::read` (the `Missing`/`recreates()`/fatal arms that `load` and `store` share),
- `flatdb::store` (verify first, then write),
- `MasternodeMetaMan::load_cache`, which E4 never calls (`|obj| obj`).

`netfulfilled_after_load` does call the production `load_cache`, but only after the harness
has already decided `loaded == true`, and only on a re-encoded rdashd-layout file that is
valid by construction. So it can never see a refusal.

**Failing scenario (run in my copy, env-gated mutant RVMUT=1).** In
`dashd-masternode/src/flatdb.rs:423`, `read` treats `IncorrectFormat` as fatal
(`Err(e) if e.recreates() && false`). rdashd would then refuse to start on any of D17's 15
cleared files, where Core recreates them. Results:

- `l1_mnsync`: **12/12 pass**, including `cleared_files_load_alike`, whose doc says "and so
  does the node, with the same object left and the same file written over them".
- dashd-masternode's own tests: only two **sporks** tests fail
  (`a_short_header_or_an_unopenable_file_recreates`, `the_store_file_round_trips_in_core_order`).
- No netfulfilled or meta test fails: neither crate's `load_cache` tests have a Format or Short
  case (`netfulfilled.rs:288-323`, `meta.rs:660-692`).

So for SP1's two files, this divergence from Core is caught only by chance, through sporks.

**Suggested fix (preferred).**
- Generalize `read_into`/`read`/`store` over the decoder and encoder, the way `decode_with`
  already generalizes `decode_into`.
- Add test-only `core_layout::load_with_magic(path, ms, &mut obj)` and
  `core_layout::store_with_magic(&obj, path, ms)` that run those same bodies. They stay
  `#[cfg(any(test, feature = "core-layout"))]`, so P2 and D3 still hold.
- In `node_load`, write the Core bytes into a tempdir. Take `loaded` from `load_with_magic`
  and `after` from the file `store_with_magic` leaves (or the untouched file when `!loaded`,
  through the same `read`).
- Keep `netfulfilled_after_load` for `CheckAndRemove`.

**Minimum acceptable.**
- Narrow the doc claims at `mnsync.rs:9-11` and `l1_mnsync.rs:427-429` to "Core's
  LoadCache/Store outcome agrees with the node's verdict-class mapping".
- Add `load_cache(true)` cases to `netfulfilled.rs` and `meta.rs` for a Format file (→ true,
  object cleared, and `store` rewrites the file) and a Short file (→ false, file untouched),
  so that RVMUT=1 is killed in the crate that owns these files.

### m-1: the netfulfilled peer pool cannot see key order or Core's re-keying on reload

- `node-l1/src/mnsync.rs:290-312` (`peer`), used by `fulfilled_script` and `fulfilled_store`.
- Core: `CNetAddr::operator<` orders by `(m_net, m_addr)`, then port; a `std::map<CService,…>`
  serialized at `SER_DISK`/`CLIENT_VERSION` is v1 (Tor/I2P are written as all zeros).

The pool has three addresses: 10.0.0.1 (two ports), 192.168.1.7 and 2001:db8::1. For these,
Core's (network, bytes) order and plain v1-byte order agree.

**Failing scenario (mutant RVMUT=2).** `NetFulfilledStore::write`
(`dashd-masternode/src/netfulfilled.rs:74`) emits services sorted by
`(v1_bytes(), port)` instead of map order. It **survives everything**: `l1_mnsync`, including
the fixed onion row (all its onions are v1 zeros, so their order is unchanged), and every
dashd-masternode test.

The E4 generator also never builds a store holding an onion. So the reload path is never
compared for:
- an onion re-keyed to IPv6 `[::]` and re-sorted,
- two collapsed keys where "first stays" (the rule the `read` doc at `netfulfilled.rs:84-85`
  claims).

**Fix.**
- Add to `peer()` an IPv6 `::1` (its v1 bytes sort before `::ffff:10.0.0.1`, but Core puts IPv4
  first) and one or two Tor v3 peers (ADDRv2 on the Core side). The step store then compares
  order across networks, and `fulfilled_store` gives E4 reloads that re-key and collapse.
- Re-run `generated_inputs_compare` to check that no generated input becomes a refusal.

### m-2: the seen-ban LRU differential never exercises the access bumps that decide survivors

- `node-l1/tests/l1_mnsync.rs:308-324`.
- Core: `unordered_lru_cache.h` `get`/`exists`/`_emplace`: every hit bumps `accessCounter`,
  and a re-emplace refreshes the entry.

The 1801-ban row inserts in order and probes only after the truncation. The random generator
(64 ops at most) never reaches 1800.

**Failing scenario (mutant RVMUT=3).** `SeenBans::get` (`meta.rs:243-249`) stops refreshing
`used`. `l1_mnsync` passes 12/12; only the crate's unit test
`seen_bans_truncate_to_the_most_recently_used_half` catches it. This is the Rust side's own
reading of Core's behaviour, with no Core cross-check.

**Fix.** Add a row:
1. 1800 bans;
2. `AlreadyHavePlatformBan(inv 0)`, `GetPlatformBan(inv 1)`, and a re-`SetPlatformBan(inv 2)`
   at a height ≥ its last one (which re-emplaces it);
3. the 1801st ban;
4. probe invs 0, 1, 2, 3 and 4.

### n-1: the sync generator never reaches "now < 900 with no tip update"

- `node-l1/src/mnsync.rs:158` (start = 1,000,000 + u16; time steps never fall below ~1e6).
- Core: `masternode/sync.cpp:30-34`: `GetTime() - nTimeLastUpdateBlockTip` with the initial 0.

An unforced `Reset` at a mock time in 1..899, before any `UpdatedBlockTip`, is skipped in
Core. The oracle accepts those times.

**Mutant RVMUT=4** treats a zero "last update" as "never", so the reset goes ahead. It
survives `l1_mnsync` and every dashd-masternode test. Production is faithful today
(`sync.rs:165-172`); this is a coverage gap.

**Fix.** Let one bit of the first script byte pick a start in 1..900 (and add a fixed row).

### n-2: magic-vs-network order is never tested with both wrong

- `node-l1/src/mnsync.rs:675-702` (`flat_input` edits) and `l1_mnsync.rs:395-406`.

P2's hash-before-magic order is covered: edit 5 flips a magic byte without re-hashing, which
gives `IncorrectHash`. Magic-before-network is not. Every edit makes a single change, and the
wrong-network case keeps the right magic. A `decode_with` that reads both headers and then
reports `Network` first would survive.

**Fix.** Add an edit that flips a magic byte and a message-start byte together and re-hashes
(Core: `IncorrectMagicMessage`), or run edit-3 files with `selector ^ 1`.

### n-3: `core-layout` is unified into rdashd's build under `--workspace --all-features`

- `dashd-masternode/Cargo.toml:21-23`, `node-l1/Cargo.toml:21`.

`cargo tree -e features -i dashd-masternode` does **not** show `core-layout` for:
- `-p rdashd`,
- `-p rdashd --all-features`,
- `--workspace`.

It **does** show it (`(command-line)`) for:
- `--workspace --all-features`,
- `-p rdashd -p node-l1 --features node-l1/oracle`.

So a `cargo build --release --workspace --all-features` links an rdashd whose dashd-masternode
carries the `pub mod core_layout`. The module is unreferenced, and production keeps its own
magic regardless (D3 holds): `decode_with` is private and `decode_into` passes `T::MAGIC`.
`dashd-chainstate/model` has the same exposure today. `test-support` instead has a
`compile_error!` under `not(debug_assertions), not(clippy)`.

**Fix.** Either copy that guard, or record in D24 that it is accepted like `model`. Note the
guard would also refuse `cargo test --release -p node-l1 --features oracle`.

## Shim fidelity (focus 1): no findings

- **E1** builds a real `CMasternodeSync` with a counting `NodeSyncNotifier`, then calls
  `UpdatedBlockTip`/`AcceptedBlockHeader`/`NotifyHeaderTip`/`Reset`/`SwitchToNextAsset`/
  `BumpAttempt`/`BumpAssetLastTime` directly. Blocks are a bare `CBlockIndex` with `nHeight`
  and `phashBlock`, which is everything `sync.cpp` reads. A null tip and a null header tip are
  allowed, as `CDSNotificationInterface` can pass them. The time is mock time throughout, and
  the state is read only through public getters.
- **E2/E3** call only public `CNetFulfilledRequestManager`/`CMasternodeMetaMan` methods. Each
  takes its own `cs_mapFulfilledRequests`/`cs`. The store bytes are `SER_DISK`/`CLIENT_VERSION`
  serializations of the base store, under the same locks, which is what `CFlatDB::CoreWrite`
  writes. `DoMaintenance` keeps its `ShutdownRequested()` check. Neither manager calls
  `LoadCache`, so `is_valid` stays false and the destructor writes nothing. `GetDataDir` does
  not create directories (`util/system.cpp:361-385`), so E2/E3 leave nothing on disk.
- **E4** runs Core's own `CFlatDB::CoreRead`, reached through a private member pointer, then
  `LoadCache(true)`, then the destructor's `Store`. This happens in a private `-datadir` that
  is forced and restored, with `ClearPathCache` on both sides, and refused while a session
  owns `gArgs`. The verdict comes from a separate `CoreRead` into a probe object, not from
  `LoadCache`'s own read. On the same unchanged file that is the same result.
- Every entry runs under `WithNetwork`'s `ProcessStateMutex`, and `MockTimeScope` restores the
  previous mock time. Refusals (flags other than 0/1, times outside 1..2^33, a truncated op,
  an undecodable ban message) are argument-domain checks only. Nothing the shim decides is
  Core's to decide.

## P2 (focus 2): met

- `decode_with` is private, and `decode_into`'s behaviour is unchanged (it passes `T::read` and
  `T::MAGIC`).
- `core_layout` is `#[cfg(any(test, feature = "core-layout"))]`, and only node-l1's `oracle`
  turns the feature on.
- `decode_into_with_magic` runs the same `decode_with` (hash, then magic, then network, then
  body, with `clear()` on Format) on the same bytes Core reads. It swaps only the magic and the
  version string.
- `read_core` matches Core on another version string (an empty object, `Ok`, trailing bytes
  ignored), and the in-place `nDsqCount` survives a later failure.
- `core_body`/`encode_with_magic` give the re-encode.
- Production magics are `rdashd-netfulfilled`/`rdashd-mncache`.
- See n-3 for the one leak path.

## ABI 30 (focus 4): no findings

- The four prototypes in `ffi.rs:511-517` match `oracle.h`: `int64_t`/`size_t`/`int`, with
  `kind`/`present` as `c_int`. `ABI_VERSION` is 30 on both sides.
- Rust owns every buffer, and nothing is handed back by pointer. `WriteBuffer` copies only when
  the size is ≤ cap and otherwise reports the required length.
- `with_buffer` retries once with that length. E1–E4 build fresh state per call, so the re-run
  is deterministic (E4 makes a new private datadir).
- Empty `script`/`file` slices pass a dangling non-null pointer with length 0, which the shim
  accepts and never dereferences.
- The record parsers check each sync record's op code, the `present`/bytes consistency and that
  nothing trails.
- `take_meta_info` walks the variable-length vote map; the ban message is a fixed 164 bytes at
  `SER_NETWORK`.
- The CService op argument is ADDRv2, as `oracle.h` documents; the fixed onion row depends on
  this and passes.

## 0ea399ae1 (the refusal filter)

It tightens, it hides nothing. The proptests and `generated_inputs_compare` now *fail* on an
oracle refusal: earlier `is_ok()` let `OutOfDomain` through, and before that up to 15 skips per
300 were allowed. `oracle_answer` maps only `ErrorKind::Argument` to `OutOfDomain`; every other
oracle error is a divergence.

## Checked and ran

- Read DEC-626, the full shim (`oracle_mnsync.cpp`, the `oracle.h` ABI 30 block, `WithNetwork`,
  `WriteBuffer`) and Core's `masternode/sync.{h,cpp}`, `netfulfilledman.{h,cpp}`,
  `masternode/meta.{h,cpp}`, `flat-database.h`, `unordered_lru_cache.h` and
  `util/system.cpp` `GetDataDir` at c3e388c24ce.
- Read all of `node-l1/src/mnsync.rs`, `node-l1/tests/l1_mnsync.rs`,
  `dash-oracle-sys/src/mnsync.rs`, the `flatdb.rs` diff, and the production `read`/`load`/
  `store`/`load_cache` paths and codecs in `netfulfilled.rs`, `meta.rs` and `sync.rs`.
- Spot-checked SB4 vector `a17-conflict-then-reload.json.gz`: only `core_commit`,
  `binary_sha256` and the archive hashes changed.
- Reflink copy `/work/scratch/bl451a-rv-opus` at 1f7b10257, run as the committed tree (see the
  note below):
  - `cargo test -p node-l1 --features oracle --test l1_mnsync`: 12/12 pass.
  - `cargo test -p dash-oracle-sys --features oracle --test mnsync`: 6/6 pass.
  - `cargo test -p dashd-masternode`: all pass.
- Env-gated mutants (one build). l1_mnsync passed 12/12 under every one:

  | Mutant | dashd-masternode tests |
  |---|---|
  | RVMUT=1: Format fatal in `flatdb::read` | 2 sporks tests fail |
  | RVMUT=2: netfulfilled keys written in v1 order | all pass |
  | RVMUT=3: LRU `get` does not refresh | 1 meta test fails |
  | RVMUT=4: a zero last-tip-update is "never" | all pass |

  The mutants are reverted in the copy; the copy is left for the manager.
- `cargo tree -e features -i dashd-masternode` for `-p rdashd`, `-p rdashd --all-features`,
  `--workspace`, `--workspace --all-features`, and `-p rdashd -p node-l1 --features
  node-l1/oracle` (n-3).

**Note for the manager:** the review worktree `/work/home/agent/workspace/rdc-wt/bl451` was
not frozen. It has an uncommitted edit to `dash-oracle-sys/tests/gov_regtest.rs` (a BL-477
stray-file test, mtime 18:49Z). I reverted it in my copy and reviewed the committed tree only.
