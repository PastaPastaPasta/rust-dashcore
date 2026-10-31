# SB12a review, Opus r3 (full)

Scope: `node/sb12a-bridge`, `0d0939b8a..f36da3f1d` (11 commits, the whole branch), consensus/hash-provenance
risk tier. I reviewed from a detached worktree with a private target (both deleted afterwards). Inputs: the
brief, Sol r1 (CHANGES-REQUESTED), Sol r2 (APPROVED), SB12a-notes D1–D19, DEC-224 as amended 10-16, and
CANDIDATE-SB12a (owner).

Note: during this review the branch moved to `d5ce9aa06` (one commit, a `dashd-node` test-scanner fix and
one D-row). It touches no bridge, `dash-consensus` or hashing code, and this review doesn't cover it.

## Findings

No blockers and no majors. Sol r1 F1–F3 are closed. Four nits, plus forward notes for SB12/SB14.

1. **NIT: the PF0 row comments are stale after f1.** `contrib/node/perfgate/workloads.toml:75-78` still
   describes the pre-f1 rows. It says "bridge: 2,000 headers dash-spv already hashed, crossed (no X11)" and
   "hash-bridge: dash-spv's hashing step and the crossing". After D17, `headers/bridge/2000` is
   `headers_from_consensus` (consensus → dash-spv, no X11), and `headers/hash_and_bridge/2000` hashes in
   `dash-consensus` (`benches/headers.rs:5-10` says this correctly). **Fix:** copy the bench's module-doc
   wording into the comment. This matters because D11 asks for these rows to be re-measured and anchored,
   so whoever anchors them reads this comment.

2. **NIT: the dev-profile comment for `dash-pow` names the wrong consumer.** `Cargo.toml:78-81`
   (`[profile.dev.package.dash-pow]`) justifies opt-level 3 by "tests that hash many block headers
   (dash-consensus, CH1)". After D16, `dash-consensus` hashes with `dash-x11`, which has its own entry
   (`:83-86`). `dash-pow`'s only remaining user in the lock is base-sdk's `dash-primitives`, so the entry
   still matches a package and Cargo won't warn. **Fix:** reword the comment to name `dash-primitives`, or
   drop the entry if no debug test still hashes through `dash-pow`.

3. **NIT (test coverage): `block_from_consensus` is outside every counted span.**
   `hash_once_tests.rs:123-131` counts only `block_to_consensus`. The reverse direction
   (`headers.rs:268-271`) decodes dashcore's block from `core_serialize`. That hashes nothing today:
   dashcore's `Block` decode is plain field decoding (`dash/src/blockdata/block.rs:57,209`). The headers
   tests do count their reverse direction (`headers_from_consensus`). **Fix:** move
   `block_from_consensus(&consensus)` inside the `before`/`since` span and keep `(0, 1)`.

4. **NIT: `debug_assertions` is the release gate for the counters.** Any downstream profile that sets
   `debug-assertions = true` in a release build would compile both thread-local counters into the hot X11
   path (`hashes/src/hash_x11.rs:40`, `dash-consensus/src/hash.rs:81`). The workspace sets no such profile
   (`Cargo.toml:66-127`), the `__` prefix marks the feature internal, and the cost is one TLS increment
   per X11. No change needed. Record it in D6 so nobody later reads "no release build emits it" as
   unconditional.

**Security-relevant, see manager.** I have forward notes for SB12/SB14 on validation entry points that
sit outside SB12a's diff. Details are filed privately next to CANDIDATE-SB12a. None of them is a defect in
this branch, and none is reachable from a peer today.

## Verification by brief item

**1. Untrusted bytes → dash-consensus: every route hashes the exact bytes it validates (release).**

- *The type.* `dash_consensus::Header { raw, hash }` has private fields
  (`dash-consensus/src/primitives/block.rs:17-21`), so only that module can build one, and its one
  constructor, `Header::new` (`:26-32`), hashes `header_bytes(&raw)`.
  - Derives: `Clone, Copy, Debug, PartialEq, Eq` only. There is no `Default`, serde, `From`/`TryFrom`, or
    `BaseCodec`/decode impl.
  - `CoreSerialize for Header` (`serialize.rs:328-329`) only writes.
  - `#![forbid(unsafe_code)]` (`lib.rs:49`), so no transmute/zeroed path exists.
  - Every construction site in the crate goes through `new`: `decode.rs:204` (`decode_header`), `:246`
    (`decode_block`), `block.rs:266` (genesis), and tests.
  - `Block::new` (`block.rs:82`) takes a `Header`, so it can't introduce a hash.
  - `from_parts_hashed` is gone; no occurrence remains in the tree.
- *Bridge entry points into dash-consensus* (`dash-spv/src/bridge/`):

  | Entry point | What it hashes | Result |
  |---|---|---|
  | `header_to_consensus` / `headers_to_consensus` (`headers.rs:175-182`) | `Header::new` | hashed |
  | `block_to_consensus` (`:230-232`) | `Header::new`, then `Block::new` | hashed |
  | `hashed_header_to_consensus` (`:191-195`) | re-hashes and compares in every build | `WrongHash` on mismatch |
  | `hashed_block_to_consensus` (`:257-261`) | re-hashes and compares in every build | `WrongHash` on mismatch |
  | `messages::headers2_to_consensus` (`messages.rs:102-119`) | base-sdk `decode_with_hasher` with a `Header::new` closure; every restored header not matched by `next_if(raw ==)` falls back to `Header::new(*raw)` | hashed |

  The `headers2_to_consensus` fallback means even a base-sdk hasher-order change can only cost an extra
  X11, never yield a mismatched pair. `decompressed_to_consensus` is gone.
- *Public storage decoders.* `HashedBlockHeader::consensus_decode` and `HashedBlock::consensus_decode`
  (`types.rs:85-94,159-168`) still read the hash apart from the header, by design (storage format). They
  reach dash-consensus only through the `hashed_*` re-hash. `bridge::headers::tests` forges both pairs
  through those decoders, with a hash that meets the target and a header whose own X11 misses it. Both
  are refused. The owner ran these in `--release`; my release run of `bridge::` is under Validation.
- *The other direction* (dash-consensus → dash-spv): `header_from_consensus` and `block_from_consensus` use
  `with_trusted_hash`, which is `pub(crate)` (`types.rs:35,107`). The hash they pass came from
  `Header::new`.
- *`headers2_to_base`* still takes the decompressor's hashes. Its result is base-sdk's wire `Headers2`,
  which carries no hash and isn't a dash-consensus value, so DEC-224 doesn't cover it. D4 states its
  residual trust honestly.
- DEC-224 as amended is satisfied structurally. `Header` itself is the "type that can only be built by
  hashing those bytes".

**2. Hash once.**

- *What the bridge's API proves* (`hash_once_tests.rs`, debug + `__x11-count`):
  - 2,000 headers: `(0 dashcore, 2,000 consensus)`.
  - Two 8,000-header `headers2` replies: `(0, 8,000)`. Header 4,000 is given an explicit prev hash, so the
    test exercises the `next_if` fallback, where 3,999 is hashed after decode, not twice.
  - A block: `(0, 1)`.
  - A stored pair: `(0, n)`.
- *Per-header hashing in base-sdk.* I checked base-sdk `dbe245f` `Headers2::read` / `uncompress`
  (`p2p_core/src/msg/headers2.rs:198-238,403-416`). It calls the hasher only for `previous` and only when
  the prev hash is compressed, so at most once per header and in order, as the bridge's comment says.
  base-sdk computes no X11 of its own in `decode_with_hasher`, which closes the counter's D6 blind spot
  for this call.
- *The live sync path is unchanged and doesn't cross yet.* `sync_manager.rs` matches the base, since the
  D10 extraction was reverted (`git diff 0d0939b8a..f36da3f1d -- dash-spv/src/sync dash-spv/src/network`
  is empty). Today dash-spv hashes once with dashcore: `HashedBlockHeader::from`, and for `headers2`
  `CompressionState::process_headers_with_hashes` in the network manager's `spawn_blocking`
  (`network/manager.rs:675`). It sends nothing to dash-consensus. So "dash-spv takes header+hash from
  the result" is a property of the bridge API that SB12/SB14 will adopt, not of today's hot path. D17
  says so. No extra X11 was added to the hot path.
- *Forward note for SB12.* The `headers2` wiring point is `manager.rs:675`. Replace that decompression
  with `headers2_to_consensus`; feeding its dashcore pairs to `hashed_header_to_consensus` would pay 2 X11
  per header. Also, the per-thread counters can't see work in `spawn_blocking`, so SB12's hash-once proof
  there needs a same-thread harness.

**3. D16: `dash-x11` in place of `dash-pow`.**

- *The call site.* `hash::x11(&header_bytes(&raw))` (`block.rs:27`): `header_bytes` returns `[u8; 80]`
  (`serialize.rs:306-315`), so the backend always sees exactly 80 bytes. `hash::x11` has no other caller
  in the crate. `dash_x11::x11` is `Implementation::auto().x11(data)` (`dash-x11/src/lib.rs:52-53`).
- *XH1 evidence* (`docs/node-program/reviews/XH1-r1-opus.md:17-22`):
  - a 1M-input differential against Core's own objects at both pins, including 1M `header` cases and
    random lengths 0..=200;
  - every mainnet (2,551,767) and testnet (1,568,460) header links under all 5 selections;
  - `dash-pow@33fcc59` equals `dash_x11::x11` on every mainnet header plus 200K random inputs.
- *What the branch itself shows.* No `dash-x11` source changed in this range. The capture tests compare
  the dash-consensus hash with dashcore's. That is dash-x11 against dash-x11, so not an independent oracle.
  The independent check is linkage: `a_headers_batch_is_hashed_once` asserts each header's
  dash-consensus hash equals the next header's Core-supplied `prev_hash` over 2,000 mainnet headers.
- *Layering.* `dash-x11` is layer P (`layers.toml:65`) with no dependencies. `dash-pow` leaves
  `dash-consensus`'s graph (`Cargo.lock`). I ran `check-layers.py`: `layers ok: L1 L2 L3 L8`.
  `dash-consensus` was already `std`, so there's no no_std regression. The cached CPU selection in
  `dash-x11` was accepted for layer P at XH1. BL-35's init/log half is left to BL-35, as D16 says.

**4. Byte-exactness, D19, F2, F3.**

- *Byte-exactness* (`tests/bridge_round_trips.rs`):
  - headers: each consensus header's `core_serialize` equals the capture's 80 bytes (`:96-103`);
  - blocks: dash-consensus's and dashcore's re-encodings equal the capture, and `decode_block` agrees on
    hash and merkle (`:129-145`);
  - headers2: base-sdk and dashcore both write the capture, and the consensus headers equal dashcore's
    decompression with hashes (`:106-127`);
  - the serialized kinds go through `round_trip` (`:64-77`);
  - capture SHA-256s are pinned by `manifest.json` (`:77`).
- *D19 vector* (`messages.rs:326-415`). I traced the version cache by hand:
  - After v1…v7 the list is `[v7…v1]` and v0 is evicted. v2 is then at position 5 (offset 6); after
    MRU, v1 is at position 6 (offset 7). Those are headers 8 and 9, matching `offsets[8..10] == [6, 7]`.
  - Header 9 has dt −600 (fits i16, so dashcore writes `Some(-600)`). Header 10 has dt 40,000 (> i16, so
    a full timestamp). Header 11 changes `nBits`. Header 5 carries a restorable prev hash.
  - base-sdk decodes the negative offset by `wrapping_add` of the sign-extended i16, as Core's
    `uint32_t + int16_t` does. dashcore's `(i64 + i64) as u32` is the same modulo 2³².
  - The vector is sound, and its doc correctly says Core's encoder never writes a negative offset.
  - The owner's early debug log (`/work/scratch/sb12a/f1-debug.log`, 06:26) shows this test failing at
    "a negative time offset", from an earlier iteration (old line 374). Their 06:57 debug run, and Sol
    r2's inspection, are at the final vector.
- *F2.* All three counter sites need `all(feature = "__x11-count", debug_assertions)`; dash-consensus also
  allows its own `cfg(test)`. The bridge's `mod hash_once_tests` needs `all(test, feature, debug_assertions)`.
  So release builds drop the module, and no counter code exists there, whatever the features. The gate's
  feature-tests helper runs `cargo test` without `--release` (`/work/scratch/sb12a/helper/gate-feature-tests.py:56`),
  so the hash-once tests do run there, filtered to `bridge::`. See nit 4 for the one caveat.
- *F3.* `under_cap` raises `SystemExit` (`sb12a-captures.py:154-159`) before every mainnet read:
  - `headers_replies` checks `height + 8000`;
  - `special_blocks` checks `top`, and its scan only goes downward;
  - `filters` checks `start + count - 1`;
  - `coinbase_clsig` checks `height`, and the locked height is below it;
  - `rpc_locks` and `listen` call `testnet_only`.
  I ran `python3 -I -B contrib/node/test-sb12a-captures.py`: 3 tests passed.

**5. Code quality and storage formats.**

- *Storage formats.* `HashedBlockHeader`/`HashedBlock` `Encodable`/`Decodable` are unchanged. The only
  diff in `types.rs` is the new `pub(crate) HashedBlock::with_trusted_hash`, so the on-disk layout is
  unchanged.
- *Code quality.* Errors use `thiserror` (`BridgeError`), docs carry `# Errors` sections, and the
  `decode_all` helper refuses leftover bytes. Naming and comment density match the surrounding node
  crates.
- *Mobile size.* `dash-spv` now depends on `dash-consensus`/`dash-p2p-core`. The owner's
  `check-mobile.sh` run shows `libdash_spv_ffi.so` at −850,368 B against the merge base, so there's no L7
  concern.
- `git diff --check 0d0939b8a f36da3f1d` is clean.

## Validation

- Ran: `git diff --check`: clean.
- Ran: `python3 -I -B contrib/node/check-layers.py`: `layers ok`.
- Ran: `python3 -I -B contrib/node/test-sb12a-captures.py`: 3 passed.
- Ran, all with `CARGO_TARGET_DIR=<private>` at f36da3f1d (build plus run 08:38–09:27 at load 200–400):
  - `cargo test -p dash-spv --release --test bridge_round_trips`: **1 passed, 0 failed**.
  - `cargo test -p dash-spv --release --lib bridge::`: **9 passed, 0 failed**. This includes both forged
    pairs (`a_forged_header_hash_pair_is_refused`, `a_forged_block_hash_pair_is_refused`) in release,
    and the D19 vector.
  - `cargo test -p dash-spv --features __x11-count --lib bridge::` (debug): **13 passed, 0 failed**. This
    includes all four `hash_once_tests` (2,000 headers `(0, 2000)`; two headers2 replies `(0, 8000)`;
    block `(0, 1)`; stored pairs `(0, n)`) and the D19 vector in debug.
  - `cargo test -p dash-spv --features __x11-count --test bridge_round_trips` (debug): **1 passed**.
  - F2 symbol check: `cargo build -p dash-spv --release --features __x11-count --lib`, then
    `nm -C … | grep -c x11_calls` on the resulting `libdash_spv`, `libdash_consensus` and
    `libdashcore_hashes` rlibs gives **0** each. Positive control: the debug rlibs built with the feature
    give 15 each. (I didn't build `--all-features` release for the whole workspace; the gating `cfg` is
    the same for every feature set.)
- Not run: XH1's historical sweep (relied on, cited above) and the PF0 re-measure (D11, manager's ask).

VERDICT: ACCEPT-WITH-NITS
