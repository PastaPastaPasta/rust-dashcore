# RP1a-2c-2 review (round 1)

**Verdict: ACCEPT-WITH-NITS**

I found no correctness or parity defect in the implemented `getblockstats` path, the
currently supported `pruneblockchain` behavior, or the subsidy seam. There are two test
coverage nits below. Counts: **0 M, 0 m, 2 n**.

## Findings

### n-1 — Mutant 33 is non-equivalent outside regtest; add a non-regtest subsidy vector

**Location:** `dashd-node/src/rpc.rs:629-631` (the call into
`block_subsidy_inner`), and `dash-consensus/src/subsidy.rs:260-266`.

The surviving mutant that replaces the indexed parent’s `nBits` with the network PoW
limit is equivalent for the current regtest DIFF because regtest’s generated blocks use
the minimum-difficulty bits and the pre-V20 formula is therefore insensitive in that
scenario. It is not equivalent on mainnet/testnet. For example, Core’s subsidy oracle
has a mainnet case for a block whose parent is at height 17,000 with `prev_bits =
508551233`: `GetBlockSubsidyInner` returns `15_300_000_000` duffs, while using the
mainnet PoW-limit compact value returns `500000000` duffs (the paired oracle values in
`dash-consensus/tests/data/oracle/subsidy.json` show this distinction). A concrete RPC input is `getblockstats(17001)` on a mainnet
chain containing that history; the `subsidy` field differs.

**Fix:** add a mainnet/testnet fixture or a direct `Chainman::block_subsidy`/subsidy
vector test at a pre-V20, non-PoW-limit difficulty (also retain a halving-boundary
case). This kills mutant 33 and proves the seam uses the parent header bits. The
production code already passes the parent bits and is correct.

### n-2 — The live DIFF never reaches the pruned-undo error path

**Location:** `rdashd/tests/rpc_diff.rs:2593-2601` and
`dashd-rpc/src/blockchain/stats.rs:193-200`.

The DIFF compares the HTTP replies byte-for-byte (after masking only `Date`; see
`rpc_diff.rs:349-372,423-445`) and covers unreadable/invalid-selection errors, but every
`pruneblockchain` call is made against a non-pruned node. Both implementations therefore
stop at Core’s `Cannot prune blocks because node is not in prune mode.` before any block
is pruned. The DIFF does not exercise `getblockstats` on a block with missing undo data,
so it cannot independently distinguish `Undo data not available (pruned data)` from
`Can't read undo data from disk`.

The unit tests do cover both mocked `UndoReadError` variants at
`dashd-rpc/src/blockchain/stats/tests.rs:178-190`; this is therefore a coverage nit,
not an implementation mismatch. Once CH8 enables pruning, add a pruned-node DIFF that
prunes a block and calls `getblockstats` by hash, asserting the exact pruned-undo error
(and a separate unreadable-file case if practical). Until then, the current refusal is
correct because init rejects `-prune` in this slice.

## What I checked

* Reviewed `git diff 4d7a9b564 50b596caf` plus quick checks of `db1155e00` and
  `4d7a9b564`; the crafted fixture rows are present and the docs pointer/D44 layer note
  are consistent.
* Compared every `getblockstats` field and gate with Dash Core v24.0.0-rc.4
  `src/rpc/blockchain.cpp:2211-2449`: fee totals and averages, truncated median,
  size-weighted percentiles, min/max fee/rate, output and UTXO deltas, subsidy, field
  ordering, selection de-duplication/error ordering, and height/hash lookup. The
  implementation matches Core’s loop at `:2322-2436` and output at `:2409-2448`.
* Checked Core’s `GetUndoChecked` errors at `src/rpc/blockchain.cpp:806-824` and
  `pruneblockchain` argument/refusal ordering at `:1077-1133`. The port intentionally
  implements only the non-prune refusal (`stats.rs:469-475`) while pruning is unsupported.
* Checked subsidy against Core `validation.cpp:1890-1895`: genesis special case, parent
  bits/height, and `DeploymentActiveAt` for V20. The halving DIFF covers regtest heights
  148–152, and the mainnet quirk-11 vector passes.
* Ran (all from the read-only worktree or its reflink copy):
  * `cargo test -p dashd-rpc --lib blockchain::stats --all-features` — 8 passed;
  * `cargo test -p dashd-node --lib --all-features` — 192 passed, 3 ignored;
  * `cargo test -p dashd-rpc --all-features` — 202 passed;
  * `cargo test -p dash-consensus --test quirk_11_subsidy_prev_bits_and_height` — 1 passed;
  * `cargo test -p rdashd --test rpc_diff getblockstats_match_dashd_v24 -- --nocapture` — passed
    (57.5 s);
  * `git diff --check` — clean.
