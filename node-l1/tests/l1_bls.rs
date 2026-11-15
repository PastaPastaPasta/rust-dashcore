//! `diff_bls_decode` (BS11a-1) against Dash Core v24's in-process oracle: every input class
//! reaches the rows it is for, and the node agrees with Core on every leg and class and on the
//! target's public seeds (written by the `bls_seeds` example from base-sdk's Core-judged
//! corpus).

use std::collections::{BTreeMap, BTreeSet};

use blst::BLST_ERROR;
use dash_oracle_sys::{BlsKind, BlsMode, Oracle, Pin};
use node_l1::bls::{
    BlsInput, COMPRESSED, R, compare_bls_decode, compare_input, decode, on_curve,
    uncompress_verdict,
};
use node_l1::{Outcome, target};

const PIN: Pin = Pin::V24_0;

/// The selector of `kind` (0, 1, 2), lazy or not, under the legacy flag or not, in `class`.
fn selector(kind: u8, lazy: bool, legacy: bool, class: u8) -> u8 {
    kind | u8::from(lazy) << 2 | u8::from(legacy) << 3 | class << 4
}

/// Splitmix64 bytes from `seed`.
fn bytes(seed: u64, n: usize) -> Vec<u8> {
    let mut state = seed;
    (0..n.div_ceil(8))
        .flat_map(|_| {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            (z ^ (z >> 31)).to_le_bytes()
        })
        .take(n)
        .collect()
}

/// The point classes' inputs with the flag byte `flags` and the rest from `seed`.
fn point(kind: u8, legacy: bool, class: u8, flags: u8, seed: u64) -> BlsInput {
    let rest = [&[flags][..], &bytes(seed, 97)].concat();
    BlsInput::read(selector(kind, false, legacy, class), &rest)
}

/// x in the basic layout with the compression flag only. The input's flag byte was `flags`:
/// bit 2 picks the other scheme's layout, and the legacy layout of a G2 x is `c0 || c1`.
fn basic_x(input: &BlsInput, flags: u8) -> Vec<u8> {
    let mut x = input.bytes.clone();
    x[0] &= 0x1f;
    if input.legacy != (flags & 4 != 0) && x.len() == 96 {
        x.rotate_left(48);
    }
    x[0] |= COMPRESSED;
    x
}

fn is_valid(input: &BlsInput) -> Option<bool> {
    decode(input).ok().map(|(valid, _)| valid)
}

/// Each point class builds what it is named for under both flags, and Core agrees on it.
#[test]
fn point_classes_reach_their_rows() {
    for kind in [0, 1] {
        for legacy in [false, true] {
            for seed in 0..32 {
                // Subgroup points: x is a subgroup point's in the flag's layout. With the
                // compression flag they decode as valid basic objects; with either sign bit
                // and no other flag, as valid legacy objects.
                for flags in [0x00, COMPRESSED] {
                    let input = point(kind, legacy, 1, flags, seed);
                    assert_eq!(
                        uncompress_verdict(input.kind, &basic_x(&input, flags)),
                        BLST_ERROR::BLST_SUCCESS
                    );
                    if legacy || flags == COMPRESSED {
                        assert_eq!(is_valid(&input), Some(true), "subgroup {kind} {legacy}");
                    }
                    assert!(compare_input(&input, PIN).is_ok(), "subgroup {kind} {legacy}");
                }
                for (class, want) in [(2, true), (3, false)] {
                    // Flag bits 0 and 1 zero a G2 x's c1 or c0; bit 2 swaps the layout.
                    for flags in [0x80, 0x81, 0x82, 0x84] {
                        let input = point(kind, legacy, class, flags, seed);
                        let x = basic_x(&input, flags);
                        assert_eq!(on_curve(uncompress_verdict(input.kind, &x)), Some(want));
                        if kind == 1 && flags & 3 != 0 {
                            let half = &x[if flags & 1 != 0 {
                                0..48
                            } else {
                                48..96
                            }];
                            assert!(half[0] & 0x1f == 0 && half[1..].iter().all(|&b| b == 0));
                        }
                        assert!(compare_input(&input, PIN).is_ok(), "class {class} {kind}");
                    }
                }
            }
        }
        // x = p is not a field element.
        let input = BlsInput::read(selector(kind, false, false, 4), &[COMPRESSED, 4, 4]);
        assert_eq!(on_curve(uncompress_verdict(input.kind, &input.bytes)), None);
        // The basic identity throws.
        let identity = BlsInput::read(selector(kind, false, false, 5), &[0xc0]);
        assert_eq!(identity.bytes[0], 0xc0);
        assert!(identity.bytes[1..].iter().all(|&b| b == 0));
        assert_eq!(compare_input(&identity, PIN), Ok(Outcome::BothReject));
    }
}

/// r: the invalid object under the legacy flag, a valid key under the basic flag (A1 §1.4.1);
/// r + 1 throws under both; zero is the invalid object under both.
#[test]
fn secret_key_classes_reach_the_order() {
    for (legacy, valid) in [(true, false), (false, true)] {
        let zero = BlsInput::read(selector(2, false, legacy, 2), &[0, 0]);
        assert_eq!(zero.bytes, [0; 32]);
        assert_eq!(is_valid(&zero), Some(false));
        assert_eq!(compare_input(&zero, PIN), Ok(Outcome::BothAccept));
        let r = BlsInput::read(selector(2, false, legacy, 1), &[0]);
        assert_eq!(r.bytes, R);
        assert_eq!(is_valid(&r), Some(valid));
        assert_eq!(compare_input(&r, PIN), Ok(Outcome::BothAccept));
        let above = BlsInput::read(selector(2, false, legacy, 1), &[1]);
        assert_eq!(compare_input(&above, PIN), Ok(Outcome::BothReject));
    }
}

/// Every selector (kind, mode, flag, class) on random rests: no divergence, and each leg sees
/// valid objects and, for eager objects, throws; each point leg sees invalid objects.
#[test]
fn every_leg_and_class_agrees() {
    #[derive(Debug, Default)]
    struct Seen {
        valid: usize,
        invalid: usize,
        threw: usize,
    }
    let mut seen = BTreeMap::<(u8, u8, bool), Seen>::new();
    for sel in 0..=255u8 {
        for seed in 0..12 {
            let rest = bytes(u64::from(sel) << 16 | seed, 97);
            if let Err(d) = compare_bls_decode(&[&[sel][..], &rest].concat(), PIN) {
                panic!("selector {sel:#04x} seed {seed}: {d}");
            }
            let input = BlsInput::read(sel, &rest);
            let leg = seen.entry((input.kind as u8, input.mode as u8, input.legacy)).or_default();
            match is_valid(&input) {
                Some(true) => leg.valid += 1,
                Some(false) => leg.invalid += 1,
                None => leg.threw += 1,
            }
        }
    }
    eprintln!("{seen:?}");
    assert_eq!(seen.len(), 10);
    for (&(kind, mode, legacy), leg) in &seen {
        // Lazy objects never throw on full-length input.
        let lazy = mode == BlsMode::Lazy as u8;
        let point = kind != BlsKind::SecretKey as u8;
        assert!(
            leg.valid > 0 && (leg.invalid > 0 || !point) && (leg.threw > 0) != lazy,
            "{kind} {mode} {legacy}: {leg:?}"
        );
    }
}

/// Short input ends Core's stream; longer input is cut to the object's length.
#[test]
fn lengths() {
    for kind in [0, 1, 2] {
        assert_eq!(
            compare_bls_decode(&[selector(kind, false, true, 0), 1, 2], PIN),
            Ok(Outcome::BothReject)
        );
    }
    let long = BlsInput::read(selector(0, false, false, 0), &[0; 100]);
    assert_eq!(long.bytes.len(), 48);
    assert_eq!(compare_bls_decode(&[], PIN), Ok(Outcome::OutOfDomain));
    // Core has no lazy secret key: the bit reads eagerly.
    assert_eq!(BlsInput::read(selector(2, true, false, 0), &[0; 32]).mode, BlsMode::Eager);
    assert!(
        Oracle::new(PIN).bls_decode(BlsKind::SecretKey, BlsMode::Lazy, false, &[0; 32]).is_err()
    );
}

/// The node agrees with Core on every public seed (`node-fuzz/public-seeds/diff_bls_decode`),
/// and the seeds reach every leg.
#[test]
fn public_seeds_agree() {
    let target = target("diff_bls_decode").expect("registered");
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../node-fuzz/public-seeds/diff_bls_decode");
    let (mut accepted, mut rejected) = (0, 0);
    let mut legs = BTreeSet::new();
    for entry in std::fs::read_dir(dir).expect(dir) {
        let path = entry.expect("entry").path();
        let input = std::fs::read(&path).expect("seed");
        match (target.compare)(&input, PIN) {
            Ok(Outcome::BothAccept) => accepted += 1,
            Ok(Outcome::BothReject) => rejected += 1,
            Ok(other) => panic!("{}: {other:?}", path.display()),
            Err(d) => panic!("{} diverges: {d}", path.display()),
        }
        legs.insert(input[0] & 0x0f);
    }
    eprintln!("{accepted} accepted, {rejected} rejected");
    assert!(accepted > 0 && rejected > 0);
    // pk and sig, eager and lazy, under both flags; sk eager under both.
    assert_eq!(legs.len(), 10, "{legs:?}");
}
