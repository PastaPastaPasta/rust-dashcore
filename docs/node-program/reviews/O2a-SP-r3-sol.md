# O2a-SP oracle delta review (r3, Sol)

Reviewed Core `d192d111cd..a575646185` and rust-dashcore `d0238b265..e60a9ada5`, including the 54 regenerated SB4 vectors. This is a narrow review of the f2 fix for my r2 Major. I assume the oracle is the only code in its process that selects Core parameters or changes `fAllowPrivateNet`, as its C API contract requires. I did not edit either branch.

## Result

The r2 Major is closed. `WithNetwork` acquires the nonrecursive `std::mutex` before checking `g_network_selected` and keeps it through the call body. Both `oracle_netinfo_validate` and `oracle_netinfo_add_entry` deserialize and run Core's verdict under that lock. `oracle_set_network` resets and applies `fAllowPrivateNet` and publishes the selection under the same lock; LLMQ and peer-session transitions also use it. A NetInfo call therefore cannot observe the transient default flag or a different selection's parameters.

Moving the lock out of `WithContext` and removing `WithLists` leaves one acquisition per path. I found no exported-entry call from a `WithNetwork` body and no path that takes `cs_main` before `ProcessStateMutex`; contextual and LLMQ paths take the process lock first. Peer-session `Hooked` calls already hold this lock. Larger `WithNetwork` operations, including block and list work, can delay a peer-session call while they run, but I found no new callback or lock cycle requiring the peer session to progress before they return.

The D44 audit is substantially correct: addrman operations, bucket, and netgroup use the process lock for routability reads; LLMQ and peer-session paths use it for their process state; the chain-reading entries use `WithNetwork`. One audit-table correction is below.

## Nits and test limits

- **D44 table:** `g_reachable_nets` is also read by `ExtNetInfo::AddEntry` through `MaybeFlipIPv6toCJDNS` (`evo/netinfo.cpp:543`, `netbase.cpp:833-838`). This call is inside `WithNetwork`, and `ReachableNets::Contains` has its own mutex, so it does not reopen the race. The table's claim that only session nodes read this global should be corrected.
- **Concurrent coverage:** `netinfo_concurrent.rs` exercises `oracle_netinfo_validate` for both NetInfo kinds, but does not call `oracle_netinfo_add_entry` concurrently. The latter is protected by the same `WithNetwork` helper, so this is a coverage suggestion rather than a blocking finding.
- **Bound:** The writer makes at least 2,000 and at most 200,000 selections, and the drop guard stops readers on normal or panicking writer exit. Calibration proves the torn `NotRoutable` verdict differs from both selections; the Mn verdict distinguishes mainnet from regtest, and the test requires both to be observed, preventing a false pass if the writer makes selections but readers miss one. There is no wall-clock timeout for a blocked FFI call or indefinitely starved writer; an outer CI job timeout is still needed to guarantee the gate cannot hang.

## Verification

- `cargo test -p dash-oracle-sys --features oracle --test netinfo_concurrent --locked` passed: 1 test, 0 failures. I used a private target directory and removed it afterward. The owner's reported f1 failures and 30/30 f2 passes were not independently repeated.
- `pins.txt` names `a575646185`; the installed plain and fuzz archive SHA-256 values match its `ca61d9e1…` and `cc0735de…` entries and their manifests. Core `oracle.h` and Rust `ffi.rs` both retain ABI 16.
- A semantic comparison of all 54 changed SB4 gzip JSON files found only provenance changes and two `instructions_u` changes: DGW `492345384 -> 491754283` and KGW `870901693 -> 869648296`. No other vector field changed.
- Both reviewed worktrees remained clean.

VERDICT: APPROVE-WITH-NITS
