# CS6 independent consensus-path review, round 2

Reviewed the fix-round delta `9c300f58a..f1f32f12c` on `node/cs6-sigcache` at `f1f32f12c`, limited to closure of my round-1 Major and regressions introduced by this delta. I assume the caller supplies the actual spent output scripts, as in round 1; the production block connector and mempool caller are still planned rather than present here.

## Findings

None.

The round-1 Major is closed. `TxSlot` has private fields and only `new(&Tx)` constructs it (`dashd-chainstate/src/checkqueue/script.rs:89-115`); it has no `From`, `Clone`, `Default`, setter, or unsafe fill path. Its `OnceLock` initializes `TxSigData` exclusively from `self.tx.raw()`. Concurrent `get_or_init` calls can compute the same transaction's data more than once, but only one value is published, and every candidate refers to the same immutable `Tx`. `check_input_scripts` derives the execution-cache key from that slot's `Tx` (`:167-175`) and feeds the slot's data to both serial and queued script checks (`:179-207`), so safe callers cannot key B while executing A. The only production execution-cache read and insert sites remain in this function (`:175,212`); `ScriptExecCache::cache` is now crate-visible rather than public (`dashd-chainstate/src/sigcache.rs:335-339`). The changed block, mempool, vector, oracle, and bench callers each construct slots from the transaction they check. The new regression (`dashd-chainstate/src/checkqueue/script/tests.rs:223-255`) checks that B's invalid signature is rejected after A was stored, including the parallel verdict and absence of a B cache entry.

I inspected the complete delta, repository references to `TxSlot`/`TxSigData::new` and execution-cache access, and the immutable `Tx` and `TxSigData` definitions. `git diff --check 9c300f58a f1f32f12c` passed. I did not rerun builds or tests; the brief reports debug/release, feature, oracle, loom, and lint evidence. No delta-introduced consensus-path issue found.

VERDICT: ACCEPT
