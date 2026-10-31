# CH3a a13 (CH3 §6.2 L1/L2): independent review r1 (Opus)

Branch `node/ch3a-a13`, worktree `rdc-wt/ch3a-a13`, head c89a0fc3b, base 463262a24. Code commits f3195b2fb,
369b2e87c, d41f970b7; the merge 34281053c changes none of the reviewed files (a13's diff is the same against 11b25d405).
All commits are signed (`%G?` = G). I did not modify the branch. Mutants ran only in a reflink copy
(`/work/scratch/ch3a-a13-review/src`, target seeded from a13's debug target).

Core reference: v24.0.0-rc.3 (`~/workspace/dash`), `consensus/tx_verify.cpp:132-208`, `coins.cpp:44-68, 303-313`,
`validation.cpp` ConnectBlock loop, `evo/specialtxman.cpp:1815` (CheckSharedCollateralSpends).

## Verdict: ACCEPT WITH NITS

No Major findings. The memo is equivalent to the direct overlay reads on every path I could construct. The one-pass
`check_tx_inputs` gives Core's verdict, reason, debug string and sum. The xcheck wiring matches D189 and does catch a
memo/overlay mismatch. Two Minor findings: a test gap that leaves one real Core divergence uncaught, and one overlay read
per input that D186's count leaves out. Neither blocks.

## 1. Equivalence of the memo with Core's loop

- **Reject order, reason and debug string.** Core: `CheckTxInputs` → `HaveInputs` (stops at the first missing input) →
  a loop that returns at its first premature or out-of-range reject → in-belowout → fee-outofrange. Then the sequence
  locks (`AccessCoin` per input), then sigops (`GetP2SHSigOpCount`'s `AccessCoin`), then scripts. `TxCoins::resolve`
  runs after `have_inputs` and stops after the first input with no coin. `check_tx_inputs` returns at that input, so it
  never asks past the memo. Every later read (`access`, sigops, the spent-script copy) runs only after `check_tx_inputs`
  has passed, so every input is resolved and present. Resolve itself can fail only with `read_failed`, the same Fatal the
  old in-`check_tx_inputs` read produced, at the same point (after `have_inputs`). So no reject changes order.
- **Stale or wrong coin.** `TxCoins` holds `&'v BlockOverlay` from the resolve to its last read. The overlay's reads are
  `&self`, and the only interior state they touch is `scratch` (bytes for DB hits, which never change a value). The first
  `&mut` call is `spend` in UpdateCoins, so the borrow checker rules out a change in between. `arena.reset()` needs
  `&mut` on the arena, so the previous transaction's memo is dead before the next one is built.
  - Intra-block spends: the next transaction's memo is resolved after the previous transaction's `add` and `spend`, from
    the child, so a double spend gives the child's tombstone, `None`, and `missingorspent`, as before.
  - Duplicate inputs (unreachable after CheckBlock's `bad-txns-inputs-duplicate`): both reads give the same coin, and
    the second `spend` is the same "SpendCoin: a checked input is spent" Fatal as before.
  - BIP30: its probes and the coinbase's outputs don't go through the memo (the coinbase's memo is empty).
  - Disconnect/undo: the memo isn't used. The undo is written from `spend`'s returned coin, not from the memo.
  - Asset unlock: `check_tx_inputs` returns before any read, as Core's does. A transfer with inputs is rejected earlier
    (`bad-assetunlocktx-have-input`), and if one got here anyway the behaviour would be the old one (the `access` Fatal).
- **`CoinRef` equality** is derived (content). `CoinsView::have_coin`'s default goes through `coin`, so no trait method
  bypasses the memo.

## 2. xcheck coverage (D189)

All four `ConnectorParts` constructions checked: `replay_core::Rig::start` (`xcheck: true`, used by `digest l2` and by
`replay_node`/`replay_window` via `run`), the connector matrix (`!setup.no_xcheck`), `node_stores` (false), and
dashd-node `Chainstate` from `CoinsSetup::xcheck`. Of that, `NodeSut` sets true (`sut.rs:323`) and `node.rs` sets false.
`diff_chain`'s fuzz target and the node-l1 chain suites reach the node through `NodeSut` (`node-l1/src/chain/node.rs:746`).
The CRASH rigs and the bench build `SerialCore::new` directly with xcheck off, as D189 says.

xcheck now reads the overlay at the outpoint the caller asked for (`tx_coins.rs:130-140`). It would catch a memo/overlay
mismatch. Mutants confirm it: M9 (xcheck off in `read`), M10 (xcheck keyed by the slot's input, the validator's old M1)
and M13 (the loop passes `false`) are all killed by `xcheck_catches_a_memo_that_answers_the_wrong_input`
(`chain/tests/connector.rs:1402`). M9 is also killed by the unit control. Neither control is tautological: each
asserts the planted fault is accepted with xcheck off and stops V with it on.

## 3. `check_tx_inputs` single read (D188)

Overflow, negative value and fee: the code after the loop (`value_in < value_out`, fee range) is unchanged. In the loop,
`wrapping_add` and the two `money_range` checks run in Core's order. Once a reject is kept, the loop stops adding and
only looks for a missing input, so with no reject `value_in` is Core's `nValueIn`, and with a reject the sum is never
used. The premature debug string is the first premature input's depth, as Core's. Class, reason and debug are the same
as before for every combination in §4, except the one the tests miss (Minor 1, which the code handles correctly).

## 4. Mutants (env-gated `A13MUT`, one build, `mutate.py` in this dir)

| # | Mutant | Result |
|---|---|---|
| M1 | later rejects overwrite the kept one | killed (`the_first_reject_…`) |
| M2 | no `continue` after a premature spend | **survived** all 42 dash-consensus test binaries (Minor 1) |
| M3 | kept reject dropped | killed (3 tests) |
| M4 | out-of-range returns at once (outranks a later missing input) | killed |
| M5 | running-sum range check dropped | killed |
| M6 | memo doesn't stop at the first missing input | killed (`the_memo_ends_…`) |
| M7 | cursor slot taken without comparing the outpoint | killed (2 tests) |
| M8 | cursor never advances | survived, equivalent (perf only) |
| M9 / M10 / M13 | xcheck off / keyed by slot / not passed by the loop | killed |
| M11 | memo ends before the missing input | killed (5 tests incl. digest connector) |
| M12 | `rposition` for `position` | survived, equivalent (same outpoint → same coin) |
| M14 | `prev_heights` reads input 0's coin for every input | **survived** the whole lib, 739 tests (Nit 1) |
| M15 | arena never reset | survived, equivalent (memory only) |

Baseline: inputs_and_locks 19/19, dash-consensus lib 162/162, dashd-chainstate lib (alloc-tag, crash, planted-bugs,
statedigest, test-support) 739/739.

## Findings

### Minor

**Minor 1. The suite doesn't pin that a premature spend is rejected before its value is added.**
`dash-consensus/src/tx/inputs.rs:95` (the `continue` after the premature reject), test at
`dash-consensus/tests/inputs_and_locks.rs:153`.

Deleting that `continue` (M2) passes all 42 dash-consensus test binaries, but it is a real divergence from Core. Take
input 0 = a non-coinbase coin of `MAX_MONEY` and input 1 = an immature coinbase of `COIN`. Core returns
`bad-txns-premature-spend-of-coinbase`, because it checks maturity before `nValueIn +=`. The mutant falls through, adds,
and overwrites the reject with `bad-txns-inputvalues-outofrange`. The new single-pass shape is what makes this
fall-through possible, so a13's test should pin it. Verified in the copy: this test passes on the branch and fails
under M2:

```rust
#[test]
fn a_premature_spend_outranks_the_sum_it_would_overflow() {
    let coins = Coins::default().with(5, 0, MAX_MONEY, 1, false).with(1, 0, COIN, 100, true);
    let got = rejection(check_tx_inputs(&Fields::spending(&[(5, 0), (1, 0)], &[1]).tx(), &coins, 101));
    assert_eq!(got.1, "bad-txns-premature-spend-of-coinbase");
}
```

(It is unreachable on a real chain, where no UTXO sum reaches MAX_MONEY. But the function is shared with the mempool
and diff-fuzzed against Core, and `the_first_reject_…`'s own premise is exactly this ordering.)

**Minor 2. D186's "three resolves per input" leaves out H8's read on V24 blocks, which the memo and xcheck don't cover.**
`dashd-chainstate/src/connect/serial.rs:873`; `docs/node-program/designs/CH3a-notes.md:127` (D186), :130 (D189).

Between the memo's resolve and UpdateCoins, `steps.shared_collateral_spends(tx, &*overlay)` reads the overlay directly.
Core's `CheckSharedCollateralSpends` (`specialtxman.cpp:1815-1830`) does an `AccessCoin` per input when V24 is active.
The hook takes `&dyn CoinsView<Error = CoinsReadError>`, so it can't get the memo, whose error type is `Fatal`. So on
V24 blocks it is four resolves per input, and that read isn't compared under xcheck. a13 didn't add it, and it is
correct as is (`&*overlay` is the same state). But D186's count, and D189's "every … read", should say that V24 adds
H8's read outside the memo. Name it as a BL candidate with D186's two-resolve alternative (route H8 through a
`CoinsReadError` view of the memo). None of the corpus legs covers V24 here: testnet 0..=6999 is pre-V24, the regtest
nodes are non-evo, and NoEvo refuses V24 under G1. So the evidence doesn't cover it either way.

### Nits

**Nit 1.** `dashd-chainstate/src/connect/serial.rs:896`: nothing in the dashd-chainstate lib pins which coin each
`prev_heights` entry comes from. M14 (every input reads input 0's height) passes all 739 tests. xcheck can't catch it,
because the read is keyed by the outpoint asked for, and here the wrong outpoint is asked. The gap predates a13, but
a13 rewrote this line. A connector case with a 2-input BIP68 spend whose inputs have different heights, where only the
second input's relative lock fails, would pin it.

**Nit 2.** D190 / the brief: the §8.1 gate says "xcheck on … `diff_chain`". The recorded evidence is node-l1's chain
suites through `NodeSut` (`l1_chain`, `l1_chain_diff`, `l1_chain_params`). There is no `fuzzctl smoke --only diff_chain`
at the head; only `diff_check_tx_inputs` was smoked. If `l1_chain_diff` is meant to be the diff_chain evidence, say so in
D190. Otherwise add a smoke row.

**Nit 3 (informational).** M8, M12 and M15 are equivalent mutants (cursor advance, `position`/`rposition`, arena
reset). No test change is needed. L2's "no per-input allocation" is pinned only by PF0's allocation counters, not by a
lib test. That is acceptable given D190's PF0 evidence.

I found nothing else a13 adds per input: no extra resolve and no allocation. The memo's slice comes from the reused
`Bump`, `format!` runs only on error paths, and xcheck's extra read is test/replay-only. D187's arena bound (≤ 24,390
inputs × 40 B, from the 1 MB transaction limit) is correct. The validator's N6 figure of 49k inputs used the block
limit instead.

No security-relevant observations.
