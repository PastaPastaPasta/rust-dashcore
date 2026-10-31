# LQ1a round-1 review (Sol)

Reviewed LQ1a's own commits through `bbf8c78a6` in a detached worktree against the pinned Dash Core v24 oracle (`dash-oracle-sys/pins.txt`: `d4c16888b45a2de134489e2d4b2c078ec819ba7f`). The integration merges were context, not review changes. The source worktree was read only.

## Findings

- **Major:** none found.
- **Minor:** none found.
- **Nit N1 — fuzz coverage of the ChainLock modifier is narrow.** The generated L1 scenario seeds the same plain coinbase at every height (`node-l1/src/llmq.rs:414-425`), so `diff_quorum_members` cannot exercise Core's non-null coinbase ChainLock branch (`src/llmq/utils.cpp:87-96`; `src/evo/cbtx.cpp:200-210`). This branch is covered by the recorded regtest test (`node-l1/tests/l1_llmq.rs:641-705`) and direct modifier tests (`dash-evo/src/llmq/tests.rs:322-393`), which caught the ChainLock mutant below. A future fuzz input with varied valid CbTx data would broaden coverage. This is a test coverage suggestion, not a parity finding.

## Core comparison

- **Gates and routes match.** Genesis and type gates, V17/DIP0020/DIP0024/IS/50_60 branches, and unknown-type refusal (`dash-evo/src/llmq/params.rs:37-87`, `members.rs:119-172`) follow Core `src/validation.cpp:6654-6705` and `src/llmq/utils.cpp:641-684`. The rotated index limit and cycle predecessor are handled before computation; LQ1b's rotated computation is intentionally outside this review.
- **Member inputs and order match.** The list is read at the base before V20 or eight blocks earlier after V20; only cycle bases of non-rotated types compute (`dash-evo/src/llmq/members.rs:183-210`; Core `src/llmq/utils.cpp:291-304,715-729`). Platform uses Evo-only entries after V19. Banned and unconfirmed entries are excluded; scores are a single SHA-256 over the stored confirmed/proTx digest and modifier, sorted descending as an `arith_uint256`, with collateral hash then index as Core's tie-break (`dash-evo/src/llmq/score.rs:13-57`; Core `src/llmq/utils.cpp:72-85,117-175` and `src/primitives/transaction.h:75-81`).
- **Modifier bytes and fallback match.** Pre-V20 hashes `(type, base hash)` for non-rotated types; V20 reads the work block, takes `(type, little-endian work height, BLS signature bytes)` when the CbTx ChainLock is valid under the context's scheme, or `(type, work hash)` otherwise (`dash-evo/src/llmq/modifier.rs:26-98`, `src/cbtx.rs:239-280`; Core `src/llmq/utils.cpp:87-115`, `src/evo/cbtx.cpp:181-210`). A withheld work list or coinbase becomes `Missing`, while Core's own unavailable block data takes its hash fallback (`dash-evo/src/llmq/ctx.rs:139-159`, `members.rs:191-198`; tests at `tests.rs:397-429`).
- **Caches match Core's keys, sizes, order, and reset behavior.** The base and indexed member caches, quorum-object cache, and scan cache use the corresponding hash or `(cycle hash, index)` keys. Per-type sizes, access stamping, and truncation after exceeding twice capacity agree with Core (`dash-evo/src/llmq/reference.rs:39-187,214-292,768-786,856-901`; Core `src/unordered_lru_cache.h:26-113`, `src/llmq/cache.h:37-100`, `src/llmq/utils.cpp:634-729`, `src/llmq/quorumsman.cpp:203-215`). The reference model is test-only.
- **Scans and signing selection match.** Count zero and disabled types return before the cache; the store block uses the same mining window boundaries. Cache hits truncate or continue from the oldest base's parent, misses scan at least `keepOldConnections`, and all built bases are materialized before truncating the returned prefix (`dash-evo/src/llmq/scan.rs:50-76,131-225`; Core `src/llmq/quorumsman.cpp:170-285`). Signing uses the same start height, rotated signer bits and `signer > size` check, or least double-SHA256 of `(type, quorum hash, selection hash)` with position tie-break (`dash-evo/src/llmq/selection.rs:21-80`; Core `src/llmq/quorumsman.cpp:711-775`). Context construction bounds positive DKG intervals, cache sizes, and rotated signing counts (`dash-evo/src/llmq/ctx.rs:45-71`).
- **Oracle refusal pairing does not accept a one-sided refusal.** Scenario loading checks both sides (`node-l1/src/llmq.rs:476-492`); member and scan/select calls require Core's expected refusal class and return divergence on a one-sided refusal (`:529-546,643-660,1248-1276`). Accepted members, base hashes, and trace events are compared. Rotated member counts are intentionally deferred to LQ1b (`:1111-1139`), while LQ1a's non-rotated path is compared fully.

## Validation and mutation checks

- Baseline: `cargo test -p node-l1 --features oracle --test l1_llmq` — **13/13 passed**, including recorded regtest/mainnet/testnet quorums, boundary rows, public seeds, and proptest twins. `cargo test -p dash-evo --features reference,test-utils --lib llmq::` — **26/26 passed**.
- Mutant 1: reverse collateral hash comparison in `score.rs:29`; `members_boundary_rows` **failed** (exit 101).
- Mutant 2: use the work hash instead of the valid coinbase ChainLock at `modifier.rs:81`; `regtest_quorums_match_quorum_info` **failed** (exit 101).
- Mutant 3: scan one fewer commitment on a cache miss at `scan.rs:149`; `scan_boundary_rows` **failed** (exit 101).
- Every mutation was restored before handoff. The pinned Core archive under `/work/cache/oracle` was linked; Core was not rebuilt. No live RPCs were run. No network-reachable panic or allocation without a chain/configuration bound was identified. Non-rotated member calculation sorts the current valid list, `O(M log M)` per cache miss; scan work scales with requested count and the configured `keepOldConnections`, as in Core.

APPROVE-WITH-NITS
