//! Byte-level generators for transaction, header and block wire data, shared by the
//! `dash-consensus` properties and the `node-l1` twins (CS0 §9.1, §9.3).
//!
//! Inputs are built from a `u64` seed, so a test prints nothing that describes its input; the
//! seed itself goes to the private sink only (CS0 §9.5). Every integer field is drawn the same
//! way: half the time uniformly over its full range, half the time from its boundary values.
//! The bytes are mostly well-formed and then sometimes mutated, so both decodable and
//! undecodable inputs occur.

#![allow(dead_code)]

/// Core's `MAX_MONEY` in duffs.
pub const MAX_MONEY: i64 = 21_000_000 * 100_000_000;

/// The proptest configuration of the properties and twins (CS0 §9.5): no failure persistence, no
/// shrinking; `PROPTEST_CASES` raises the case count for a deeper run.
// Test code may read the environment; `allow` rather than `expect`, since the crates that
// include this file do not all ban it.
#[allow(clippy::disallowed_methods)]
pub fn proptest_config(default_cases: u32) -> proptest::test_runner::Config {
    proptest::test_runner::Config {
        cases: std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default_cases),
        failure_persistence: None,
        max_shrink_iters: 0,
        ..proptest::test_runner::Config::default()
    }
}

/// The input of one case: `build`'s wire data from `seed`, then [`mutate`]d.
pub fn mutated(seed: u64, build: fn(&mut Rng) -> Vec<u8>) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    let wire = build(&mut rng);
    mutate(&mut rng, wire)
}

/// splitmix64.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (`n > 0`).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }

    pub fn chance(&mut self, one_in: u64) -> bool {
        self.below(one_in) == 0
    }

    pub fn pick<T: Copy>(&mut self, values: &[T]) -> T {
        values[self.below(values.len() as u64) as usize]
    }

    pub fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next_u64() as u8).collect()
    }

    /// A `u16` field: half uniform, half from its boundaries.
    pub fn field_u16(&mut self) -> u16 {
        if self.chance(2) {
            self.next_u64() as u16
        } else {
            self.pick(&[0, 1, 2, 3, 0x7ffe, 0x7fff, 0x8000, 0x8001, 0xfffe, 0xffff])
        }
    }

    /// An `i16` field: half uniform, half from its boundaries.
    pub fn field_i16(&mut self) -> i16 {
        self.field_u16() as i16
    }

    /// A `u32` field: half uniform, half from its boundaries.
    pub fn field_u32(&mut self) -> u32 {
        if self.chance(2) {
            self.next_u64() as u32
        } else {
            self.pick(&[0, 1, 2, 3, 0x7fff_ffff, 0x8000_0000, 0xffff_fffe, 0xffff_ffff])
        }
    }

    /// An `i64` amount field: mostly within `0..=MAX_MONEY`, otherwise any value.
    pub fn field_amount(&mut self) -> i64 {
        match self.below(4) {
            0 => self.next_u64() as i64,
            1 => self.pick(&[i64::MIN, -1, 0, 1, MAX_MONEY, MAX_MONEY + 1, i64::MAX]),
            _ => self.below(MAX_MONEY as u64 + 1) as i64,
        }
    }
}

pub fn compact_size(n: u64, out: &mut Vec<u8>) {
    if n < 253 {
        out.push(n as u8);
    } else if n <= 0xffff {
        out.push(253);
        out.extend_from_slice(&(n as u16).to_le_bytes());
    } else if n <= 0xffff_ffff {
        out.push(254);
        out.extend_from_slice(&(n as u32).to_le_bytes());
    } else {
        out.push(255);
        out.extend_from_slice(&n.to_le_bytes());
    }
}

fn byte_vector(rng: &mut Rng, max: u64, out: &mut Vec<u8>) {
    let len = rng.below(max + 1) as usize;
    compact_size(len as u64, out);
    out.extend(rng.bytes(len));
}

/// The transaction type half: half from Core's named types and the next value, half any value.
fn type_field(rng: &mut Rng) -> u16 {
    if rng.chance(2) {
        rng.below(14) as u16
    } else {
        rng.field_u16()
    }
}

/// One transaction's wire bytes, laid out as Core serializes a transaction.
pub fn tx_bytes(rng: &mut Rng) -> Vec<u8> {
    let mut out = Vec::new();
    let version = rng.field_i16();
    let tx_type = type_field(rng);
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(&tx_type.to_le_bytes());
    let inputs = rng.below(4);
    compact_size(inputs, &mut out);
    for _ in 0..inputs {
        if rng.chance(4) {
            out.extend_from_slice(&[0; 32]);
        } else {
            out.extend(rng.bytes(32));
        }
        out.extend_from_slice(&rng.field_u32().to_le_bytes());
        byte_vector(rng, 40, &mut out);
        out.extend_from_slice(&rng.field_u32().to_le_bytes());
    }
    let outputs = rng.below(4);
    compact_size(outputs, &mut out);
    for _ in 0..outputs {
        out.extend_from_slice(&rng.field_amount().to_le_bytes());
        byte_vector(rng, 40, &mut out);
    }
    out.extend_from_slice(&rng.field_u32().to_le_bytes());
    // Core writes the payload when HasExtraPayloadField() holds; sometimes do the opposite.
    let payload_field = version >= 3 && tx_type != 0;
    if payload_field != rng.chance(8) {
        byte_vector(rng, 200, &mut out);
    }
    out
}

/// One header's 80 wire bytes.
pub fn header_bytes(rng: &mut Rng) -> Vec<u8> {
    let mut out = Vec::with_capacity(80);
    out.extend_from_slice(&(rng.field_u32() as i32).to_le_bytes());
    out.extend(rng.bytes(64));
    for _ in 0..3 {
        out.extend_from_slice(&rng.field_u32().to_le_bytes());
    }
    out
}

/// One block's wire bytes: a header, a count and that many transactions.
pub fn block_bytes(rng: &mut Rng) -> Vec<u8> {
    let mut out = header_bytes(rng);
    let txs = rng.below(5);
    compact_size(txs, &mut out);
    for _ in 0..txs {
        out.extend(tx_bytes(rng));
    }
    out
}

/// Half the time unchanged; otherwise truncated, extended, overwritten in a 4-byte window, or
/// with one bit flipped, at a uniformly chosen place.
pub fn mutate(rng: &mut Rng, mut bytes: Vec<u8>) -> Vec<u8> {
    if bytes.is_empty() || rng.chance(2) {
        return bytes;
    }
    let at = rng.below(bytes.len() as u64) as usize;
    match rng.below(4) {
        0 => bytes.truncate(at),
        1 => {
            let len = 1 + rng.below(8) as usize;
            let extra = rng.bytes(len);
            bytes.extend(extra);
        }
        2 => {
            for i in at..(at + 4).min(bytes.len()) {
                bytes[i] = rng.next_u64() as u8;
            }
        }
        _ => bytes[at] ^= 1 << rng.below(8),
    }
    bytes
}
