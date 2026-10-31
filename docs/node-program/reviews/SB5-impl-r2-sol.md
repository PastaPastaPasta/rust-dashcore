# SB5 implementation — narrow Sol r2

Reviewed on 2026-10-11. **The three r1 findings are closed. One major gap remains in DEC-187 amendment 3(c): a missing issued id is not necessarily detected at the next by-id cache miss.** The paging and revised memory fixtures pass, including the original wide-tree witness. The new watermark correctly refuses unissued records, but the issued-and-missing half currently depends on which reader API makes the next request.

## Scope and authority

- Exact head: `247962fbf2656a4b10c4b5d24f7572bbf8e9f69d`, branch `node/sb5-feeds`.
- Base: SB4 fix r2 `20422f0349e9c2b47b4a037826f17900f22b11a9`.
- Clean detached review worktree: `/work/scratch/sb5-r2-sol/wt`. No implementation source was edited, committed or pushed. No GitHub operations or private-directory reads were performed. Additional probes were written outside the checkout, under `/work/scratch/sb5-r2-sol/probes/`.
- Binding authority: `node-program/DECISIONS.md`, DEC-187 with amendments 2 and 3; the reviewed head's SB5 D75–D81 notes; r1 at `node-program/reviews/SB5-impl-r1-sol.md`; and `briefs/REVIEW-COMMON.md`. Used the code-review-validator workflow.
- Compared logically with r1 head `b640572b4`, rather than treating rewritten-base changes as SB5 fixes. `range-diff` preserves all 52 historical SB5 commits, with three patches adjusted for the rebase: F5b's SB4 test adaptations, F7's anchor/floor API and checks, and the public-API listing. Eight subsequent commits implement/document this round's changes. The saved comparison is `range-diff.txt` in the scratch directory.

## Findings

### R2-M1 — major: public by-id misses silently accept the absence of an issued record

**Locations:** `dash-chain/src/shared.rs:197` (the `None` arm falls through at `:201`); `dash-chain/src/cold.rs:211`; `dash-chain/src/reader.rs:187`, `:194`, and `:233`. The narrow passing regression is `dash-chain/src/chain/spv_tests.rs:1668`.

**Mechanism.** `Clock::admit(None, issued)` returns `Ok(None)` before inspecting the watermark. `Shared::try_node` propagates that absence without distinguishing an issued id from one the chain never held. `ChainReader::try_entry` therefore answers `Ok(None)`. The initial backend-link guards in `try_locator`, `try_ancestor`, and `try_lca` likewise treat the absent issued id as unknown. None of these paths calls `missing_record`, so the fail-stop latch stays clear.

The new test `an_issued_id_with_no_record_shows_past_the_cache` calls the crate-private **required-link** helper `reader.try_link(y)` after clearing the cache. That helper explicitly converts any `None` to corruption. It proves that particular walk check, but does not prove the ruling's broader statement, “it is detected when a by-id read misses the cache.”

**Independent witness.** The unchanged library is linked by `probes/paging.rs`. Use window 10, cold capacity 1, an anchored 30-header trunk, and one stale sibling inserted after the third trunk header. The stale sibling is issued id 28, below `meta.next_id = 56`, and is cold. After opening the valid store, the adapter makes that sibling absent and returns the trunk's eighth header by hash under id 28, as in the new implementation regression. A by-hash lookup admits it. A lookup of another cold header naturally evicts it from the one-entry cache. The next `try_entry(28)` performs a real store by-id read; the probe asserts the read counter increases by exactly one.

Observed output, reproduced after the final probe changes:

```text
issued missing id BlockId(28): try_entry Ok(None); latch false
issued missing id BlockId(28): try_locator Ok([]); latch false
issued missing id BlockId(28): try_ancestor Ok(None); latch false
```

The adapter also hides the stale record under its own hash; this is an issued missing record, not an unknown caller id. Source and successful output are preserved as `probes/paging.rs` and `probes/paging.log`.

**Impact and attribution.** Corrupt persisted state can be reported as an ordinary absent entry or empty locator, allowing the caller to continue without the promised latch. This is a remaining gap under D81/DEC-187 amendment 3(c), not a claim that r2 introduced the pre-existing optional-reader guards. The unissued-record admission fix itself works.

**Suggested fix.** Enforce issued-but-absent integrity at the shared by-id resolution seam, preserving `Ok(None)` for genuinely unissued ids. Keep the post-store watermark check for found records. Order the missing-record check carefully relative to issuance: for example, acquire a watermark snapshot before resident resolution, then classify an absent id below that snapshot as corruption. A reader racing a newly issued, still-resident entry must not be falsely rejected. Add public `try_entry`, locator, and ancestor regressions with natural cache eviction, plus a concurrent-issuance negative control; do not rely only on the private required-link helper.

## Fix verification by mechanism

### R1-M1 / D75 — closed

`HeaderStore` now fills caller-owned slices through `children_from` and `leaves_from` (`store.rs:55`). The chain uses a 64-id child page or a 64-entry `(id, work)` leaf page on its stack. There is no heap vector proportional to the stored sibling/leaf count in these SPV enumeration paths.

The children cursor merges the stored page and `Dirty::first_from` using the smaller next id, advancing both sources on equality (`chain.rs:337`). Dirty scanning comes from the bitset, independently of insertion order. The cold-leaf scan computes a fresh `u64` parent mask per page and excludes resident entries and uncommitted parents; it updates the maximum only after a complete successful scan (`chain.rs:885`).

`next_page` advances past the last returned id. A short page continues; an empty page terminates. Checked pages require the first id to be at least the requested cursor, strict order throughout, and a count within the supplied slice (`shared.rs:353`). An id-space endpoint also terminates after `u32::MAX`.

The independent full test run includes the page-boundary, short-page, parent-mask, dirty interleaving, and ordering tests. In particular, `children_merge_pages_and_the_step` dirties an already stored id in the middle of the second page while higher stored ids and three new ids remain, and verifies exact ordered deduplication. It repeats with ten-entry pages. Leaf tests repeat with seven-entry pages.

Additional external probes verified:

- 150 stored siblings plus three uncommitted siblings produce exactly 153 marked competing roots, strictly in id order.
- Child and leaf duplicates within a page, duplicates repeated across the boundary, and oversized reported counts fail with typed corruption and trip the latch.
- A record's id changed after the first child page, so that the next page returns an earlier id, fails the cursor/order check and trips the latch.

Legitimate ids and parent hashes are immutable. In the prescribed step, enumeration occurs before stage 3 commits; residency eviction does not alter store membership. Thus a valid row does not move across the id cursor during enumeration. Paging assumes the prescribed single-writer/store contract; it cannot detect an arbitrary store that silently omits rows while returning otherwise valid ordered pages.

The original 9,000-sibling scenario passes:

| Fixture | Lock step | Quiet step | Allowances |
|---|---:|---:|---|
| A-F12 wide tree | 432,528 B | 44,768 B | 718,832 B / 205,244 B |
| Same fixture reopened before the lock | 385,056 B | 44,768 B | Same |

The original r1 standalone `wide.rs`, relinked unchanged against this head, also passes twice: lock 432,528 B, quiet 45,056 B. Its observation/baseline differs slightly from the integration fixture; both are far below the cap and replace the former multi-megabyte witness. The fixtures also execute the quiet step after reopening at the locked sibling.

### R1-M2, amendments 2(a) and 3(b) — closed

`FeedConfig::validate` accepts at most 64 feeds (`feed.rs:225`), and chain configuration validation invokes it. Registry vectors reserve `max_feeds` at construction and every republish; table entries and the free-index vector also reserve at construction. The derived term is exactly

```text
2 × (ArcInner<Registry> + max_feeds × RegistryEntry)
```

On this build that is 736 B at eight feeds and 5,216 B at 64. A retained registry keeps the old publication alive while `exact_registry_term` measures one deregistration: exactly 2,608 B. No vector-growth allocation is hidden by a released old registry in that test. The original in-hold 64-feed retirement witness observes 7,344 B against the revised cap of 18,964 B.

The active-path term is two paths. One path is a full-capacity leaf, 4,144 B including its Arc allocation, plus four inner levels at 560 B each: 6,384 B. Four levels suffice for all u32 heights. `exact_path_term` pushes within an existing deepest leaf against a retained view, so it measures actual copy-on-write allocations rather than fresh residency nodes; the old path stays held. It measures exactly 6,384 B, hence `ACTIVE_PATHS_BYTES = 12,768`.

`STEP_FIXED_BYTES` uses a `repr(C)` ArcInner layout with padding: snapshot 288 B plus status facts 112 B = **400 B**. `exact_fixed_term` holds the old values, forces both new publications, and asserts **both end and peak** equal 400 B. Its warm-up excludes arc-swap's once-per-thread debt-node initialization; that is a persistent thread initialization cost, not a repeated publication cost. This is an isolated publication test, not a claim that an entire empty step allocates only 400 B.

The pending queue reserves one segment slot at construction. Pending Composite data uses boxed leg slices, charged to the pending leg-copy term; there are no two segment Arc allocations. `composite_fanout_and_retry` verifies the full-slot/retry path retains the reserved queue capacity. The writer's no-pending-before-next-sub-step rule establishes why one slot suffices.

Current step caps: desktop 433,104 B; mobile 202,704 B; mobile B=500 118,704 B.

### Amendment 2(b)–(d), two-account A-F12 — verified for the reviewed paths

The three full-step fixtures construct `HeaderBatch` before measurement. They exclude exactly its header/hash buffers and assert their complete freed byte total: 288,000 B at B=2,000, and 72,000 B at B=500. A replacement allocation would be counted; the original buffer's excluded free does not exempt its replacement.

Inspected every `fault::residency` scope:

- `insert_entry`: arena installation and hash-index insertion; separately resident-height insertion and `chunk_live` update.
- `open::load_record`: those same residency structures. Cold ancestor reads, MTP scratch, and candidate-leaf updates occur outside the scope.
- `active::fresh`: only the empty node's full-capacity vector and its Arc. `Arc::make_mut` and `Node::clone` run outside it, so copying published paths remains step work.

There is no user callback, store read, plan construction, batch copy, registry rebuild, or fanout inside these closures. The flag restores its prior state on unwind. I found no current step allocation hidden inside a residency scope.

The allocator assigns each counted allocation to STEP or RES. Address tracking preserves residency attribution when a block is freed outside the flag. The aggregate account receives the same additions. Store transactions and test callbacks are intentionally excluded; the batch's two frees are separately checked. No unexplained allocation exclusion was found. These are thread-local allocator observations under the stated start/end convention, not a process-RSS or arbitrary cross-thread lifetime proof.

Re-derived held-residency bound, with `n = N_win + B` and `k = ceil(n/1024) + 1 + S`:

```text
k × 245,920 + 8,192 + index_bytes(n) + resident_bytes(n)
  + grown_table_bytes(k, 8) + 2 × active_bytes(n)
```

The 245,920 B includes 1,024 real 240-byte entries and the chunk header/Arc. The 8,192 B is the relevant second-level table; construction's first-level directory is outside this held account. `index_bytes` includes the old table beside the new one. The hashbrown 0.17.1 resize code confirms in-place rehash only when new live occupancy is at most half capacity; otherwise it grows. On this x86-64 build an index bucket costs five bytes plus a 16-byte group per table. `exact_index_table` measures six actual capacities. `exact_resident_nodes` measures the first B-tree leaf (104 B), then the 12th key's additional leaf and internal node (104 + 200 B). The resident bound uses minimum non-root occupancy, and `chunk_live` gets its own growth/rehash term. Fresh active nodes coexist with the pre-step published tree, hence two active rows. The three plain fixtures have S=0.

| Case | Step / bound | Held residency / bound |
|---|---:|---:|
| Desktop, n=12,001, 13 chunks | 188,416 / 435,644 B | 3,551,512 / 3,914,016 B |
| Mobile, n=4,049, 5 chunks | 53,440 / 205,244 B | 1,366,056 / 1,528,192 B |
| Mobile B=500, n=2,549, 4 chunks | 47,104 / 121,244 B | 1,066,256 / 1,169,344 B |

Accounting limitation: `Counting::realloc` records old-plus-new overlap in LIVE/RES, but releases the old address from HELD before recording the new address. Thus HELD's generic peak is not conservative for a copying residency realloc. The current reviewed residency structures allocate new hash tables/nodes, or reserve fixed vector capacities, rather than grow via realloc, so this does not invalidate the three bounds above. Preserve that restriction or make HELD's realloc peak include the old buffer before extending the measured residency paths.

### D81 unissued watermark — first half verified; second half requires R2-M1

`Shared.issued` is AtomicU32. Insertion installs the arena/index before publishing `next_id` with Release; the record is committed later. The cache holds its mutex across the store read and loads the watermark with Acquire **after** the read, before allocating/admitting the returned record. An id at or above it produces `StoreError::Corrupt`, and Shared's wrapper trips the latch. Cache hits retain records that already passed admission; the watermark is monotonic for a live chain.

A valid committed record's issue precedes its commit. Under the store's synchronized read-after-commit contract, a read returning that commit orders the later watermark load after issuance. An earlier watermark sample could be stale, which is why the post-read load is required. This argument depends on the stated HeaderStore publication contract; Acquire alone does not supply the store's synchronization.

`open` restores the watermark from `meta.next_id` before its first record read (`open.rs:78`); only metadata/max-id queries precede it. The independent suite passes refusal by hash and id, reopen restoration/refusal at exactly next_id, the unissued locator case, and skip-id corruption. The cold-cache loom models use an unrestricted std-atomic watermark; they do not separately model the new issuance ordering. Missing issued records are detected by required-link walks, but not by every next public by-id miss, as R2-M1 demonstrates.

### R1-m1 / D78 and D79 rebase — verified

P2's `Side::step`, A-23's property helper, and SB5's write-step `drive`/`plan` no longer build a full stage-1 plan. They derive the target BlockRef directly, write a moved target when appropriate, and activate with bounded segmented plans. The independent run includes P2 and the directed/restart consumers. SB4 compatibility helpers and the intentional single-step reference still use full plans; they are not the corrected SPV driver recipe.

`open` rejects a genesis hash above height zero, a non-genesis hash at zero, and non-genesis heights 1–23 as `Corruption::Anchor`, tripping the latch (`open.rs:56`). A normal accepted tip below the anchor is `ActiveMismatch`, even when above the floor (`:86`). The external probe specifically sets tip height to anchor−1, above anchor−24: it returns ActiveMismatch and trips the latch. ActiveChain construction derives the floor from the anchor, matching the rewritten SB4 API.

Nonempty `r.disconnected` assertions remain in the Permanent/NoStore node hold and enforce-feed tests. SPV tests check `VirtualTip.disconnected`, and the remaining SPV assertions against the report explicitly require it to be empty. This preserves DEC-187(1)'s single disconnect list.

## Independent validation and limits

All builds, lint runs, and probes used nice 10, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=` (matching the implementer), and `TMPDIR=/work/scratch/sb5-r2-sol/tmp`, verified mode 0700. Toolchain: rustc 1.98.1, x86-64 Linux. The implementer's target was reflink-copied into this review's isolated build directory; Cargo rebuilt the relevant artifacts. No target directory was shared with the implementer. Normal and loom targets were deleted after validation; probe sources, binaries, logs, and the clean detached checkout remain.

| Command/check | Result | Scratch log |
|---|---|---|
| `cargo test -p dash-chain --all-features` | 415 passed, no failures; 11 ignored | `test.log` |
| `RUSTFLAGS='--cfg loom' cargo test -p dash-chain --release --lib loom_ -- --test-threads=1` | 30 passed, no failures; 15 ignored negative entries exercised by child harnesses | `loom.log` |
| `cargo test -p dash-chain --all-features --test step_budget -- --ignored --nocapture --test-threads=1` | All three nightly allocator variants passed | `nightly-budget.log` |
| `cargo clippy -p dash-chain --all-features --all-targets -- -D warnings` | Passed | `clippy.log` |
| `cargo fmt -p dash-chain --check` | Passed | `fmt.log` |
| Standalone copy of the exact budget test source, linked to this head; exact/full-step/wide/registry filters | 11 passed; measurements printed | `probes/budget.log` |
| Original r1 standalone wide witness, linked to this head | Passed twice | `probes/wide.log`, `probes/wide-rerun.log` |
| External paging, issued-missing public readers, below-anchor tip | Expected behavior asserted, exit 0; reproduces R2-M1 | `probes/paging.log` |

Nightly observations: 50k reorg 45,072 B with a 20,480 B sub-step account; 50k marks 1,595,888 B with an 851,984 B overlay table; 100-stale-chunk fixture 103 held chunks, Nwin=2,149, S=99, retained 24,605,752 B. No timing failure required an isolated rerun.

This is the requested narrow follow-up, not a new review of all SB5 production integration. Did not rerun the full workspace, MSRV, Miri, PF0, all per-commit builds, or actual SPV/full-node drivers. The implementer's 44/44 per-commit claim and Opus review remain external evidence. The store paging and visibility proofs retain the single-writer and synchronized read-after-commit assumptions; the tests use MemHeaderStore, not SQLite/device crash experiments.

The unissued-record check should remain, and the missing-issued resolution should be corrected before this gate passes.

**Verdict:** CHANGES-REQUIRED
Sol gate satisfied: no
