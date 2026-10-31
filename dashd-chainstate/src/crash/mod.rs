//! The CRASH model's systems under test (CH2 §6.4; feature `crash`, tests and node-fuzz only):
//! CH2's real cache, flush and recovery on ST3a's power-loss harness (`dashd_store::faulty`).
//!
//! - [`ChainSut`] (the chainstate profile): [`CoinsCache`](crate::coins::CoinsCache), W through
//!   [`FlushControl`](crate::flush::FlushControl) and the [`InlineDriver`](crate::flush::InlineDriver),
//!   [`Recovery`](crate::recovery::Recovery), and ST2's block files on `FaultyFlatFiles`, over the
//!   chainstate `FaultyEngine`; synthetic blocks ([`synth`]), a mini coins connect (no scripts), a
//!   stub evo state in `mn_diff`, and a presumed effect in `llmq_snapshot`; with `prune=`, ST2's
//!   prune mode and the flush's prune steps (D-b2-1). b1 and b2 have no subscribers, so every
//!   scenario is ST3a's `subscribers: None` variant.
//! - [`CoinsSut`] (the coins profile): the cache, W and recovery over the chainstate engine, no
//!   block data and no evo state; one-batch flushes, since no marker can be replayed.
//! - [`ChainRefs`]: CH2's `RefExtractor` (§6.4), ST3a §6.2's with the same `finalizes` scopes.
//! - [`ChainRef`]: the reference.
//! - [`coins_replay_blind`]: §6.5's blind-recovery windows, for ST3a's `sweep`.
//! - [`chainstate_crash`]: node-fuzz's workloads of §6.4's operation families, for ST3a's `sweep`.
//! - [`WorldObserver`], [`WorldSink`]: the seam's adapters onto the world's marks.
//!
//! Design rows (commit 6):
//! - **D-c6-1.** With `old` null, recovery's replay starts at height 1 (Core
//!   `validation.cpp:5288-5289`), so R6 (b) and the extractor's marker path skip genesis.
//! - **D-c6-2.** C9 is split: `crash` rolls in W's flushes of the forward run only, `replay` in
//!   the first recovery's replay only, not nested (R7a compares a nested recovery with one that
//!   exits by design). Both are capped at [`MAX_CRASH_AFTER`].
//! - **D-c6-3.** [`CoinsSut`] flushes in one batch: it has no block files, so no marker could be
//!   replayed.
//! - **D-c6-4.** The extractor sizes a block's records from the generator's cache
//!   ([`synth::lookup`]).
//! - **D-c6-5 (DEC-196, superseding DEC-193).** A load whose coins tip the
//!   index marks failed never keeps that tip. While it strictly outworks every valid block, V
//!   refuses to start with [`INVALID_TIP`], a model message (DEC-165 (3)); at equal work V reorgs
//!   to the valid tip; with less, activation is the usual one. "Valid" is what a load knows: a
//!   candidate, a block whose whole path has data and no failed mark. R6 (e): V opened only where
//!   E does not require the refusal, and its final tip is not failed in E. The harness judges an
//!   abort by its message alone, so `ChainSut::open` turns a refusal E does not allow (E allows one
//!   only when the loaded tip is failed and at least as high as every candidate) into an abort no
//!   table sanctions, which R0 reports. A refusal E allows is judged as an open is, but for
//!   activation's outcome, which `may_refuse` judged (Sol r1, r2): every check of `check`, in
//!   checking mode as the harness's are, against [`ChainRef::checks_view`]. `open` does not get the
//!   harness's reference, and the checks query only blocks, by hash, never listing them: a hash
//!   fixes a synthetic block's height, branch, parent and validity, so the blocks this process
//!   generated answer for the run's own as the harness's reference does. They also hold the blocks
//!   other configurations generated, so R1's unknown-block clause accepts an index entry for one,
//!   which a run-local reference rejects (and V, regenerating under this configuration, may). No
//!   image holds one: the fault model never imports another run's index or file records, so every
//!   stored hash is made in this run (Sol r3); if the model ever permits records persisted across
//!   runs, this bound must be revisited. So R1, R5, R3 and R4 run on the
//!   recovered tip, coins and evo state, both recovered R2 clauses (`head_blocks` gone, `evo_tip`
//!   the tip), R6 (a), R6 (c)'s invalid-marked-valid, R13 on the effects, R6 (b) and C9; the last
//!   two are vacuous there, as recovery finds no marker and appends nothing (a flush after `x`
//!   lands on a valid tip), and so is R6 (c)'s connect rule, as activation refuses before any
//!   connect. A violation turns the refusal into an unsanctioned abort that names it (round 4),
//!   since the restart it advises trusts those stores. The image before the index-only write does
//!   not stand in for these checks: code run only when the loaded tip is failed runs on no other
//!   image (Sol r2). `T31`, `LostEffect`, `RefuseCoin`, `RefuseHead` and `RefuseEvo` witness R5,
//!   R13, R3 and R2 there (D-c6-9). The message advises `-reindex`, then any `invalidateblock`
//!   again, as the rebuilt index has none of its marks; or `-reindex-chainstate`, which keeps
//!   them, only with `-txindex` off: Dash turns `-txindex` on by default, and
//!   `-reindex-chainstate` is refused while it is on. The model has `W`, the
//!   `-reindex-chainstate` wipe, but not `-reindex`'s index rebuild.
//! - **D-c6-6.** ST2's link-count rule reads a block or undo file's link count before writing it,
//!   a read Core does not make. A fault there fails the write, so V reports the write's text, and
//!   the message table accepts it after a block-file read in `AcceptBlock` ("Failed to write
//!   block") and in `ConnectTip` ("Failed to write undo data", beside "Failed to read block": the
//!   harness's cause does not say which read failed). Round 3 found it as an R8 on a `LinkCount`
//!   fault in `save_block`. The table cannot key a text to its read, so the message tests pin
//!   each: the undo write's link-count read gives the write's text, an undo read
//!   "Failed to disconnect block…", and a block read "Failed to read block".
//! - **D-c6-7 (planted bugs, `tests/crash_planted.rs`).** ST3a's T-bugs are re-planted where V's
//!   shape allows: T8's forward leg reads `old`'s branch where it reaches, blocks off the marker's
//!   path (`IndexAccess` cannot list every `HAVE_DATA` block above the fork, as the toy's does);
//!   T31 erases the recovered tip's own record, the only one recovery can name;
//!   T20 and T21 (CH2's halves) name the highest accepted dirty block above F's tip, which V has
//!   not connected; X14 counts `presumed_effects_seq + 1` as covered once B1 is durable. X13
//!   stays with `coins_cache_model` and the cache's tests: an early release changes memory and
//!   lookups, never what reaches the disk. A witness expects the state invariant beside a
//!   trace-shape one (R12 with D1f for T29 and X14, R7a for T27, R12 for T31; R0 for T21, whose
//!   recovery refuses the unapplied tip), and a repro must not reproduce unplanted. X14's witness
//!   is S1's effect at 25, written after flush 1. An effect written inside an `F d`, X14's own
//!   window, trips D1f unplanted: ST3a's `SinceFlushStart` scope counts V's append for a block
//!   above the flush's tip as one of the flush's batches. That loses nothing: the record stays
//!   above the tip, or with no durable tip yet, recovery keeps it (D-c6-12).
//! - **D-c6-8 (FA7).** The harness has no FA7 invariant, so `ChainSut`'s forward check reports
//!   one as R8 with a detail starting `FA7: `. It starts at the first injected read fault, block
//!   files' included, or at the first connect that reads `K`'s coin from the database: not one
//!   the cache serves, nor one after a flush rewrote the coin. A disconnect's read of it is not a
//!   start: the crash images judge that path by R3. From there: no `Connected`, applied or not; no verdict or status log (V
//!   marks a status only through `fail()`, which logs it); and the op that met the start returned
//!   the fatal error and tripped the latch. `K` ([`ChainOp::Corrupt`]) writes past the guard, so
//!   the reference takes it as a no-op and the forward run's only violation is the refused op.
//! - **D-c6-9 (controls).** `Unmarked` is DEC-185 (3)'s negative; `NoRoll` is C9's (round 1's
//!   m2): the first recovery never arms the replay roll, so every `replay=k` must fail. X8 is
//!   caught by a branch bound: S4's images take at most 4 branches each unplanted, the test allows
//!   8, X8's take 64. The lost-file repro (`T1-lost-file`) is pinned by its directory decision,
//!   since the search keeps one finding per (invariant, detail) and reaches it through another.
//!   `LostEffect` is Sol r1's probe in `ChainSut::open`, not a bug of V's: it deletes every
//!   independent effect record (`i:` in `llmq_snapshot`) after recovery, before activation. Its repros are an ordinary restart
//!   (R13) and DEC-196's refusal (`LostEffect-refused`, R0 naming R13). Sol r2's probes act there
//!   too, Durable: `LostCoin` deletes the first coin on every open (R3 on an ordinary restart), and
//!   `RefuseCoin` only when E requires the refusal, so the image before the index-only write is
//!   clean with it and its refusal reports R0 naming R3; `RefuseHead` writes a `head_blocks` marker
//!   naming the tip and `RefuseEvo` deletes `evo_tip` there, R0 naming R2. Each refusal witness is
//!   checked in as `<bug>-refused`, `T31-refused` (R0 naming R5) with them.
//! - **D-c6-10 (`coins_replay_blind`, §6.5).** The smoke is ST3a's `sweep` over
//!   [`coins_replay_blind`]'s windows: 200 seeded cases in the PR suite (calibrated clean on 24,000
//!   over six seeds), and controls T7 (R7a) and T8 (R6) that must be found. Its windows have no
//!   `invalid=`, effects, presumed effects, `a`, `i`, `v`, `x` or `W`, and `sweep` injects no
//!   read faults, so it never reaches DEC-196, FA7 or the replay roll: the suites above cover
//!   those. Its node-fuzz target lands with b3's merge fuzz budget, as `chainstate_crash` does: a
//!   target registered now would need §6.5's merge hours at commit 6. §6.7's `cargo tree` check
//!   covers `crash` where CH2 names `seam`, the name commit 6 did not keep
//!   (`tests/test_feature_scope.rs`). It follows normal edges; on dev edges the benches'
//!   `compile_error!` keeps `crash` and `planted-bugs` out of what PF0 measures, and `model`
//!   stays unguarded (no bench enables it).
//! - **D-c6-11 (PR budget, §6.4).** The CRASH suite (`crash`, `crash_planted`,
//!   `test_feature_scope`) costs 37–41 user CPU-s (40–44 with system time) on agentbox at load
//!   45–85, two runs: 22.5–26.5, 13.8 and 0.6–0.8 with D-c6-12's and Sol r1's and r2's cases (Sol
//!   r2 measured 34.35 warmed), against the ≤ 60 CPU-s budget and CH2's estimate of 40. b2's
//!   pruning brings it to 47–54 (D-b2-8).
//! - **D-c6-12 (DEC-202, no durable tip).** No durable tip is a logically empty store. V writes
//!   an effect at its block's connect, ahead of the flush, so a kill before the first durable tip
//!   can leave effect records (`llmq_snapshot`, the one effect column family since CH7 Q1) with
//!   no tip.
//!   Recovery's step 5 keeps them, as it keeps effects above a tip, and the connects from height
//!   1 rewrite them. Any coins or evo-state record, or an `evo_tip`, with no tip stays
//!   corruption; with a tip or a marker nothing changes. Keeping, not dropping: R13 requires an
//!   effect a completed barrier covered to survive, A1 §2.10.1 persists an effect no less durably
//!   than Core, and with a marker whose `old` is null the coins-only replay would not rewrite a
//!   dropped one. Round 5's four repros and an image after a wipe are regress cases, S1-early
//!   adds an effect before S1's first flush to the exhaustive suite, and `NoTipEmpty`, the old
//!   rule, is the planted negative (R0). A kept record the restart does not rewrite is as
//!   harmless as one above a tip, as only the active chain's blocks read effects: V never reads
//!   the independent `i:` records (R13's check alone does), and the stub evo fold reads the presumed record at
//!   the reader height only, after the active chain's own base block, connected in the same
//!   epoch when no tip is durable, rewrote it. R12's continuation flushes the activated tip, so
//!   R3 compares the evo state with [`ChainRef`]'s fold of that chain's own base;
//!   `a_kept_effect_off_the_restarted_branch_is_never_read` restarts onto branch A after a kill
//!   on B, and a base connect that skips the rewrite fails it (R12). V's real effect, the
//!   rotation snapshot, is keyed by its cycle base's hash (A1 §2.10.1), so a reader never looks
//!   up an off-chain one.
//!
//! Design rows (CH2b b2, pruning):
//! - **D-b2-1 (the prune model).** `prune=` turns on ST2's prune mode and step 0's files host
//!   with Core's regtest `nPruneAfterHeight` under `-fastprune` (100). Before `A t` sets an
//!   automatic target the node is `-prune=1`'s: automatic checks select nothing, and `p h`
//!   (`pruneblockchain h`, a NONE call with the manual height) prunes. `MIN_BLOCKS_TO_KEEP` is
//!   Core's 288, so the prune scenarios run at production scale: with 64 KiB blk files file 0
//!   holds heights 0–122 and file 1 heights 123–243, and S5 prunes at 410; S5p is S5 one-batch,
//!   with the clears in Bp. V's clears on the arena are the node's `prune_clear`: data, undo and
//!   positions, the entries dirty.
//! - **D-b2-2 (IF_NEEDED call sites).** In prune mode `c n` makes Core's IF_NEEDED call after
//!   each connect, so an armed automatic check is decided between connects; outside prune mode
//!   there is no call, so the other scenarios' traces are b1's. The call comes after V's connect,
//!   at its new tip, where Core's is inside `ConnectTip` before `SetTip`; a decision lands one
//!   block later, and the heights in the scenarios are the model's. `F d`'s connects make no call.
//! - **D-b2-3 (startup scan).** Recovery runs CH1-node's startup step after the index loads: with
//!   `pruned_files` durable, ST2's scan unlinks the pruned files left on disk, inside BestEffort
//!   marks, as Core's `ScanAndUnlinkAlreadyPrunedFiles` does in `LoadBlockIndex`.
//! - **D-b2-4 (candidates).** Activation's candidates and R6 (e)'s best valid tip need data only
//!   off the active chain (Core's `FindMostWorkChain`): a pruned block on the loaded chain stays
//!   on a candidate's path, a pruned side branch is no candidate. The SUT and R6 (e) share
//!   `candidates`, so a mutation of it passes CRASH; the unit test
//!   `candidates_need_data_only_off_the_active_chain` kills it (p3's mutation m7).
//! - **D-b2-5 (R5, R9, FA2).** R5 entitles an epoch's own prunes: a file one of its selections
//!   names may read as an empty record, and the file's index entries may lose their data, undo
//!   and positions. R9: with `pruned_files` durable, no pruned file below the durable last file
//!   is on disk after the startup scan, unless its remove failed in this epoch (best-effort; the
//!   next scan retries it). FA2 on every forward run: a selection is within its decision's D,
//!   and re-arms exactly when an automatic decision's D met the target and the selection leaves
//!   it unmet.
//! - **D-b2-6 (planted prune bugs).** T5 unlinks at step 7, before Bk or Bp (Core's own order),
//!   and step 12 then finds nothing to remove; T11 writes Bk Buffered behind the pre-Bk barrier;
//!   T13 selects before B1 and writes the clears and `pruned_files` in B1; T19 selects before B1
//!   and B1 omits every record the prune changed; T25 unlinks after a failed Bk, past the latch
//!   (through the guard, the latch the failure tripped refuses the unlink); T39 runs the
//!   automatic selector again at step 7; T40 never re-arms. Witnesses: T5 on S5 with both kinds
//!   and T11 on S5 from the unlink with PowerLoss (R0, D1e), T13 on S5 and T19 on S5b (R0, D1b),
//!   T25 on S5 with an `Applied` failure of Bk (R8, D1e), T39 and T40 on S14b's forward run
//!   (FA2). A Bp that is not Durable is D1e on S5p's forward run (Opus r1's M6).
//! - **D-b2-7 (T15 has no site).** ST3a's T15 selects against V's tip where the flush's tip is
//!   lower. Here step 0 decides on V, at Core's chain tip of the call: the flush's tip, or inside
//!   a disconnect the block being disconnected. Step 7 selects `D ∩ eligible` under the bound
//!   from that decision, a subset of D, and W reads no later tip of V's; V checks its own later
//!   tip only to decline (D-b2-9), never to widen a selection. No placement of the bug changes a
//!   selection, so T15 is not re-planted. S5c keeps its V-ahead shape: D = {0} at 525, and V at
//!   535 when W selects would make file 1 eligible.
//! - **D-b2-8 (PR budget with pruning).** The CRASH suite costs 47–54 user CPU-s (51–58 with
//!   system time) on agentbox at load 30–37, two runs: 28.7–34.3, 17.7–19.2 and 0.6; the base
//!   before b2 measured 34.6–39.4 beside it. Every prune scenario runs the forward model over
//!   at least 410 blocks (D-b2-1), so to stay within the ≤ 60 CPU-s budget the PR runs start
//!   late: S5's clean run at the selection, S5p's and T11's witness at the unlink, and the
//!   witness table builds a scenario only for its own bug (S5's Bk fault and S14b's target run
//!   the forward model). The clean runs of S5 from the flush's begin, S5b, S5c and S14b are
//!   nightly (`#[ignore]`), as ST3a moved its S5b, S5c and S14b clean runs (DEC-101 Q5): 7.0,
//!   6.9, 34.7 and 15.6 CPU-s alone, and so is S5d (D-b2-9), about 10. The nightly set is `cargo
//!   test -p dashd-chainstate --features alloc-tag,crash --test crash -- --ignored`:
//!   `s5_c10_whole_flush`, `s5b_c10_no_index_write`, `s5c_c6_c10_v_ahead`,
//!   `s14b_c10_automatic_target_and_v_growth` and
//!   `s5d_c10_v_rewinds_inside_the_flush_and_declines` (BL-126 adds the farm's `crash-nightly`
//!   job that runs it).
//! - **D-b2-9 (V's decline, DEC-211).** Step 7's request carries the selection's highest block
//!   and its rule; V answers at its working tip, and declines a selection that tip's rule no
//!   longer allows, as a rewind inside the flush can leave it. W then prunes nothing and re-arms
//!   the automatic check. The automatic rule's bound is Core's `last_prunable`, which wraps below
//!   `MIN_BLOCKS_TO_KEEP`, behind Core's gate: no selection at a tip at or below
//!   `nPruneAfterHeight` (Core v24 `node/blockstorage.cpp:194`, `FindFilesToPrune`'s early
//!   return); `-fastprune`'s 100 keeps the wrap at tips 101–287. `D d` is the rewind: a flush V
//!   does not wait for, during whose files barrier V disconnects `d` blocks. Of the pruning
//!   scenarios only the nightly S5d uses it (D-b2-8): the automatic decision at 410 selects file 0
//!   (bound 122), V answers at 409 and declines (the trace's `PruneDeclined`, flush 2, file 0), and
//!   the re-armed check prunes at 410's reconnect. Core's sequential order would consume the check
//!   at 409 (bound 121, nothing selected) and prune only once a later allocation re-arms it;
//!   DEC-211's re-arm prunes earlier, and the end state is the prune-first linearization's (Core
//!   pruning at 410 before the rewind). With X33 planted (W ignores the decline) its clean run
//!   finds R1, R12, D1a and D1e. In the PR set V moves back inside such a flush only where nothing
//!   prunes, in C3-real (`D d` and `R fork:len`, CH3b D206) and the `chainstate_crash` smoke's
//!   generated workloads (`D d`), and V waits for a manual prune, so no PR scenario declines: a
//!   declined flush writes what a flush without a prune writes. The decline, its re-arm, the next
//!   flush's selection at V's tip and X33 are pinned there by the machine and control tests.
//! - **D-b2-10 (removal warnings, Opus r1's n4).** A pruned file that cannot be removed, at step
//!   12 or in the startup scan, is logged as a `blockstorage` warning (`Prune: Failed to remove
//!   file …`), and so is a failed sync of the blocks directory after the removals (`Prune: Failed
//!   to sync the blocks directory …`). Core's `UnlinkPrunedFiles` ignores its removals' error
//!   code and syncs nothing (CH2 §3.9 D8), so it logs neither. Both keep running and leave the
//!   file to the next startup scan: the only difference is these log lines, which compat's log
//!   diff must allow.
//! - **D-b2-11 (the barrier's directory sync, DEC-212, BL-124).** Each `Flush` at a files
//!   barrier, the blk file's and the rev file's, makes one `fsync` of `blocks/`, Core's
//!   `DirectoryCommit`, never retried. A failure is logged as a `blockstorage` warning (`Failed to
//!   sync the blocks directory …`) and the flush goes on; Core ignores the result and logs
//!   nothing, so compat's log diff must allow the line. `fdatasync` failures still abort. ST3a's
//!   R8 does not count a failed FS sync-dir as fatal, and R1 and D1(a) entitle a reference into a
//!   file whose entry the failed sync left pending as a create, if a crash then loses it. For a
//!   blk file R1's entitlement holds only on the load's refusal (BL-127, Opus r1's m-4): CH1-node's
//!   load, as Core's `LoadBlockIndexDB` (`node/blockstorage.cpp:377-390`), opens every blk file a
//!   `HAVE_DATA` entry names and refuses with "Error loading block database" at the first that
//!   does not open; recovery models the check, the refusal is sanctioned only when the file was
//!   lost so and E's records pass R1, and an open that succeeds has every such file. Rev files
//!   are not checked at load (the undo read fails later), so theirs stays. `NoBlkChk`, which skips
//!   the check, is its control. V also
//!   syncs the datadir, which holds `blocks/`'s own entry, per sequence until a sync of both
//!   succeeds (`blocks/` may not exist yet at the startup scan's); Core never syncs it, so this is
//!   durability Core lacks, not a parity change. That sync is one attempt too, made whether or
//!   not `blocks/`'s succeeded, and a failure of it is entitled the same way (a crash may lose
//!   `blocks/` with its files, as in Core). A failure of the datadir's sync alone is logged with
//!   the same prefix and names the datadir (`Failed to sync the blocks directory: <datadir>: …`);
//!   Core logs neither, so this has no parity effect.

mod blind;
mod chain;
mod coins;
mod extractor;
mod fa;
mod mini;
mod real;
mod reference;
mod roundtrip;
mod stores;
mod subs;
pub mod synth;
mod workload;
mod world;

use std::fmt;
use std::str::FromStr;

pub use blind::coins_replay_blind;
pub use chain::ChainSut;
pub use coins::CoinsSut;
pub use extractor::ChainRefs;
pub use reference::ChainRef;
pub use roundtrip::connect_disconnect_roundtrip;
pub use subs::{SUB_STORE, SubConfig, SubMode};
pub use workload::chainstate_crash;
pub use world::{WorldObserver, WorldSink};

/// The engine's name in the world: the chainstate.
pub const CHAIN_STORE: &str = "chainstate";
/// `blocks/` under ST3a's datadir.
pub const BLOCKS_DIR: &str = "/dd/blocks";
/// Regtest's message start.
pub const MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];

/// Core's call paths, by label (DEC-162 (4), DEC-165 (3)).
pub const PATH_LOAD: &str = "LoadChainstate";
/// Inside `LoadChainstate`: the load's blk-file check (BL-127).
pub const PATH_LOAD_INDEX: &str = "LoadBlockIndexDB";
pub const PATH_DISCONNECT: &str = "DisconnectTip";
pub const PATH_CONNECT: &str = "ConnectTip";
pub const PATH_ACCEPT: &str = "AcceptBlock";
pub const PATH_FLUSH: &str = "FlushStateToDisk";

// Core: validation.cpp:2300, :3282, :3621 (an undo read that fails while disconnecting)
pub const DISCONNECT_FAILED: &str = "Failed to disconnect block; see debug.log for details";
// Core: validation.cpp:3412
pub const BLOCK_READ_FAILED: &str = "Failed to read block";
// Core: node/blockstorage.cpp:869, :784
pub const BLOCK_WRITE_FAILED: &str = "Failed to write block";
pub const UNDO_WRITE_FAILED: &str = "Failed to write undo data";
/// V's refusal to start on a loaded tip the index marks failed that outworks every valid block
/// (DEC-196): a model message (DEC-165 (3)), not Core's. It advises `-reindex` first, as
/// `-reindex-chainstate` is refused while `-txindex` is on and Dash turns it on by default;
/// `-reindex` rebuilds the index without `invalidateblock`'s marks, so the calls are repeated.
pub const INVALID_TIP: &str = "The chainstate tip is invalid and no valid block has at least its \
     work; restart with -reindex, then repeat any invalidateblock calls (or use \
     -reindex-chainstate, which keeps them, if -txindex is off).";

/// The log line that names an independent effect once its batch has returned (R13).
pub(crate) const EFFECT_WRITTEN: &str = "effect written ";
/// The log line before `wipe_chainstate` (C12): from it, the empty chainstate is an allowed
/// recovered state.
pub(crate) const WIPE_BEGUN: &str = "wipe begun";

/// A [`ChainSut`] configuration (one repro line).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChainConfig {
    /// S, W's and recovery's coin sub-batch bound in bytes; `None`: one-batch flushes.
    pub sub_batch: Option<u64>,
    /// Heights of branch 0 whose blocks are invalid (they spend a coin no block creates).
    pub invalid: Vec<u32>,
    /// Heights whose connect writes an independent Buffered effect (`i:<height>` in
    /// `llmq_snapshot`).
    pub effects: Vec<u32>,
    /// The presumed effect: its base height, whose connect writes it Buffered
    /// (`llmq_snapshot`), and the reader height, whose stub evo fold reads it.
    pub presumed: Option<(u32, u32)>,
    /// `-dbcrashratio` in W (C9): the roll comes up after this many partial sub-batches of each
    /// flush of the forward run (R12's continuation flushes to completion).
    pub crash_after: Option<u32>,
    /// `-dbcrashratio` in recovery (C9): the roll comes up after this many partial sub-batches of
    /// the first recovery's replay.
    pub crash_replay: Option<u32>,
    /// `-prune`: the automatic target in bytes, 0 for manual pruning only (`-prune=1`); `None`
    /// does not prune. `prune=` in the repro line only when set.
    pub prune: Option<u64>,
    /// Core's `nPruneAfterHeight`; `None` is regtest's `-fastprune` 100 (chainparams.cpp:839),
    /// below which no automatic check runs. Under `MIN_BLOCKS_TO_KEEP` + 1 the automatic bound
    /// wraps past every prune lock, as Core's does (blockstorage.cpp:198), so the b3 scenarios with
    /// an automatic target and a lock set it above (D16). `pah=` in the repro line only when set.
    pub prune_after: Option<u64>,
    /// b3's test subscriber (`subs`); `index=` in the repro line only when set. `None` is ST3a's
    /// `subscribers: None` variant.
    pub index: Option<SubConfig>,
    /// Core mode (CH3a a3, CH2 §6.4's swap): V connects through the real `ConnectBlock`
    /// ([`SerialCore`](crate::connect::SerialCore)) on regtest blocks it accepts, not the mini
    /// connect on the synthetic ones; an invalid block's script or input check fails there
    /// ([`synth`]). `connect=core` in the repro line only when set.
    pub core: bool,
    /// A planted bug, armed in the chainstate's guard (so in W and recovery) and in the coins
    /// cache; `planted=` in the repro line only when set.
    #[cfg(feature = "planted-bugs")]
    pub planted: Option<crate::Planted>,
}

/// The largest `crash_after` and `crash_replay`: the roll's seed search costs about e·k² rolls.
pub const MAX_CRASH_AFTER: u32 = 64;

/// The length of the common prefix of two block paths: the fork's index.
pub(crate) fn common_prefix<T: PartialEq>(a: &[T], b: &[T]) -> usize {
    a.iter().zip(b).take_while(|(a, b)| a == b).count()
}

fn list(v: &[u32]) -> String {
    if v.is_empty() {
        "-".into()
    } else {
        v.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
    }
}

fn parse_list(s: &str) -> Result<Vec<u32>, ()> {
    if s == "-" {
        return Ok(Vec::new());
    }
    s.split(',').map(|x| x.parse().map_err(|_| ())).collect()
}

fn opt(v: Option<impl fmt::Display>) -> String {
    v.map_or("-".into(), |v| v.to_string())
}

fn parse_opt<T: FromStr>(s: &str) -> Result<Option<T>, ()> {
    if s == "-" {
        Ok(None)
    } else {
        s.parse().map(Some).map_err(|_| ())
    }
}

impl fmt::Display for ChainConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let presumed = self.presumed.map(|(b, r)| format!("{b}:{r}"));
        write!(
            f,
            "sub={} invalid={} effects={} presumed={} crash={} replay={}",
            opt(self.sub_batch),
            list(&self.invalid),
            list(&self.effects),
            opt(presumed),
            opt(self.crash_after),
            opt(self.crash_replay),
        )?;
        if let Some(target) = self.prune {
            write!(f, " prune={target}")?;
        }
        if let Some(height) = self.prune_after {
            write!(f, " pah={height}")?;
        }
        if let Some(index) = self.index {
            write!(f, " index={index}")?;
        }
        if self.core {
            write!(f, " connect=core")?;
        }
        #[cfg(feature = "planted-bugs")]
        if let Some(bug) = self.planted {
            write!(f, " planted={bug}")?;
        }
        Ok(())
    }
}

impl FromStr for ChainConfig {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        let mut c = Self::default();
        for kv in s.split_whitespace() {
            let (k, v) = kv.split_once('=').ok_or(())?;
            match k {
                "sub" => c.sub_batch = parse_opt(v)?,
                "invalid" => c.invalid = parse_list(v)?,
                "effects" => c.effects = parse_list(v)?,
                "presumed" => {
                    c.presumed = match v {
                        "-" => None,
                        _ => {
                            let (b, r) = v.split_once(':').ok_or(())?;
                            Some((b.parse().map_err(|_| ())?, r.parse().map_err(|_| ())?))
                        }
                    }
                }
                "crash" => c.crash_after = parse_opt(v)?,
                "replay" => c.crash_replay = parse_opt(v)?,
                "prune" => c.prune = parse_opt(v)?,
                "pah" => c.prune_after = Some(v.parse().map_err(|_| ())?),
                "index" => c.index = Some(v.parse()?),
                "connect" => {
                    c.core = match v {
                        "core" => true,
                        "mini" => false,
                        _ => return Err(()),
                    }
                }
                #[cfg(feature = "planted-bugs")]
                "planted" => c.planted = Some(v.parse()?),
                _ => return Err(()),
            }
        }
        if [c.crash_after, c.crash_replay].into_iter().flatten().any(|k| k > MAX_CRASH_AFTER) {
            return Err(());
        }
        Ok(c)
    }
}

/// A [`ChainSut`] op (ST3a's toy grammar, the subscriber-free part, plus the wipe).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChainOp {
    /// `c n`: connect `n` blocks of the current branch, accepting them as needed.
    Connect(u32),
    /// `a n`: accept `n` blocks ahead of the tip (data and index entry, not connected).
    Accept(u32),
    /// `f`: an ALWAYS flush (the ALWAYS shortcut's index-only write if nothing is dirty).
    Flush,
    /// `i`: the periodic index-only write.
    IndexWrite,
    /// `r fork:len`: disconnect to height `fork`, then connect `len` blocks of a new branch.
    Reorg {
        fork: u32,
        len: u32,
    },
    /// `v`: a `TestBlockValidity` connect of the first accepted block (`applied = false`).
    TestValidity,
    /// `x h`: `invalidateblock` at height `h`.
    Invalidate(u32),
    /// `W`: `-reindex-chainstate`'s wipe, then the reconnect from the block files.
    Wipe,
    /// `C`: R12's continuation: one connect, then a flush.
    Continue,
    /// `F d`: a flush V does not wait for; V connects `d` blocks once W's files barrier has
    /// returned (C6: `HAVE_DATA` blocks above `new` appended during the flush).
    FlushDuring(u32),
    /// `D d`: a flush V does not wait for; V disconnects `d` blocks once W's files barrier has
    /// returned, a rewind inside the flush: V answers step 7 at the lower tip (DEC-211).
    FlushRewind(u32),
    /// `R fork:len`: a flush V does not wait for; V runs `r fork:len` once W's files barrier has
    /// returned, the whole reorg inside the flush (CH3 §4.6's C3-real).
    FlushReorg {
        fork: u32,
        len: u32,
    },
    /// `K`: the tip's coinbase coin overwritten on disk with bytes that do not decode, past the
    /// guard (FA7's corrupt stored coin); a later connect that spends it from the database reads
    /// it, unless a flush has rewritten it.
    Corrupt,
    /// `p h`: `pruneblockchain h`, a manual prune check (a flush only if it finds files).
    Prune(u32),
    /// `A t`: the automatic prune target set to `t` bytes, and a check made due, with no flush
    /// now (the next call site decides).
    AutoTarget(u64),
    /// `q h`: `pruneblockchain`'s height `h` for the next `f`'s check, with no flush now (the
    /// toy's `q`).
    PruneRequest(u32),
    /// `s n`: `n` steps of the subscriber (ST3a's toy `s`): a Syncing one's catch-up, a Synced
    /// one's events.
    SubSync(u32),
    /// `k`: a Syncing subscriber's cadence timer, its capped commit now (the toy's `K`).
    SubTick,
    /// `D` (bare; `D d` is [`Self::FlushRewind`]): the subscriber disabled for the rest of the
    /// epoch (S11c, the toy's `D`). `Dp h`: with W's manual prune at `h` run once the
    /// deregistration has released the subscriber's lock, as a W thread may (D36).
    SubDisable(Option<u32>),
}

impl fmt::Display for ChainOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(n) => write!(f, "c{n}"),
            Self::Accept(n) => write!(f, "a{n}"),
            Self::Flush => f.write_str("f"),
            Self::IndexWrite => f.write_str("i"),
            Self::Reorg {
                fork,
                len,
            } => write!(f, "r{fork}:{len}"),
            Self::TestValidity => f.write_str("v"),
            Self::Invalidate(h) => write!(f, "x{h}"),
            Self::Wipe => f.write_str("W"),
            Self::Continue => f.write_str("C"),
            Self::FlushDuring(d) => write!(f, "F{d}"),
            Self::FlushRewind(d) => write!(f, "D{d}"),
            Self::FlushReorg {
                fork,
                len,
            } => write!(f, "R{fork}:{len}"),
            Self::Corrupt => f.write_str("K"),
            Self::Prune(h) => write!(f, "p{h}"),
            Self::AutoTarget(t) => write!(f, "A{t}"),
            Self::PruneRequest(h) => write!(f, "q{h}"),
            Self::SubSync(n) => write!(f, "s{n}"),
            Self::SubTick => f.write_str("k"),
            Self::SubDisable(None) => f.write_str("D"),
            Self::SubDisable(Some(h)) => write!(f, "Dp{h}"),
        }
    }
}

impl FromStr for ChainOp {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        let (head, rest) = s.split_at(s.char_indices().nth(1).map_or(s.len(), |(i, _)| i));
        let num = |r: &str| r.parse::<u32>().map_err(|_| ());
        Ok(match head {
            "c" => Self::Connect(num(rest)?),
            "a" => Self::Accept(num(rest)?),
            "f" if rest.is_empty() => Self::Flush,
            "i" if rest.is_empty() => Self::IndexWrite,
            "v" if rest.is_empty() => Self::TestValidity,
            "W" if rest.is_empty() => Self::Wipe,
            "C" if rest.is_empty() => Self::Continue,
            "K" if rest.is_empty() => Self::Corrupt,
            "k" if rest.is_empty() => Self::SubTick,
            "D" if rest.is_empty() => Self::SubDisable(None),
            "D" if rest.starts_with('p') => Self::SubDisable(Some(num(&rest[1..])?)),
            "s" => Self::SubSync(num(rest)?),
            "x" => Self::Invalidate(num(rest)?),
            "F" => Self::FlushDuring(num(rest)?),
            "D" => Self::FlushRewind(num(rest)?),
            "p" => Self::Prune(num(rest)?),
            "q" => Self::PruneRequest(num(rest)?),
            "A" => Self::AutoTarget(rest.parse().map_err(|_| ())?),
            "r" | "R" => {
                let (fork, len) = rest.split_once(':').ok_or(())?;
                let (fork, len) = (num(fork)?, num(len)?);
                if head == "r" {
                    Self::Reorg {
                        fork,
                        len,
                    }
                } else {
                    Self::FlushReorg {
                        fork,
                        len,
                    }
                }
            }
            _ => return Err(()),
        })
    }
}

/// Parses a workload: ops separated by `;`.
///
/// # Panics
///
/// On an op that does not parse.
pub fn ops(s: &str) -> Vec<ChainOp> {
    s.split(';').map(|o| o.trim().parse().unwrap_or_else(|()| panic!("op `{o}`"))).collect()
}
