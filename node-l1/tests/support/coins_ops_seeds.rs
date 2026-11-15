//! Hand-written `diff_coins_ops` inputs (O2a-CO, CH2c), each read by `node_l1::coins_ops::
//! script_from_bytes` in its mode into the script its comment names. Shared by `l1_coins_ops` and
//! the `coins_ops_seeds` example, which writes each to `node-fuzz/public-seeds/diff_coins_ops`
//! behind its mode byte.

// The example writes the inputs only.
#![allow(dead_code)]

use node_l1::coins_ops::{Allowed, Mode};

/// A seed: its name, its mode, its script bytes and how its run ends.
pub struct Seed {
    pub name: &'static str,
    pub mode: Mode,
    pub input: &'static [u8],
    pub expect: Expect,
}

/// How a seed's run ends.
pub enum Expect {
    /// Every op compared, with a tip flush among them.
    Full,
    /// Both sides threw Core's `logic_error`.
    BothLogicError,
    /// Ended by a by-design difference.
    Ended(Allowed),
    /// A HaveCoinInCache answer skipped as `Allowed::ChildReadCaching`.
    Skips,
}

pub const SEEDS: &[Seed] = &[
    // Tip ops over an empty database, a best block, a flush, then reads from the database.
    Seed {
        name: "tip-flush",
        mode: Mode::Aligned,
        input: &[
            0, 1, 5, 0, 2, 0x84, 5, 1, 3, 1, 4, 2, 2, 1, 6, 2, 7, 1, 7, 0, 8, 5, 2, 3, 2, 4, 1, 2,
            2, 8, 7, 0,
        ],
        expect: Expect::Full,
    },
    // An unspent coin added again without possible_overwrite: Core's logic_error on both.
    Seed {
        name: "tip-overwrite",
        mode: Mode::Aligned,
        input: &[0, 1, 4, 0, 1, 4],
        expect: Expect::BothLogicError,
    },
    // possible_overwrite over an unspent coin, then a spend of the DIRTY non-FRESH coin.
    Seed {
        name: "tip-possible-overwrite",
        mode: Mode::Aligned,
        input: &[0, 1, 4, 7, 1, 8, 0, 1, 0x84, 2, 1, 5, 1, 8],
        expect: Expect::Full,
    },
    // An unspendable output: neither side adds it.
    Seed {
        name: "tip-unspendable",
        mode: Mode::Aligned,
        input: &[0, 1, 6, 4, 1, 7, 1, 8],
        expect: Expect::Full,
    },
    // A block's view that spends a database coin and a tip coin, adds coins, reads, commits; then
    // a flush.
    Seed {
        name: "block-commit",
        mode: Mode::Aligned,
        input: &[
            0, 1, 4, 7, 1, 8, 0, 2, 0x88, 9, 2, 1, 2, 2, 0, 3, 12, 3, 3, 4, 1, 5, 2, 6, 8, 5, 3, 3,
            1,
        ],
        expect: Expect::Full,
    },
    // A coin added and spent in the same block's view (FRESH, so nothing reaches the tip).
    Seed {
        name: "block-fresh-spend",
        mode: Mode::Aligned,
        input: &[9, 0, 5, 4, 2, 5, 6, 5, 5, 8],
        expect: Expect::Full,
    },
    // A block's view dropped: the tip is as before.
    Seed {
        name: "block-drop",
        mode: Mode::Aligned,
        input: &[0, 1, 4, 9, 2, 1, 0, 2, 8, 7, 3, 1, 3, 2, 7, 1, 8],
        expect: Expect::Full,
    },
    // DEC-131: the block adds, without possible_overwrite, a coin the tip holds unspent.
    Seed {
        name: "block-fresh-at-add",
        mode: Mode::Aligned,
        input: &[0, 1, 4, 9, 0, 1, 8],
        expect: Expect::Ended(Allowed::FreshAtAdd),
    },
    // A database coin read through the block's view: Core's tip caches it, CH2's does not.
    Seed {
        name: "block-read-caching",
        mode: Mode::Aligned,
        input: &[0, 1, 4, 7, 1, 8, 9, 3, 1, 4, 1, 5, 1, 7, 5, 1],
        expect: Expect::Skips,
    },
    // A database coin read through a block's view, then an unspendable add of its outpoint, which
    // neither side makes: Core's tip still holds the coin from the read.
    Seed {
        name: "block-read-then-unspendable",
        mode: Mode::Aligned,
        input: &[7, 1, 0, 1, 0, 8, 9, 3, 1, 7, 0, 1, 2, 5, 1],
        expect: Expect::Skips,
    },
    // Uncache: a clean entry goes, a DIRTY one stays.
    Seed {
        name: "tip-uncache",
        mode: Mode::Aligned,
        input: &[0, 1, 4, 7, 1, 8, 3, 1, 5, 1, 6, 1, 5, 1, 0, 2, 8, 6, 2, 5, 2],
        expect: Expect::Full,
    },
    // A database coin read through one block's view, then added FRESH by the next: Core's tip
    // holds it from the read, so only Core's commit throws (CH2 §2.4.2, O-R6-m3).
    Seed {
        name: "block-commit-read-caching",
        mode: Mode::Aligned,
        input: &[0, 1, 4, 7, 1, 8, 9, 4, 1, 6, 9, 0, 1, 8, 6],
        expect: Expect::Ended(Allowed::ChildReadCaching),
    },
    // Two blocks committed in a row, each flushed.
    Seed {
        name: "two-blocks",
        mode: Mode::Aligned,
        input: &[7, 1, 9, 0, 1, 4, 0, 2, 0x89, 6, 8, 9, 2, 1, 0, 3, 13, 6, 8, 3, 2, 3, 3],
        expect: Expect::Full,
    },
    // CH2c: Core's map entries (ENTRY) on the tip, with a block's view open and after its commit.
    Seed {
        name: "aligned-entries",
        mode: Mode::Aligned,
        input: &[
            0, 0, 4, 0, 1, 5, 7, 1, 0xf0, 0, 9, 3, 2, 2, 0, 0xf0, 0, 0, 3, 9, 6, 0xf0, 0, 0xf0, 3,
            8, 0xf0, 0, 3, 1,
        ],
        expect: Expect::Full,
    },
    // CH2c hidden mode: F frozen, written by W in steps around tip and block ops, released, then
    // frozen again and finished by the tip flush.
    Seed {
        name: "hidden-w-steps",
        mode: Mode::Hidden,
        input: &[
            0, 0, 4, 0, 1, 5, 0, 2, 8, 7, 1, 0xf1, 0, 0xf1, 6, 0, 3, 9, 2, 0, 0xf1, 6, 0xf1, 0, 3,
            0, 4, 1, 9, 2, 1, 0, 4, 12, 6, 0xf1, 0, 0xf0, 1, 8, 3, 2, 8,
        ],
        expect: Expect::Full,
    },
    // CH2c hidden mode: a coin released to the database is added again; Core's tip still holds
    // it, so only Core refuses.
    Seed {
        name: "hidden-flush-point-add",
        mode: Mode::Hidden,
        input: &[0, 0, 4, 7, 1, 0xf1, 0, 0xf1, 0, 0, 0, 8],
        expect: Expect::Ended(Allowed::FlushPoint),
    },
    // CH2c hidden mode: a block's view adds a released coin FRESH; only Core's commit throws.
    Seed {
        name: "hidden-flush-point-commit",
        mode: Mode::Hidden,
        input: &[0, 0, 4, 7, 1, 0xf1, 0, 0xf1, 0, 9, 0, 0, 8, 6],
        expect: Expect::Ended(Allowed::FlushPoint),
    },
    // A FRESH coin frozen, then spent twice: Core erased it at the first spend, CH2 left a
    // tombstone over F, so the second spend's answer is uncompared (D3).
    Seed {
        name: "hidden-respend",
        mode: Mode::Hidden,
        input: &[0, 0, 1, 7, 0xff, 0xff, 0, 2, 0, 2, 0, 8],
        expect: Expect::Full,
    },
    // A flushed coin added again without `possible_overwrite`, uncached: Core takes it FRESH over
    // its database's coin, so its spend leaves the database's (D26). Aligned mode compares it all.
    Seed {
        name: "aligned-fresh-over-base",
        mode: Mode::Aligned,
        input: &[0, 0, 0, 7, 1, 8, 0, 0, 5, 2, 0, 4, 0, 8],
        expect: Expect::Full,
    },
    // The same add in hidden mode, on the tip and through a block's view: the run ends at the
    // freeze that follows.
    Seed {
        name: "hidden-fresh-over-base",
        mode: Mode::Hidden,
        input: &[0, 0, 0, 7, 1, 8, 0, 0, 5, 0xf1, 0, 2, 0, 8],
        expect: Expect::Ended(Allowed::FreshOverBase),
    },
    Seed {
        name: "hidden-fresh-over-base-child",
        mode: Mode::Hidden,
        input: &[0, 0, 0, 7, 1, 8, 9, 0, 0, 5, 6, 0xf1, 0, 2, 0, 8],
        expect: Expect::Ended(Allowed::FreshOverBase),
    },
    // The same add, spent before a freeze: both sides erase the FRESH coin and the run goes on.
    Seed {
        name: "hidden-fresh-over-base-spent",
        mode: Mode::Hidden,
        input: &[0, 0, 0, 7, 1, 8, 0, 0, 5, 2, 0, 0xf1, 0, 4, 0, 8],
        expect: Expect::Full,
    },
    // A clean coin frozen into F, uncached from Core's tip only, then added again: Core takes it
    // FRESH over its database's, CH2 refuses it because F holds it (D26).
    Seed {
        name: "hidden-fresh-over-base-refused",
        mode: Mode::Hidden,
        input: &[0, 0, 0, 7, 1, 8, 3, 0, 0xf1, 0, 6, 0, 0, 0, 5],
        expect: Expect::Ended(Allowed::FreshOverBase),
    },
];
