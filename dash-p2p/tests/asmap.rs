//! `DecodeAsmap`'s `SanityCheckASMap(bits, 128)` (Core util/asmap.cpp:135-222): a file that
//! fails it decodes to nothing, which Core's init refuses ("Could not parse asmap file").
//!
//! The hand-assembled programs use Core's encodings (`DecodeBits` with `TYPE_BIT_SIZES`,
//! `ASN_BIT_SIZES`, `MATCH_BIT_SIZES`): `RETURN` is `0`, `MATCH` `110`, `DEFAULT` `111`; an ASN
//! below 2¹⁵ is `0` and 15 bits; a one-bit match is `0` and its bit; an eight-bit match is
//! `1111111` and its 8 bits.

use dash_p2p::netgroup::{NetGroupManager, sanity_check_asmap};

/// Core's `src/test/data/asmap.raw` (see `tests/data/README.md`).
const ASMAP_RAW: &[u8] = include_bytes!("data/asmap.raw");

const RETURN: &str = "0";
const MATCH: &str = "110";
const DEFAULT: &str = "111";

/// AS 1: the exponent bit 0, then 15 mantissa bits for `asn - 1`.
const AS1: &str = "0000000000000000";
/// A one-bit match (`MATCH_BIT_SIZES` value 2, `bit_width - 1 = 1`).
const MATCH1: &str = "00";
/// An eight-bit match (value 256, `bit_width - 1 = 8`).
const MATCH8: &str = "111111100000000";

/// Bits from `0`/`1` text, then the file bytes holding them (least significant bit first, zero
/// padded), as `DecodeAsmap` unpacks them.
fn file(program: &[&str], pad: usize) -> Vec<u8> {
    let mut bits: Vec<bool> = program.concat().chars().map(|c| c == '1').collect();
    bits.resize(bits.len().div_ceil(8) * 8 + pad * 8, false);
    bits.chunks(8)
        .map(|byte| byte.iter().enumerate().fold(0u8, |b, (i, &bit)| b | (u8::from(bit) << i)))
        .collect()
}

fn sane(bytes: &[u8]) -> bool {
    let decoded = NetGroupManager::decode_asmap(bytes);
    let bits: Vec<bool> =
        bytes.iter().flat_map(|&b| (0..8).map(move |i| (b >> i) & 1 == 1)).collect();
    assert_eq!(sanity_check_asmap(&bits, 128), !decoded.is_empty());
    if !decoded.is_empty() {
        assert_eq!(decoded, bits);
    }
    !decoded.is_empty()
}

/// Core's test map passes; every truncation of it, and one zero byte more, fails.
#[test]
fn core_asmap_raw_and_its_truncations() {
    assert!(sane(ASMAP_RAW));
    assert!(NetGroupManager::new(NetGroupManager::decode_asmap(ASMAP_RAW), false).using_asmap());
    for len in 0..ASMAP_RAW.len() {
        assert!(!sane(&ASMAP_RAW[..len]), "truncated to {len} bytes");
    }
    let mut longer = ASMAP_RAW.to_vec();
    longer.push(0);
    assert!(!sane(&longer));
    // A truncated file leaves the manager on /16 grouping, which the host must refuse.
    assert!(
        !NetGroupManager::new(NetGroupManager::decode_asmap(&ASMAP_RAW[..10]), false).using_asmap()
    );
}

/// `RETURN AS1` is 17 bits: 7 zero padding bits are sane, a set padding bit or 8 more are not,
/// and 16 bits cut the ASN.
#[test]
fn padding() {
    assert!(sane(&file(&[RETURN, AS1], 0)));
    assert_eq!(file(&[RETURN, AS1], 0), [0, 0, 0]);
    assert!(!sane(&[0, 0, 0x80]), "nonzero padding bit");
    assert!(!sane(&file(&[RETURN, AS1], 1)), "excessive padding");
    assert!(!sane(&[0, 0]), "ASN straddles EOF");
    assert!(!sane(&[]), "EOF without RETURN");
}

#[test]
fn default_rules() {
    assert!(sane(&file(&[DEFAULT, AS1, MATCH, MATCH1, RETURN, AS1], 0)));
    assert!(!sane(&file(&[DEFAULT, AS1, RETURN, AS1], 0)), "RETURN right after DEFAULT");
    assert!(
        !sane(&file(&[DEFAULT, AS1, DEFAULT, AS1, MATCH, MATCH1, RETURN, AS1], 0)),
        "two DEFAULTs"
    );
}

#[test]
fn match_rules() {
    assert!(sane(&file(&[MATCH, MATCH1, RETURN, AS1], 0)));
    assert!(sane(&file(&[MATCH, MATCH8, MATCH, MATCH1, RETURN, AS1], 0)));
    assert!(!sane(&file(&[MATCH, MATCH1, MATCH, MATCH1, RETURN, AS1], 0)), "two short matches");
    // Sixteen eight-bit matches consume the 128 input bits; a seventeenth runs past them.
    let full: Vec<&str> = [MATCH, MATCH8].repeat(16);
    assert!(sane(&file(&[full.concat().as_str(), RETURN, AS1], 0)));
    let over: Vec<&str> = [MATCH, MATCH8].repeat(17);
    assert!(!sane(&file(&[over.concat().as_str(), RETURN, AS1], 0)));
}

/// `JUMP` (`10`, then the offset: `0` and 5 bits for 17 to 48) must land exactly on the next
/// instruction after the taken branch's `RETURN`.
#[test]
fn jump_rules() {
    const JUMP: &str = "10";
    // Offset 17 skips exactly `RETURN AS1` (17 bits): both branches return.
    let jump17 = "0".to_owned() + "00000";
    assert!(sane(&file(&[JUMP, &jump17, RETURN, AS1, RETURN, AS1], 0)));
    // Offset 18 lands one bit into the second RETURN: unreachable code.
    let jump18 = "0".to_owned() + "00001";
    assert!(!sane(&file(&[JUMP, &jump18, RETURN, AS1, RETURN, AS1, "0"], 0)));
    assert!(!sane(&file(&[JUMP, &jump17], 0)), "jump out of range");
    // Past the target the program must go on.
    assert!(!sane(&file(&[JUMP, &jump17, RETURN, AS1], 0)), "ASN straddles EOF");
}
