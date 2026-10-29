//! [`Coin`] and [`CoinRef`]: an unspent output with the height and coinbase flag of the
//! transaction that created it, owned or borrowed from its serialized bytes, and Core's two
//! serializations of it: the coins database's (`Coin::Serialize`, [`coin_bytes`]) and the undo
//! record's (`TxInUndoFormatter`).
//!
//! Both serializations are written from the coin as Core holds it after reading, never by copying
//! the bytes it was read from: Core reads an amount outside the money range, an off-curve key of
//! codes 4 and 5 and an oversized raw script into a different coin, and writes that coin. So a
//! coin read from the database and written to an undo record (or back) goes through
//! [`CoinRef::write`] / [`CoinRef::write_undo`], which only copies where that gives Core's bytes.

use dash_primitives::TxOut;

use super::compress::{compress_amount, decompress_amount, read_script_ref};
use super::script::{CoinScript, ScriptRef};
use crate::Amount;
use crate::primitives::read::{read_varint_u32, read_varint_u64, read_whole};
use crate::primitives::serialize::size_of;
use crate::primitives::{CoreWrite, ReadError, ReadErrorKind, write_varint};

/// The largest height a coin holds: Core's `Coin::nHeight` is a 31-bit field.
pub const MAX_COIN_HEIGHT: u32 = (1 << 31) - 1;

/// `AmountCompression`: `CompressAmount` takes the `CAmount` converted to `uint64_t`.
// Core: compressor.h:98 AmountCompression
fn compressed_amount(value: Amount) -> u64 {
    compress_amount(value as u64)
}

// Core: coins.h:61 Coin::Serialize; compressor.h:113 TxOutCompression
/// Core's `Coin::Serialize` of `Coin(out, height, coinbase)`: the coins database's value,
/// `VARINT(height·2 + coinbase) ‖ AmountCompression(value) ‖ ScriptCompression(script)`, written
/// from the output without building a [`Coin`]: [`CoinRef::from_output`], then
/// [`CoinRef::write`].
///
/// # Panics
///
/// If the output is null (value −1): Core asserts `!IsSpent()`. A caller that wants an error
/// instead checks [`CoinRef::is_spent`] first.
pub fn coin_bytes(out: &TxOut, height: u32, coinbase: bool, w: &mut impl CoreWrite) {
    CoinRef::from_output(out, height, coinbase).write(w);
}

/// A coin borrowed from its serialized bytes (either serialization), decoded without allocating:
/// the script stays compressed until [`ScriptRef::expand`]. Equality is that of the coins Core
/// holds after reading ([`ScriptRef`]'s), whatever form each was read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoinRef<'a> {
    value: Amount,
    script_pubkey: ScriptRef<'a>,
    height: u32,
    coinbase: bool,
}

impl<'a> CoinRef<'a> {
    /// `Coin(out, height, coinbase)`, borrowing the output's script (`AddCoins`, and the mempool
    /// view's `Coin(tx.vout[n], MEMPOOL_HEIGHT, false)`). The height is stored in 31 bits, as
    /// Core's bitfield stores it: a larger height keeps its low 31 bits.
    pub fn from_output(out: &'a TxOut, height: u32, coinbase: bool) -> CoinRef<'a> {
        CoinRef {
            value: out.value,
            script_pubkey: ScriptRef::from_script(out.script_pubkey.as_bytes()),
            height: height & MAX_COIN_HEIGHT,
            coinbase,
        }
    }

    /// The coin with `height` and `coinbase` for its own: `ApplyTxInUndo`'s fill-in of a legacy
    /// undo entry's metadata from another output of its transaction ([`Self::height`]).
    // Core: validation.cpp:2258-2266 ApplyTxInUndo
    pub fn with_metadata(self, height: u32, coinbase: bool) -> CoinRef<'a> {
        CoinRef {
            height: height & MAX_COIN_HEIGHT,
            coinbase,
            ..self
        }
    }

    /// `out.nValue`, as Core reads it (`DecompressAmount`, converted to `CAmount`).
    pub fn value(&self) -> Amount {
        self.value
    }

    /// `out.scriptPubKey`, still compressed.
    pub fn script_pubkey(&self) -> ScriptRef<'a> {
        self.script_pubkey
    }

    /// `nHeight`. Zero in an undo entry means the entry lacks its metadata: undo written before
    /// the per-output format (Dash v0.12.2.2) kept the height and coinbase flag only in the entry
    /// of a transaction's last spent output, and Core's `ApplyTxInUndo` takes them from another
    /// unspent output of the same transaction ([`super::access_by_txid`]) or fails the
    /// disconnect. No coin the node creates has height 0, so the decoder only reports it; the
    /// caller decides (CH2's recovery treats it as corruption; importing normalises legacy undo).
    pub fn height(&self) -> u32 {
        self.height
    }

    /// `IsCoinBase()`.
    pub fn is_coinbase(&self) -> bool {
        self.coinbase
    }

    /// `IsSpent()`: Core's null `CTxOut`, value −1. Decoding never produces one: with a zero
    /// exponent `DecompressAmount` returns at most ⌊(2⁶⁴−2)/90⌋·10 + 9 < 2⁶¹, and with a positive
    /// one a multiple of 10, so never 2⁶⁴−1. [`CoinsView::coin`](super::CoinsView::coin)'s "never
    /// spent" and [`CoinRef::write`]'s assert rely on that.
    // Core: coins.h:80 Coin::IsSpent; primitives/transaction.h:207 CTxOut::IsNull (v23.1.8 :187)
    pub fn is_spent(&self) -> bool {
        self.value == -1
    }

    /// The owned coin.
    pub fn to_coin(&self) -> Coin {
        Coin {
            value: self.value,
            script_pubkey: self.script_pubkey.to_script(),
            height: self.height,
            coinbase: self.coinbase,
        }
    }

    /// `nHeight * 2 + fCoinBase`, the first `VARINT` of both serializations.
    fn code(&self) -> u32 {
        self.height * 2 + u32::from(self.coinbase)
    }

    fn write_out(&self, w: &mut impl CoreWrite) {
        write_varint(compressed_amount(self.value), w);
        self.script_pubkey.write_compressed(w);
    }

    fn read_out(input: &mut &'a [u8], code: u32) -> Result<CoinRef<'a>, ReadErrorKind> {
        let value = decompress_amount(read_varint_u64(input)?) as Amount;
        debug_assert!(value != -1, "DecompressAmount never returns 2^64 - 1");
        Ok(CoinRef {
            value,
            script_pubkey: read_script_ref(input)?,
            height: code >> 1,
            coinbase: code & 1 == 1,
        })
    }

    // Core: coins.h:61 Coin::Serialize
    /// Core's `Coin::Serialize` of this coin.
    ///
    /// # Panics
    ///
    /// If the coin is spent: Core asserts `!IsSpent()`.
    pub fn write(&self, w: &mut impl CoreWrite) {
        assert!(!self.is_spent(), "Coin::Serialize: the coin is spent");
        write_varint(u64::from(self.code()), w);
        self.write_out(w);
    }

    /// The size of [`CoinRef::write`]'s output.
    pub fn serialized_size(&self) -> usize {
        size_of(|w| self.write(w))
    }

    // Core: coins.h:69 Coin::Unserialize
    /// Core's `Coin::Unserialize`: one coin from the front of `input`, which advances past it on
    /// success and stays put on failure. Trailing bytes are the caller's: Core's database read
    /// ignores them, and reports a value that fails to decode as no coin (`CDBWrapper::Read`).
    pub fn decode(input: &mut &'a [u8]) -> Result<CoinRef<'a>, ReadError> {
        read_whole(input, |input| {
            let code = read_varint_u32(input)?;
            CoinRef::read_out(input, code)
        })
    }

    // Core: undo.h:26 TxInUndoFormatter::Ser
    /// Core's `TxInUndoFormatter::Ser`, the undo record's entry for a spent coin:
    /// `VARINT(height·2 + coinbase)`, a zero byte where older releases wrote the transaction
    /// version (only when the height is not zero), and `TxOutCompression(out)`.
    pub fn write_undo(&self, w: &mut impl CoreWrite) {
        write_varint(u64::from(self.code()), w);
        if self.height > 0 {
            w.write(&[0]);
        }
        self.write_out(w);
    }

    /// The size of [`CoinRef::write_undo`]'s output.
    pub fn undo_size(&self) -> usize {
        size_of(|w| self.write_undo(w))
    }

    // Core: undo.h:36 TxInUndoFormatter::Unser
    /// Core's `TxInUndoFormatter::Unser`. The version after a non-zero height is any
    /// `VARINT(unsigned int)` and is dropped, so writing the coin back gives a zero byte there.
    pub(crate) fn read_undo(input: &mut &'a [u8]) -> Result<CoinRef<'a>, ReadErrorKind> {
        let code = read_varint_u32(input)?;
        if code >> 1 > 0 {
            read_varint_u32(input)?;
        }
        CoinRef::read_out(input, code)
    }

    /// One undo entry (one input's spent coin) from the front of `input`, which advances past it
    /// on success and stays put on failure. A zero [`CoinRef::height`] is an entry without its
    /// metadata, which the caller handles. A disconnect does not restore an entry whose script
    /// [`ScriptRef::is_unspendable`] (for example a raw script over `MAX_SCRIPT_SIZE`, read as
    /// `OP_RETURN`): Core's `AddCoin` returns early for it (`coins.cpp:72`).
    pub fn decode_undo(input: &mut &'a [u8]) -> Result<CoinRef<'a>, ReadError> {
        read_whole(input, CoinRef::read_undo)
    }
}

/// Core's `Coin`: a `CTxOut`, the height of the block that created it and whether the creating
/// transaction was a coinbase. Its serializations are [`CoinRef`]'s, through [`Coin::as_ref`].
// Core: coins.h:31 Coin
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Coin {
    value: Amount,
    script_pubkey: CoinScript,
    height: u32,
    coinbase: bool,
}

impl Coin {
    /// `Coin(CTxOut, nHeight, fCoinBase)`. The height is stored in 31 bits, as Core's bitfield
    /// stores it: a larger height keeps its low 31 bits.
    pub fn new(value: Amount, script_pubkey: &[u8], height: u32, coinbase: bool) -> Coin {
        Coin {
            value,
            script_pubkey: CoinScript::from(script_pubkey),
            height: height & MAX_COIN_HEIGHT,
            coinbase,
        }
    }

    /// The coin for output `out` of a transaction at `height` (`AddCoins`).
    pub fn from_output(out: &TxOut, height: u32, coinbase: bool) -> Coin {
        Coin::new(out.value, out.script_pubkey.as_bytes(), height, coinbase)
    }

    /// The coin borrowed, for its serializations and for [`super::CoinsView`].
    pub fn as_ref(&self) -> CoinRef<'_> {
        CoinRef {
            value: self.value,
            script_pubkey: ScriptRef::from_script(&self.script_pubkey),
            height: self.height,
            coinbase: self.coinbase,
        }
    }

    /// `out.nValue`.
    pub fn value(&self) -> Amount {
        self.value
    }

    /// `out.scriptPubKey`.
    pub fn script_pubkey(&self) -> &CoinScript {
        &self.script_pubkey
    }

    /// `nHeight`; see [`CoinRef::height`] for a zero height in an undo record.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// `IsCoinBase()`.
    pub fn is_coinbase(&self) -> bool {
        self.coinbase
    }

    /// `IsSpent()`: Core's null `CTxOut`, value −1.
    pub fn is_spent(&self) -> bool {
        self.value == -1
    }

    /// [`CoinRef::write`]: Core's `Coin::Serialize`, the coins database's value.
    ///
    /// # Panics
    ///
    /// If the coin is spent: Core asserts `!IsSpent()`.
    pub fn write(&self, w: &mut impl CoreWrite) {
        self.as_ref().write(w);
    }

    /// [`Coin::write`] into a vector.
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.serialized_size());
        self.write(&mut out);
        out
    }

    /// The size of [`Coin::write`]'s output.
    pub fn serialized_size(&self) -> usize {
        self.as_ref().serialized_size()
    }

    /// [`CoinRef::decode`], owned.
    pub fn decode(input: &mut &[u8]) -> Result<Coin, ReadError> {
        CoinRef::decode(input).map(|coin| coin.to_coin())
    }

    /// [`CoinRef::write_undo`]: the undo record's entry for this coin.
    pub fn write_undo(&self, w: &mut impl CoreWrite) {
        self.as_ref().write_undo(w);
    }

    /// The size of [`Coin::write_undo`]'s output.
    pub fn undo_size(&self) -> usize {
        self.as_ref().undo_size()
    }

    /// [`CoinRef::decode_undo`], owned.
    pub fn decode_undo(input: &mut &[u8]) -> Result<Coin, ReadError> {
        CoinRef::decode_undo(input).map(|coin| coin.to_coin())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p2pkh(fill: u8) -> Vec<u8> {
        let mut s = vec![0x76, 0xa9, 20];
        s.extend([fill; 20]);
        s.extend([0x88, 0xac]);
        s
    }

    #[test]
    fn coins_round_trip_through_both_serializations() {
        for coin in [
            Coin::new(50 * 100_000_000, &p2pkh(1), 1, true),
            Coin::new(0, &[], MAX_COIN_HEIGHT, false),
            Coin::new(21_000_000 * 100_000_000, &[0x51; 200], 0, true),
            Coin::new(1, &p2pkh(9), 123_456, false),
        ] {
            let bytes = coin.serialize();
            assert_eq!(bytes.len(), coin.serialized_size());
            let mut cursor = &bytes[..];
            assert_eq!(Coin::decode(&mut cursor), Ok(coin.clone()));
            assert!(cursor.is_empty());

            let mut undo = Vec::new();
            coin.write_undo(&mut undo);
            assert_eq!(undo.len(), coin.undo_size());
            let mut cursor = &undo[..];
            assert_eq!(Coin::decode_undo(&mut cursor), Ok(coin.clone()));
            assert!(cursor.is_empty());
        }
    }

    /// Amount compression is defined for `0..=MAX_MONEY` only; outside it the value does not
    /// survive, as in Core (the oracle comparisons check what it becomes).
    #[test]
    fn amounts_outside_the_money_range_do_not_round_trip() {
        for value in [i64::MAX, -2] {
            let coin = Coin::new(value, &[0x51], 1, false);
            let decoded = Coin::decode(&mut &coin.serialize()[..]).unwrap();
            assert_ne!(decoded.value(), value);
        }
    }

    #[test]
    fn the_height_is_a_31_bit_field() {
        assert_eq!(Coin::new(1, &[], u32::MAX, false).height(), MAX_COIN_HEIGHT);
        assert_eq!(Coin::new(1, &[], 1 << 31, false).height(), 0);
    }

    #[test]
    fn a_p2pkh_coin_is_core_layout() {
        // VARINT(2·100 + 1) = 0x80 0x49; CompressAmount(1 DASH) = 9; script code 0 and the hash.
        let coin = Coin::new(100_000_000, &p2pkh(0xab), 100, true);
        let mut expected = vec![0x80, 0x49, 0x09, 0x00];
        expected.extend([0xab; 20]);
        assert_eq!(coin.serialize(), expected);

        // The undo entry has the dummy version after the code.
        let mut undo = Vec::new();
        coin.write_undo(&mut undo);
        let mut expected_undo = vec![0x80, 0x49, 0x00, 0x09, 0x00];
        expected_undo.extend([0xab; 20]);
        assert_eq!(undo, expected_undo);
    }

    #[test]
    fn an_undo_entry_at_height_zero_has_no_version() {
        let coin = Coin::new(1, &p2pkh(2), 0, true);
        let mut undo = Vec::new();
        coin.write_undo(&mut undo);
        assert_eq!(&undo[..3], &[0x01, 0x01, 0x00]);
    }

    #[test]
    fn a_non_zero_legacy_version_is_read_and_dropped() {
        // Code (height 1, not coinbase), version VARINT(300), amount, P2PKH.
        let mut bytes = vec![0x02];
        write_varint(300, &mut bytes);
        bytes.push(0x09);
        bytes.push(0x00);
        bytes.extend([5; 20]);
        let mut cursor = &bytes[..];
        let coin = Coin::decode_undo(&mut cursor).unwrap();
        assert!(cursor.is_empty());
        let mut again = Vec::new();
        coin.write_undo(&mut again);
        assert_eq!(again[1], 0, "written back as zero");
        assert_eq!(again.len(), bytes.len() - 1);
    }

    #[test]
    fn a_failed_decode_leaves_the_cursor() {
        let bytes = Coin::new(1, &p2pkh(3), 7, false).serialize();
        let truncated = &bytes[..bytes.len() - 1];
        let mut cursor = truncated;
        let error = Coin::decode(&mut cursor).unwrap_err();
        assert_eq!(error.kind, ReadErrorKind::EndOfData);
        assert_eq!(cursor.len(), truncated.len());
    }

    #[test]
    fn coin_bytes_is_the_coins_serialization() {
        use dash_primitives::TxOut;
        for (value, script, height, coinbase) in
            [(123_456_789i64, p2pkh(4), 2_000_000u32, false), (0, vec![0x51; 40], u32::MAX, true)]
        {
            let out = TxOut {
                value,
                script_pubkey: script.clone().into(),
            };
            let mut bytes = Vec::new();
            coin_bytes(&out, height, coinbase, &mut bytes);
            assert_eq!(bytes, Coin::new(value, &script, height, coinbase).serialize());
            assert_eq!(bytes, Coin::from_output(&out, height, coinbase).serialize());
        }
    }

    #[test]
    fn a_coin_ref_borrows_and_writes_both_forms() {
        let coin = Coin::new(100_000_000, &p2pkh(0xab), 100, true);
        let bytes = coin.serialize();
        let mut cursor = &bytes[..];
        let view = CoinRef::decode(&mut cursor).unwrap();
        assert!(cursor.is_empty());
        // The script stays compressed: special code 0 and the hash, borrowed from `bytes`.
        assert_eq!(view.script_pubkey().as_special(), Some((0, &bytes[4..])));
        let mut buf = [0u8; crate::coins::compress::MAX_SPECIAL_SCRIPT_LEN];
        assert_eq!(view.script_pubkey().expand(&mut buf), &p2pkh(0xab)[..]);
        assert_eq!(view.to_coin(), coin);
        let mut again = Vec::new();
        view.write(&mut again);
        assert_eq!((again.len(), view.serialized_size()), (bytes.len(), bytes.len()));
        assert_eq!(again, bytes);

        // Database bytes to an undo entry, through the coin Core holds.
        let mut undo = Vec::new();
        view.write_undo(&mut undo);
        assert_eq!(undo.len(), view.undo_size());
        let mut expected = Vec::new();
        coin.write_undo(&mut expected);
        assert_eq!(undo, expected);
        let entry = CoinRef::decode_undo(&mut &undo[..]).unwrap();
        assert_eq!(entry.to_coin(), coin);
    }

    /// Core reads an off-curve code-4 key into an empty script, and writes that: the bytes
    /// change, so neither serialization may be copied from the other.
    #[test]
    fn an_off_curve_key_is_written_as_core_read_it() {
        let mut x = [0u8; 32];
        x[31] = 5;
        let bytes = [&[0x02, 0x09, 0x04][..], &x].concat();
        let view = CoinRef::decode(&mut &bytes[..]).unwrap();
        let mut buf = [0u8; crate::coins::compress::MAX_SPECIAL_SCRIPT_LEN];
        assert!(view.script_pubkey().expand(&mut buf).is_empty());
        let mut again = Vec::new();
        view.write(&mut again);
        assert_eq!(again, [0x02, 0x09, 0x06]);
        assert_eq!(again.len(), view.serialized_size());
        assert_eq!(view.to_coin().serialize(), again);
    }

    #[test]
    fn a_zero_height_undo_entry_is_reported() {
        // Code 0 (height 0, not coinbase): no version byte follows.
        let entry = [0x00, 0x09, 0x00].iter().chain(&[7u8; 20]).copied().collect::<Vec<_>>();
        let view = CoinRef::decode_undo(&mut &entry[..]).unwrap();
        assert_eq!(view.height(), 0);
        assert!(!view.is_coinbase());
    }

    #[test]
    #[should_panic(expected = "the coin is spent")]
    fn serializing_a_spent_coin_panics_like_cores_assert() {
        Coin::new(-1, &[], 1, false).serialize();
    }
}
