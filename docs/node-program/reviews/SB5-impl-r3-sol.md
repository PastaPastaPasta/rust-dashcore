# SB5 implementation — narrow Sol r3, R2-M1 only

Reviewed on 2026-10-11. **R2-M1 is closed.** An issued id absent from both residency and cold storage now produces corruption and trips the latch at the shared fallible by-id resolution seam. The independent r2 witness reproduces the corrected behavior on every requested public reader path. Loom L14 accepts the production order and catches the planted late-watermark fault.

## Scope and findings

- Exact head: `da6be95946967934ffee2e78af71264279ff1075`, branch `node/sb5-feeds`; base `20422f0349e9c2b47b4a037826f17900f22b11a9`.
- Reviewed the real diff from r2 head `247962fbf2656a4b10c4b5d24f7572bbf8e9f69d`: implementation/test commit `65cee39af73faf9e0e0bf8e29cb139b909188ba3` and D82 documentation commit `da6be9594`.
- Authority: `node-program/DECISIONS.md:198`, DEC-187 amendment 3(c); the reviewed branch's `docs/node-program/designs/SB5-dash-chain-feeds.md:1413` (D81) and `:1416` (D82); SB0 §9.5; r2 report `node-program/reviews/SB5-impl-r2-sol.md`; and the reviewer contract. Used the code-review-validator workflow.
- Clean detached checkout: `/work/scratch/sb5-r3-sol/wt`. No implementation source was edited, committed or pushed. Probes were written outside the checkout. No GitHub operations, private-directory reads, or process termination were performed.

**No blocker, major, minor or nit findings in this scope.** This approval closes only R2-M1; it is not a new certification of all SB5 behavior.

## Record retention: verified within a live chain generation

`HeaderStore` exposes reads and no deletion (`dash-chain/src/store.rs:43`). `StepRow` has exactly `Record`, `Status`, `LeafAdd`, and `LeafRemove` (`step.rs:63`); removing a leaf changes the leaf index, not its header record. `Tables::apply` refuses an already-existing record id or hash before inserting (`mem_store.rs:238`).

`insert_entry` assigns the current `next_id`, sets the arena entry, then advances and publishes the watermark (`chain.rs:647`, `:660`, `:664`). There are no skipped allocations on this path. `write_header_step` compares the unique new dirty ids above `store.max_id()` with the full interval up to `next_id`, then streams each new record (`chain/sb5.rs:38`, `:51`). Under the chain's dirty-id invariant, the count check enforces density. A failed write leaves the dirty set available for rewriting.

Stage 4 invokes `stage4_effects`, then `evict` (`chain/sb5/segmented.rs:144`; `chain.rs:2137`, `:2176`). The **commit-before-stage-4 requirement belongs to the step protocol**, as D82 says. Neither `activate_segmented` nor `evict` validates a committed watermark or a commit token. The reviewed driver recipe commits before activation (`test_util.rs:676`), and the failed-step tests stop before stages 4 and 5. A caller that activates after an unsuccessful commit violates that protocol and can now cause the expected missing-record corruption. This review does not claim the API prevents such misuse.

`Tables::rollback` removes only `Undo::Insert` rows, restoring statuses, leaves, active rows, and metadata separately (`mem_store.rs:360`). Those insert undos arise only from records newly added by that failed transaction; uniqueness checks prevent deletion of a previously committed row. Application and rollback run under the database mutex, so readers cannot observe their intermediate state. In a compliant step, these newly issued ids remain resident: no stage 4 follows the failed commit. Rollback itself has no arena check; its residency guarantee comes from that same protocol.

The search found **no production reset caller**. The calls are unit/integration fixtures, feature-gated test support, and a benchmark fixture. `MemHeaderStore` itself is compiled only for tests or `test-util` (`lib.rs:82`). Power-loss simulation is another table-replacement operation, but it invalidates all outstanding store handles and requires fresh chain/readers/cache; it is not a legitimate live-reader deletion (`mem_store.rs:578`).

### Candidate fallback is a lifecycle exception, not an unconditional retention guarantee

SB0 §9.5 explicitly permits clearing header tables while the anchor is Candidate. `reset` implements that clear (`mem_store.rs:548`). A replacement chain constructs a new `Shared` and watermark (`chain.rs:569`; `shared.rs:183`), but **a new Shared alone does not protect the replacement's latch from old readers** when the store and fail-stop service are shared.

The independent `probes/fallback.rs` demonstrates D82's exact caveat. It retains an old reader with cold issued id 28, naturally removes that id from its one-record cache, resets the shared store, and commits a fresh genesis chain over the same store and latch. The replacement sees id 28 as unknown and initially leaves the latch clear. The old reader then returns `Err(Storage(Corrupt("entry 28 has no record")))` and trips the latch shared with the replacement. Thus old readers must be retired, including in-flight reads and retained snapshot users, **before** reset. There is no runtime enforcement of retirement in `reset`.

D82 states this obligation explicitly, and no production fallback caller exists in this head. I found no additional live-generation deletion counterexample. The assertion that nothing deletes an issued record is valid with this generation boundary and the commit/eviction protocol, not across arbitrary resets with surviving readers.

Reopen sets `self.next_id = meta.next_id` and calls `shared.issue` before its first record read (`chain/sb5/open.rs:77`); only metadata and maximum-id reads precede that restoration. A lost Buffered suffix therefore gets the restored generation's watermark, not the old process's issued watermark.

## By-id coverage and ordering

`resolve_by_id` loads the watermark with Acquire **before** invoking the resident closure (`shared.rs:62`). If the resident lookup misses, it performs the cold read; `None` below the saved watermark calls `missing_record`, whereas an absent id at or above that snapshot remains `Ok(None)`. `missing_record` calls `corrupt`, which trips `FailStop` before returning the error (`shared.rs:239`, `:396`). Found records still pass the cache's separate, **post-store-read** watermark check (`cold.rs:213`), preserving D81's refusal of unissued records.

All production single-record SPV cache reads by id enter `Shared::try_node` and then `resolve_by_id` (`shared.rs:205`, `:230`, `:313`). `Shared::node` adds the required-link check. `Shared::try_link` first tries a resident-only link, and backend `try_entry` first tries a resident-only entry (`shared.rs:259`, `:466`). On either miss they call `try_node`, which repeats the resident lookup **after** taking its own watermark snapshot. They do not pass a resident miss sampled before issuance into the resolver. Their successful fast paths are safe because they already have a resident entry.

This covers `ChainReader::try_entry`, the initial locator/ancestor/LCA guards, and subsequent link/hash reads in their walks (`reader.rs:187`, `:194`, `:203`, `:233`, `:258`). `try_ancestor` below the floor and LCA's short circuit after an unknown first argument may return without reading another id; they are not by-id storage misses. Resident-only APIs (`entry`, `ancestor`, and `IndexAccess`) retain their existing residency semantics. The infallible `locator` wraps the fallible locator and returns an empty vector after corruption, with the latch already tripped.

The ordering argument holds under the documented store contract:

1. The writer sets the arena slot before the Release watermark store (`chain.rs:660`; `shared.rs:191`). An Acquire snapshot covering the id orders that initialization before the subsequent resident lookup.
2. If that lookup observes the entry's eviction, the eviction follows its record commit. The evicted bit is published with Release and inspected with Acquire (`arena.rs:145`, `:37`); chunk removal uses the synchronized ArcSwap slot (`arena.rs:153`). Observing either absence after issuance therefore leads to a store read after the commit.
3. The store must return a committed row on that read, without a snapshot pinned before the commit (SB5 §3.7). `MemHeaderStore` provides this with its database mutex. A cache hit also provides a record; it cannot manufacture an absent row.
4. An id issued after the early snapshot may legitimately miss both sources while its commit is pending. Its absence is not rejected. Moving the watermark read after a resident miss instead creates the false-refusal interval exercised by L14.

### Bypasses: enumerated and checked for this scope

`resolve_by_id` is the single-record read-through seam, **not every raw store query**. The following bypasses remain:

- `open` reads the tip and cold skip targets directly by id. A missing tip yields `Corruption::MissingRecord`; a missing skip target fails the skip check and yields `Corruption::SkipHeight`; both trip the latch (`chain/sb5/open.rs:81`, `:224`, `:232`).
- Snapshot range reads use `ColdWalk::walk_down` past the cache. Its wrapper requires the requested count, the requested first id, and consecutive linked records; a short or unlinked result is corruption and trips the latch (`cold.rs:166`). The independent bulk probe removes an active cold record and verifies a real bulk read: `range` returns `Corrupt("walk_down(43, 19): 11 records, not 19 linked")`, latch true. `at_height_checked` also returns corruption with a tripped latch.
- `children_from`, `leaves_from`, `from_height`, and `active_from` are bulk enumerations. They do not perform the admission watermark check, as D81 explicitly states. Page wrappers check ordering/count and trip on storage errors; ids subsequently resolved through `node` reach the shared seam. A record silently omitted from an enumeration is not detected by the watermark alone. The ruling does not require a whole-store gap scan at enumeration or reopen.
- The feature-gated `commit_mem_step` test compatibility helper probes dirty ids directly to distinguish records already stored from newly issued, still-resident records. That is a write-set construction probe, not a public cold reader bypass (`chain/mem_step.rs:23`).

No bypass was found that turns an issued **single-record by-id cache miss** into an ordinary unknown-id answer.

## Independent validation

All builds and probes used nice 10, `CARGO_BUILD_JOBS=8`, `TMPDIR=/work/scratch/sb5-r3-sol/tmp` (verified 0700), and `RUSTC_WRAPPER=`. Toolchain: rustc 1.98.1, x86-64 Linux. Debug and loom builds used this detached worktree's own target directories. The entire review target directory, including the loom target, was deleted after validation. Probe sources, binaries, artifact mapping and logs remain under `/work/scratch/sb5-r3-sol/`.

- `cargo test -p dash-chain --all-features`: **417 passed, 0 failed, 11 ignored**, including doctests. Includes the four-public-path regression, issuance smoke, restored watermark, dense new ids, commit-failure, and eviction-order tests. Log: `test.log`.
- `RUSTFLAGS='--cfg loom' cargo test -p dash-chain --release --lib cold::loom_tests -- --nocapture --test-threads=1`: **6 passed**. L14's production order passes. Its planted late-watermark control actually panics at `cold/loom_tests.rs:98` with `an issued id rejected as missing: Err(Corrupt("no record"))`, satisfying its `should_panic` assertion. L6's two positive and two negative variants also pass. Log: `loom-cold.log`.
- `cargo clippy -p dash-chain --all-features --all-targets -- -D warnings`: passed. Log: `clippy.log`.
- `cargo fmt -p dash-chain --check`: passed. Log: `fmt.log`.
- The **byte-identical r2 witness**, linked to this head, completes its paging checks and now prints `try_entry Err(Storage(Corrupt("entry 28 has no record"))); latch true`. It then exits 101 at its obsolete `Ok(None)` assertion, as expected. The copy's SHA-256 matches the original. Source/log: `probes/paging-r2-original.rs`, `probes/paging-r2-original.log`.
- The adapted witness preserves the same issued sibling id 28, watermark 56, window 10, one-record cache, by-hash admission and natural eviction. Each fallible path gets a **fresh chain and latch**, verifies exactly one real store by-id miss, and returns the same corruption with latch true: `try_entry`, `try_locator`, `try_ancestor`, `try_lca(y, tip)`, and `try_lca(tip, y)`. The infallible `locator` returns empty with latch true. Controls at `next_id`, above it, and `u32::MAX` retain ordinary unknown-id answers across every fallible path with the latch clear. Source/log: `probes/paging.rs`, `probes/paging.log`; exit 0.
- Shared-store/latch fallback and cold bulk probes: passed, exit 0. Sources/logs: `probes/fallback.rs`, `probes/fallback.log`, `probes/bulk.rs`, `probes/bulk.log`.

## Assumptions and limits

The proof relies on one header writer, commit-before-eviction, read-after-commit storage visibility, and retiring old-generation readers before a candidate reset. These are explicit protocol/lifecycle obligations, not newly enforced API checks. L14 models residency as an Acquire/Release flag and uses the production resolver, watermark and cache with a synchronized row store; it does not model ArcSwap/OnceLock arena internals or SQLite. Its model bounds preemptions at three. The threaded issuance test passed, but D82 correctly describes it as a weak smoke test; it is not the ordering control.

D82 also accurately notes that `contrib/node/check-loom.sh` currently selects only the entry seqlock model. L14 was run directly here; this report does not claim that existing CI selects it. No full-workspace, MSRV, allocator/nightly budget, all-loom, device-crash, or production fallback-driver certification was attempted for this narrow round.

**Verdict:** APPROVE
Sol gate satisfied: yes
