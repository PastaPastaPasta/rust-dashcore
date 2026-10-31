//! The regtest miner the `submitblock` tests share (BL-455): coinbase-only blocks, or blocks of
//! given transactions, ground until they meet regtest's target or miss it.

use dash_consensus::primitives::{decode_header, decode_tx};

/// One block's hex and hash (internal byte order).
pub struct Mined {
    pub hex: String,
    pub hash: [u8; 32],
}

/// The regtest genesis block's time; the blocks follow it a minute apart.
pub const GENESIS_TIME: u32 = 1_417_713_337;

/// A coinbase for `height` (BIP34's `OP_<height>` to 16, then a one-byte push) paying `value`
/// duffs to `OP_TRUE`.
pub fn coinbase(height: u8, value: i64) -> Vec<u8> {
    let mut tx = 1i32.to_le_bytes().to_vec();
    tx.push(1);
    tx.extend([0; 32]);
    tx.extend(u32::MAX.to_le_bytes());
    match height {
        1..=16 => tx.extend([2, 0x50 + height, 0]),
        17..=127 => tx.extend([3, 1, height, 0]),
        _ => panic!("no coinbase script for height {height}"),
    }
    tx.extend(u32::MAX.to_le_bytes());
    tx.push(1);
    tx.extend(value.to_le_bytes());
    tx.extend([1, 0x51]);
    tx.extend(0u32.to_le_bytes());
    tx
}

/// `tx`'s txid (internal byte order).
pub fn txid(tx: &[u8]) -> [u8; 32] {
    *decode_tx(&mut &tx[..]).unwrap().into_value().txid().as_bytes()
}

/// A block of `txs` on `prev`, its merkle root `merkle` (the first transaction's txid when
/// `None`), ground until its hash meets the regtest target, or misses it when `meets` is false.
pub fn mine(
    prev: &[u8; 32],
    time: u32,
    txs: &[Vec<u8>],
    merkle: Option<[u8; 32]>,
    meets: bool,
) -> Mined {
    let merkle = merkle.unwrap_or_else(|| txid(&txs[0]));
    for nonce in 0u32.. {
        let mut header = 0x2000_0000i32.to_le_bytes().to_vec();
        header.extend(prev);
        header.extend(merkle);
        header.extend(time.to_le_bytes());
        header.extend(0x207f_ffffu32.to_le_bytes());
        header.extend(nonce.to_le_bytes());
        let hash = *decode_header(&mut header.as_slice()).unwrap().into_value().hash().as_bytes();
        // The target is 0x7fffff << 232: a top byte under 0x7f meets it, one above misses it.
        if !(if meets {
            hash[31] < 0x7f
        } else {
            hash[31] > 0x7f
        }) {
            continue;
        }
        let mut block = header;
        block.push(u8::try_from(txs.len()).unwrap());
        for tx in txs {
            block.extend(tx);
        }
        return Mined {
            hex: hex(&block),
            hash,
        };
    }
    unreachable!("a nonce meets the regtest target")
}

/// A block at `height` on `prev` whose coinbase is `cb`.
pub fn next_with(prev: &[u8; 32], height: u8, cb: Vec<u8>) -> Mined {
    mine(prev, GENESIS_TIME + 60 * u32::from(height), &[cb], None, true)
}

/// A block at `height` on `prev` whose coinbase pays `value`.
pub fn next(prev: &[u8; 32], height: u8, value: i64) -> Mined {
    next_with(prev, height, coinbase(height, value))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// [`hex`]'s inverse; panics on anything but pairs of hex digits.
pub fn parse_hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}
