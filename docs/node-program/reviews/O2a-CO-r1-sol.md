# O2a-CO review, round 1

Reviewed rust-dashcore `7129592d7` and Dash Core `06926097b45850dfa24502683880e7206d001bc4` against the brief. No blocking correctness or FFI safety finding.

## Oracle semantics

- `oracle_coins_ops` constructs a fresh `CCoinsViewDB` and `CCoinsViewCache` stack per call. Each operation dispatches to Core's `AddCoin`, `SpendCoin`, `AccessCoin`, `HaveCoin`, `HaveCoinInCache`, `Flush`, `Sync`, `Uncache`, `BatchWrite`, `DynamicMemoryUsage`, or best-block method (`oracle_coins.cpp:225-355`). `ENTRY` and `SEED` alone touch the map directly, as documented test helpers. The database state reader uses Core's DB wrapper and iterator.
- `USAGE` calls `CCoinsViewCache::DynamicMemoryUsage()` and `GetCacheSize()` directly (`oracle_coins.cpp:321-324`). Core computes this from its pool/map allocation estimate and cached coin script usage (`coins.cpp:40-42`); it is not a measurement of malloc's actual returned bytes. The `uint64_t`/`u32` wire fields match `size_t`/`unsigned int` on the pinned x86_64 build. A caller can interleave `USAGE` after each successful op; a terminal logic error or assertion has no post-op usage record.
- The shim checks the two reachable fatal assertions before calling Core and ends the script, which avoids process termination. Core's actual `std::logic_error` text is retained by the Rust binding. Deterministic cache salts remove hash iteration variability; `-dbcrashratio=0` prevents a simulated `_Exit` during small DB batches. The per-call `ArgsOverride` restores the previous forced settings.

## A1: child-read caching

The oracle is faithful here. A child `FetchCoin` calls through its parent, so a database hit inserts a clean coin into both child and tip (`coins.cpp:44-59`); CH2's `BlockOverlay::resolve`/`have_coin` use `&self` and do not cache (`overlay.rs:204-233`). The harness records a key only when that database-hit condition holds, and clears it after a tip read/spend, effective tip add, uncache, commit of that key, or tip flush (`node-l1/src/coins_ops.rs:545-654,717-835`). The unspendable-add fix correctly leaves the marker in place. The seed cases reach the skipped `HaveCoinInCache` answer and a later FRESH commit error.

This is a **comparison limitation for CH2c**, not an oracle error: `Allowed::ChildReadCaching` skips a tip cache answer or ends the run at an add/commit. In particular, the commit allowance checks that *some* dirty FRESH child key is in `child_read`, then accepts any Core logic error at that commit (`node-l1/src/coins_ops.rs:803-834`). In the current Core path that key is enough to cause `FreshMisapplied`, so the allowance is justified, but the early end cannot detect another error at the same commit or any later op. CH2c should use its planned aligned mode, or segregate scripts with this exception, before treating a passing fuzz campaign as full operation parity. It should count and report allowed exits and skips separately from fully compared runs.

## ABI, coverage, and safety

- `ffi::ABI_VERSION`, `oracle.h`, and the smoke test say 15. The `pins.txt` v24.0 line, both archive MANIFESTs, and actual SHA-256 hashes agree: plain `991d0e7385ed0c0461d4ab6c530d2ece79c4c4bef756421dbe3edd5fe37056d5`, fuzz `462b533cfd736dab942f11c72c6a2acb210e8226f3bd564fe00a9acb2423e355`. Both archives export `oracle_v24_0_coins_ops`; their `oracle.h` files match the Core head exactly. This verifies the checked-in artifacts and metadata, not an independent archive rebuild.
- The binding encodes one script, decodes per-op outputs with no trailing bytes, and runs the call under the pin's exclusive lock. Core wraps the body with `Guarded`, holds `ProcessStateMutex` while changing gArgs, checks buffer pointers, and uses `WriteBuffer`'s size negotiation. The buffer retry reruns a stateless script. I found no lifetime, ownership, or crossing-the-C-ABI exception issue in the new path.
- `dash-oracle-sys/tests/coins_ops.rs` transcribes Core's four single-entry state tables and covers logic-error texts, assertions, DB batch sizes, Sync, child read caching, invalid scripts, and repeatable usage. `l1_coins_ops` runs 13 hand-written seeds and 600 deterministic random inputs, including commits and tip flushes. `diff_coins_ops` is not yet registered as a node-fuzz target and the L1 smoke harness does not compare usage or entry flags; the notes explicitly assign those to CH2c's aligned mode. This is remaining CH2c work, not evidence that O2a-CO has fuzz campaign coverage today.

## Verification

Inspected both source heads and the pinned archive manifests, hashes, headers, and exports. Targeted `cargo test -p dash-oracle-sys --features oracle --test coins_ops`: 12 passed, 0 failed.

VERDICT: PASS WITH CH2c FOLLOW-UP
