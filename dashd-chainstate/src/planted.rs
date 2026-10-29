//! Planted bugs (feature `planted-bugs`, test builds only; CH2 §6.4). Each must be caught by the
//! test named beside it. A test arms one on the instance it drives: the coins cache's with
//! `CoinsCache::plant`, the flush's and recovery's with `GuardedEngine::plant` (the CRASH suite's
//! `planted=` configuration does both).

/// A planted bug.
#[cfg(feature = "planted-bugs")]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Planted {
    // ---- ST3a's T-bugs, re-planted in W and recovery (`tests/crash_planted.rs`) ----
    /// B1 before the files barrier.
    T1,
    /// The index-only write without the files barrier.
    T2,
    /// The marker written in Bk instead of B1.
    T3,
    /// The evo state and `evo_tip` in Bk instead of B1.
    T4,
    /// The pruned files unlinked at step 7, before Bk or Bp: Core's order, which unlinks before
    /// the coins are written.
    T5,
    /// The files barrier syncs only the current block and undo files.
    T6,
    /// Recovery erases the marker in its first batch.
    T7,
    /// Recovery's forward leg reads `old`'s branch where it reaches, blocks off the marker's
    /// path (the toy replays every `HAVE_DATA` block above the fork; `IndexAccess` cannot list
    /// them).
    T8,
    /// W continues after a failed `sync_data` of the files barrier.
    T9,
    /// Bk Buffered behind the pre-Bk barrier, then the unlink.
    T11,
    /// `ConnectBlock` sets `HAVE_UNDO` before its undo record is written: the record waits for
    /// the next connect's undo write, at the position it then takes (ST3a's toy defers it the
    /// same way). Planted in the real connect ([`SerialCore`](crate::connect::SerialCore)), so
    /// only the CRASH suite's core mode (`connect=core`) reaches it (CH3a a3).
    T12,
    /// CH3a a13's memo answers a read of input `i` ≥ 1 with input `i − 1`'s coin (a wrong
    /// outpoint-to-slot mapping): caught by xcheck (`chain/tests/connector.rs`'s
    /// `xcheck_catches_a_memo_that_answers_the_wrong_input`), which without it shows the invalid
    /// block it connects.
    L1,
    /// The selection before B1, its clears and `meta.pruned_files` in B1 (A1 rev1; Core writes
    /// the clears with the index, before the coins).
    T13,
    /// B1 drops the dirty `block_files` records.
    T14,
    /// A split flush's B1 Buffered.
    T17,
    /// No pre-Bk barrier, and Bk Buffered.
    T18,
    /// The selection before B1, and B1 omits every record the prune modified, the cleared
    /// entries and the pruned files' infos (DEC-59 literal).
    T19,
    /// The CH2 half of T20: a split flush's B1 names an accepted block above F's tip, which V
    /// has not connected, as the evo tip and the marker's `new`.
    T20,
    /// The CH2 half of T21: the marker alone names that block.
    T21,
    /// The evo state committed after Bk.
    T24,
    /// The unlink after a failed (`Applied`) Bk, past the latch the failure tripped.
    T25,
    /// Bk Durable with no pre-Bk barrier.
    T26,
    /// Recovery's final batch with no barrier.
    T27,
    /// No barrier before B1 while a presumed effect is pending.
    T29,
    /// Recovery erases an index record once it is done: the recovered tip's own record, the only
    /// one recovery can name (the toy erases every entry above the tip).
    T31,
    /// Bk Buffered behind the pre-Bk barrier.
    T34,
    /// rev3's final selection: an automatic check's selector run again at step 7, not `D ∩
    /// eligible`.
    T39,
    /// No re-arm after a selection that leaves the target unmet.
    T40,
    // ---- Opus's CH2 additions ----
    /// No `sync_wal` at open.
    X6,
    /// The replay reads the coins keys before writing them.
    X8,
    /// `add` ignores F's unspent coin: caught by the L0 (b) rows.
    X9,
    /// A prefetch result admitted with a stale `fetch_gen`: caught by xcheck as a read failure, and
    /// with xcheck off in the model's view (a resurrected coin).
    X10,
    /// V releases F before W has finished with it, past the release guard: caught by the model's
    /// meter (tagged ≠ Σ − R), and with the meter off by a lookup mismatch.
    X13,
    /// W treats effects written after the freeze as covered by the pre-B1 barrier.
    X14,
    /// Above the guard: `CoinsCache::fetch_bytes` (the fetch's database read) maps a read error to
    /// a missing coin (FA7).
    X16a,
    /// Inside the guard: `GuardedEngine`'s `GuardedRead::read` (the point read) swallows the
    /// engine error without tripping the latch.
    X16b,
    /// A stored coin that fails to decode reads as missing, as Core reads it (DEC-78 reversed):
    /// caught by `corrupt_coin_aborts`.
    X21,
    /// `merge` drops the child's best block (Sol's S1 mutation): caught by the model's best-block
    /// checks.
    X29,
    /// `have_inputs` resolves an input without caching it in active (Opus high's MB): caught by
    /// the model's tip entries (O1).
    X30,
    /// `fetch` caches a database or prefetch hit as DIRTY (Opus high's MI): caught by the model's
    /// tip entries (O1).
    X31,
    /// `admit_prefetch` skips its epoch check (O5), so a debug build admits a batch whose
    /// snapshot came first: the fence test shows the coin it resurrects.
    X32,
    /// W ignores V's decline of step 7's selection (DEC-211's negative): the files are unlinked
    /// with nothing cleared, so V's index keeps data in files that are gone.
    X33,
    // ---- ST3a's subscriber T-bugs and CH2's subscriber X-bugs (CH2b b3) ----
    /// Step 5's commit and controls published before B1 returns, so a subscriber commits its
    /// locator at `new` first.
    T10,
    /// The lock from the subscriber's in-memory best height, not its durable locator.
    T16,
    /// The subscriber's locator batch without its `sync_wal`.
    T22,
    /// The lock registered only when the subscriber starts, after activation.
    T28,
    /// The lock at the locator's height, not at `LCA(L, new)`.
    T30,
    /// Step 0's dry run under the actual (pre-commit) locks, not the predicted ones.
    T32,
    /// A Syncing subscriber's cadence commit is not capped at `commit_cap`.
    T33,
    /// A Synced subscriber is captured only if it has processed `new` (its best height is at
    /// `new`'s), rather than by its class.
    T35,
    /// The rewind commit is Buffered.
    T36,
    /// No checkpoint at the limit: case B parks instead.
    T37,
    /// The checkpoint is Buffered.
    T38,
    /// The checkpoint skips the journal's keyed rows (the staged data).
    T41,
    /// V's backpressure wait does not service `FlushDue`: the threaded test's watchdog fires.
    X15,
    /// A backward B1 without the rewind gate (ST3a's T42).
    X19,
    // ---- Controls ----
    /// DEC-185 (3)'s negative: recovery's fatal block read returns its error without the
    /// `StoreFailed` and `Abort` marks, so R8 must fail, as the harness supplies no mark.
    Unmarked,
    /// C9's control (round 1's m2): the CRASH SUT never arms recovery's `-dbcrashratio` roll, so
    /// the check that the roll came up must fail.
    NoRoll,
    /// DEC-202's negative: with neither tip nor marker, recovery requires an empty chainstate
    /// again, effect column families included, so a kill before the first durable tip after an
    /// effect must refuse to start (R0).
    NoTipEmpty,
    /// Sol r1's probe: after recovery, before activation, the CRASH SUT deletes every
    /// independent effect record (`i:` in `llmq_snapshot`, Durable), so an effect a completed barrier covered is lost. R13 must
    /// find it on an ordinary restart and under DEC-196's warranted refusal (R0 naming R13).
    LostEffect,
    /// `RefuseCoin`'s control: the same coin deleted on every open, found by R3 on an ordinary
    /// restart.
    LostCoin,
    /// Sol r2's probe: the first coin deleted only when E requires DEC-196's refusal, code the
    /// image before the index-only write never runs. The refusal's R0 must name R3.
    RefuseCoin,
    /// As `RefuseCoin`, a `head_blocks` marker naming the tip written: R0 naming R2.
    RefuseHead,
    /// As `RefuseCoin`, `evo_tip` deleted: R0 naming R2.
    RefuseEvo,
    /// S11c's control (b3 D33): the subscriber driver opens without Core's prune check, so a
    /// disabled index catches up over pruned data, which R11b must find.
    NoPruneChk,
    /// S11c's disable writes its durable record after the deregistration released the lock
    /// (Sol r1's major 2), so W's prune at that boundary leaves an image with the catch-up's data
    /// gone and no record: the refusal is R11b (D36).
    LateRecord,
    /// A mandatory subscriber's open returns with its locator off the durable tip's path, as a
    /// backward flush left it (Sol r2's blocker 1): no open-time rewind, so a crash before its
    /// catch-up leaves L ahead of G, which R11a must find (D45).
    NoOpenRwd,
    /// CRASH's load skips CH1-node's blk-file check (BL-127's control): a restart whose blk file
    /// a failed directory sync lost loads instead of refusing, which R1 must find.
    NoBlkChk,
}

#[cfg(feature = "planted-bugs")]
impl Planted {
    pub const ALL: [Self; 68] = {
        use Planted::*;
        [
            T1, T2, T3, T4, T5, T6, T7, T8, T9, T11, T12, T13, T14, T17, T18, T19, T20, T21, T24,
            T25, T26, T27, T29, T31, T34, T39, T40, X6, X8, X9, X10, X13, X14, X16a, X16b, X21,
            X29, X30, X31, X32, X33, T10, T16, T22, T28, T30, T32, T33, T35, T36, T37, T38, T41,
            X15, X19, Unmarked, NoRoll, NoTipEmpty, LostEffect, LostCoin, RefuseCoin, RefuseHead,
            RefuseEvo, NoPruneChk, LateRecord, NoOpenRwd, NoBlkChk, L1,
        ]
    };
}

#[cfg(feature = "planted-bugs")]
impl std::fmt::Display for Planted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

#[cfg(feature = "planted-bugs")]
impl std::str::FromStr for Planted {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        Self::ALL.into_iter().find(|b| b.to_string() == s).ok_or(())
    }
}

/// Whether `$planted`, an `Option<Planted>`, is planted bug `$bug` (or any of `A | B`): always
/// false without the feature, which compiles the check out.
macro_rules! planted {
    ($planted:expr, $($bug:ident)|+) => {{
        #[cfg(feature = "planted-bugs")]
        let on = matches!($planted, $(Some($crate::planted::Planted::$bug))|+);
        #[cfg(not(feature = "planted-bugs"))]
        let on = false;
        on
    }};
}
pub(crate) use planted;
