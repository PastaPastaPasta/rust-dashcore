# T13 govsync/govobjvote adaptation f7abb5ef3: Opus review

Reviewer: Claude Opus 5.5 (independent; I did not read the Sol review before writing this).
Scope: `dash-p2p/src/policy.rs` at f7abb5ef3 (parent d0ae018cb), base-sdk 8382b76 against the old pin 28db98c,
Core v24.0.0-rc.3 (`~/workspace/dash`; `net_governance.cpp`, `common/bloom.h` and `governance/vote.h` are
byte-identical between rc.3 and the checkout).

## What I ran

- `cargo test -p dash-p2p` at f7abb5ef3 (`RUSTC_WRAPPER=` because the shared sccache stalled; separate target under
  my scratch dir). All suites pass: lib 129 (128 + my probe), policy_read 5, policy_table 3, and every other
  integration suite.
- **Differential probe.** I appended the same `#[cfg(test)] mod review_probe` to policy.rs in two throwaway worktrees:
  f7abb5ef3 (new pin), and `e4628edd3^` (the train before the re-pin: old base-sdk 28db98c plus the old
  `GovSyncRead::tail` and the `gov_rest` sig check). For 43 crafted payloads it prints `decode()`'s result (consumed, or
  the `P2pDecodeError`), `read::<GovVote>` / `read::<GovSync>` / `read::<GovSyncRead>` (consumed or `ReadError`,
  including the throw text and `misbehaving()`), and for `GovSyncRead` the three fields plus whether `encode()` equals
  `payload[..consumed]`. Cases:
  - govobjvote: signature counts 0/1/64/65/66/95/96/97 with all their bytes; truncated signatures (50 of which 10 are
    present, 65/64, 96/0, 64/63, 97/0, 200/0, 0xfd cut); non-canonical counts 65 and 64; 65 + 5 trailing bytes;
    64 + 1 trailing byte (so the payload has 65 bytes after the count); payload lengths 0/83/84.
  - govsync: lengths 31 and 32; data 2 with tails 0..10; data 36000 + 9; 36001 + 9; 36001 with no bytes; data 5 with
    3 present; `nHashFuncs` = 51 with `nFlags` = 0xff, alone and with 3 trailing bytes; a non-canonical count.

  **Result:** in every case the accept/throw verdict, the bytes consumed, the throw text, the type name and the score
  are identical. The only differences are inside `decode()`'s `P2pDecodeError` payload (see N3), and those come from
  the base-sdk re-pin, not from this commit's logic. `encode()` reproduced the consumed bytes in every accepted govsync
  case. The worktrees and targets are removed.

## The `read_as_core` govobjvote arm (policy.rs:1151-1160)

**Correct.** The questions in the brief, one by one:

1. **Is it only the signature size that gets remapped?** Yes. Base-sdk 8382b76 returns the *top-level*
   `P2pDecodeError::InvalidValue` from `GovVote::decode_msg` in exactly one place: the 65/96 check
   (`p2p_core/src/msg/gov.rs:65-69`). Every other failure in that read comes back wrapped as
   `P2pDecodeError::Consensus(..)`, which the arm's pattern doesn't match (it is `P2pDecodeError::InvalidValue`, not
   `Consensus(DecodeError::InvalidValue)`). That covers the outpoint, parent hash, time, and
   `read_limited_byte_vec(96)`'s Eof, non-minimal count or over-limit count. `VoteOutcome` and `VoteSignal` are
   `enum_map!` types with an `Unknown` arm over `u32` (`primitives/src/gov.rs:220-253`), so they take any value,
   just as Core's enum read does. Trailing bytes are no error at all: `dispatch` reports the bytes it read
   (probe: 65 + 5 trailing gives `Ok(consumed 150)`, as before and as in Core, which never checks for an empty stream
   here).
2. **Is `consumed = 0` right?** Yes, because the value is never used. On this path the arm returns `Err` whatever
   happens. If `gov_rest` agrees the size is wrong, `?` returns its `Failure::gov(BadLen{[65,96], len}, "bad
   governance vote signature size")`, which is the same `Failure` the pre-re-pin path built after a successful read
   (probe: `decode` gives `Consensus(BadLen{expected:[65,96], actual:n})` and `read` gives that text for n = 0, 1,
   64, 66 and 95, identical before and after). If `gov_rest` disagrees, base-sdk's own error is returned. `gov_rest`
   re-derives the length from offset 84 with the same canonical `CompactSize::decode_size` base-sdk uses, so the two
   cannot disagree today and that fallback can't be reached.
3. **Truncated or over-long signatures.** Over 96: `gov_limit` refuses it before the read, with "Vector length limit
   exceeded", unchanged. A truncated signature, whatever its declared size, fails in base-sdk's `read_bytes` with
   `Eof` *before* the size check, so it never reaches the arm and keeps Core's end-of-data text. Core does the same:
   `LIMITED_VECTOR` reads the bytes before `vote.h:199` checks the size. That includes a wrong declared size that is
   also cut short (50 declared, 10 present), where Core throws end-of-data and not "bad … size". The probe confirms all
   seven truncated cases. A wrong size followed by extra bytes (64 + 1) is "bad … size", as in Core.
4. **Order.** Core reads the outpoint, hash, outcome, signal and time, then the limited count, the bytes and the size
   check (`vote.h:192-201`). The node runs `gov_limit` (count > 96) first, but only once `count_at(84)` exists, which
   means all 84 head bytes are present. So the first failing check is Core's first failing check in every case.
   Unchanged by this commit.
5. **Score.** `ReadError::misbehaving()` is `None` for every `Throw`. The handler score comes from the policy row
   (`ThrowOutcome::Handler{100, "malformed governance vote"}`, matching `net_governance.cpp:207-211`, which catches
   any `ios_base::failure`). Since the `Failure`/`Throw` is unchanged, so is the score.

## `GovSyncRead` (policy.rs:945-985)

**Correct for anything decoded.** `GovSync` now reads the whole `CBloomFilter` (`FilterLoad::decode`: `vData` as
`LIMITED_VECTOR(36000)`, then `u32`, `u32`, `u8`), which is `bloom.h:78` exactly. `GovSyncRead::decode` and
`decode_msg` are now `GovSync` plus a copy of the fields, so the bytes consumed are Core's (32 + count + n + 9, no
trailing check). The probe shows `encode()` equal to `payload[..consumed]` in every accepted case. Before the re-pin,
`GovSync::encode` stopped after `vData` and `GovSyncRead::encode` appended the tail. Now `GovSync::encode` writes the
tail itself, and dropping the three extra writes is what keeps the output from carrying the tail twice.

**Caps.** `MAX_BLOOM_FILTER_SIZE` is now enforced inside base-sdk's read as well (`read_limited_byte_vec(36000)`),
but `gov_limit` still refuses a count over 36000 first, with Core's "Vector length limit exceeded" text (probe:
36001 with and without its bytes; same result before and after). `MAX_HASH_FUNCS` has *not* moved into the decode
path. `FilterLoad::decode` takes any `nHashFuncs`, and `is_within_size_constraints()` is a separate call, which
matches Core: `IsWithinSizeConstraints()` runs after the read and scores +100 with no throw
(`net_governance.cpp:107-110`). The probe accepts `nHashFuncs` = 51 and `nFlags` = 0xff and consumes 43 bytes,
before and after. No caller in the workspace consumes govsync's hash-funcs bound yet (the only `GovSyncRead`
consumer is node-fuzz's `readers_agree`, which still holds: its "last nine bytes" image equals the copied fields).

## Findings

No blockers. Nits:

- **N1 (API, low). `GovSyncRead` now has two copies of each tail field, and `encode` ignores one of them.**
  `hash_funcs`, `tweak` and `flags` are `pub` and duplicate `sync.bloom_filter.{hash_funcs,tweak,flags}`.
  `encode()` (policy.rs:976) writes only `sync`. A caller that builds or mutates a `GovSyncRead` through the
  top-level fields gets bytes that don't reflect them, and the derived `PartialEq` can call two values that encode the
  same bytes unequal. No code does this today, which keeps it a nit. It would be simpler to retire `GovSyncRead` in
  favour of `GovSync` (both are in `read`'s table, and node-fuzz can read `m.bloom_filter.*`), or to keep it as a thin
  view with accessor methods and no stored copies. At minimum, document that `encode` writes `sync`'s filter.
- **N2 (dead code and a stale comment, low). The tail branch of `gov_rest`'s govsync arm (policy.rs:1230-1245) can't
  run any more.** Every reader that reaches it (`GovSync` through `decode` or `read`, and `GovSyncRead`) has already
  read the 9-byte tail, so `remaining < BLOOM_TAIL` is never true when `read` succeeded, and
  `vdata_end + BLOOM_TAIL == consumed`. The new doc comment ("this keeps the throw's text and its order after
  `vData`") and the old inline comment ("whether or not the reader went on into the tail ([`GovSyncRead`] does)")
  describe a job the code no longer does. The text and order are kept by base-sdk's `FilterLoad` read, which fails
  at the same field with the same end-of-data text. Suggest reducing the arm to `Ok(consumed)`, or a `debug_assert_eq!`
  that the result equals `vdata_end + BLOOM_TAIL`, and fixing both comments. Behaviour doesn't change.
- **N3 (observability, informational). `decode()`'s error *payload* differs from before the re-pin, though never its
  class or text.**
  - govsync with a short tail: the error was `Eof{needed: 9, remaining: r}` from `gov_rest`; it is now base-sdk's
    per-field `Eof{needed: 4|1, …}`.
  - A truncated vote signature or govsync `vData`: the error was `CompactSizeExceedsLimit{limit: bytes_left}`; it is
    now `Eof{needed: n, remaining}`, because base-sdk now uses `read_limited_byte_vec`.

  `Throw::from_decode` maps all of these to `DataStream::read(): end of data`, so nothing Core-observable changes.
  The second difference is base-sdk's own, not this commit's. Anything that snapshots the `P2pDecodeError` *variant*,
  for example a dde-probe bs7 `class.rs` run that turns `CompactSizeExceedsLimit{limit≠MAX}` into "other" and `Eof`
  into "eof", may move from "other" to "eof" on these rows. That move is towards Core. If such a run has a frozen
  expectation file, refresh it with the train.
- **N4 (defensive fallback, low).** The unreachable `return Err(e.into())` in the new arm would surface base-sdk's
  text ("invalid value for governance vote signature size: iostream error") instead of Core's. Keeping it is
  reasonable as a guard. If base-sdk ever adds an earlier top-level `InvalidValue` to `GovVote::decode_msg` for a
  vote whose signature is *also* the wrong size, the arm would report the signature throw instead of the earlier one.
  A `debug_assert!` on the fallback, or matching on `field == "governance vote signature size"` in the guard, would
  catch that drift in tests. Either is optional.

## Test strength

`govobjvote_signature_sizes_are_cores` would catch a remap that was *dropped*: `read` would then throw "invalid value
for …", not "bad governance vote signature size". It would **not** catch a remap that is *too broad*, for example a
guard of `Err(_) if cmd == GOVOBJVOTE`, or one that matched `Consensus(_)` as well. Such a remap would turn a
truncated wrong-size signature into "bad … size" where Core throws end-of-data, and every case in the test carries
its full signature bytes. It also asserts only `decode(..).is_ok()`, never the error value that `decode` hands the
node. Suggested additions, all of which pass at f7abb5ef3 per the probe:

```rust
// Declared 50 (a wrong size) but only 10 bytes: Core runs out of data inside LIMITED_VECTOR
// before vote.h:199's size check, so the throw is end-of-data, not "bad … size".
let mut p = vec![0u8; GOV_VOTE_SIG_AT];
p.extend(compact(50));
p.extend([3u8; 10]);
assert_eq!(thrown(read::<GovVote>(&mut &p[..], &ctx)).what,
           "DataStream::read(): end of data: iostream error");
// Same for a right size cut short (65 declared, 64 present).
// decode() returns the pre-re-pin error value for a wrong size, not base-sdk's InvalidValue.
let mut p = vec![0u8; GOV_VOTE_SIG_AT];
p.extend(compact(64));
p.extend([3u8; 64]);
assert!(matches!(decode(&cmd("govobjvote"), &p, &ctx),
    Err(P2pDecodeError::Consensus(DecodeError::BadLen { actual: 64, .. }))));
```

For govsync, the test already covers 36000/36001, the short tail and the field order. Two more would help: a
`nHashFuncs` over `MAX_HASH_FUNCS` (51) that must still *decode* (the bound is the handler's, after the read), and an
`encode` round trip of `GovSyncRead` (`encode(read(p)) == p[..consumed]`) to pin the "no double tail" property this
commit relies on.

## Out of scope / not checked

- I did not review base-sdk 8382b76 beyond the decoders this commit relies on (`GovVote::decode_msg`, `GovSync`,
  `FilterLoad`, `read_limited_byte_vec`, `dispatch`).
- No dashd run; the bar was Core's source.

VERDICT: APPROVE-WITH-NITS
