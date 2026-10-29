//! Core's coin compression (`compressor.h`, `compressor.cpp`): `CompressAmount`, and
//! `ScriptCompression` with its six special scripts. Identical at v23.1.8 and v24.0.0-rc.3.

use std::num::Wrapping as W;

use secp256k1::PublicKey;

use super::script::{CoinScript, ScriptRef};
use crate::primitives::read::read_bytes;
use crate::primitives::{CoreWrite, ReadErrorKind, read_varint_u32, write_varint};

/// Core's `MAX_SCRIPT_SIZE`.
// Core: script/script.h:33 MAX_SCRIPT_SIZE
pub const MAX_SCRIPT_SIZE: usize = 10_000;

/// `ScriptCompression::nSpecialScripts`: codes below it are the special forms; a raw script of
/// length `n` is written as `VARINT(n + 6)`.
// Core: compressor.h:62 ScriptCompression::nSpecialScripts
pub const SPECIAL_SCRIPTS: u32 = 6;

const OP_RETURN: u8 = 0x6a;
const OP_DUP: u8 = 0x76;
const OP_EQUAL: u8 = 0x87;
const OP_EQUALVERIFY: u8 = 0x88;
const OP_HASH160: u8 = 0xa9;
const OP_CHECKSIG: u8 = 0xac;

// Core: compressor.cpp:149 CompressAmount
/// Core's `CompressAmount`. Its arithmetic is `uint64_t`, so it wraps where Core's does (only
/// for amounts far above `MAX_MONEY`, which Core's comment excludes but its callers can pass).
pub fn compress_amount(mut n: u64) -> u64 {
    if n == 0 {
        return 0;
    }
    let mut e = 0;
    while n.is_multiple_of(10) && e < 9 {
        n /= 10;
        e += 1;
    }
    if e < 9 {
        let d = n % 10;
        n /= 10;
        (W(1) + (W(n) * W(9) + W(d) - W(1)) * W(10) + W(e)).0
    } else {
        (W(1) + (W(n) - W(1)) * W(10) + W(9)).0
    }
}

// Core: compressor.cpp:168 DecompressAmount
/// Core's `DecompressAmount`, wrapping as Core's `uint64_t` arithmetic does.
pub fn decompress_amount(mut x: u64) -> u64 {
    if x == 0 {
        return 0;
    }
    x -= 1;
    let mut e = x % 10;
    x /= 10;
    let mut n = if e < 9 {
        let d = (x % 9) + 1;
        x /= 9;
        x.wrapping_mul(10).wrapping_add(d)
    } else {
        x + 1
    };
    while e > 0 {
        n = n.wrapping_mul(10);
        e -= 1;
    }
    n
}

/// Core's `IsToPubKey` for the uncompressed form: a 65-byte key with header 0x04 that is a point
/// on the curve (`CPubKey::IsFullyValid`, libsecp256k1's `secp256k1_ec_pubkey_parse`).
// Core: compressor.cpp:40 IsToPubKey
fn is_valid_uncompressed_key(key: &[u8]) -> bool {
    PublicKey::from_slice(key).is_ok()
}

// Core: compressor.cpp:55 CompressScript, with IsToKeyID, IsToScriptID and IsToPubKey (:19-53)
/// Core's `CompressScript`: the special code (0–5) and its 20 or 32 bytes, borrowed from
/// `script`, when it is one of the special scripts (P2PKH, P2SH, or P2PK with a compressed key or
/// a valid uncompressed key), `None` otherwise.
pub fn compress_script(script: &[u8]) -> Option<(u8, &[u8])> {
    let s = script;
    if s.len() == 25
        && s[0] == OP_DUP
        && s[1] == OP_HASH160
        && s[2] == 20
        && s[23] == OP_EQUALVERIFY
        && s[24] == OP_CHECKSIG
    {
        return Some((0x00, &s[3..23]));
    }
    if s.len() == 23 && s[0] == OP_HASH160 && s[1] == 20 && s[22] == OP_EQUAL {
        return Some((0x01, &s[2..22]));
    }
    // The compressed-key form is taken without checking the key, as Core's `CPubKey::Set` does.
    if s.len() == 35 && s[0] == 33 && s[34] == OP_CHECKSIG && (s[1] == 0x02 || s[1] == 0x03) {
        return Some((s[1], &s[2..34]));
    }
    if s.len() == 67
        && s[0] == 65
        && s[66] == OP_CHECKSIG
        && s[1] == 0x04
        && is_valid_uncompressed_key(&s[1..66])
    {
        return Some((0x04 | (s[65] & 0x01), &s[2..34]));
    }
    None
}

// Core: compressor.cpp:86 GetSpecialScriptSize
/// Core's `GetSpecialScriptSize`: the bytes after special code `n`.
pub fn special_script_size(n: u32) -> usize {
    match n {
        0 | 1 => 20,
        2..=5 => 32,
        _ => 0,
    }
}

/// The longest script a special form decompresses to: uncompressed P2PK.
pub const MAX_SPECIAL_SCRIPT_LEN: usize = 67;

// Core: compressor.cpp:95 DecompressScript
/// Core's `DecompressScript` for special code `n` (0–5) and its [`special_script_size`] bytes,
/// into `buf`: the script's length, or `None` where Core returns false (an x coordinate of codes
/// 4 and 5 that is not on the curve, or a code above 5).
///
/// # Panics
///
/// If `data` is shorter than [`special_script_size`]`(n)`.
pub fn decompress_script_into(
    n: u32,
    data: &[u8],
    buf: &mut [u8; MAX_SPECIAL_SCRIPT_LEN],
) -> Option<usize> {
    match n {
        0x00 => {
            buf[..3].copy_from_slice(&[OP_DUP, OP_HASH160, 20]);
            buf[3..23].copy_from_slice(&data[..20]);
            buf[23..25].copy_from_slice(&[OP_EQUALVERIFY, OP_CHECKSIG]);
            Some(25)
        }
        0x01 => {
            buf[..2].copy_from_slice(&[OP_HASH160, 20]);
            buf[2..22].copy_from_slice(&data[..20]);
            buf[22] = OP_EQUAL;
            Some(23)
        }
        0x02 | 0x03 => {
            buf[0] = 33;
            buf[1] = n as u8;
            buf[2..34].copy_from_slice(&data[..32]);
            buf[34] = OP_CHECKSIG;
            Some(35)
        }
        0x04 | 0x05 => {
            let mut key = [0u8; 33];
            key[0] = n as u8 - 2;
            key[1..].copy_from_slice(&data[..32]);
            // CPubKey::Decompress: secp256k1_ec_pubkey_parse, then serialize uncompressed.
            let key = PublicKey::from_byte_array_compressed(key).ok()?;
            buf[0] = 65;
            buf[1..66].copy_from_slice(&key.serialize_uncompressed());
            buf[66] = OP_CHECKSIG;
            Some(67)
        }
        _ => None,
    }
}

// Core: script/script.h:534 CScript::IsUnspendable
/// Core's `CScript::IsUnspendable`: starts with `OP_RETURN`, or is longer than
/// `MAX_SCRIPT_SIZE`. Such an output never becomes a coin (`CCoinsViewCache::AddCoin`).
pub fn is_unspendable(script: &[u8]) -> bool {
    script.first() == Some(&OP_RETURN) || script.len() > MAX_SCRIPT_SIZE
}

// Core: compressor.h:65 ScriptCompression::Ser
/// Writes `ScriptCompression(script)`: the compressed form of a special script, otherwise
/// `VARINT(len + 6)` and the script.
pub fn write_compressed_script(script: &[u8], w: &mut impl CoreWrite) {
    if let Some((code, data)) = compress_script(script) {
        w.write(&[code]);
        w.write(data);
        return;
    }
    // Core: `unsigned int nSize = script.size() + nSpecialScripts`.
    write_varint(u64::from((script.len() as u32).wrapping_add(SPECIAL_SCRIPTS)), w);
    w.write(script);
}

// Core: compressor.h:77 ScriptCompression::Unser
/// Reads `ScriptCompression` without decompressing it: a special form stays a code and its
/// bytes, a raw script is borrowed, and a raw script longer than `MAX_SCRIPT_SIZE` is skipped and
/// read as `OP_RETURN`, as Core reads it.
pub fn read_script_ref<'a>(input: &mut &'a [u8]) -> Result<ScriptRef<'a>, ReadErrorKind> {
    let n = read_varint_u32(input)?;
    if n < SPECIAL_SCRIPTS {
        let data = read_bytes(input, special_script_size(n))?;
        return Ok(ScriptRef::special(n as u8, data));
    }
    let size = (n - SPECIAL_SCRIPTS) as usize;
    if size > MAX_SCRIPT_SIZE {
        read_bytes(input, size)?;
        return Ok(ScriptRef::from_script(&[OP_RETURN]));
    }
    Ok(ScriptRef::from_script(read_bytes(input, size)?))
}

/// Reads `ScriptCompression` into a script, as Core does into a freshly constructed (empty) one:
/// a special form decompresses, and where Core's `DecompressScript` fails the script stays empty.
pub fn read_compressed_script(input: &mut &[u8]) -> Result<CoinScript, ReadErrorKind> {
    read_script_ref(input).map(|script| script.to_script())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::serialize::size_of;

    const COIN: u64 = 100_000_000;
    const CENT: u64 = 1_000_000;

    /// Core's `compress_amounts` fixed points (`src/test/compress_tests.cpp`).
    #[test]
    fn amounts_match_cores_fixed_points() {
        for (amount, compressed) in [
            (0, 0x0),
            (1, 0x1),
            (CENT, 0x7),
            (COIN, 0x9),
            (50 * COIN, 0x32),
            (21_000_000 * COIN, 0x1406f40),
        ] {
            assert_eq!(compress_amount(amount), compressed, "{amount}");
            assert_eq!(decompress_amount(compressed), amount, "{compressed}");
        }
    }

    /// Core's `compress_amounts` round-trip loops, scaled down.
    #[test]
    fn amounts_round_trip_like_cores_loops() {
        for i in 1..=10_000u64 {
            assert_eq!(decompress_amount(compress_amount(i)), i);
            assert_eq!(decompress_amount(compress_amount(i * CENT)), i * CENT);
            assert_eq!(decompress_amount(compress_amount(i * COIN)), i * COIN);
        }
        for i in 0..100_000u64 {
            assert_eq!(compress_amount(decompress_amount(i)), i);
        }
    }

    #[test]
    fn amount_arithmetic_wraps_instead_of_panicking() {
        for n in [u64::MAX, u64::MAX - 1, 1 << 63, i64::MAX as u64] {
            let _ = decompress_amount(compress_amount(n));
            let _ = decompress_amount(n);
        }
    }

    fn compressed(script: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        write_compressed_script(script, &mut out);
        assert_eq!(out.len(), size_of(|w| write_compressed_script(script, w)));
        out
    }

    fn round_trip(script: &[u8]) -> CoinScript {
        let bytes = compressed(script);
        let mut cursor = &bytes[..];
        let decoded = read_compressed_script(&mut cursor).unwrap();
        assert!(cursor.is_empty());
        decoded
    }

    /// The secp256k1 generator.
    const GX: [u8; 32] = [
        0x79, 0xbe, 0x66, 0x7e, 0xf9, 0xdc, 0xbb, 0xac, 0x55, 0xa0, 0x62, 0x95, 0xce, 0x87, 0x0b,
        0x07, 0x02, 0x9b, 0xfc, 0xdb, 0x2d, 0xce, 0x28, 0xd9, 0x59, 0xf2, 0x81, 0x5b, 0x16, 0xf8,
        0x17, 0x98,
    ];
    const GY: [u8; 32] = [
        0x48, 0x3a, 0xda, 0x77, 0x26, 0xa3, 0xc4, 0x65, 0x5d, 0xa4, 0xfb, 0xfc, 0x0e, 0x11, 0x08,
        0xa8, 0xfd, 0x17, 0xb4, 0x48, 0xa6, 0x85, 0x54, 0x19, 0x9c, 0x47, 0xd0, 0x8f, 0xfb, 0x10,
        0xd4, 0xb8,
    ];

    /// The shapes of Core's `compress_script_to_*` tests (`src/test/compress_tests.cpp`).
    #[test]
    fn special_scripts_compress_and_round_trip() {
        let mut p2pkh = vec![OP_DUP, OP_HASH160, 20];
        p2pkh.extend([7u8; 20]);
        p2pkh.extend([OP_EQUALVERIFY, OP_CHECKSIG]);
        let out = compressed(&p2pkh);
        assert_eq!((out.len(), out[0]), (21, 0x00));
        assert_eq!(&*round_trip(&p2pkh), &p2pkh[..]);

        let mut p2sh = vec![OP_HASH160, 20];
        p2sh.extend([7u8; 20]);
        p2sh.push(OP_EQUAL);
        let out = compressed(&p2sh);
        assert_eq!((out.len(), out[0]), (21, 0x01));
        assert_eq!(&*round_trip(&p2sh), &p2sh[..]);

        let mut p2pk = vec![33, 0x03];
        p2pk.extend(GX);
        p2pk.push(OP_CHECKSIG);
        let out = compressed(&p2pk);
        assert_eq!((out.len(), out[0]), (33, 0x03));
        assert_eq!(&*round_trip(&p2pk), &p2pk[..]);

        let mut p2pk_u = vec![65, 0x04];
        p2pk_u.extend(GX);
        p2pk_u.extend(GY);
        p2pk_u.push(OP_CHECKSIG);
        let out = compressed(&p2pk_u);
        // GY is even.
        assert_eq!((out.len(), out[0]), (33, 0x04));
        assert_eq!(&*round_trip(&p2pk_u), &p2pk_u[..]);
    }

    /// Core's `compress_p2pk_scripts_not_on_curve`: an uncompressed key off the curve stays raw,
    /// and an x coordinate off the curve under codes 4 and 5 decompresses to nothing.
    #[test]
    fn keys_off_the_curve() {
        let mut off = vec![65, 0x04];
        off.extend([0u8; 64]);
        off.push(OP_CHECKSIG);
        assert!(compress_script(&off).is_none());
        let out = compressed(&off);
        assert_eq!(out[0] as usize, 67 + SPECIAL_SCRIPTS as usize);
        assert_eq!(&*round_trip(&off), &off[..]);

        // x = 5 is not the x coordinate of a curve point (5^3 + 7 = 132 is not a square mod p).
        let mut x = [0u8; 32];
        x[31] = 5;
        assert!(decompress_script_into(4, &x, &mut [0; MAX_SPECIAL_SCRIPT_LEN]).is_none());
        let mut bytes = vec![0x05];
        bytes.extend(x);
        assert!(read_compressed_script(&mut &bytes[..]).unwrap().is_empty());
    }

    #[test]
    fn unspendable_is_cores_rule() {
        assert!(is_unspendable(&[OP_RETURN]));
        assert!(is_unspendable(&[OP_RETURN, 0x51]));
        assert!(!is_unspendable(&[]));
        assert!(!is_unspendable(&[0x51, OP_RETURN]));
        assert!(!is_unspendable(&vec![0x51; MAX_SCRIPT_SIZE]));
        assert!(is_unspendable(&vec![0x51; MAX_SCRIPT_SIZE + 1]));
    }

    #[test]
    fn oversized_raw_scripts_become_op_return() {
        let mut bytes = Vec::new();
        write_varint((MAX_SCRIPT_SIZE + 1 + SPECIAL_SCRIPTS as usize) as u64, &mut bytes);
        bytes.extend(vec![0x51; MAX_SCRIPT_SIZE + 1]);
        let mut cursor = &bytes[..];
        assert_eq!(&*read_compressed_script(&mut cursor).unwrap(), &[OP_RETURN]);
        assert!(cursor.is_empty());

        // Exactly MAX_SCRIPT_SIZE is kept.
        let script = vec![0x51; MAX_SCRIPT_SIZE];
        assert_eq!(&*round_trip(&script), &script[..]);

        // A skipped script must still be present in full.
        bytes.pop();
        assert_eq!(read_compressed_script(&mut &bytes[..]), Err(ReadErrorKind::EndOfData));
    }
}
