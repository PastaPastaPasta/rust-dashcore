//! The UTXO-set hashes of Core's coin statistics (`kernel/coinstats.cpp`): the coin serialization
//! MuHash hashes (`TxOutSer`), and `hash_serialized_2`, the legacy SHA256d over the set in
//! coins-database order (`gettxoutsetinfo`'s default hash type), with Core's precedence bug
//! (quirk 33).

use std::collections::BTreeMap;
use std::ops::Range;

use dash_primitives::{BlockHash, OutPoint, TxHash};

use super::coin::CoinRef;
use super::compress::MAX_SPECIAL_SCRIPT_LEN;
use super::muhash::MuHash3072;
use crate::Amount;
use crate::hash::HashWriter;
use crate::primitives::{CoreWrite, write_compact_size, write_varint};

/// Core's `TxOutSer`: the outpoint (txid, then the index as a 32-bit little-endian integer),
/// `nHeight * 2 + fCoinBase` as a 32-bit little-endian integer, then the output (`CTxOut`: the
/// 64-bit value and the script with its length).
// Core: kernel/coinstats.cpp:51 TxOutSer
pub fn tx_out_ser(outpoint: &OutPoint, coin: &CoinRef<'_>, w: &mut impl CoreWrite) {
    w.write(outpoint.hash.as_bytes());
    w.write(&outpoint.index.to_le_bytes());
    let code = coin.height().wrapping_mul(2).wrapping_add(u32::from(coin.is_coinbase()));
    w.write(&code.to_le_bytes());
    w.write(&coin.value().to_le_bytes());
    write_script(coin, w);
}

/// The coin's script as `CScript` serializes it: the expanded script with its length.
fn write_script(coin: &CoinRef<'_>, w: &mut impl CoreWrite) {
    let mut buf = [0; MAX_SPECIAL_SCRIPT_LEN];
    let script = coin.script_pubkey();
    let script = script.expand(&mut buf);
    write_compact_size(script.len() as u64, w);
    w.write(script);
}

/// [`tx_out_ser`]'s bytes.
fn tx_out_bytes(outpoint: &OutPoint, coin: &CoinRef<'_>) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(96);
    tx_out_ser(outpoint, coin, &mut bytes);
    bytes
}

impl MuHash3072 {
    /// Adds a coin to the set: [`MuHash3072::insert`] of its [`tx_out_ser`] bytes, as the
    /// coinstats index does for each new spendable output.
    // Core: index/coinstatsindex.cpp:167 (CoinStatsIndex::CustomAppend)
    pub fn insert_coin(&mut self, outpoint: &OutPoint, coin: &CoinRef<'_>) {
        self.insert(&tx_out_bytes(outpoint, coin));
    }

    /// Removes a spent coin (its undo record's height and coinbase flag): [`MuHash3072::remove`]
    /// of its [`tx_out_ser`] bytes.
    // Core: index/coinstatsindex.cpp:188 (CoinStatsIndex::CustomAppend)
    pub fn remove_coin(&mut self, outpoint: &OutPoint, coin: &CoinRef<'_>) {
        self.remove(&tx_out_bytes(outpoint, coin));
    }
}

/// `VARINT_MODE(n, VarIntMode::NONNEGATIVE_SIGNED)` of a `CAmount`. Core's writer stops at the
/// first value `<= 0x7F`, so a negative amount (no coin read from a valid chain has one) is the
/// single byte of its low seven bits.
// Core: serialize.h:387 WriteVarInt
fn write_amount_varint(value: Amount, w: &mut impl CoreWrite) {
    if value < 0 {
        w.write(&[(value & 0x7f) as u8]);
    } else {
        write_varint(value as u64, w);
    }
}

/// One output of the transaction being hashed: whether `nHeight * 2 + fCoinBase` is non-zero,
/// and its bytes after the group header in `HashSerialized2::fragments`.
struct Output {
    code_nonzero: bool,
    fragment: Range<usize>,
}

/// Core's `hash_serialized_2` over a UTXO set, fed coin by coin in the coins database's order.
///
/// Core walks its database cursor, collects the outputs of one txid in a map keyed by index, and
/// hashes each transaction as its txid ends (`ApplyHash`):
/// `txid ‖ VARINT(header) ‖ for each output by index: VARINT(index + 1) ‖ script ‖
/// VARINT(value)` ‖ `VARINT(0)`, after the best block's hash, all under SHA256d.
///
/// **Quirk 33.** The header is `VARINT(nHeight * 2 + fCoinBase ? 1u : 0u)`: the conditional
/// binds the whole sum, so Core writes 1, not the height and coinbase code, for the lowest-index
/// output of every transaction (0 only for a non-coinbase output at height 0, which no UTXO set
/// holds).
///
/// Order: transactions must arrive in ascending txid byte order (Core's key order) with each
/// transaction's outputs together; within a transaction any order gives Core's hash, as Core's
/// map sorts them. A repeated index replaces the earlier output, as Core's map does.
pub struct HashSerialized2 {
    writer: HashWriter,
    txid: Option<TxHash>,
    outputs: BTreeMap<u32, Output>,
    fragments: Vec<u8>,
}

impl HashSerialized2 {
    /// `PrepareHash`: starts the hash with the set's best block.
    // Core: kernel/coinstats.cpp:185 PrepareHash
    pub fn new(best_block: &BlockHash) -> Self {
        let mut writer = HashWriter::new();
        writer.input(best_block.as_bytes());
        Self {
            writer,
            txid: None,
            outputs: BTreeMap::new(),
            fragments: Vec::new(),
        }
    }

    /// Adds the next coin of the set.
    ///
    /// # Panics
    ///
    /// When `outpoint`'s txid is below the previous coin's (out of the database's order).
    // Core: kernel/coinstats.cpp:116 ComputeUTXOStats (the cursor loop)
    pub fn push(&mut self, outpoint: &OutPoint, coin: &CoinRef<'_>) {
        if let Some(txid) = self.txid {
            // Checked before anything of this coin is written.
            assert!(
                txid.as_bytes() <= outpoint.hash.as_bytes(),
                "hash_serialized_2: transactions out of the database's order"
            );
            if txid != outpoint.hash {
                self.apply(&txid);
            }
        }
        self.txid = Some(outpoint.hash);
        let start = self.fragments.len();
        let w = &mut self.fragments;
        write_varint(u64::from(outpoint.index.wrapping_add(1)), w);
        write_script(coin, w);
        write_amount_varint(coin.value(), w);
        self.outputs.insert(
            outpoint.index,
            Output {
                // Quirk 33: `nHeight * 2 + fCoinBase ? 1u : 0u`.
                code_nonzero: coin.height() != 0 || coin.is_coinbase(),
                fragment: start..self.fragments.len(),
            },
        );
    }

    /// `ApplyHash` for `txid`'s outputs, collected since the previous transaction.
    // Core: kernel/coinstats.cpp:72 ApplyHash
    fn apply(&mut self, txid: &TxHash) {
        let w = &mut self.writer;
        w.input(txid.as_bytes());
        let first = self.outputs.values().next().expect("a transaction has an output");
        write_varint(u64::from(first.code_nonzero), w);
        for output in self.outputs.values() {
            w.input(&self.fragments[output.fragment.clone()]);
        }
        write_varint(0, w);
        self.outputs.clear();
        self.fragments.clear();
    }

    /// `FinalizeHash`: the SHA256d, in Core's internal byte order (RPC prints it reversed).
    // Core: kernel/coinstats.cpp:193 FinalizeHash
    pub fn finish(mut self) -> [u8; 32] {
        if let Some(txid) = self.txid {
            self.apply(&txid);
        }
        self.writer.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash_primitives::TxOut;
    use dash_types::Numeric as _;

    fn outpoint(byte: u8, index: u32) -> OutPoint {
        OutPoint {
            hash: TxHash::from_lendian([byte; 32]),
            index,
        }
    }

    fn output(value: Amount, script: &[u8]) -> TxOut {
        TxOut {
            value,
            script_pubkey: script.to_vec().into(),
        }
    }

    #[test]
    fn tx_out_ser_layout() {
        let out = output(5_000, &[0x51, 0x52]);
        let mut bytes = Vec::new();
        tx_out_ser(&outpoint(7, 3), &CoinRef::from_output(&out, 10, true), &mut bytes);
        let mut expected = vec![7; 32];
        expected.extend(3u32.to_le_bytes());
        expected.extend(21u32.to_le_bytes());
        expected.extend(5_000i64.to_le_bytes());
        expected.extend([2, 0x51, 0x52]);
        assert_eq!(bytes, expected);
    }

    /// The header is 1 whatever the height (quirk 33), and the outputs are hashed by index
    /// whatever order they arrive in.
    #[test]
    fn hash_serialized_2_layout_and_quirk_33() {
        let best = BlockHash::from_lendian([9; 32]);
        let (a, b, c) = (output(1, &[0x51]), output(300, &[]), output(7, &[0x6a]));
        let mut hasher = HashSerialized2::new(&best);
        hasher.push(&outpoint(1, 200), &CoinRef::from_output(&b, 5, false));
        hasher.push(&outpoint(1, 0), &CoinRef::from_output(&a, 40, false));
        hasher.push(&outpoint(2, 0), &CoinRef::from_output(&c, 0, false));
        let got = hasher.finish();

        let mut w = HashWriter::new();
        w.input(&[9; 32]);
        w.input(&[1; 32]);
        w.input(&[1]); // the header: 1, not VARINT(80)
        w.input(&[1, 1, 0x51, 1]); // index 0: VARINT(1), script, VARINT(1)
        w.input(&[0x80, 0x49, 0, 0x81, 0x2c]); // index 200: VARINT(201), empty, VARINT(300)
        w.input(&[0]);
        w.input(&[2; 32]);
        w.input(&[0]); // height 0, not a coinbase: the one case that writes 0
        w.input(&[1, 1, 0x6a, 7, 0]);
        assert_eq!(got, w.finish());
    }

    #[test]
    #[should_panic(expected = "transactions out of the database's order")]
    fn a_lower_txid_after_another_panics() {
        let out = output(1, &[0x51]);
        let coin = CoinRef::from_output(&out, 1, false);
        let mut hash = HashSerialized2::new(&BlockHash::from_lendian([0; 32]));
        hash.push(&outpoint(2, 0), &coin);
        hash.push(&outpoint(1, 0), &coin);
    }

    #[test]
    fn a_transactions_outputs_hash_in_index_order_whatever_their_order() {
        let (a, b) = (output(1, &[0x51]), output(2, &[0x52]));
        let hash = |pushes: &[(u8, u32, &TxOut)]| {
            let mut hash = HashSerialized2::new(&BlockHash::from_lendian([0; 32]));
            for &(txid, index, out) in pushes {
                hash.push(&outpoint(txid, index), &CoinRef::from_output(out, 1, false));
            }
            hash.finish()
        };
        assert_eq!(
            hash(&[(1, 5, &a), (1, 2, &b), (3, 0, &a)]),
            hash(&[(1, 2, &b), (1, 5, &a), (3, 0, &a)])
        );
    }

    #[test]
    fn amount_varint_matches_cores_writer() {
        let mut w = Vec::new();
        write_amount_varint(-1, &mut w);
        write_amount_varint(-200, &mut w);
        write_amount_varint(0x80, &mut w);
        assert_eq!(w, [0x7f, 0x38, 0x80, 0x00]);
    }
}
