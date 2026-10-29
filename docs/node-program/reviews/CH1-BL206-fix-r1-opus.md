# CH1 BL-206 fix: narrow review round 1 (Opus)

Range: `6372baa1d..7175a43ac` (85f39dd7f, 7175a43ac), `dash-chain/src/cold.rs`, `dash-chain/src/cold/tests.rs`.

## Verdict: ACCEPT WITH NITS

No blocking issue. The id map has no HashDoS exposure. Nothing iterates it. The `Hasher` impl is correct for
`BlockId`. The multiplicative hash has a real low-bit weakness, but no realistic id pattern reaches it. The test
also cannot detect that weakness. Both are nits with a fix that costs about one instruction.

## Answers to the five questions

### 1. HashDoS: no

- **Where the ids come from.** `BlockId(pub u32)` is defined at `dash-consensus/src/ctx.rs:13`. It is a dense
  arena counter. `Chain::insert_entry` assigns `BlockId(self.next_id)` and then does `next_id += 1` and
  `issue()` (`dash-chain/src/chain.rs:657-675`). SB5 reopen seeds it from `meta.next_id`
  (`chain/sb5/open.rs:77`).
- **Which ids become keys.** Only ids of records the store returns: `get_or_load(id)` from `Shared::cold_by_id`,
  and `load_by_hash` → `store.by_hash` → `rec.id`. `admit_found` rejects any id at or above `issued`
  (`cold.rs:250-256`).
  - A peer's hash bytes go into `by_hash`, which still uses SipHash. They never become an `IdHasher` key.
- **What a peer can influence.** A peer cannot choose id values. It can only change how many headers are
  accepted and in what order, and each accepted header needs valid PoW. Our own access pattern chooses which
  ids get read cold.
- **Worst case if ids were shaped anyway.** The map holds at most `cap` keys (default 1024,
  `config.rs:48`).
  - If every key landed on the same h1, an operation would scan at most cap/16 groups.
  - The tags (top 7 bits) stay well mixed even for strided ids, so false key compares stay rare (see §2).
  - That is the same O(cap) bound the `by_hash` comment already accepts (`cold.rs:34-36`), and the CLOCK
    sweep has the same bound.

### 2. Distribution: top bits good; low bits only as good as the id's low bits (nit N1)

- **Top bits.** `h = id * 0x9e37_79b9_7f4a_7c15 mod 2^64` has strong top bits: every id bit feeds bits 57..63.
  All 128 tags appear for ids `0..1024`, for `576*i` and for `1024*i`.
- **Low bits.** The low k bits of `id * odd` depend only on the low k bits of `id`. hashbrown 0.15's `h1` is
  `hash as usize & bucket_mask`. So the bucket index is exactly as spread as the id's low bits:
  - consecutive ids: a perfect bijection, which is ideal;
  - an id set with stride 2^k: collapses onto `buckets / 2^k` starting positions.
- **Does the test check both?** No: see N2.

### 3. Leftovers from 85f39dd7f: none in the tree

7175a43ac fully reverts 85f39dd7f:
- `FixedSip` and its `#[inline(always)] hash_one` are gone;
- its test `fixed_sip_hashes_as_the_default_hasher` is gone;
- `by_hash` is back to `BuildHasherDefault<DefaultHasher>`.

`git grep 'FixedSip\|inline(always)'` finds nothing in `cold.rs` or `cold/`. The net diff has no forced
inline, so nothing is dead. Only the history carries the add-then-remove (N4).

### 4. Iteration order: nothing depends on it

The `map` field is private to `cold`. Its only uses are:
- `get` (`cold.rs:171,223,266`);
- `insert` (`:287`);
- `remove` (`:300,315`);
- `clear` (`:307`);
- `len` (`:178-179`).

Nothing iterates it. There is no `iter`, `keys`, `values`, `drain` or `retain`. `Clock` and `ColdCache` derive
no `Debug` or serde. Eviction order comes from the ring's `hand`, not from the map. Changing the hasher only
changes probe sequences, so behaviour stays the same. The owner's claim holds.

### 5. `Hasher` correctness: correct for the only key type

- **`BlockId` takes `write_u32`.** The derived `Hash` on a `u32` newtype calls `state.write_u32(n)`. The
  override sets the state to `n`, and `finish` multiplies. `assert_eq!(hashes[5], 5 * K)` pins that this path is
  the one taken. Through `write` the result would differ (`5 << 24` before the multiply).
- **`write`.** It shifts bytes in with `rotate_left(8) ^ b`:
  - up to 8 bytes, it packs them injectively;
  - beyond 8, it XOR-folds them in;
  - it neither panics nor returns a silent constant.
  - Inherited `write_u64` and `write_usize` route here correctly.
  - No current key reaches it.
- **One hazard (nit N3).** `write_u32` *assigns* rather than combines. For a composite key, every `u32`
  written before the last would be dropped.

## Findings

### N1 (nit): `dash-chain/src/cold.rs:67-70`, plus doc `:46-50`. Multiply without a final mix: the bucket index ignores high id bits

- **Scenario.** The cached ids form a stride-2^k set with 2^k ≥ the bucket count, such as `1024*i`.
  - All keys then share one h1, and each insert or lookup probes ~cap/32 groups on average (cap/16 at worst).
  - The table below comes from a simulation of hashbrown 0.15's insert path: 16-wide groups, triangular probe,
    `h1 = hash & mask`, `h2 = hash >> 57`. The script is `/work/scratch/ch1-bl206fx-r1-opus/sim.py`.

  | id set (n = 1024, 2048 buckets) | `mul` (this PR) avg / max groups | `mul.rotate_left(26)` avg / max |
  |---|---|---|
  | consecutive | 1.00 / 1 | 1.00 / 1 |
  | stride 24 / 288 (DKG intervals) | 1.00 / 1 | 1.00 / 1 |
  | stride 576 | 1.50 / 2 | 1.00 / 1 |
  | stride 1024 | **16.50 / 32** (1.5 false tag matches per op) | 1.00 / 1 |
  | stride 2048 | **32.50 / 64** | 1.00 / 1 |
  | random | 1.00 / 2 | 1.01 / 3 |

  The results are the same at n = 600, 4096 and 16384.
- **Realistic impact.** Ids track heights on a linear chain, and Dash's quorum strides (24, 288, 576) are not
  large powers of two. So nothing realistic hits the bad rows, and no peer can pick ids (§1). That makes this a
  robustness nit, not a bug.
- **A doc inaccuracy.** The doc comment says the control bytes take "the top bits". That holds on 64-bit only.
  On 32-bit targets, hashbrown's `Tag::full` takes bits 25..31 (`MIN_HASH_LEN` = `size_of::<usize>()`).
- **Fix.** End `finish` with `self.0.wrapping_mul(K).rotate_left(26)`. This is rustc-hash 2's approach: it moves
  the product's well-mixed high bits into h1, and its tags (product bits 31..37) still depend on every id bit.
  - It costs one `rol` and does not affect inlining.
  - It changes probe sequences, so re-measure the three PF0 rows. Consecutive ids stay at 1.00 groups.
  - If PF0 favours the plain multiply, keep it and document instead: "the bucket index reflects only the id's
    low bits; fine for dense ids, poor for ids strided by large powers of two".

### N2 (nit): `dash-chain/src/cold/tests.rs:199-211`. The low-bit assertion cannot fail

- **Problem.** For ids `0..1024`, `h & 1023` is a bijection for any odd multiplier, so `low.len()` is always
  exactly 1024. The `> 600` slack hides that the check is a tautology. The weakness in N1 shows up only for
  strided ids, which the test never tries.
- **Scenario.** Replace the hash with an identity-like one that has weak top bits, or keep the current one:
  - for `1024*i`, only 1 of 1024 low-10-bit values is distinct, and the test still passes;
  - the doc line "distinct ids spread over ... its low index bits" overstates what is checked.
- **Fix.**
  - Keep the consecutive case and assert `top.len() == 128` and `low.len() == 1024`.
  - Add a strided case, for example `(0..1024).map(|i| BlockId(i * 1024))` and `i * 576`.
  - Assert that the low 11 bits there have more than ~400 distinct values. Rotate-26 gives 427 for stride 1024
    and 645 for stride 576. The current hash gives 2 and 32.
  - If the plain multiply stays (N1's alternative), drop the low-bit claim from the test's doc comment instead.

### N3 (nit): `dash-chain/src/cold.rs:62-65`. `write_u32` overwrites instead of combining

- **Today.** `BlockId` writes exactly one `u32`, so this is correct.
- **Scenario.** Someone reuses `IdHasher` for a `(BlockId, u32)` or other composite key. Every key with the same
  last `u32` then collides into one bucket chain, with no compile error or test failure.
- **Fix.** `self.0 = self.0.rotate_left(32) ^ u64::from(n);`. The state starts at 0, so a single write gives
  exactly today's value. Hashes and PF0 counts stay the same, and the hasher becomes safe to combine.
  - Alternatively, a one-line comment: "one `u32` per key: `BlockId` only".

### N4 (nit, history): 85f39dd7f is fully undone by 7175a43ac

- **Problem.** 85f39dd7f adds `FixedSip`, forced-inline `hash_one` and a test. Ten minutes later 7175a43ac
  removes all of it. Its message ("Forcing std's SipHash inline only moved the cost ... +1.6%") explains why.
  The history therefore carries a commit whose subject claims a fix that does not hold, and that commit is a
  slower bisect point.
- **Fix.** Squash 85f39dd7f into 7175a43ac and keep the "forced inline only moved the cost" sentence in the
  message.
  - If the train process wants the negative result on record, keep both commits; that is acceptable too.

## What I ran

- `git diff 6372baa1d 7175a43ac` and `git show` on each commit.
- Read `cold.rs` at 7175a43ac, plus the cache's callers in `shared.rs:200-440`.
- Traced id issuance (`chain.rs:657-675`, `sb5/open.rs:77`), `BlockId` (`dash-consensus/src/ctx.rs:13`) and
  `DEFAULT_COLD_CACHE` (`config.rs:48`).
- Grepped the id map's uses in `cold.rs` and `cold/` for iteration, Debug and serde.
- Read hashbrown 0.15.3's `h1` (`raw/mod.rs:61`) and `Tag::full` (`control/tag.rs:36`) to confirm the bit usage
  on 64-bit and 32-bit targets.
- `sim.py`: a Python simulation of hashbrown's insert probe for consecutive, strided (24, 288, 576, 1024, 2048)
  and random id sets. It compares plain `mul`, `mul.rotate_left(26)` and a folded 128-bit multiply at
  n = 600, 1024, 4096 and 16384. I also counted distinct top-7 and low-10/11-bit values per pattern.
- Ran `cargo test -p dash-chain --lib cold` in a detached worktree at 7175a43ac:
  - `CARGO_TARGET_DIR=/work/scratch/ch1-bl206fx-r1-opus/target`, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER` unset;
  - run through `systemd-run --user --unit=ch1fx-r1-test --wait -p MemoryMax=20G`;
  - result: **46 passed, 0 failed**, including `cold::tests::the_id_hasher_spreads_ids`, with no warnings.
- Removed the worktree and the target dir afterwards.
