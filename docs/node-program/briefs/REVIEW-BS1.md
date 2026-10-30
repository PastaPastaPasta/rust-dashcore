# Review brief: BS1 (base-sdk re-pin + #1110/#1142 + dedups)

Program context: a multi-month effort turning rust-dashcore into a full Dash node with bit-for-bit consensus parity with Dash Core. Read /work/home/agent/workspace/node-program/PROCESS.md (review rules) and research/R3-base-sdk.md §4.2 Phase 1.

Branch under review: `node/bs1-repin-base-sdk` in /work/home/agent/workspace/rust-dashcore (worktree /work/home/agent/workspace/rdc-wt/bs1-repin-base-sdk). Base: `node/integration`. Diff: `git -C /work/home/agent/workspace/rdc-wt/bs1-repin-base-sdk diff $(git -C /work/home/agent/workspace/rdc-wt/bs1-repin-base-sdk merge-base HEAD node/integration)..HEAD`, plus `git log` for the 27 commits (#1110 and #1142 are upstream PRs by kwvg, cherry-picked; see `gh pr view 1110 -R dashpay/rust-dashcore` and `gh pr view 1142 -R dashpay/rust-dashcore`).

Scope: (1.1) re-pin dash-pkc/dash-types to base-sdk develop 33fcc59, plus a one-rev test; (1.2) land #1110 and #1142 (Arith256-backed Work/Target, nBits decode via dash_num::CompactTarget, work computed like Core's GetBlockProof); (1.3) dash-spv ChainWork → dashcore::Work; (1.4) EddsaPkHash newtype over dash_pkc's; (1.5) drop the `codec` feature, use ff directly.

The author reports: fmt/clippy clean, cargo test --workspace 2565 passed/0 failed with the dashd suites included; persisted formats unchanged with pinned tests; behaviour changes: Work/Target display is hex, negative or overflowing nBits → zero work, wrapping Work addition.

What to check (be adversarial):
1. Consensus parity of the PoW/work maths against Dash Core: compare `arith_uint256::SetCompact`/`GetCompact` (src/arith_uint256.cpp), `GetBlockProof` (src/chain.cpp), and CheckProofOfWork (src/pow.cpp) in /work/home/agent/workspace/dash. Negative or overflow flags, the zero target, edge cases of the compact encoding (mantissa high bit, size>32, size<3), and work = ~target/(target+1)+1. Write or run extra edge-case tests if needed; you may add scratch tests in a temporary copy, but do NOT commit to the branch.
2. Whether "wrapping Work addition" can diverge from Core's behaviour in any reachable case (Core's arith_uint256 operator+ also wraps; confirm).
3. Conflict resolution correctness between #1110 and #1142 (nothing lost or duplicated).
4. Persisted and serialized format stability (bincode/serde/FFI strings) for Work, Target, ChainWork, EddsaPkHash, PlatformNodeId. Display change from decimal to hex: who consumes Display (FFI, RPC JSON, logs, Platform)? Does anything parse it back?
5. API breakage for downstream consumers (platform, dash-evo-tool, the FFI crates) and whether it is reasonable.
6. General correctness, test quality and dead code.

Re-run the gates yourself at minimum: `cargo test -p dashcore -p dash-spv --lib` and the pow tests. (The full dashd suite is optional; it is slow and the box is busy.)

Output: write your review to the path given in your task. Each finding gets: severity (blocker / major / minor / nit), file:line, the concrete failure scenario, and a suggested fix. End with a verdict: APPROVE, APPROVE-WITH-NITS, or CHANGES-REQUIRED. Don't modify the branch.
