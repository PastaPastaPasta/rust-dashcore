# CH3 design review r1: independent Opus review

**Reviewed:** `docs/node-program/designs/CH3-chainstate.md` at **b5ede63b5** (rev1), branch `node/ch3-design`. The branch was not edited.

**Against:**
- Core v24.0.0-rc.3: the owner's extract at `/work/scratch/ch3-design/core/src` (cited as `v:` for `validation.cpp`), spot-checked against `~/workspace/dash` at the tag.
- CH1-node at 7ae200d74 and CS6 at f1f32f12c.
- The integration stores as cut at 0d0939b8a: ST2 `dashd-store`, CH2 `coins/overlay.rs`, `store/guarded.rs`.
- The PLAN, A1 (Appendix D), O8, CH2 and CS6 designs.

**Independence:** I read nothing of the owner's self-review except rev1's change-log line, and I read the Sol review only after writing everything above "Comparison with the Sol review".

**Numbering.** The brief's "D13 commit before BlockChecked" is the doc's **D14**. The brief's "D19 interim answers" is **D18** (D19 is the attempt contract). The brief's "D25–D27 libstdc++ resizing" is **D25** (D26 is CONS-11, D27 the crash grid). This review uses the doc's numbers.

**Assumptions.**
- (A1) P2P block download (P2b) can be composed into rdashd before CH7a, SP1 and GV1b land. PLAN M4 includes "a P2P sync from the oracle", and I found nothing that stops an operator from pointing rdashd at mainnet or testnet peers.
- (A2) §7.2's interim VerifyDB check runs over CH3a's connector, as §7.2 says.

## Summary

The core of the design is right. I traced the following against Core and found no ordering, reject-string or result-class error:
- every step of §2.2/§2.3 against `v:2469-2935`;
- §2.4's precedence matrix against `CheckInputScripts` with `pvChecks`;
- BIP30/BIP34, the script and lock-time flags, the undo bytes and T12 order;
- `DisconnectBlock`, `TestBlockValidity`, D25's accounting formulas, and §2.1.1's AbortNode counts.

The changes required are at the edges:
1. Nothing stops a production rdashd from accepting, at a live mainnet or testnet tip, blocks every dashd rejects. That covers NoEvo past DIP3 and, after CH7a, D18's superblock regime. A fail-closed guard is proposed in M1.
2. The one-overlay-per-call connector cannot run VerifyDB levels 3–4, and §7.2 relies on that, so the a2 seam has to change shape now.
3. The M3 tier-1 functional tests (FTs) that test exactly this code (`feature_block` and friends) appear in no row's acceptance.

| Severity | Count |
|---|---|
| Blocker | 0 |
| Major | 3 |
| Minor | 14 |
| Nit | 7 |

---

## Major

### M1. Nothing fails closed when rdashd meets a block its interim regime cannot judge (§2.7, §3.1 `NoEvo`, §3.3 `NodeHelper`, D18, R6)

**There are two layers, and the design guards neither in production.**

**(a) `NoEvo` is exact only below DIP3.** `NoEvo` behaves as follows:
- H2 `verify_best_block` answers true.
- H4 answers `Ok(empty)`: no CbTx, ProTx, QC, credit-pool, MNHF or asset-lock/unlock checks.
- H5 answers true.

Core's masternode-payee check runs **whatever the sync or governance state**. `IsBlockPayeeValid` calls `IsTransactionValid` first (`masternode/payments.cpp:397`), and that depends only on `DeploymentDIP0003Enforced` and the deterministic list (`:227-240`).

So at or above DIP3 (mainnet 1,028,160, testnet 7,000), `NoEvo` accepts blocks that *every* dashd rejects. That is not D18's node-local-state difference; it is a missing consensus check. The design knows this, since L2 stops at 1,028,159 and 6,999, but no production code enforces it.

**(b) After CH7a, D18 remains until SP1 and GV1b.** §2.7 itself says that at a live tip, a Core node that has finished MN sync checks every superblock height above its best ChainLock, while rdashd accepts. "No M3 exit runs rdashd at a live mainnet or testnet tip" is a statement about tests, not a guard. M4 gives the node P2P sync before M5 closes the gap.

**Scenario.** An operator runs rdashd at M3 or M4 (CH3a + P2b) on mainnet against public peers. IBD passes 1,028,160. A block whose coinbase pays the wrong masternode, or carries no CbTx, is connected, its undo is written, and it is served onward. The node silently forks off the network.

**Fix: the proposed guard.** It has two checks, G1 and G2, scoped and failing as follows:
- **Scope:** mainnet and testnet only. Regtest and devnet keep the declared regime, so the FTs (which stay below 432 or raise it with `-dip3params`), L3 and NodeSut are unaffected.
- **How it fails:** `ConnectOutcome::SystemError(text)`. V emits `block_checked` with an error state, ABC returns false, and `AbortNode` runs, which is Core's own fail-closed path.
- **What it never does:**
  - It is never `Invalid`: there is no `FAILED_VALID`, nothing is cached, and no peer is punished.
  - It is raised before step 22: there is no undo, no `HAVE_UNDO` and no `VALID_SCRIPTS`.
  - It emits no effects; the attempt log is empty at that point.
- **Afterwards:** the block stays `HAVE_DATA`, so a capable build can connect it.
- **Text:** e.g. `rdashd cannot validate block <hash> at height <h> on <chain>: <capability> is not built (ALLOWED-DIFFS D18)`.

**G1 (slice a3; CH7a lifts it).**
- Add `EvoHooks::COMPLETE: bool`, false for `NoEvo`.
- In `connect_block` and `test_block_validity`, right after step 5 and before H2 (step 6, the first evo-dependent step): if `!H::COMPLETE`, DIP0003 is active at the block, and the chain is mainnet or testnet, return `SystemError`.
- `disconnect_block` of such a block under `NoEvo` returns `Failed`, with the connector's AbortNode text. This is unreachable while G1 holds, but cheap.

**G2 (slice a3; SP1 + GV1b lift it).**
- Add an **unrecorded** helper method, `interim_refusal(h) -> Option<&'static str>`, defaulting to `None`. `IbdHelper` and `RecordedHelper` keep `None`, so O8's query stream is untouched.
- `NodeHelper` returns `Some` when all of these hold:
  - the chain is mainnet or testnet;
  - `!is_ibd`;
  - `CSuperblock::IsValidBlockHeight(h)`;
  - `best_cl_height < h`;
  - SP1 or GV1b is absent.
- The connector calls it at step 19, before `sb_required`.

This is exactly the set where a synced Core could answer differently. At non-superblock heights, the synced and unsynced verdicts coincide (`payments.cpp:313-387`, `:392-440`).

The consequence is deliberate: a live mainnet node stops at the next superblock height that is not yet ChainLocked (about monthly), and a testnet node within about an hour. Old-budget heights lie below the superblock start and are never met outside IBD on these chains; include them anyway if it costs nothing.

**Why both checks are needed over time.** Before CH7a, G1 alone refuses every live-tip block, because every live-tip height is past DIP3. G2 is the one left standing between CH7a and M5.

**Tests (a3):**
- **G1 refuses:** mainnet params, `NoEvo`, a block at the DIP3 height. Expect `SystemError`; the block stays `HAVE_DATA`, with no undo and no `FAILED_*`; V stops.
- **G1 on regtest:** the same block on regtest connects.
- **G2 refuses:** superblock height, `!ibd`, no ChainLock. Expect `SystemError`.
- **G2 passes:** each of these connects: a ChainLock at or above `h`, `ibd`, a non-superblock height.

**§11 deltas:**
- CH7a removes G1; SP1 and GV1b remove G2.
- `ALLOWED-DIFFS` D18 reads "mainnet/testnet: refused; regtest/devnet: declared".

**Optional outer layer.** ND1 could refuse to start on mainnet or testnet without the full capability set unless a hidden developer flag is given. That is coarser, so it is a second line of defence, not a substitute for G1 and G2.

### M2. The connector cannot run VerifyDB levels 3–4, which §7.2 uses as CH3a's interim check (§2.11, §2.12, §7.2, D3)

**Core.** `CVerifyDB::VerifyDB` (`v:5030-5180`):
- opens one evo transaction (`:5047`) and **one** `CCoinsViewCache coins(&CoinsTip())` (`:5055`);
- level 3: `DisconnectBlock(block, pindex, coins)` for each of up to `nCheckDepth` blocks, into that same view (`:5110-5113`), with the budget on `coins.DynamicMemoryUsage() + CoinsTip().DynamicMemoryUsage()` (`:5108`);
- level 4: `ConnectBlock(block, state, pindex, coins)` forward over it (`:5160`);
- nothing is flushed, and the evo transaction is dropped.

**Proposed.** Each call builds one `BlockOverlay::new(&mut cache)` and commits it into the active layer: `commit(BlockRef(prev))` on disconnect (§2.11). `BlockOverlay` has no best block of its own (`coins/overlay.rs`; CH2 DEC-142), so overlays do not stack. `AttemptOrigin::VerifyDb` only selects the digest tag (§2.15).

**Scenario.** Take §7.2's "levels 0–3 of a direct `ChainCommand::VerifyDb` over CH3a's connector": disconnect the tip, then tip−1. Either way it goes wrong:
- If each disconnect commits, VerifyDB rewinds the live coins cache (and, after CH7a, the evo window).
- If commit is suppressed, the second disconnect cannot see the coins the first one restored, and reports `Unclean` or an inconsistency.

CH2 already notes that "CH5 must build VerifyDB's multi-block view". But that view has to pass through the connector seam that a2 freezes.

**Fix (a2).**
- Give `connect_block` and `disconnect_block` an explicit target: `ViewTarget::{Active, Scratch(&mut ScratchView)}`. `ScratchView` is a multi-block overlay over the active cache with its own best block; CH2's owner co-designs it.
- Commit-or-not follows the target, **never** `origin`.
- The evo side (H1, H6, H10) takes the same target, so CH7a can open a never-committed evo transaction as Core does at `:5047`.
- Test: levels 3 and 4 over 6 blocks; assert that the tip's coins and evo state are byte-identical before and after.

If CH3a would rather not build `ScratchView`, drop the §7.2 claim and make M3 exit 1's `verifychain 4 0` an explicit CH5 dependency. The seam still needs the target parameter now.

### M3. The M3 tier-1 FTs that test this code are in no row's acceptance (§1.1, §7.5, §11)

**What A1 lists.** A1 Appendix D's M3 row (`A1-architecture.md:2874`; PLAN M3 exit 5) lists:
- `feature_block`, `feature_cltv`, `feature_dersig`, `feature_csv_activation`;
- `feature_nulldummy`, `feature_bip68_sequence`, `feature_assumevalid`, `feature_abortnode`;
- plus CH3b's four.

**Who owns them.** None of these eight appears in any PLAN row (`grep` over PLAN; `feature_dbcrash` is CH5's). SW-FT runs the passlist, but it does not own the ConnectBlock behaviour that makes these tests pass. CH3b's acceptance names only the four reorg/RPC FTs.

**Why it matters.** `feature_block` is Core's own end-to-end test of exactly §2.2–§2.5: BIP30, CVE-2012-2459, sigops, sizes, maturity, the value bound and script failures. It is a stronger check than any directed test, and it runs without evo. At rc.3:
- `feature_block` and `feature_csv_activation` set `-dip3params=2000:2000`;
- `feature_cltv`, `feature_dersig` and `feature_assumevalid` set `9000:9000`;
- `feature_nulldummy` (`105:105`) and `feature_bip68_sequence` (regtest's 432) cross DIP3, so they need CH7a.

**Scenario.** CH3a merges green on directed tests. At M3 exit 5, `feature_block` fails on a check-order detail, and no row owns the fix.

**Fix (§11 PLAN deltas):**
- CH3a's acceptance adds `feature_block`, `feature_cltv`, `feature_dersig`, `feature_csv_activation` and `feature_assumevalid`. Their dependencies are the same as CH3b's four (ND1, RP1a `submitblock`/`generate*`, P2b).
- CH3b adds `feature_abortnode`. That needs m11, and CH5's VerifyDB for its restart half.
- CH7a's acceptance takes `feature_nulldummy` and `feature_bip68_sequence`.
- Name the slice that wires them (extend b2, or add a slice).

---

## Minor

### m1. The IS removed set misses chained child locks (§2.2 step 18, §3.3, D20)

**Core.** `RemoveConflictingISLockByTx` calls `RemoveConflictingLock` (`instantsend/instantsend.cpp:339-350`), and that calls `db.RemoveChainedInstantSendLocks`. The lock **and its chained children** go synchronously, and later `ConflictingISLockIfAny` calls see them gone.

**Proposed.** The attempt-local removed set holds only the lock that `is_conflict` reported.

**Scenario.**
- T is locked (L). C spends T:0 and an unrelated outpoint O, and is locked too (Lc, a chained child of L).
- A ChainLocked block contains X, which spends T's input, and Z, which spends O.
- Core: X's loop removes L and Lc, so Z finds no conflict.
- Proposed: Z's `is_conflict` returns Lc. That produces one extra `RemoveConflictingIsLock` effect, one extra "overrides islock" log line, and one extra `is_conflict` decision.
- The extra decision makes O8's strict-stream comparison diverge. The verdict is unchanged.

**Fix.** The removed set must be the closure Core removes. Either the IS owner's `is_conflict` excludes descendants of any lock in the set (the IS owner holds the chain index), or `remove_conflicting_is_lock` inserts the chained set. Add Z to the a3 directed test.

### m2. `RecordedHelper`'s removal rule misaligns the decision stream (§3.3)

**The design says** `remove_conflicting_is_lock` "consumes the next recorded `is_conflict` answer".

**Core.** `RemoveConflictingISLockByTx` re-queries `isman.GetConflictingLock` internally (`evo/chainhelper.cpp:93-99`). O8 records `is_conflict` once per loop condition (`ConflictingISLockIfAny`, `:79-85`), not that inner query.

**Scenario.** In the two-lock directed test, the replay's removal consumes the second loop condition's recorded answer. The next `is_conflict` then reads the wrong entry, and the replay stops on a false decision miss.

**Fix.** The replay's removal consumes nothing and only emits the effect.

### m3. H3 and `MnRewardEra` are deployment queries, not evo hooks (§2.2 step 14, §3.1)

**Core.** Neither function reads evo state:
- `GetSpecialTxRules(pprev)` is `{v24: DeploymentActiveAfter(V24), evo_shares: DeploymentActiveAfter(EVO_SHARES)}` (`v:6797-6803`).
- `GetMnRewardEraAfter(pprev)` reads V20 and MN_RR (`v:5897-5903`).

`special_tx_rules.v24` also picks `check_superblock = DisallowDuplicates` vs `AllowDuplicates` (`v:2841-2843`), which is CH3a's own code, and it is `IsBlockPayeeValid`'s `strict_multiplicity`. The design's "`MnRewardEra` … come[s] from H3's rules" is inaccurate: `SpecialTxRules` carries no era.

**Scenario.** A `RecordedHelper` replay of a regtest chain past V24, where Core recorded `sb_required = true`. `NoEvo`'s all-false rules give `AllowDuplicates` where Core used `DisallowDuplicates`, which matters once EV3's trigger branch is in.

**Fix.** Compute both from `cx.deployments` in CH3a and pass them as inputs to H4, H5 and H8. Drop H3 from the hook table.

### m4. Slice dependencies are incomplete (§8.1)

Three dependencies are missing:
- **a3 needs a8.** a3 needs real deployments through V: the step-12 flags, step 14 and the CSV lock-time flags. But only a8 (§2.16) points `ConnectCx::deployments` at the wired cache, and a8 appears in no dependency. Either move the wiring into a2 (leaving a8 only the warning) or make a3 depend on a8.
- **a7 needs a4 and a8.** a7's L2 runs at `-par` maximum (a4), and its `--require` set includes `dep`, which comes from a8's wiring.
- **a10 needs a5.** NodeSut's `Invalidate`, `Reconsider`, `Precious` and `Restart` need disconnect (a5). Today the sentence lists only a4 and a6.

**Fix.** "a3 needs a1, a2 (and a8 unless a2 carries the wiring); a7 needs a3, a4, a5, a8; a10 needs a4, a5, a6."

### m5. "Inline control below assumevalid" needs a CS6 API that does not exist (§2.2 step 13, §11)

**CS6 at f1f32f12c.** `CheckQueue::control` always enters `pool.in_place_scope` when a pool exists. The only inline mode is `CheckQueue::serial()`, which is chosen at construction.

**Proposed.** Step 13 says the connector opens CS6's control "in inline mode" when `!fScriptChecks`. §11 lists no CS6 delta for it.

**Fix.** Either add the delta with CS6's owner (e.g. `control_inline`), or drop the claim. It affects performance only.

### m6. CheckBlock's `fChecked` memo is not ported, so tip connects do redundant work (§2.2 step 3; performance)

**Core.** `CheckBlock` returns at once when `block.fChecked` is set (`v:4374-4375`). It is set after a full pass with both POW and merkle checks (`v:4424-4425`). A block that arrives through ProcessNewBlock and is connected from the same object — the live-tip case — skips the merkle tree and the per-transaction `CheckTransaction` inside ConnectBlock.

**Proposed.** The connector always reruns `check_block`. That adds a merkle tree (SHA256d per transaction, on a box without SHA-NI) and per-transaction checks to every tip connect. PF0's connect-latency-tip row then compares two CheckBlocks against dashd's one, which breaks "never slower than dashd" by construction.

**Fix.** Pass a `checked` flag, set by CH1-node's ProcessNewBlock path for the same block object. Blocks read from disk start unchecked, as in Core.

### m7. Copying spent scripts before the exec-cache probe adds work on cache hits (§2.9, D9; performance)

**Core.** `CheckInputScripts` probes the script-exec cache **before** it touches coins. `spent_outputs` and `txdata.Init` come after the probe. On a hit, Core does no coin lookup and no copy.

**Proposed.** The connector does one more cache probe, a decompress and a copy per input, on every hit. At the tip, hits are the common case: transactions are mempool-validated. D9 counts "25–40 bytes" but leaves out the lookup and the decompression.

**Fix.** Add a CS6 probe-first entry point as a §11 delta: `exec_hit(slot, flags, erase)`, then a check that skips the second probe. This also avoids soft-erasing the entry twice. Alternatively, record it as a §6.2 lever with a PF0 row before a4 merges.

### m8. The assumevalid-on L2 run may never take the skip (§7.2)

**Why it can't skip.** A reindex-style replay of blocks up to 1,028,159 has its best header at 1,028,159. That breaks the §2.8 conditions:
- Condition 1 fails: mainnet's `defaultAssumeValid` is not in that index.
- Condition 4 fails: the chain's work is below `MinimumChainWork`.
- Condition 5 fails for the last two weeks of blocks.

So the "assumevalid on" run checks scripts everywhere, matches the off run trivially, and proves nothing.

**Fix:**
- Preload the oracle's full header chain before the replay.
- Assert the count of skipped blocks.
- Add directed decision tests that flip each of conditions 1–5.
- Run `feature_assumevalid` (see M3).

### m9. MuHash cannot be seeded from a golden hash, and the per-PR window has no stated start state (§2.15, §7.2)

**The problem.** The golden `muhash` is the finalized hash of the accumulator, so the accumulator cannot be recovered from it. From genesis the seed is the empty set, which is fine. But the per-PR window (600,000→625,000) needs a chainstate at 599,999, and the design names no source for it until O9b's snapshots land.

**Fix:**
- State the source: a cached rdashd replay to 599,999, or a Core `dumptxoutset` loaded into rdashd's stores.
- Build the accumulator by hashing the loaded set.
- Check its finalized hash against golden(599,999) before replaying.

### m10. The undo read trusts ST2's size header; error texts and one corruption case differ from Core (§2.10.2)

**ST2.** `read_sized` (`dashd-store/src/flatfile/blocks.rs:866-896`) reads the 4-byte size **before** the record, bounds it, reads `size + 32` bytes, then checks the checksum.

**Core.** `UndoReadFromDisk` (`blockstorage.cpp:540-575`) streams the record from `pos` through a hashing verifier and never reads the size header.

The two agree on intact files. On corruption they differ:
- **A damaged size that still passes the bounds:** rdashd reports "Checksum mismatch" and aborts the disconnect, where Core decodes an intact record and continues.
- **An out-of-bounds size:** ST2 raises `FlatFileError::Corrupt`, which is not one of Core's four texts, although §2.10.2 promises "the same four log texts".

**Fix.**
- Give the error mapping in a table.
- Either stream from `pos`, or declare the damaged-header case in ALLOWED-DIFFS.

### m11. `feature_abortnode` cannot pass: a deleted rev file stays readable through ST2's handle cache (§2.10.2; ST2)

**The test.** `test/functional/feature_abortnode.py` unlinks `rev00000.dat` under a running node, forces a reorg, and expects "Failed to disconnect block", an abort, and an init error on restart.

**Core.** Core opens the file by path for each read, so the read fails with "OpenUndoFile failed".

**ST2.** `OsFs` keeps up to 64 open handles and reads through them:
- reads: `flatfile/fs.rs:69-130`, `read_exact_at` at `:195-197`;
- `len` uses `fstat` on the cached handle: `:184-189`;
- only `link_count` notices `nlink == 0`: `:170-182`.

The rev file was just written, so its handle is cached. The read succeeds from the unlinked inode, the reorg goes through, and the FT fails.

**Fix.** In `OsFs::len` and `read_exact_at`, treat a cached handle with `nlink == 0` as gone: forget it and reopen by path. `len` already calls `metadata()`, so this costs nothing. Otherwise, declare the difference.

**Remaining dependencies.** The restart half needs CH5's VerifyDB (level ≥ 2 reads undo). BL-122 already assigns the asserted-log row to RP0/O11, but the read semantics are ST2's and CH3a's.

### m12. D14 is right for BlockChecked, but H7's MN-list signal needs an ordering rule for CH7a (§2.13, H7)

**D14 holds for BlockChecked.** Its listeners are:
- `PeerManagerImpl::BlockChecked` (`net_processing.cpp:2314-2342`): punishment via `mapBlockSource`, and HB-announce gated on `IsInitialBlockDownload` and in-flight blocks;
- the `submitblock` state catcher (`rpc/mining.cpp:999`).

No wallet, ZMQ, mempool or governance listener implements BlockChecked. IBD reads the chain tip, which neither order has moved yet.

**The exception is H7.** `NotifyMasternodeListChanged` (`v:2918-2921`) runs synchronously inside ConnectBlock, **before** `view.Flush()` and before the tip moves. Its listener (`CDSNotificationInterface`) runs `govman.CheckAndRemove` at that point, while `CoinsTip()` is still at pprev. CH7a "stages the signal" without a stated position. If the signal fires after the commit, listeners see the block's coins.

**Fix.** Make it a CH7a requirement in §2.13 and §11: the staged signal fires before the commit and before the tip moves, or CH7a proves that each listener reads neither coins nor the tip.

### m13. Test-plan gaps: no L2-scale disconnect differential and no CH3 fuzz targets (§7)

**(a) A reverse replay.** After the forward L2 run, disconnect back N blocks (e.g. 10,000, across the CSV and CLTV boundaries). At each height, compare MuHash, txouts and amount with golden(h−1). This is the only way to exercise `ApplyTxInUndo` on real mainnet-scale undo; directed tests and ChainSut don't reach that scale.

**(b) Fuzz targets.** None is named for CH3. Add:
- `undo_decode`: never panics, and round-trips whenever decode succeeds;
- `connect_disconnect_roundtrip`: random small blocks over a random UTXO set restore it byte for byte, with `Unclean` vs `Failed` classified correctly;
- `disconnect_pool`: operation sequences checked differentially against b1's Core vectors.

Register all three in node-fuzz smoke.

### m14. L3 must state the dashd oracle's regime (§7.3)

**The issue.** The value-bound operators ("budget window, superblock height off-trigger") are judged by `NodeHelper` with `sb_required = false`. dashd has to be in the same regime:
- **MN sync unfinished:** no peers and no `mnsync next`, since a lone regtest dashd stays unsynced.
- **Reachable heights:** regtest's budget window (1000) and superblock heights (≥1500) lie above regtest DIP3 (432), where `NoEvo` cannot go.

**Fix.**
- Both sides run with `-dip3params` above 1500.
- dashd has no peers and is never sent `mnsync`.
- Each case asserts that dashd's `mnsync status` is not synced.

---

## Nit

- **n1 (D18).** `NodeHelper`'s `gov_valid = false` is Core with `-disablegovernance` plus unfinished sync, not default Core: Core's governance is valid after `LoadCache`. Verdicts are identical on the `NoCheck` path, but production decision streams and the MNPAYMENTS/GOBJECT log lines differ from a default dashd. Say so.
- **n2 (D2).** The `ChainstateCell` mutex is a V-only leaf. Use `try_lock().expect(...)`, so that a violation panics in tests instead of hanging.
- **n3 (R7, §2.9).** `bumpalo::Bump::alloc_slice_copy(&self, …) -> &[u8]` with `reset(&mut self)` gives the same push-only, address-stable, chunked arena with no new `unsafe`. Weigh it before a4's review of a bespoke one.
- **n4 (D14).** With the commit inside `connect_block`, `-debug=bench`'s Connect and Flush times are attributed differently from Core's ConnectTip lines. This is debug-only; mention it in D14.
- **n5 (§2.1.1).** One rule would be simpler and would keep Core's log position. The connector reports every AbortNode raised inside the Core functions it ports (ConnectBlock, DisconnectBlock, WriteUndoDataForBlock); V reports those in ConnectTip, DisconnectTip and ABC; `SystemError` carries `reported: true`. Core logs the MUTATED and evo-inconsistency AbortNodes inside ConnectBlock, before any of rdashd's post-return logging.
- **n6 (§7.2).** The testnet window 0→6,999 does not reach CSV, which activates at 8,064 on testnet. CSV activation is covered only by mainnet's window. That is fine, but say so.
- **n7 (D25).** State in the rationale that pool exactness never affects consensus (block validity, UTXO set, MN list). It decides only which disconnected transactions return to the mempool and which leave with reason REORG. It is a mempool-parity property, and §8.2 rightly keeps it off the consensus slices.

---

## Answers to the brief's focus items

1. **Check order and precedence.**
   - The §2.2 order, strings and result classes match `v:2469-2935`.
   - §2.4 is right. Every block flag is in `MANDATORY_SCRIPT_VERIFY_FLAGS`, so the non-mandatory retry never changes a block result. `CheckInputScripts` with `pvChecks` only queues and never fails inline, so with threads only non-script failures can win before `Wait`, and the script answer is `block-validation-failed`. With `-par=1` it is the transaction's own reason (D5).
   - BIP30/34 heights, the BIP34 hash shortcut and the no-exemption behaviour are right. The flags match `GetBlockScriptFlags` (`v:2419-2448`). The assumevalid decision matches; only its test is weak (m8).
   - The hook positions are right, except that H3 is not a hook (m3).
   - Every reject string comes from CS3's CheckBlock, from ConnectBlock's own sites, or from payments (`bad-cb-amount` and `bad-cb-payee` are RESULT_UNSET).
2. **Undo.**
   - The format (TxInUndoFormatter with the dummy VARINT) and T12's order (write, then `nUndoPos`, `HAVE_UNDO`, dirty) are right. Genesis gets no undo and no `VALID_SCRIPTS`. The legacy height-0 path uses `access_by_txid`, which exists in CH2's view.
   - D14 is unobservable for notifications, ZMQ, mempool, wallet and the digest. Only BlockChecked sits between the two events; its listeners are listed in m12, and the digest records at `connect_block`'s end.
   - The caveats are H7 (m12) and the bench lines (n4). Reads differ from Core only on corruption (m10, m11).
3. **Disconnect and reorg.**
   - The three-way result is right, and `DisconnectTip` treats anything but OK as failure.
   - D25's formulas check out: `reserve` on every add, the shrinking `rehash`, `take()` and then `rbegin` iteration in `MaybeUpdateMempoolForReorg`, and capacities equal to sizes because `DisconnectTip` reads blocks from disk. Exactness matters for mempool contents only (n7).
   - CONS-11's two restart cases are sound, and comparing case 2 with dashd rather than with a fixed status is correct.
   - Reorg across a flush is sound: the C3-real grid plus the node-level loop, and the prefetch is cleared on disconnect.
4. **TestBlockValidity and the digest.**
   - The ChainLock pre-check gives `INVALID_PREV` (`v:4936-4939`) and is unrecorded.
   - The digest attempt is the ConnectBlock call, which matches O8's definition (one ConnectBlock or DisconnectBlock call).
   - MuHash seeding needs fixing (m9), and the VerifyDb origin needs M2.
5. **D18 and R6.** The interim regime is exactly Core's not-synced regime *only* for the governance half, with the n1 caveat. `NoEvo` past DIP3 is not a sync state at all. No fail-closed guard exists; M1 proposes G1 and G2.
6. **D2/R2, D9 and concurrency.**
   - The `ChainstateCell` as a V-only leaf is consistent with CH2b's `FlushSite` (n2).
   - D9's lifetimes are right. `slots` outlives the scope, and CS6's `control` runs the closure on V through `in_place_scope`, so the closure needs no `Send`. Workers only borrow `slots` and the arena.
   - W reads F; the overlay never writes F.
   - Arena: see n3.
7. **Slicing.**
   - The dependencies are incomplete (m4).
   - The payments split with EV3 is clear: a1's second commit lands in EV3's module, co-reviewed.
   - Left unowned: the FTs (M3), the VerifyDB view (M2), and the CS6 API deltas (m5, m7).
8. **Tests.** L2 and L3 are strong. The gaps are M3, m8, m9, m13 and m14.

## Verified correct

- The step order, strings and result classes of §2.2/§2.3 (`v:2469-2935`), including the IS loop's query order: `ibd`, `cl_has` once per block, `has_lock_inputs`, then `is_conflict` per iteration.
- The §2.4 precedence matrix and D5.
- BIP30/BIP34: `IsBIP30Repeat` (`v:6785`), the activation heights, the hash shortcut.
- Script and lock-time flags per deployment.
- Undo: the record format, T12's order, the genesis case, and the legacy path.
- `DisconnectBlock`'s order and three-way result (`v:2282-2378`, `ApplyTxInUndo` at `v:2252-2278`).
- §2.1.1's AbortNode counts, including the 2 vs 1 split between ABC and InvalidateBlock.
- The needed CH1-node fix: `block_checked` must not fire on `Err(Fatal)` (`abc.rs:538-547`).
- `TestBlockValidity`'s classes and the digest scope.
- D25's formulas and the libstdc++ bucket policy.
- CS6: `control` runs on V, and there is no exec-cache insert from parallel runs.
- §2.11's read order and its fault mapping (`CoinsReadError` → `Fatal`).
- CONS-11's restart cases, and C3-real.

No possible security notes.

---

## Comparison with the Sol review

*(Appended after reading `CH3-design-r1-sol.md`.)*

Sol's verdict is the same, CHANGES-REQUIRED (1 Blocker, 3 Major, 2 Minor).

**Where we agree on substance but grade differently:**

| Topic | Sol | This review | Note |
|---|---|---|---|
| No runtime limit on the interim regime | B1 (Blocker) | M1 (Major) | Same diagnosis. I graded it Major because the fix is additive and fits in a3; both of us require it before a3 merges. Sol's scope is stricter: before CH7a, fail closed past DIP3 on **every** network. Mine exempts regtest and devnet as declared differences. Sol's scope costs little, because NodeSut, L3 and the evo-free FTs stay below DIP3 or raise it, so I would accept it. My G1/G2 is one concrete form of Sol's "readiness boundary". Sol additionally asks for the check on ABC, recovery and restart, and for a restart test at the boundary; I agree with both. |
| MuHash seeding | M2 (Major) | m9 (Minor) | Same finding and the same fix: scan the start set or import its state, and replay from genesis until segment seeds exist. Sol is right that the per-PR gate cannot run as written. I graded it Minor because the nightly and merge gate from genesis is unaffected. |
| `RecordedHelper` removal | M3 (Major) | m2 (Minor) | Same finding and fix. Sol's `[A, null]` / `[A, B, null]` test is a good addition. The FT Sol cites (`feature_llmq_is_cl_conflicts`) is evo, so it gates CH7b's replay rather than CH3a's. |
| Slice dependencies | m1 (a7 needs a4, a8) | m4 | Mine also adds a3 → a8 (deployment wiring) and a10 → a5 (NodeSut's invalidate and restart). |
| D14, D25, precedence, BIP30/34, undo | scope notes | verified / n7 | Agreed: D14 is unobservable for BlockChecked, and D25 matters for mempool parity, not consensus. |

**What Sol found and I missed:**
- **Sol M1: commit before effect application.** If `connect_inner` commits the overlay before `finish_attempt` applies effects, an effect-write fault leaves the active cache holding the block while V gets `Fatal`. Core runs effects inside ConnectBlock, before `view.Flush()` (`v:3430-3448`).
  - The doc is ambiguous on exactly where the commit sits relative to `connect_inner`. Either way, Sol's fix is right and free: commit only after `finish_attempt` succeeds.
  - I would grade it Minor. `Fatal` stops V before any reader or freezer touches the active layer, and the latch blocks persistence. But the invariant should not depend on that.
- **Sol m2: the F01 process test can deadlock.** `Flush` is class M (`CH1-node.md:156`), served only when V is idle or at a release point. A hook that holds V paused while the test waits for `Flush` never completes. The hook must serve class-M commands while paused, or the test must make the write on V before pausing. Agreed.

**What this review found that Sol did not:**
- M2: the VerifyDB multi-block view and the a2 seam's target parameter.
- M3: the eight M3 tier-1 FTs with no owner, including `feature_block`.
- m1: chained child IS locks.
- m3: H3 and `MnRewardEra` are deployment queries.
- m5 and m7: CS6 API deltas (inline control; probe the exec cache before copying).
- m6: the CheckBlock `fChecked` memo.
- m8: the assumevalid-on run never skips.
- m10: the undo size-header and error-text mapping.
- m11: `feature_abortnode` vs ST2's handle cache.
- m12: H7's ordering for CH7a.
- m13: a reverse-replay differential and fuzz targets.
- m14: L3's dashd regime.
- The nits.

**Combined recommendation for rev2:**
- Apply both reviews' Majors.
- Adopt Sol's M1 (commit after effects) and m2 (F01 hook) alongside this review's M1–M3.
- For the guard, take Sol's all-network scope before CH7a, together with G1/G2's concrete form and tests.

VERDICT: CHANGES-REQUIRED
