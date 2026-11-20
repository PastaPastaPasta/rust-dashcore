//! Quirk 11 (R4 §1.15): Core computes a block's subsidy from the *previous* block's `nBits` and
//! height, with history baked in:
//!
//! - on mainnet up to previous height 4500 the difficulty ignores the size byte ("a bug which
//!   caused diff to not be correctly calculated"), so encodings of one target that differ in
//!   their words give different subsidies there, and the same from 4501;
//! - each formula's base is truncated toward zero, never rounded, and then clamped;
//! - the formulas switch at previous heights 5465, 17000 and 24000, the last only for a
//!   difficulty above 75;
//! - the decline is iterative, `s -= s / 14` once per 210240 blocks, from block 210241;
//! - the treasury share starts at block 328010 (`nPrevHeight > nBudgetPaymentsStartBlock`).
//!
//! The expected values are Core's `GetBlockSubsidyInner` and `GetSuperblockSubsidyInner` on both
//! pins (`tests/data/oracle/quirks.json`); each check compares Core's answers with each other to
//! show the quirk, and the port's with Core's.

mod common;

use std::collections::BTreeMap;

use common::{ENCODINGS_OF_2_232, chain_params, data, int, int32, subsidy_params, uint32};
use dash_consensus::fp::Difficulty;
use dash_consensus::subsidy::{COIN, block_subsidy_helper};

#[test]
fn quirk_11_subsidy_prev_bits_and_height() {
    let sub = subsidy_params(&chain_params("v24.0", "main"));
    let quirks = data("oracle/quirks.json");
    // (label, prev_bits, prev_height) -> Core's (block, treasury)
    let mut core = BTreeMap::new();
    for case in quirks["subsidy"].as_array().unwrap() {
        assert_eq!(case["chain"], "main");
        let (bits, prev_height) = (uint32(&case["prev_bits"]), int32(&case["prev_height"]));
        let split = (int(&case["block"]), int(&case["treasury"]));
        let port = block_subsidy_helper(bits, prev_height, false, &sub).expect("a subsidy");
        assert_eq!((port.block, port.treasury), split, "{case}");
        core.insert((case["label"].as_str().unwrap(), bits, prev_height), split);
    }
    let at = |label: &str, bits: u32, h: i32| core[&(label, bits, h)];

    // the early difficulty: three encodings of 2^232, one ConvertBitsToDouble value
    let words = ENCODINGS_OF_2_232;
    let d = Difficulty::from_bits(words[0]);
    assert!(words.iter().all(|&w| Difficulty::from_bits(w) == d));
    let early: Vec<_> = words.iter().map(|&w| at("early_difficulty", w, 4500)).collect();
    let later: Vec<_> = words.iter().map(|&w| at("early_difficulty", w, 4501)).collect();
    assert_ne!(early[0], early[1], "the size byte is ignored up to 4500");
    assert_eq!(early[1], early[2]);
    assert!(later.iter().all(|&s| s == later[0]), "from 4501 the words are one difficulty");

    // truncation, not rounding: the genesis difficulty (2.4e-4) gives 1110.46, 153.78 and 26.63,
    // which become 500 (clamped), 153 (not 154) and 25 (clamped). Only 153 tells truncation from
    // rounding; with integer bounds, truncating and clamping commute.
    let genesis = 0x1e0f_fff0;
    assert_eq!(at("truncation", genesis, 5000).0, 500 * COIN);
    assert_eq!(at("truncation", genesis, 10_000).0, 153 * COIN);
    assert_eq!(at("truncation", genesis, 30_000).0, 25 * COIN);

    // the formula boundaries, for a difficulty of 16 (<= 75) and of 4096 (> 75)
    let (low, high) = (0x1c0f_ffff, 0x1b0f_ffff);
    for bits in [low, high] {
        assert_ne!(at("formula_boundary", bits, 5464), at("formula_boundary", bits, 5465));
        assert_eq!(at("formula_boundary", bits, 16_999), at("formula_boundary", bits, 5465));
    }
    // at 17000 the high difficulty leaves the CPU formula, the low one only at 24000
    assert_ne!(at("formula_boundary", high, 16_999), at("formula_boundary", high, 17_000));
    assert_eq!(at("formula_boundary", low, 16_999), at("formula_boundary", low, 23_999));
    assert_ne!(at("formula_boundary", low, 23_999), at("formula_boundary", low, 24_000));
    assert_eq!(at("formula_boundary", high, 17_000), at("formula_boundary", high, 24_000));

    // the decline: one pass of s -= s / 14 at each multiple of 210240, truncating
    let s = |h: i32| at("decline_and_treasury", high, h);
    for (before, after) in [(210_239, 210_240), (420_479, 420_480)] {
        let (b, a) = (s(before).0 + s(before).1, s(after).0 + s(after).1);
        assert_eq!(a, b - b / 14, "{after}");
    }
    // the treasury share: none for block 328009, 10% from block 328010
    assert_eq!(s(328_008).1, 0);
    let total = s(328_009).0 + s(328_009).1;
    assert_eq!(s(328_009).1, total / 10);
}
