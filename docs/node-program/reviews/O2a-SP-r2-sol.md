# O2a-SP oracle delta review (r2, Sol)

Reviewed Core `8319ddce40..d192d111cd` and rust-dashcore `dcbf2130a..d0238b265` (excluding generated test data), against the r1 findings and the v24 Core block connection path. This is a narrow source review of the delta. I did not edit either branch.

## Finding

### Major — NetInfo still reads `fAllowPrivateNet` without the process-state lock

**Location:** Core `src/test/oracle/oracle.cpp:3767-3805`, with `WithNetwork` at `:264-268`; compare `src/netaddress.cpp:95, 464-470`.

`oracle_netinfo_validate` and `oracle_netinfo_add_entry` call `WithNetwork`, which only checks the atomic `g_network_selected` flag. Neither entry holds `ProcessStateMutex` while `Validate()` or `AddEntry()` calls `CService::IsRoutable()`. `oracle_set_network` writes the plain `bool fAllowPrivateNet` under that mutex, and LLMQ or peer-session creation can reset it there via `ForgetNetwork`. A concurrent NetInfo call can therefore race the write and use a flag from a different selection. The sequential default/allowed/default test cannot exercise this.

Hold `ProcessStateMutex` through each NetInfo operation and check `g_network_selected` after acquiring it, so selection, flag and verdict are one operation. Cover a concurrent selection/NetInfo call if this entry supports concurrent callers.

## Re-check of r1 findings and the rest of the delta

- **Mined commitments:** The new helper uses a queue with no worker threads and accepts only `Wait()`'s result, as `ProcessBlock` does. It then calls `Verify(..., false)` for the member-tail check. The block path obtains members through the supplied context; a missing quorum becomes `ORACLE_E_CONTEXT`. `oracle_llmq_set_mined` computes members from its seeded state. On a missing list or row it restores the raw `llmq_S` rows, rolls back EvoDB writes, restarts the managers and caches, and returns `ORACLE_E_CONTEXT` without poisoning. The new tests cover the old three-bits/no-members fixture, invalid signatures, member tail, and a valid smaller member set. I found no remaining positive commitment fixture of the r1 kind. The earlier structural refusal helper runs before queued signature checks, so a commitment with multiple faults can receive a different diagnostic order from `ProcessBlock`; its accept/refuse result is unaffected.
- **Network state:** `oracle_set_network` resets the flag before parsing, applies the option after `SelectParams`, and refuses a live LLMQ context or peer session. `ForgetNetwork` resets it; the peer session saves globals after that reset. The address bucket and netgroup entries now hold the same lock. The NetInfo finding above remains.
- **First header:** Both context builders link a first header above height zero to a height-minus-one placeholder. The parentless refusal therefore applies to genesis. The new window and genesis cases exercise both sides of that boundary.
- **Node LQ1a and ABI:** The node harness derives the seeded operator keys, signs the commitment hash, and obtains rotated members from Core after precompute. The revised rotated test cycle meets the member minimum. Core's header and the Rust FFI constant both say ABI 16. The plain and fuzz archive SHA-256 values match `pins.txt` and identify Core `d192d111cd` in their manifests.

The supplied GATE-FAST result is 5121 passed, zero failed, with 267 oracle-diff checks passed. Locally, `cargo test -p dash-oracle-sys` passed (11 tests), then `cargo test -p dash-oracle-sys --features oracle` passed (185 tests, including the LLMQ, NetInfo and block-path suites). These are sequential tests; they do not exercise the concurrent NetInfo race above.

VERDICT: CHANGES-REQUESTED
