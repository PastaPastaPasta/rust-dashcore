# CH3a a8 + a3, Opus r1 (second reviewer, consensus)

Range: a8 = `721815def..cfa6b4496`, a3 = `refs/review/ch3a-a3-base..ff4b4482d` (head ff4b4482d).
Core: v24.0.0-rc.3 (`v:N` = `src/validation.cpp`; `bs:N` = `src/node/blockstorage.cpp`; `stm:N` =
`src/evo/specialtxman.cpp`). I reviewed from the `node/ch3a-a3` worktree and `git show` of the Core tag.
I made no edits and ran no cargo builds (tier-1 is reported green on this head). Static check run:
`contrib/node/ch3a-connect-census.py --core <v24.0.0-rc.3 src>` → `11 literal rejects, 3 computed, 5
AbortNode, 16 log lines; 4 exempt; 0 failures`.

**Summary.** I found no consensus divergence. Every check runs in Core's order, with Core's reject
string and DoS class. No input I could construct gets a different first failure, except the
declared readiness refusals (D18, D54, D63, D69) and the declared undo-I/O differences (D66, D77).
The bracket meets D33/D71/D74/D75. The undo record is Core's `CBlockUndo` framing. The a8 warning
matches `UpdateTip`/`WarningBitsConditionChecker`. The findings below are two test gaps and three
hygiene nits. Nothing is security-relevant: no embargo file was written (see the last section).

## Findings

### F1 (Minor, tests): assumevalid's fact resolution is pinned for conditions 2 and 5 only
`dashd-chainstate/src/connect/serial.rs:314-335` resolves Core's five conditions (`v:2543-2568`)
against `cx`. The pure decision `script_checks` is covered with hand-set facts
(`dash-consensus/src/connect.rs:224`, `assumevalid_each_condition_alone_turns_checks_on`). Through
the connector, `assumevalid_skips_scripts_on_its_chain_only`
(`chain/tests/connector.rs:1788`) varies only condition 2 (av on another branch) and passes
condition 5. Two mistakes would pass every test:
- reading the tip or the block instead of `cx.best_header` for condition 3 (`v:2552`);
- `>` for `>=` against `cx.min_chain_work` (`v:2553`).
**Fix:** add two rows to that test:
1. av = b3, with headers on `side` past b3's work so the best header does not lead to b1. Scripts
   are then checked (the mandatory-flag reject).
2. av = b3, with `-minimumchainwork` set above the best header's work. Scripts are checked. Set it
   equal, and they are skipped.

### F2 (Minor, tests / follow-up): rev bytes are pinned transitively, not against a Core rev file
`a_block_connects_with_its_undo_and_valid_scripts` (`connector.rs:1049`) decodes the streamed
`CBlockUndo` with CS5a's decoder and asserts `wire_is_canonical()`. Its rows include the height-0
coin, which has no dummy byte (`undo.h:27-31`). `write_undo_slice_matches_write_undo`
(`dashd-store/src/flatfile/tests.rs`) shows the slice path writes ST2's record, and ST2's framing
and checksum tests are its own. Each link is anchored, but no test byte-compares a rev file that
`CoreConnector` wrote with Core's for the same chain. The check I would want is magic, size,
`CBlockUndo`, then `Hash(pprev hash ‖ undo)` (`bs:509-538`, `bs:779`'s `+ 40`).
**Fix (not blocking a3):** a follow-up row for a7 or conformance. Replay a Core regtest recording
that keeps its `blocks/rev*.dat` through `CoreConnector`, and `cmp` the rev files.

### F3 (Nit): `AttemptUnwind` is disarmed before the commit
`connector.rs:176` disarms the guard right after `finish_attempt`. The commit runs later
(`:180-189`: `ov.commit`, then CH7a's `commit_tx`). A panic in `commit_tx` after `ov.commit`
succeeded leaves the active cache at the block and evo at the parent. Only the hold's `FailStop`
or V's panic path latches that state: in production, ND1's `ReadFailStop` aborts the process, so
it is safe on the tip path. A tip-path-less caller (a6's TBV commits nothing; CH5's VerifyDB
commits to scratch) is not exposed today.
**Fix:** keep the guard armed until after the commit `match`. That costs nothing and makes D71's
"nothing partially committed on unwind" hold for the commit too. Otherwise, say in D71 that the
commit is covered by the hold, not by the guard.

### F4 (Nit): `begin_tx` runs even when the coins lock failed
`connector.rs:161` builds the tuple `(&mut ov, self.hooks.begin_tx(..))`, so `begin_tx` is called
when `ov` is already `Err`, and `rollback_tx` then undoes it. This is harmless under `NoEvo`, and
the outcome is correct (the lock's `Fatal`, reported once, bracket closed). CH7a's `begin_tx`
would still open an evo DB transaction for nothing.
**Fix:** match on `ov` first and call `begin_tx` only in the `Ok` arm.

### F5 (Nit): `NoEvoSteps` restates `NoEvo`'s answers
`connect/bench.rs:58-115` repeats `NoEvo`'s H1/H2/H4/H5/H6/H8 answers (`evo.rs`, `impl EvoHooks
for NoEvo`). They agree today: I checked each one. If they drift, `connect_block_micro` and the
CRASH core mode (D91, D95) would measure or model a different connect than production. H4 already
shares `pre_dip3_special_txs`.
**Fix:** move the rest into free functions both impls call, or add one test asserting that both
answer the same.

## What I verified (no finding)

**1. ConnectBlock order, reasons and DoS class** (`serial.rs:361-780` vs `v:2469-2935`)
- CheckBlock with POW and merkle checked (D42 skip): `BLOCK_MUTATED` → `AbortNode` "Corrupt block…"
  (`v:2505-2514`).
- `bad-chainlock` / `BLOCK_CHAINLOCK`, only with `pprev` (`v:2516-2519`).
- G1 is placed before any evo read. A refusal is never a verdict.
- `VerifyBestBlock` when DIP0003 is active at the block → `EvoDbInconsistencyMessage` (`v:2525-2531`).
- Genesis returns before `SetBestBlock` (via the bracket), before `RaiseValidity`, and before the
  BLS commit (`v:2536-2540`).
- fScriptChecks is resolved before BIP30 (`v:2542` vs `:2590`).
- BIP30: `IsBIP30Repeat` is always false in Dash (`v:6785-6789`). The skip test is
  `pprev->GetAncestor(BIP34Height)` with the hash compare (`v:2598-2601`). The probe covers every
  output of every tx, coinbase included, `bad-txns-BIP30` / CONSENSUS. D59 boundaries are tested.
- `bad-sb-start` (`v:2617-2622`).
- `nLockTimeFlags` and the block script flags are taken at the block (`v:2626-2632`).
- Subsidy at the block with V20 at the block. `GetSpecialTxRules(pprev)` (`v:2658-2659`).
- H4 runs before the UTXO loop over the parent view, with `fJustCheck=false` and
  `fCheckCbTxMerkleRoots=fScriptChecks` (`v:2663`).
- Per-tx order matches `v:2694-2767` exactly:
  - `CheckTxInputs` → `BLOCK_CONSENSUS` with the tx reason and debug;
  - H8 only when V24 is active at the block;
  - `nFees += txfee`, then `MoneyRange` → `bad-txns-accumulated-fee-outofrange`;
  - prevheights via `AccessCoin`, then `SequenceLocks` → `bad-txns-nonfinal`;
  - sigops over every tx incl. the coinbase → `bad-blk-sigops` against `MaxBlockSigOps(DIP0001
    at block)`;
  - `CheckInputScripts` (not for coinbase, only if fScriptChecks, caches consulted and not stored,
    `fCacheResults=fJustCheck`), with the mandatory/non-mandatory retry kept (`v:2198-2223`, CS1);
  - UpdateCoins: SpendCoin into the undo, then `AddCoins(overwrite=coinbase)`.
- IS loop: skipped in IBD. `has_chainlock` is computed once. The while loop removes or rejects
  `conflict-tx-lock` / `BLOCK_CHAINLOCK`. `RemoveConflictingISLockByTx` re-finds the same
  `GetConflictingLock` (`evo/chainhelper.cpp:93-98`), so passing the found lock is equivalent.
  Termination is Core's: it relies on the IS store shrinking (D35 removed view). The production
  `NodeHelper` returns `None`.
- `check_superblock` comes from the helper and `rules.v24`. `bad-cb-amount` / UNSET, then the
  masternode half before the superblock half, then `bad-cb-payee` / UNSET
  (`masternode/payments.cpp`'s `IsBlockPayeeValid` order; D63 refusals before H5).
- Undo, then `RaiseValidity(VALID_SCRIPTS)`, then `WriteBestBlock` / BLS commit / list-changed
  (`v:2888-2921`).
- Sequence locks' context: height = block, MTP = `pprev`'s, coin ancestor
  `GetAncestor(max(h-1,0))` reached from `prev` (equal to Core's walk from `pindex` because the coin
  height ≤ block height) (`consensus/tx_verify.cpp`).

**Arithmetic**
- Each fee is checked by `MoneyRange` inside `CheckTxInputs`, so `fees + fee ≤ 2·MAX_MONEY` cannot
  overflow i64.
- `subsidy + fees` is bounded the same way.
- Sigops is `u32::wrapping_add`, which is Core's `unsigned int +=`. It cannot be reached anyway
  under the block size.
- The heights are u32 and never negative.
- `undo_size` `u32::try_from` cannot fail: a 2 MB block's undo is under ~0.5 GB.
- `GetBlockProofEquivalentTime` is a bit-exact port (`chain.cpp:147-162`).

**2. Undo**
- Streamed bytes = `serialize(CBlockUndo)`: count = `vtx.size()-1`, one `CTxUndo` per non-coinbase
  tx (an asset unlock gets an empty one), `VARINT(h·2+cb)`, the zero dummy only when `h>0`, then
  `TxOutCompression`.
- The write is skipped iff `GetUndoPos()` is non-null (HAVE_UNDO and a file).
- `FindUndoPos` advances `nUndoSize` before the space check, as in Core. The size is `undo + 40`.
- Checksum over the pprev hash. `nUndoPos` = data position. `HAVE_UNDO` is set only after
  `write_undo` returns (`serial.rs:765-770`), which is T12.
- `FlushUndoFile` condition includes `m_undo_height_in_last_blockfile` (`bs:791-795`).
- Reports: `Disk space is too low!` with the user message, then the FindUndoPos line, in Core's
  order. The open line, then "Failed to write undo data". D66/D77 are declared, and Core's
  `ios_base::failure` does propagate to `ProcessMessages`' catch. The latched case is silent.
  `a_failed_undo_write_stops_v_as_core_does` pins report and line counts per fault.

**3. Bracket**
- Nothing fallible sits between `begin_attempt` and `finish_attempt` except values.
- The lock is taken after L3 (D74).
- `finish_attempt` → disarm → `end_attempt` on every path. `finished_outcome` keeps a reported
  outcome and lets a `Fatal` win over `Connected`/`Invalid` (D75).
- Coins and evo are committed only on `Connected`. `rollback_tx` runs on every other outcome and
  on a failed commit.
- An unwind trips the latch before the attempt drops (tested through V).
- On a `commit_tx` failure after `ov.commit`, V stops before any flush (D45), so the in-memory
  split is never durable. Core's `dbTx->Commit()` is infallible.

**4. assumevalid** is exact (`v:2543-2568`, including `>= MinimumChainWork`, `<= 1209600`, and
"not indexed ⇒ check"). The flags pass through to `CheckInputScripts` and to H4's
`fCheckCbTxMerkleRoots`. `pre_dip3_special_txs` matches `stm:816-875` below DIP3:
- the raw `nType==5` at `vtx[0]` → `bad-cbtx-invalid` / `bad-cbtx-payload` (no payload below
  version 3, `GetTxPayload` → nullopt);
- the `i==0 && raw type 5` skip;
- `bad-tx-type-dip3-inactive` with Core's debug text;
- the signed 16-bit `nVersion` (`primitives/transaction.h:253`).

The v3+ CbTx is G1's (D54).

**5. a8**
- `WarningBits` matches `v:2395-2417`: `BeginTime 0`, `EndTime INT64_MAX`, `SignalHeight 0`, period
  `nMinerConfirmationWindow`, threshold `nRuleChangeActivationThreshold` regardless of `nAttempt`.
  The condition is `MinBIP9WarningHeight`, the top bits, the bit, and `!(ComputeBlockVersion(pprev)
  >> bit & 1)`.
- `GetStateFor` is the shared walk, moved unchanged (`versionbits.cpp:33-135`).
- `UpdateTip` (`v:3219-3236`): only out of IBD. The state is for the block after the new tip. ACTIVE
  → `DoWarning`; LOCKED_IN → `AppendWarning` with ", " into ` warning='…'`. The text is
  `Unknown new rules activated (versionbit %i)`. `fWarned` gates `AlertNotify` once.
- Note: v24 has no "unknown versions over 100 blocks" counter (that is pre-0.19 Bitcoin). The
  threshold-machine form above is the right reference.
- D83's EHF-DEFINED reasoning holds: every block the warning walk reads lies on a chain G1 lets
  through, so it is below V20, where Core's EHF states are DEFINED too.
- `CompleteAncestry` reads the same arena fields, by the same walk as `HeaderChain`'s impl, and it
  reads only immutable fields. It is tested against the chain, including entries added after the
  handle was taken.

**6. Tests.** The directed rejects use Core's literal reasons and are census-gated against the
Core source (0 failures). The DoS class and report counts are asserted per row. The guard, IS and
bracket tests drive real V. Gaps are F1 and F2. The CRASH T12/T20/T21/C11 repros pin rdashd's
durability invariants, not Core behaviour; that is their purpose.

## Security
Nothing to embargo. A readiness refusal (G1/G2, D54/D63) stops the node on a block it cannot judge.
A peer can produce one cheaply only on regtest or devnet: on mainnet and testnet, pre-DIP3 heights
are buried under the minimum chainwork, and a node at the tip is refused by design (D18).

VERDICT: APPROVE-WITH-NITS
