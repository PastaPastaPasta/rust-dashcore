//! The regtest miner the `submitblock` tests share (BL-455): coinbase-only blocks, or blocks of
//! given transactions, ground until they meet regtest's target or miss it; and the transactions
//! of blocks with many outputs and spends of them, for tests that need a UTXO set (CH3b b4b).

use dash_consensus::primitives::{decode_header, decode_tx};

/// One block's hex and hash (internal byte order).
pub struct Mined {
    pub hex: String,
    pub hash: [u8; 32],
}

/// The regtest genesis block's time; the blocks follow it a minute apart.
pub const GENESIS_TIME: u32 = 1_417_713_337;

/// A coinbase for `height` (BIP34's push, then an empty push) paying `value` duffs to `OP_TRUE`.
pub fn coinbase(height: u8, value: i64) -> Vec<u8> {
    coinbase_paying(u16::from(height), &[], &[value])
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
    mine_version(prev, time, txs, merkle, meets, 0x2000_0000)
}

/// [`mine`] with the header's `version`.
pub fn mine_version(
    prev: &[u8; 32],
    time: u32,
    txs: &[Vec<u8>],
    merkle: Option<[u8; 32]>,
    meets: bool,
    version: i32,
) -> Mined {
    let merkle = merkle.unwrap_or_else(|| txid(&txs[0]));
    for nonce in 0u32.. {
        let mut header = version.to_le_bytes().to_vec();
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
        compact_size(&mut block, txs.len());
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

/// BIP34's `CScript() << height`: `OP_<height>` to 16, then a minimal `CScriptNum` push.
pub fn height_push(height: u16) -> Vec<u8> {
    if let 1..=16 = height {
        return vec![0x50 + height as u8];
    }
    let mut n = height.to_le_bytes().to_vec();
    while n.last() == Some(&0) {
        n.pop();
    }
    if n.last().is_some_and(|b| b & 0x80 != 0) {
        n.push(0);
    }
    [vec![n.len() as u8], n].concat()
}

/// Bitcoin's compact size of `n`, appended to `out`.
pub fn compact_size(out: &mut Vec<u8>, n: usize) {
    match n {
        0..=0xfc => out.push(n as u8),
        0xfd..=0xffff => {
            out.push(0xfd);
            out.extend((n as u16).to_le_bytes());
        }
        _ => {
            out.push(0xfe);
            out.extend(u32::try_from(n).unwrap().to_le_bytes());
        }
    }
}

/// `values` as outputs to `OP_TRUE`, with their count.
fn op_true_outputs(tx: &mut Vec<u8>, values: &[i64]) {
    compact_size(tx, values.len());
    for value in values {
        tx.extend(value.to_le_bytes());
        tx.extend([1, 0x51]);
    }
}

/// A coinbase for `height` paying `outputs` (values) to `OP_TRUE`. `tag` follows the height in
/// its script, so blocks at one height on two branches have distinct coinbases (no BIP30 repeat).
pub fn coinbase_paying(height: u16, tag: &[u8], outputs: &[i64]) -> Vec<u8> {
    let mut script = height_push(height);
    script.push(u8::try_from(tag.len()).unwrap());
    script.extend(tag);
    let mut tx = 1i32.to_le_bytes().to_vec();
    tx.push(1);
    tx.extend([0; 32]);
    tx.extend(u32::MAX.to_le_bytes());
    compact_size(&mut tx, script.len());
    tx.extend(script);
    tx.extend(u32::MAX.to_le_bytes());
    op_true_outputs(&mut tx, outputs);
    tx.extend(0u32.to_le_bytes());
    tx
}

/// A version-1 transaction spending the `OP_TRUE` outputs `inputs` (txid, vout) with empty
/// scripts, paying `outputs` (values) to `OP_TRUE`.
pub fn spend(inputs: &[([u8; 32], u32)], outputs: &[i64]) -> Vec<u8> {
    let mut tx = 1i32.to_le_bytes().to_vec();
    compact_size(&mut tx, inputs.len());
    for (txid, vout) in inputs {
        tx.extend(txid);
        tx.extend(vout.to_le_bytes());
        tx.push(0);
        tx.extend(u32::MAX.to_le_bytes());
    }
    op_true_outputs(&mut tx, outputs);
    tx.extend(0u32.to_le_bytes());
    tx
}

/// A block of `txs` (the coinbase first) on `prev`, with their merkle root, meeting the target.
pub fn mine_txs(prev: &[u8; 32], time: u32, txs: &[Vec<u8>]) -> Mined {
    mine(prev, time, txs, Some(merkle_root(txs)), true)
}

/// The merkle root of `txs` (internal byte order).
pub fn merkle_root(txs: &[Vec<u8>]) -> [u8; 32] {
    dash_consensus::merkle::compute_merkle_root(txs.iter().map(Vec::as_slice).map(txid).collect()).0
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// [`hex`]'s inverse; panics on anything but pairs of hex digits.
pub fn parse_hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}
