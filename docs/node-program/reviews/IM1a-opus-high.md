# IM1a — Opus-high sequential review (+SEC)

Reviewed **e8cdec55 → dd013473** (7 commits) on `node/im1a-core-format-detect`, worktree
`~/workspace/rdc-wt/im1a`, after Sol r1/r2. I followed `briefs/REVIEW-COMMON.md` with its security
section. The branch was not edited, committed or pushed. This file is uncommitted. All Core
citations are **v24.0.0-rc.3** (`git show v24.0.0-rc.3:<path>` in `~/workspace/dash`). I take
R2-M1 (NaN decay) as verified, as the brief asks; R2-m1 is re-checked below.

**Verdict: APPROVE-WITH-NITS.** There are no blockers and no majors. The five minors are about the
documented contract (the docs, the bounds table and the fuzz models), not about how the readers
decode. I'd fix m1 and m3 before merge; the rest can be a follow-up.

## What I ran

All runs used `TMPDIR=/dev/shm/im1a-oh-tmp` (mode 700), `RUSTC_WRAPPER=`, `nice -n 10`. Load
average was about 96 on 32 vCPUs, so wall times are only indicative. Scratch is in
`/work/scratch/im1a-opus-high/` (`gen/` scripts, `uv/` Core UniValue judges, `probe/` reader probe).

- `cargo test -p dash-coreimport`: all pass, including `netstate` 46/46, `netstate_memory` 1/1 and
  the 36 lib tests.
- **Core-judged JSON differential.** I compiled Core v24.0.0-rc.3's own
  `src/univalue/lib/*.cpp` plus a 40-line judge that emulates `ReadSettings` (empty file, `read`,
  `isObject`, duplicate top-level key). For each input it dumps the full tree: types, number text,
  string bytes in hex, member order and duplicates. I compared that with `settings::parse`. The
  corpus was seeds covering every lexer and grammar quirk, depths 510 to 513, UTF-8 filter edge
  bytes and surrogates, plus 40,000 random mutations, run with two seeds. Result: **84,277
  inputs, 0 mismatches** (2,852 that Core accepts).
- **Core-judged `getInt<int>` / `getInt<int64_t>`** (the `std::from_chars` path used for the ban
  version and timestamps): **2,872 inputs, 0 mismatches**.
- **`LookupSubNet` differential against glibc 2.43 `getaddrinfo`.** Python `socket.getaddrinfo`
  with Core's hints (`AF_UNSPEC`, `SOCK_STREAM`, `IPPROTO_TCP`, `AI_NUMERICHOST`) stood in for
  glibc. I re-implemented `LookupSubNet`, `ParseUInt8`, `SetLegacyIPv6`, the internal-prefix
  refusal and both `CSubNet` constructors, which gives the decision table the Rust code ports.
  The corpus was 96 host forms by 41 netmask forms, plus 6,000 mutations. Host forms covered every
  `inet_aton` shape, `0x`, octal, IPv4 tails, scope ids by number, by interface (`lo`, `eth0`) and
  malformed, brackets, and the TORv2 and internal prefixes. Netmask forms covered CIDR, `+`, `+-`,
  leading zeros, hex and decimal `inet_aton` masks such as `4294967040`, IPv4-mapped masks,
  bracketed masks and scoped masks. Result: **9,659 inputs, 0 mismatches** (1,012 accepted).
- **Hostile files** through `read_file_capped` and each reader (release probe, `ru_maxrss`):

| input | result | peak RSS / input |
|---|---|---|
| `banlist.json`, 16 MiB of 510-deep `[[…0…]]` chains in an unused member | Ok | **72.99×** (1,195,768 KiB), matching the table's 73× |
| `mempool.dat`, 256 MiB of 10,324,440 minimal 26-byte records | Ok, 51 s | **3.46×** (905,744 KiB), see m2 |
| `peers.dat`, 65,536 v2 entries plus 3.75 M bucket references, 16 MiB, valid checksum | Ok, 0.27 s | 3.31× |
| `fee_estimates.dat`, largest valid shape (1,000 buckets, 1,008 periods, 3 horizons, 48 MB) | Ok | 2.05× |
| `settings.json` at depth 512 / 513 | Ok / refused | stack, see m3 |
| a FIFO, a symlink to `/dev/zero`, `/dev/tty` | refused immediately (`not a regular file` / ENXIO), never blocked | — |
| `sniff` on the 16 MiB `peers.dat` | `PeersDat`, format 4 | 3.30× |

## Findings

### m1 — minor: the fee-estimates staleness rule is missing from the loader contract

`dash-coreimport/src/netstate/mod.rs:68`–`78` lists what a file does not decide and what the
owning loaders must not skip. `fee_estimates.rs:1`–`10` and `:154`–`158` don't mention it either.
Core does not read `fee_estimates.dat` at all when the file is older than `MAX_FILE_AGE` (60 h),
unless `-acceptstalefeeestimates` is set. See `policy/fees.cpp:570`–`575`
(`GetFeeEstimatorFileAge() > MAX_FILE_AGE && !read_stale_estimates → return`) and
`policy/fees.h:31`.

**Scenario:** a datadir last run 3 days ago. Core starts with default estimates. A loader built
from this reader's contract imports the old estimates and serves stale fee rates where Core would
not.

**Fix:** add the file's mtime, the clock and `-acceptstalefeeestimates` to the "What a file does not
decide" list. Add the 60-hour rule to "what the readers leave to the owning loaders", and cite it in
`fee_estimates.rs`.

### m2 — minor: the bounds table has no memory row for the binary readers; `mempool.dat` is about 3.5× its size

The table (`mod.rs:32`–`42`) bounds memory only for JSON. `mempool::load` keeps one 64-byte
`MempoolTx` (`mempool.rs:60`) per record of at least 26 bytes, in a doubling `Vec`. Measured at
256 MiB: **3.46× including the input**. At the 1 GiB cap (`mempool.rs:39`) that is about
**3.5 GiB of RSS** and a few minutes of CPU on this box for a hostile file. It is bounded and
proportional, but not stated. `error.rs:3` ("never allocates more than the input length can
justify") is too vague to check. `peers.dat` reaches 3.3× through `bucket_refs`.

**Fix:** add a row per binary reader: the peak multiple of the input (mempool ≈ 3.5×, peers ≈ 3.3×,
fee estimates ≈ 2×, anchors ≈ 1×) and the time order. Optionally lower the mempool cap, or offer a
streaming visitor, since Core itself streams and applies records one by one.

### m3 — minor: the JSON recursion needs about 190 KiB of stack (release) and up to 1 MiB (debug); overflow is an abort

`json.rs:17` says the depth limit is enforced "in a recursive reader whose frames are small". Core's
`UniValue::read` keeps its stack on the heap (`std::vector<UniValue*> stack`, `univalue_read.cpp:267`),
so a 512-deep file cannot exhaust Core's thread stack. Measured with `settings-depth512.json`
(511 `{"k":` levels around `{}`) on threads of a given size:

- release: overflows at 128 KiB, parses at 192 KiB;
- debug: overflows at 512 KiB, parses at 1 MiB.

A stack overflow is `fatal runtime error: stack overflow, aborting`, which is a process abort, not a
`FormatError`. A hostile `banlist.json` would kill a caller that parses on a small or already deep
stack, for example a debug build inside a deep async stack, or a pool thread created with a small
`stack_size`.

**Fix:** either state the requirement (at least 256 KiB of free stack in release, 1 MiB in debug)
next to `MAX_JSON_DEPTH` and the public entry points, with a test that parses depth 512 on a thread
of that size, or port Core's explicit-stack loop. The grammar already matches (0 mismatches above),
so the second option is mechanical.

### m4 — minor: the six fuzz models judge self-consistency on the accept path only; one restates the reader

Every model starts `let Ok(..) = reader(data) else { return Ok(()) }` (`coreimport_netstate.rs:62`,
`:103`, `:146`, `:235`, `:272`, `:343`), so a reader that wrongly **refuses** an input Core accepts
is never a divergence. Within the accept path:

- **Independent** (good): the mempool byte accounting and stage rules (`:280`–`:316`), the banlist
  order written out as Core's `CSubNet::operator<` (`:164`–`:181`), the mask-shape checks, the
  entry-size lower bounds, and `sniff` agreeing with `detect_named`.
- **Mirror:** the fee-estimates shape predicate (`:362`–`:370`) restates the reader's own checks
  line for line. R2-M1 is an instance of what a mirror catches: it can only drift from the reader;
  it cannot catch the reader drifting from Core. `banlist` `:186` re-runs `lookup_subnet` on its
  own output, which is self-consistency, as the comment says. The determinism checks are trivially
  true for pure functions.

The JSON and subnet fidelity therefore rests on hand-derived integration-test verdicts. My Core and
glibc differentials above found nothing, but they live in my scratch directory, not in the
program's gates.

**Fix:** turn the two differentials into permanent judges. The UniValue judge is Core's four
`univalue` files plus about 40 lines, and builds in seconds. `LookupSubNet` can be judged against
the host's `getaddrinfo` called through libc with Core's hints. Host them as a test or as an
external-judge target on the O3 farm, so accept/refuse and tree shape are judged by Core rather than
by the reader.

### m5 — minor: the resolver emulation is glibc's, but the docs present the host resolver as an input rather than a difference

`mod.rs:68`–`74` and `subnet.rs:9`–`15` say the platform resolver is "an input of Core's own
decision, not a difference from it". The code, however, implements glibc's rules on every host:
`inet_aton_exact` (`subnet.rs:233`) is glibc's `__inet_aton_exact`, and `scope_id_ok`
(`subnet.rs:351`) is glibc's `__inet6_scopeid_pton`, with a first-character digit check. Only
`if_nametoindex` is taken from the host. On a non-glibc host, Core asks that host's libc. As I
recall FreeBSD's `ip6_str2scopeid`, which KAME-derived resolvers such as macOS's share, its numeric
fallback is a bare `strtoul` check (`*ep == '\0'`), which takes `+5`, a leading space and an empty
scope (`2001:db8::1%`). This module refuses all three. I have not checked that against Apple's
libinfo source. Musl and Windows have their own rules.

**Scenario:** a macOS Core datadir with a ban `fe80::1%+4/64`. Core on macOS keeps the ban; this
reader drops it as an invalid subnet. I could not check this here because agentbox has no Mac; the
glibc side is verified (0 mismatches).

**Fix:** state that the module is glibc-exact, and list non-glibc hosts (macOS, Windows, musl) as a
documented deviation in the bounds or deviation table. Alternatively, on those hosts call the
platform `getaddrinfo` with Core's hints instead of the port.

### n1 — nit: anchors post-processing is missing from the loader duties

Core keeps at most `MAX_BLOCK_RELAY_ONLY_ANCHORS` (2) anchors (`net.cpp:4027`–`4030`) and deletes
`anchors.dat` after every read, successful or not (`addrdb.cpp:249`). Add both to
"what the readers leave to the owning loaders" (`mod.rs:76`).

### n2 — nit: for a replaced duplicate ban, `BanEntry::address` comes from the last entry; Core's key comes from the first

`banlist.rs:105` uses `BTreeMap::insert`, which keeps the first key and replaces the value. The
value carries the later entry's `address` text. In Core, `insert_or_assign` keeps the first
`CSubNet` key, and that key's `ToString` (in `listbanned` and the `banlist.json` Core writes back)
includes the first entry's `%scope`. For example, `fe80::1%2` followed by `fe80::1%3`: Core
displays `%2`, while the reader's `address` is `fe80::1%3`. The ban itself is identical. A one-line
doc note on `BanEntry::address` is enough.

### n3 — nit: `NetAddr::Invalid` and `NetAddr::Ipv6([0; 16])` are the same `CNetAddr` in Core

Core represents an ignored address as `NET_IPV6 ::` (`UnserializeV2Stream`, `SetLegacyIPv6`), which
equals a stored `::` under `CNetAddr::operator==`/`<`. The reader gives them distinct values
(`netaddr.rs:34`). A consumer that deduplicates or keys on `NetAddr` (Core's `mapAddr`) will
disagree with Core on such a pair. Both are invalid and get pruned, so there is no behavioural
effect today. Note it on the enum, or normalise.

### n4 — nit: `read_file_capped` opens a device node before refusing it

The descriptor check is race-free: `open(O_NONBLOCK|O_NOCTTY)`, then `fstat` on the descriptor,
which Sol r2 F1 confirmed. I replayed a FIFO, a symlink to `/dev/zero` and `/dev/tty`. However,
`open` itself has driver side effects for some nodes (a serial line raises DTR, a tape device
rewinds). Core's `fopen` has the same effect, so this is not a deviation. On Linux,
`O_PATH` + `fstat`, then reopening `/proc/self/fd/N`, would avoid it if wanted.

## Checked and found correct (no finding)

- **The JSON bound is worst-case (R2-m1 is closed).** Requested bytes per input byte are at most
  64. A container with n children owns at least n+1 bytes of its own syntax and a `Vec` of at most
  max(4, 2n) slots; that ratio peaks at n = 1 (128 / 2). An object member is 224 / 5. Scalars are
  at most 8 per byte. A chain of depth d around a scalar, as a root element, costs
  (128d + 88) / (2d + 2) < 64 including the root slot at 2× and `values()`. `Value` = 32 and
  `(JsonStr, Value)` = 56 are pinned by the test. On glibc that becomes 72 per byte plus the input:
  my independent 72.99× agrees with Sol's 73.08×. Strings, keys, `\u` escapes and the
  duplicate-key `HashSet` all stay well below. No lookup is quadratic (`member` and `get` are linear
  per entry, so linear in total).
- **The 4,096-element reservation clamp** is applied at every `with_capacity` sized by a count read
  from the file (`anchors.rs`, `peers.rs`, `mempool.rs`, `fee_estimates.rs` ×3, `tx.rs` ×3). JSON
  uses `Vec::new()`, and `decode_base32` is sized by the input text.
- **File type checks** are on the opened descriptor, with no pathname pre-check. Directories,
  FIFOs, devices and symlinks to them are refused; symlinks to regular files are followed, as with
  `fopen`.
- **The UniValue port** (`json.rs`): the lexer, including NUL past the end standing in for C's
  terminator, a lone `-` at the end, `strncmp` keywords, the `\u` bounds and `hatoui`; the UTF-8
  filter (`univalue_utffilter.h`) line for line; the recursive grammar equals Core's expect-mask
  state machine, the depth rule `stack.size() > 512` and the trailing-token check. The 84K
  differential confirms all of it.
- **`ReadSettings`**: empty file (not deleted, as documented), non-object, top-level duplicates
  only. **`CBanDB::Read`/`BanMapFromJson`**: `getValues` on an object gives member values in file
  order; the order of checks is version (`getInt<int>`), `get_str`, `LookupSubNet`, then
  timestamps; `insert_or_assign` semantics; `banmap_t` order matches `CSubNet::operator<`, with the
  `Network` enum order checked at `netaddress.h:43`–`65`.
- **`LookupSubNet`**: last `/`, bracket strip after the split, `SetSpecial` (TORv3 checksum, I2P,
  new-style `DecodeBase32` with 1/3/4/6 padding), `MaybeFlipIPv6toCJDNS`, `ParseUInt8`'s `+`/`+-`
  rules, the full-netmask path including IPv4-mapped masks and contiguity, the internal-prefix
  refusal, and TORv2 mapping to `::`. Confirmed by the 9,659-input glibc differential.
- **`mempool.dat` prefix semantics** (`mempool_persist.cpp` `LoadMempool`): a record counts only
  when transaction, time and delta were all read; `mapDeltas` is applied whole or not at all, then
  the unbroadcast set; `std::map` keeps the first value, `std::set` collapses repeats; a version
  other than 1, or a truncated header, applies nothing. `read_tx` matches Dash's DIP2 layout.
- **`peers.dat`**: the order magic → format → compat (`< 32` is corrupt; `lowest_compatible > 4`
  is the `.bak`-and-recreate class) → counts → entries → buckets (`0 <= index < nNew`) → asmap
  checksum → checksum, matching `addrman.cpp:234`–`351` and `addrdb.cpp:97`–`119`. Disk `CAddress`
  version masking and the BIP155 per-network length errors match `protocol.h:432`–`476` and
  `netaddress.cpp:44`–`89`.
- **`fee_estimates.dat`**: every rejection Core makes is made; check ordering differs only where
  the file is refused either way. The `< 140100` case reads no height, where Core reads one, but
  either way Core does not use the file's estimates.
- **Panics and overflow**: every count-driven loop consumes at least 4 bytes per iteration or fails
  (the peers buckets and entries, mempool records, deltas and set, anchors); `take` cannot
  overflow; the `inet_aton` and scope accumulators fail at `u32::MAX` before any overflow;
  `get_int` uses `i128` with a bound; the shifts in `prefix_byte` and `ConvertBits` are within
  range; `expect`s are only on fixed-size slices. No hostile input above panicked.

## Notes

- Nothing seen during this review needs the embargo channel.
- The probe crate needed the workspace's `[patch]` sections copied into its manifest. A bare
  path-dependency build fails in `dash-consensus` because it resolves the wrong base-sdk revision.
