# ND1 2b-1 narrow review, Sol round 1

**Head reviewed:** 2eeaf7421 (f223f2a0c and 2eeaf7421), base bf5c3e29a  
**Reference:** Dash Core v24.0.0-rc.3  
**Scope:** the six changed files in dashd-node; review only.

## Method and validation

I reviewed the complete diff, the 2b plan and the relevant Core v24 sources:

- Core src/txdb.cpp:300-344 (LoadBlockIndexGuts);
- Core src/node/blockstorage.cpp:268-390;
- Core src/node/chainstate.cpp:177-260;
- Core src/dbwrapper.cpp:133-138;
- Rust dashd-node/src/chain.rs:115-143, steps.rs:121-169, chainstate.rs:235-290;
- Rust dash-chain/src/chain.rs:1004-1108;
- Rust dashd-store/src/rocks.rs:797-846 and dashd-chainstate/src/store/guarded.rs:300-320.

The targeted changed tests passed:

    cargo test -p dashd-node --features test-support index_records
    # 2 passed
    cargo test -p dashd-node --features test-support the_coins_open_on_the_steps_hub
    # 1 passed
    cargo fmt --all -- --check
    # passed

## Answers to the review questions

### 1. index_records

For the schema used here, the scan is faithful on an undamaged database. dashd-node/src/chain.rs:126-142 selects only the block_index column family, scans with an empty prefix, and therefore yields every row in the engine's key order. This is the equivalent of Core's Seek((DB_BLOCK_INDEX, uint256())) and prefix break in txdb.cpp:304-339; the separate column family has no other key families to terminate the scan. The key is converted to a 32-byte BlockHash and the value is yielded unchanged as StoredRecord, so HeaderChain::load_records performs the value decode, structural checks and proof-of-work check at dash-chain/src/chain.rs:1040-1108.

The two recorded deviations are sound for this schema. A non-32-byte key cannot be an unrelated Core key because this column family stores index rows only, so treating it as corruption and failing the load is safer than silently truncating the index. Returning a read error from the iterator also fails the load, whereas Core does not inspect the underlying iterator status and can continue with a short index. Both deviations affect damaged stores only; an undamaged datadir has the same records, error text and startup outcome.

There is one carried data-integrity caveat for the 2b-2/SB4 handoff: Core derives the inserted key from the decoded value's ConstructBlockHash() (txdb.cpp:313), while index_records supplies the physical column-family key (chain.rs:141) and decode_checked verifies proof of work against that key (dash-chain/src/chain.rs:476-500). A damaged row whose key and header hash differ is therefore not rejected at this seam in the same way as Core. This is reachable only on corrupt data and is already identified in the plan as an SB4/2b-2 follow-up; the loader should either validate the equality or explicitly retain the deviation.

### 2. Error mapping

The mapping is correct for the intended engine error classes. ErrorKind::Corruption becomes StoreError::Corrupt, and a malformed key also becomes StoreError::Corrupt (chain.rs:129-139). open_block_index maps those errors to LoadFailure::LoadingBlockDb and therefore Core's Error loading block database; other engine kinds become LoadFailure::Store, which maps to Error opening block database with the cause (dashd-chainstate/src/chain/load.rs:155-179, dashd-node/src/chainstate.rs:130-150). A transient I/O or no-space error consequently does not receive the corruption classification or the wrong corruption/reindex advice. The deliberate D14 behavior is that a read failure is reported rather than treated as a successful partial scan. No message or exit-code change is exposed for an undamaged datadir.

### 3. One SubscriberHub

The hub plumbing preserves behavior. ChainSteps::load creates one Arc<SubscriberHub> immediately before invoking the coins opener (dashd-node/src/steps.rs:149-153), stores that same Arc, and passes a reference to Chainstate::open_on; recovery receives that exact reference at dashd-node/src/chainstate.rs:257-267. The new test confirms the opener's pointer is the steps hub and that a fresh open gets a fresh hub (dashd-node/src/sut.rs:414-436). Normal callers construct a new ChainSteps per open, so no path creates two hubs for one load. On the devnet path V starts before the hub is created, but that start runs the pending devnet action with the existing no-coins/no-flush seams; recovery cannot run until after the hub insertion.

The remaining recovery-side assertion (t_ack/tip observed by the same hub) correctly belongs with the stored-index path in 2b-2.

### 4. Raw-engine scan

The helper itself is a raw KvRead scan and does not call GuardedEngine. That is the required direction: GuardedEngine::iter_prefix routes each read error through read_error (dashd-chainstate/src/store/guarded.rs:314-320), which trips the fail-stop latch and invokes the node abort sink. An index-load read error must instead be classified as an init failure before the guarded engine is used for recovery.

The current head does not wire index_records into production startup yet, so the source cannot enforce this choice: its signature accepts any KvRead, including a guard. The 2b-2 integration must open/retain the raw engine for this scan, then construct the guarded engine for recovery and all later reads. A regression test should exercise the actual production call path and assert that an injected index read error leaves the guard latch untripped while returning the init error.

### 5. Performance

**Minor — the eventual RocksDB scan uses the default cache policy.** dashd-store/src/rocks.rs:797-804 builds ReadOptions without fill_cache(false); index_records will therefore populate the shared RocksDB block cache when 2b-2 wires it to a mainnet-sized index. Core sets iteroptions.fill_cache = false in src/dbwrapper.cpp:136. This is a startup memory/cache-pollution concern and does not change any current PF0 counter result on the empty-index head. At the 2b-2 checkpoint, measure RSS and cache footprint with a synthetic roughly 2.3M-record index; if material, add an additive uncached-read option and keep the ordinary point-read policy unchanged.

## Findings and test coverage

### Minor M1 — raw-engine use is not enforced at the seam

**Location:** dashd-node/src/chain.rs:126; the guarded implementation is dashd-chainstate/src/store/guarded.rs:314-320.

The helper's generic &impl KvRead API allows a future caller to pass GuardedEngine, which would latch and abort on an index iterator error. The current diff has no production caller, so this is an integration risk rather than a failure on the present startup path.

**Fix:** wire the 2b-2 scan before guard construction (or expose a narrowly typed raw-scan seam), and add an end-to-end test proving an injected iterator error returns Error opening block database/the appropriate load class without tripping the guard.

### Nit N1 — key/header identity is not regression-tested

**Location:** dashd-node/src/chain.rs:141; Core reference src/txdb.cpp:313; consumer dash-chain/src/chain.rs:476-500.

The tests cover key order, another column family, short keys, I/O, no-space and corruption-kind errors. They do not cover a 32-byte key that differs from the hash of the decoded header. Add a targeted case or record the SB4-owned validation/deviation explicitly when 2b-2 wires Stored.

### Nit N2 — error coverage is limited to engine kinds

**Location:** dashd-node/src/chain.rs:129-133, test dashd-node/src/chain/tests.rs:802-820.

The mapping test covers Io, NoSpace, Corruption, and a malformed key, which are the kinds raw RocksDB iteration currently produces. It does not cover non-engine StoreError::Codec/UnknownDatabase; those are not produced by the raw iterator today, so this is a documentation/test-completeness nit rather than a behavior defect.

## Verdict

No Blocker or Major finding. The two killed mutants are the right direct checks for this diff: mapping a corruption error as I/O fails index_records_keeps_a_corrupt_index_apart_from_a_failed_read, and handing the opener a fresh hub fails the_coins_open_on_the_steps_hub. The missing production raw-scan integration and the cache policy are the material 2b-2 follow-ups; the remaining tests cover the changed helper's reachable branches.

VERDICT: ACCEPT WITH NITS

