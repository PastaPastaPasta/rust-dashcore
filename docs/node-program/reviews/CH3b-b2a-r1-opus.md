# CH3b b2a — independent review, round 1 (Opus)

Branch `node/ch3b-b2a`, range `d14c25fcd..376dc8cdf` (12 commits, 13 files). Head verified at 376dc8cdf.
Read against CH3 §7.5/§8.2, O2b-diff-chain.md, the pinned shim (`oracle_chain.cpp` at 77dbec8242,
`node/bl211-oracle-v24.0`) and Dash Core v24.0.0-rc.3.

**Verdict: ACCEPT WITH NITS.** All five claims hold. The comparisons are real. No plane passes silently, and
mutants of the order, the overlay, the digest, the CL record and the block-RPC dispatch are all killed. What's
left is test gaps: one served parameter that nothing exercises, one weak assertion, and the Diff-mode
classification items the tier-1 test (DEC-504) should cover.

## Findings

### Major
None. (The missing `fuzzctl smoke --only diff_chain` is superseded by DEC-504, so it's not flagged.)

### Minor
1. **`reconsider`'s `ignore_chainlocks` is served but nothing tests it.** See `node-l1/src/chain/node.rs:604-607`
   and `node-l1/tests/l1_chain_reorg.rs:155`. Mutant m10 inverts the flag passed to V, and it survives all six
   reorg suites and all of `l1_chain_diff`. In Core the flag decides whether `ResetBlockFailureFlags` clears a
   ChainLock-conflict mark (`rpc/blockchain.cpp` reconsiderblock, `ResetBlockFailureFlags(pblockindex,
   ignore_chainlocks)`). `a_chainlock_on_a_fork_is_enforced_onto_it` already builds that state, with the main
   block marked conflicting. A directed `RECONSIDER` of that block with `ignore_chainlocks` false, then true,
   held the same on both sides, would kill the mutant. Diff mode can't reach this state while `CL_RECORD` is
   refused, so the directed suite is the only place it can be covered.
2. **The INVALIDATE/RECONSIDER test doesn't pin what RECONSIDER does.** At `node-l1/tests/l1_chain_reorg.rs:117`
   it asserts only that some op reorgs, and the first `INVALIDATE` already satisfies that. Every op is held equal
   to Core, so if both sides' RECONSIDER became a no-op, the test would still pass. My probe shows the programs
   really do what the doc says: tip RECONSIDER connects 1; mid-chain RECONSIDER disconnects 2 and connects 4;
   the fork RECONSIDER disconnects 1 and connects 3. Assert the per-op depths the way the race and depth tests
   do.
3. **Diff-mode classification gaps for the planned tier-1 test (DEC-504).** No test plants a `post.utxo`
   difference on V's *real* step, i.e. one that also differs in the gap planes:
   - `a_difference_behind_a_gap_plane_is_named_by_its_own_plane` (`l1_chain_diff.rs:553`) plants only `index`
     and `tips`.
   - `each_plane_of_a_reorg_is_compared` (`:501`) plants events, checks, tip, best header, index and tips.
   - `every_plane_of_a_step_is_compared` covers `post.utxo`, but only with Core's step standing in for the
     node's, so there are no gaps.

   The tier-1 test should assert:
   - (a) a utxo-only difference on a real node connect, and on a reorg step, is `Diverged("post.utxo")`, not the
     `records` or `post.flushes` gap;
   - (b) `INVALIDATE`, `RECONSIDER` and `PRECIOUS` as an input's first op are served and end on the
     `post.flushes` gap, not `Unsupported` (the implementer's untracked probe saw `gen001: precious -> gap
     post.flushes`);
   - (c) `CL_RECORD` stays `Unsupported` in Diff mode (it already is, in `each_unsupported_op_kind_is_refused`).

   Context: until a7, every Diff-mode input whose first op is a block ends there on the `records` gap. So in
   Diff mode the new utxo plane is compared on the first op only, and all the multi-op coverage comes from the
   directed suites.

### Nits
1. **Two mutants are killed only by the chainstate unit test.** m1 (key-byte, i.e. little-endian index, order)
   and m3 (skip F) are killed only by `tip_coins_lays_f_then_active_over_the_database`
   (`dashd-chainstate/src/coins/tests/cache.rs:161`). That's expected: regtest never has a vout ≥ 256, and F is
   never alive at a post in these suites, so the Core-checked suites can't see either. D192 could say so, so
   nobody assumes the Core comparison covers the F overlay.
2. **Wrong wording in a doc comment.** At `l1_chain_reorg.rs:101`, "with the chain built on past the invalidated
   block" is wrong: `extend main 2` after the invalidation builds a new branch on the invalidated block's parent
   (tip 201→203), not past it.
3. **The legacy reorg test doesn't check that b1's outputs are gone.** At
   `dashd-chainstate/src/chain/tests/connector/disconnect.rs:765` the sibling case checks the two restored
   coins but not that `t1`'s outputs were removed after the reorg. One more outpoint in the snapshot would
   cover it.
4. **The reorg depth tests rely on the generator for spends.** At `l1_chain_reorg.rs:69` nothing in the test
   asserts that the blocks spend. My probe confirms they do: every new block spends 2 coins, the branches share
   32 spent outpoints at depth 30, and txouts go 503→590→593 across the reorg. Asserting `w.blocks[..].spends`
   through `same_program_in` would make the name self-checking.
5. **`attach_parts` is `&mut self` in every build** (`dashd-node/src/chainstate.rs:410`), only to keep the
   test-support cell. It's harmless; returning the cell through `AttachCoins` under the same cfg would keep the
   production signature as it was.
6. **O2b-diff-chain.md:330 still lists `utxo` among the planes V lacks.** The implementer's D192 notes this row
   is O2b's to update; make sure someone owns it.

## Checks answered

- **Does each comparison really compare?** Yes.
  - `assert_posts` requires `node.utxo.is_some()` and compares `utxo` before every other post field.
  - m6 (the digest of an empty set) fails all 6 reorg tests and 11 of `l1_chain_diff`'s.
  - m4 (spent entries not removed), m5 (skip active), m7 (drop a coin) and m11 (amount not summed) each fail
    all 6 reorg tests, on `block: utxo` at the first spending block.
  - Events are taken once in `finish` (`listener.take()` after the fence and the queue sync) and compared
    before anything drains them. `record_verified_chainlock` takes the listener first and finishes like a step,
    so nothing leaks into the next op.
- **Is the COutPoint order exactly Core's?** Yes. Core orders by `hash.Compare` (memcmp of the internal bytes)
  and then `n` numerically (`primitives/transaction.h:75-79`, `uint256.h:55`). `OutPointKey.0[..32]` is
  `op.hash.as_bytes()`, which is the same internal order, followed by `vout()` as a u32.
  - m1 (little-endian bytes) is killed by the unit test.
  - m2 (reversed txid) and m15 (descending index) are killed by all 6 reorg tests.
  - `utxo_digest` matches the shim's `Utxo()` (`oracle_chain.cpp:650-673`): one SHA-256 over `outpoint <<
    uint32(h*2+cb) << txout`, then the count and the int64 total. `tx_out_ser` expands the compressed script, so
    it serializes the same bytes.
- **Is the CL pairing faithful?** Yes.
  - The session suppresses the record's zero-delay schedule (`skip_cl_schedule`, `handler.cpp:200-213`), so
    Core enforces only at `CL_ENFORCE`. V queues `ClEnforce{Recorded}` after the reply, which is real Core's
    order, and the step's fence pulls it in.
  - The only difference the pairing hides is Core's recorded-but-unenforced state between the two ops. V never
    has that state, and real Core leaves it at once. The test puts no op between the two.
  - The split point is pinned on both sides (`(0, 2)` for Core, `2` for V). The joined events and checks are
    compared in order, and so are both replies and the full post after `CL_ENFORCE`.
  - Mapping the reply from `relay` gives Core's `accepted`/`not-better`. The absence of a `seen` check is
    documented, and `CL_RECORD` stays refused in Diff mode.
  - m8 (skip the record) is killed ("V reorgs at the record").
- **Is the coin read free of flush and side effects?** Yes.
  - It's a class R `Query` (`actor.rs:1497`): a closure over `&ChainQuery`, with no scope or flush step.
  - `tip_coins` takes `&self` and reads a `MemoryEngine` snapshot cursor.
  - The cell is V's leaf. W holds the engine, not the coins mutex, and the connector's guard lives for one
    attempt only, so the `try_lock` on V is uncontended between commands.
  - It meets DEC-501's "no-flush test-support read".
- **Do the reorg tests reach their depths and really spend?** Yes.
  - The depth is asserted on Core's `BlockDisconnected` counts (`[depth]`), and V's events are held equal to
    them.
  - The probe confirms the spends and re-spends (Nit 4). The race, INVALIDATE/RECONSIDER and mid-batch programs
    behave as their docs say (probe output in `logs/`).
- **Is the `cfg(not(test))` switch absent from every non-test build?** Yes.
  - `cfg(test)` is set only for dashd-chainstate's own unit-test build. dashd-node, rdashd and node-l1, with or
    without `test-support`/`sut`, compile dashd-chainstate as a plain lib, so they keep the `debug_assert`.
  - `cargo tree -p rdashd -e features,normal -i dashd-chainstate` and the normal workspace graph show no
    `test-support`. Both lib.rs canaries refuse it in optimized builds.
  - The new `tip_coins` is `cfg(any(test, feature = "test-support"))`, and `coins_cell`/`NodeSut::tip_coins` are
    behind dashd-node's `test-support`. Nothing leaks into production.
  - The `v:3621` citation checks out against rc.3 (`AbortNode "Failed to disconnect block"`).
- **Security:** nothing security-relevant found.

## Evidence

The tree is a reflink copy (`tree/`, `.git` pointer disabled, `target` symlink repointed to my own target),
built with mutants gated on `B2AMUT` (`mutate.py`) and run via `run.sh` under the units `ch3b-b2a-rev-1..3`.
The untracked `zz_b2a_diff_probe.rs` was never built or run.

| run | result |
|---|---|
| baseline: chainstate `tip_coins` + legacy | 3/3 ok |
| baseline: `l1_chain_reorg` | 6/6 ok (34 s) |
| baseline: `l1_chain_diff` | 16 ok, 1 ignored |
| review probe (spends, block-RPC per-op) | ok |
| m1 key-byte order | unit KILLED; reorg survives (expected) |
| m2 reversed txid | KILLED (unit, 6/6 reorg) |
| m15 descending vout | KILLED (unit, 6/6 reorg) |
| m3 skip F | unit KILLED; reorg + diff survive (F never alive at a post) |
| m4 keep spent | KILLED (unit, 6/6) |
| m5 skip active | KILLED (unit, 6/6) |
| m6 empty-set digest | KILLED (6/6 reorg, 11 diff) |
| m7 drop last coin | KILLED (6/6) |
| m11 amount not summed | KILLED (6/6) |
| m8 skip CL record | KILLED (CL test) |
| m9 INVALIDATE served as PRECIOUS | KILLED (reorg invalidate test, `the_block_rpcs_are_the_same_on_both_sides`) |
| m10 `ignore_chainlocks` inverted | **SURVIVED** all (Minor 1) |

Logs: `/work/scratch/ch3b-b2a-review/logs/`. Scratch to remove after the round: `/work/scratch/ch3b-b2a-review`.
Its target was seeded from `/work/scratch/ch3b-b2a/target`, so delete this one first.
