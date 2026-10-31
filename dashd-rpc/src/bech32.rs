//! Core's `bech32.cpp` (RP1a-3a D24): Bech32/Bech32m encoding and decoding, and `LocateErrors`,
//! which names why a string is no Bech32(m) string and where. Strings are bytes, as Core's
//! `std::string` is; error locations are byte offsets.
// Core: bech32.cpp, bech32.h

/// `bech32::Encoding`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Bech32,
    Bech32m,
}

/// `CharLimit::BECH32`: BIP173/350's limit for addresses.
pub const CHAR_LIMIT: usize = 90;

const CHECKSUM_SIZE: usize = 6;

/// `CHARSET`.
pub(crate) const CHARSET: &[u8; 32] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

/// `CHARSET_REV`, for bytes below 128 (`CheckCharacters` refuses the rest first).
fn charset_rev(c: u8) -> Option<u8> {
    let c = c.to_ascii_lowercase();
    CHARSET.iter().position(|&x| x == c).map(|i| i as u8)
}

/// `GenerateGFTables`: GF(1024) as GF(32)[x]/(x^2 + 9x + 23), its powers of `e` and their logs.
struct Gf1024 {
    exp: [i16; 1023],
    log: [i16; 1024],
}

const GF: Gf1024 = gf_tables();

const fn gf_tables() -> Gf1024 {
    let mut gf32_exp = [0i8; 31];
    let mut gf32_log = [0i8; 32];
    let fmod = 41;
    gf32_exp[0] = 1;
    gf32_log[0] = -1;
    gf32_log[1] = 0;
    let mut v: i32 = 1;
    let mut i = 1;
    while i < 31 {
        v <<= 1;
        if v & 32 != 0 {
            v ^= fmod;
        }
        gf32_exp[i] = v as i8;
        gf32_log[v as usize] = i as i8;
        i += 1;
    }
    let mut exp = [0i16; 1023];
    let mut log = [0i16; 1024];
    exp[0] = 1;
    log[0] = -1;
    log[1] = 0;
    v = 1;
    i = 1;
    while i < 1023 {
        let v0 = v & 31;
        let v1 = v >> 5;
        let v0n = if v1 != 0 {
            gf32_exp[((gf32_log[v1 as usize] as i32 + gf32_log[23] as i32) % 31) as usize] as i32
        } else {
            0
        };
        let v1n = (if v1 != 0 {
            gf32_exp[((gf32_log[v1 as usize] as i32 + gf32_log[9] as i32) % 31) as usize] as i32
        } else {
            0
        }) ^ v0;
        v = (v1n << 5) | v0n;
        exp[i] = v as i16;
        log[v as usize] = i as i16;
        i += 1;
    }
    Gf1024 {
        exp,
        log,
    }
}

/// `SYNDROME_CONSTS`.
const SYNDROME_CONSTS: [u32; 25] = syndrome_consts();

const fn syndrome_consts() -> [u32; 25] {
    let mut out = [0u32; 25];
    let mut k = 1;
    while k < 6 {
        let mut shift = 0;
        while shift < 5 {
            let b = GF.log[1 << shift] as usize;
            let c0 = GF.exp[(997 * k + b) % 1023] as u32;
            let c1 = GF.exp[(998 * k + b) % 1023] as u32;
            let c2 = GF.exp[(999 * k + b) % 1023] as u32;
            out[5 * (k - 1) + shift] = (c2 << 20) | (c1 << 10) | c0;
            shift += 1;
        }
        k += 1;
    }
    out
}

/// `EncodingConstant`.
fn encoding_constant(encoding: Encoding) -> u32 {
    match encoding {
        Encoding::Bech32 => 1,
        Encoding::Bech32m => 0x2bc8_30a3,
    }
}

/// `PolyMod`.
fn poly_mod(values: &[u8]) -> u32 {
    let mut c: u32 = 1;
    for &v in values {
        let c0 = c >> 25;
        c = ((c & 0x1ff_ffff) << 5) ^ u32::from(v);
        for (bit, k) in [0x3b6a_57b2, 0x2650_8e6d, 0x1ea1_19fa, 0x3d42_33dd, 0x2a14_62b3]
            .into_iter()
            .enumerate()
        {
            if c0 & (1 << bit) != 0 {
                c ^= k;
            }
        }
    }
    c
}

/// `Syndrome`: s_997, s_998 and s_999 of the residue, 10 bits each.
fn syndrome(residue: u32) -> u32 {
    let low = residue & 0x1f;
    let mut result = low ^ (low << 10) ^ (low << 20);
    for (i, k) in SYNDROME_CONSTS.iter().enumerate() {
        if (residue >> (5 + i)) & 1 != 0 {
            result ^= k;
        }
    }
    result
}

/// `CheckCharacters`: the offsets of characters outside 33..=126, and of letters whose case
/// differs from the first letter's.
fn check_characters(s: &[u8], errors: &mut Vec<i32>) -> bool {
    let (mut lower, mut upper) = (false, false);
    for (i, &c) in s.iter().enumerate() {
        if c.is_ascii_lowercase() {
            if upper {
                errors.push(i as i32);
            } else {
                lower = true;
            }
        } else if c.is_ascii_uppercase() {
            if lower {
                errors.push(i as i32);
            } else {
                upper = true;
            }
        } else if !(33..=126).contains(&c) {
            errors.push(i as i32);
        }
    }
    errors.is_empty()
}

/// `PreparePolynomialCoefficients`.
fn prepare(hrp: &[u8], values: &[u8]) -> Vec<u8> {
    let mut ret = Vec::with_capacity(hrp.len() * 2 + 1 + values.len() + CHECKSUM_SIZE);
    ret.extend(hrp.iter().map(|c| c >> 5));
    ret.push(0);
    ret.extend(hrp.iter().map(|c| c & 0x1f));
    ret.extend_from_slice(values);
    ret
}

/// `VerifyChecksum`.
fn verify_checksum(hrp: &[u8], values: &[u8]) -> Option<Encoding> {
    let check = poly_mod(&prepare(hrp, values));
    [Encoding::Bech32, Encoding::Bech32m].into_iter().find(|&e| check == encoding_constant(e))
}

/// `bech32::Encode` of 5-bit `values`.
///
/// # Panics
/// If `hrp` has an uppercase letter or a value is 32 or more, as Core asserts and indexes.
pub fn encode(encoding: Encoding, hrp: &str, values: &[u8]) -> String {
    assert!(!hrp.bytes().any(|c| c.is_ascii_uppercase()), "a lowercase HRP");
    let mut enc = prepare(hrp.as_bytes(), values);
    enc.extend([0; CHECKSUM_SIZE]);
    let m = poly_mod(&enc) ^ encoding_constant(encoding);
    let mut ret = String::with_capacity(hrp.len() + 1 + values.len() + CHECKSUM_SIZE);
    ret.push_str(hrp);
    ret.push('1');
    ret.extend(values.iter().map(|&v| char::from(CHARSET[usize::from(v)])));
    ret.extend(
        (0..CHECKSUM_SIZE).map(|i| char::from(CHARSET[((m >> (5 * (5 - i))) & 31) as usize])),
    );
    ret
}

/// `bech32::DecodeResult`, when valid: the encoding, the lowercase HRP and the data without
/// the checksum.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded {
    pub encoding: Encoding,
    pub hrp: String,
    pub data: Vec<u8>,
}

/// `bech32::Decode(str)` with the default limit.
pub fn decode(s: &[u8]) -> Option<Decoded> {
    if !check_characters(s, &mut Vec::new()) || s.len() > CHAR_LIMIT {
        return None;
    }
    let pos = s.iter().rposition(|&c| c == b'1')?;
    if pos == 0 || pos + CHECKSUM_SIZE >= s.len() {
        return None;
    }
    let values = s[pos + 1..].iter().map(|&c| charset_rev(c)).collect::<Option<Vec<u8>>>()?;
    let hrp: Vec<u8> = s[..pos].iter().map(u8::to_ascii_lowercase).collect();
    let encoding = verify_checksum(&hrp, &values)?;
    Some(Decoded {
        encoding,
        // `check_characters` passed: printable ASCII.
        hrp: String::from_utf8(hrp).expect("ASCII"),
        data: values[..values.len() - CHECKSUM_SIZE].to_vec(),
    })
}

/// `bech32::LocateErrors(str)` with the default limit: an empty message when `s` is a valid
/// Bech32 or Bech32m string, else why and, where it can tell, the offsets at fault.
// Core: bech32.cpp LocateErrors
pub fn locate_errors(s: &[u8]) -> (String, Vec<i32>) {
    let mut error_locations = Vec::new();
    if s.len() > CHAR_LIMIT {
        return ("Bech32 string too long".into(), (CHAR_LIMIT as i32..s.len() as i32).collect());
    }
    if !check_characters(s, &mut error_locations) {
        return ("Invalid character or mixed case".into(), error_locations);
    }
    let Some(pos) = s.iter().rposition(|&c| c == b'1') else {
        return ("Missing separator".into(), Vec::new());
    };
    if pos == 0 || pos + CHECKSUM_SIZE >= s.len() {
        return ("Invalid separator position".into(), vec![pos as i32]);
    }
    let hrp: Vec<u8> = s[..pos].iter().map(u8::to_ascii_lowercase).collect();
    let length = s.len() - 1 - pos;
    let mut values = Vec::with_capacity(length);
    for (i, &c) in s.iter().enumerate().skip(pos + 1) {
        match charset_rev(c) {
            Some(v) => values.push(v),
            None => return ("Invalid Base 32 character".into(), vec![i as i32]),
        }
    }
    let (exp, log) = (|i: usize| i32::from(GF.exp[i]), |i: usize| i32::from(GF.log[i]));
    let at = |p: usize| (s.len() - p - 1) as i32;
    let mut error_encoding = None;
    for encoding in [Encoding::Bech32, Encoding::Bech32m] {
        let mut possible = Vec::new();
        let residue = poly_mod(&prepare(&hrp, &values)) ^ encoding_constant(encoding);
        if residue == 0 {
            return (String::new(), Vec::new());
        }
        let syn = syndrome(residue);
        let (s0, s1, s2) =
            ((syn & 0x3ff) as usize, ((syn >> 10) & 0x3ff) as usize, (syn >> 20) as usize);
        let (l_s0, l_s1, l_s2) = (log(s0), log(s1), log(s2));
        if l_s0 != -1 && l_s1 != -1 && l_s2 != -1 && (2 * l_s1 - l_s2 - l_s0 + 2046) % 1023 == 0 {
            // One error.
            let p1 = ((l_s1 - l_s0 + 1023) % 1023) as usize;
            let l_e1 = l_s0 as usize + (1023 - 997) * p1;
            if p1 < length && l_e1.is_multiple_of(33) {
                possible.push(at(p1));
            }
        } else {
            // Two errors.
            for p1 in 0..length {
                let s2_s1p1 = s2 as i32
                    ^ if s1 == 0 {
                        0
                    } else {
                        exp((l_s1 as usize + p1) % 1023)
                    };
                if s2_s1p1 == 0 {
                    continue;
                }
                let l_s2_s1p1 = log(s2_s1p1 as usize);
                let s1_s0p1 = s1 as i32
                    ^ if s0 == 0 {
                        0
                    } else {
                        exp((l_s0 as usize + p1) % 1023)
                    };
                if s1_s0p1 == 0 {
                    continue;
                }
                let l_s1_s0p1 = log(s1_s0p1 as usize);
                let p2 = ((l_s2_s1p1 - l_s1_s0p1 + 1023) % 1023) as usize;
                if p2 >= length || p1 == p2 {
                    continue;
                }
                let s1_s0p2 = s1 as i32
                    ^ if s0 == 0 {
                        0
                    } else {
                        exp((l_s0 as usize + p2) % 1023)
                    };
                if s1_s0p2 == 0 {
                    continue;
                }
                let l_s1_s0p2 = log(s1_s0p2 as usize);
                let inv_p1_p2 = 1023 - log((exp(p1) ^ exp(p2)) as usize);
                let l_e2 = l_s1_s0p1 as usize + inv_p1_p2 as usize + (1023 - 997) * p2;
                if !l_e2.is_multiple_of(33) {
                    continue;
                }
                let l_e1 = l_s1_s0p2 as usize + inv_p1_p2 as usize + (1023 - 997) * p1;
                if !l_e1.is_multiple_of(33) {
                    continue;
                }
                if p1 > p2 {
                    possible.extend([at(p1), at(p2)]);
                } else {
                    possible.extend([at(p2), at(p1)]);
                }
                break;
            }
        }
        if error_locations.is_empty()
            || (!possible.is_empty() && possible.len() < error_locations.len())
        {
            error_locations = possible;
            if !error_locations.is_empty() {
                error_encoding = Some(encoding);
            }
        }
    }
    let message = match error_encoding {
        Some(Encoding::Bech32m) => "Invalid Bech32m checksum",
        Some(Encoding::Bech32) => "Invalid Bech32 checksum",
        None => "Invalid checksum",
    };
    (message.into(), error_locations)
}

/// `ConvertBits<FROM, TO, PAD>` over `input`: `None` when Core's returns false.
// Core: util/strencodings.h ConvertBits
pub(crate) fn convert_bits(input: &[u8], from: u32, to: u32, pad: bool) -> Option<Vec<u8>> {
    let (mut acc, mut bits) = (0u64, 0u32);
    let maxv = (1u64 << to) - 1;
    let max_acc = (1u64 << (from + to - 1)) - 1;
    let mut out = Vec::with_capacity(input.len() * from as usize / to as usize + 1);
    for &v in input {
        acc = ((acc << from) | u64::from(v)) & max_acc;
        bits += from;
        while bits >= to {
            bits -= to;
            out.push(((acc >> bits) & maxv) as u8);
        }
    }
    if pad {
        if bits > 0 {
            out.push(((acc << (to - bits)) & maxv) as u8);
        }
    } else if bits >= from || ((acc << (to - bits)) & maxv) != 0 {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests;
