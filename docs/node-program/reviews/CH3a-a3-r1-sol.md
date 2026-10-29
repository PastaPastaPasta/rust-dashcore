# CH3a a8 + a3 independent consensus review

Reviewed detached `ff4b4482d` against Dash Core `v24.0.0-rc.3`. I assumed the reported GATE-FAST and feature-gated CRASH results are accurate; I did not rerun their suites. The obsolete integration base is outside this review.

## Finding

- **Minor — differential evidence is deferred.** `dashd-chainstate/src/chain/tests/connector.rs:1074` and `:1117`, `dashd-chainstate/src/chain/tests/deployments.rs:453`, and `contrib/node/ch3a-connect-census.py:12` assert local expected results or check that Core's literal reasons appear in Rust source and tests. They do not compare the same crafted block's first failure, result class, or warning state with a running Core node. Core reference: `src/validation.cpp:2698-2703`, `:2731-2734`, `:2740-2759`, `:3222-3236`. **Fix:** keep the stated a7/L3 runtime Core differential gate and include cases with two simultaneous faults, plus warning threshold boundaries, before claiming full behavioral parity. This is a coverage limit for a3/a8, not a demonstrated divergence.

## Checks without findings

- `SerialCore::connect` follows Core's CheckBlock, ChainLock, BIP30/BIP34, special transaction, input, fee, sequence lock, sigops, serial script, IS, reward, undo and status order. The visible reject strings and result classes match their Core sites. The pinned-source census reports 11 literal rejects, 3 computed reasons, 5 AbortNode sites, 16 log lines, and zero failures. Parallel script failures remain in a4 by design.
- Streamed undo uses the same CompactSize nesting, height-zero legacy exception, `magic || size || undo || SHA256d(parent || undo)` record, and data position as Core (`src/undo.h:23-49`, `src/node/blockstorage.cpp:509-538`, `:773-803`). `HAVE_UNDO` follows a successful write; the declared D66 flush-failure difference stops the connect.
- The attempt opens before the active coins lock and reaches `finish_attempt` on each returned outcome; failures roll back the evo transaction. The unwind guard covers the open attempt until `finish_attempt` returns. The test for an evo commit failure explicitly records Core's coins-before-evo commit order (`src/validation.cpp:3446-3448`), so that partial in-memory state is not a newly introduced parity claim.
- The assumevalid ancestry/work/equivalent-time gate and its script flag pass-through match `src/validation.cpp:2542-2571`. The IS helper contract excludes removed locks for this attempt; the directed tests cover the chained-child and terminating-null cases.
- The a8 warning checker uses Core's period walk and condition, and the owned complete ancestry reads the same immutable header fields. Active warnings are staged until hold release under the documented D82 ordering choice; the predicate and text match `src/validation.cpp:2395-2417`, `:3222-3236`.

VERDICT: APPROVE-WITH-NITS
