//! [`PackedCoin`]: a coin as Core's `Coin` serialization, the bytes the coins database stores, kept
//! inline up to [`INLINE_MAX`] bytes.

use dash_consensus::coins::compress::{
    MAX_SPECIAL_SCRIPT_LEN, SPECIAL_SCRIPTS, compress_amount, compress_script,
    decompress_script_into,
};
use dash_consensus::coins::{CoinRef, ScriptRef};
use dash_consensus::primitives::{ArrayWriter, CoreWrite, ReadError, TxOut, write_varint};

use super::ledger::{HeldOut, LedgerBox};
use crate::error::CoinsError;

/// The longest coin kept inline. Every P2PKH and P2SH coin fits at any height and amount: a
/// height code below 2²⁷ takes ≤ 4 bytes, a compressed amount up to `MAX_MONEY` ≤ 8, and the
/// compressed script 21, so ≤ 33 bytes (CH2 §2.2).
pub const INLINE_MAX: usize = 34;

/// A coin as Core's `Coin::Serialize` writes it (`coins.h:60-66`, both pins):
/// `VARINT(height·2 + coinbase) ‖ VARINT(CompressAmount(value)) ‖ ScriptCompression(script)`.
/// The cache value and the database value are the same bytes, so a flush copies them. Two values
/// are equal when their bytes are.
#[derive(Clone)]
pub struct PackedCoin(Repr);

#[derive(Clone)]
enum Repr {
    Inline {
        len: u8,
        bytes: [u8; INLINE_MAX],
    },
    Long(Box<[u8]>),
    /// A long undo coin a block overlay's `spend` returned, counted in the native ledger
    /// (`Usage::held_out`) until it drops (CH2 §2.5 "Ownership").
    Held(LedgerBox),
}

impl PartialEq for PackedCoin {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl Eq for PackedCoin {}

/// A stored value that is not a coin.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeFailure {
    #[error("undecodable coin: {0}")]
    Malformed(ReadError),
    /// A coin followed by more bytes (CH2 §3.9 D7, rev2 n7).
    #[error("{0} bytes after the coin")]
    TrailingBytes(usize),
}

impl PackedCoin {
    /// `Coin(out, height, coinbase)`'s bytes (CS5a's `coin_bytes`). The height keeps its low 31
    /// bits, as Core's bitfield does.
    ///
    /// # Errors
    ///
    /// [`CoinsError::ValueRange`] for a null output (value −1), which Core's `AddCoin` asserts
    /// against.
    pub fn from_output(out: &TxOut, height: u32, coinbase: bool) -> Result<Self, CoinsError> {
        if out.value == -1 {
            return Err(CoinsError::ValueRange);
        }
        Ok(Self::from_coin(CoinRef::from_output(out, height, coinbase)))
    }

    /// The bytes of `coin`, written as Core writes the coin it holds ([`CoinRef::write`]): an undo
    /// entry restored by a disconnect goes through here, never by copying its bytes. Encodes into
    /// the inline buffer first, so an inline coin does not allocate.
    ///
    /// # Panics
    ///
    /// If `coin` is spent, as Core's `Coin::Serialize` asserts. Decoded coins never are.
    pub fn from_coin(coin: CoinRef<'_>) -> Self {
        let mut inline = ArrayWriter::<INLINE_MAX>::new();
        write_coin(&coin, &mut inline);
        match inline.as_slice() {
            Some(bytes) => Self::inline(bytes),
            None => {
                let mut long = Vec::with_capacity(inline.written());
                write_coin(&coin, &mut long);
                Self(Repr::Long(long.into_boxed_slice()))
            }
        }
    }

    fn inline(bytes: &[u8]) -> Self {
        let mut buf = [0; INLINE_MAX];
        buf[..bytes.len()].copy_from_slice(bytes);
        Self(Repr::Inline {
            len: bytes.len() as u8,
            bytes: buf,
        })
    }

    /// Bytes a coins cache layer or the database holds, already known to decode: they came from
    /// a `PackedCoin`.
    pub(crate) fn from_trusted(bytes: &[u8]) -> Self {
        debug_assert!(CoinRef::decode(&mut &bytes[..]).is_ok());
        if bytes.len() <= INLINE_MAX {
            Self::inline(bytes)
        } else {
            Self(Repr::Long(bytes.into()))
        }
    }

    /// A long coin's bytes, already known to decode, taken over without a copy.
    pub(crate) fn from_long(bytes: Box<[u8]>) -> Self {
        debug_assert!(bytes.len() > INLINE_MAX);
        Self(Repr::Long(bytes))
    }

    /// A long coin's box, taken over without a copy; any other coin comes back as it is.
    pub(crate) fn into_long(self) -> Result<Box<[u8]>, Self> {
        match self.0 {
            Repr::Long(bytes) => Ok(bytes),
            repr => Err(Self(repr)),
        }
    }

    /// This coin as an undo coin: a long coin's bytes move into a [`LedgerBox`] counted in
    /// `ledger`; an inline coin stays a value.
    pub(crate) fn held(self, ledger: &HeldOut) -> Self {
        match self.0 {
            Repr::Long(bytes) => Self(Repr::Held(LedgerBox::new(bytes, ledger))),
            repr => Self(repr),
        }
    }

    /// A value read from the `coins` column family, validated by [`CoinRef::decode`], which must
    /// consume all of it.
    ///
    /// # Errors
    ///
    /// [`DecodeFailure`] when the bytes are not exactly one coin. Core reads an undecodable value
    /// as no coin (`dbwrapper.h:314-316 / :324-326`) and a coin with trailing bytes as that coin;
    /// the coins table turns both into a corruption abort instead (DEC-78, CH2 §3.9 D7).
    pub fn from_db(bytes: &[u8]) -> Result<Self, DecodeFailure> {
        let mut rest = bytes;
        CoinRef::decode(&mut rest).map_err(DecodeFailure::Malformed)?;
        if !rest.is_empty() {
            return Err(DecodeFailure::TrailingBytes(rest.len()));
        }
        Ok(Self::from_trusted(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        match &self.0 {
            Repr::Inline {
                len,
                bytes,
            } => &bytes[..usize::from(*len)],
            Repr::Long(bytes) => bytes,
            Repr::Held(held) => held.as_bytes(),
        }
    }

    /// The coin, borrowed from its bytes.
    pub fn as_ref(&self) -> CoinRef<'_> {
        decode_trusted(self.as_bytes())
    }

    /// Whether the bytes are kept inline (≤ [`INLINE_MAX`]).
    pub fn is_inline(&self) -> bool {
        matches!(self.0, Repr::Inline { .. })
    }

    /// Heap bytes this value owns beyond itself.
    pub fn heap_bytes(&self) -> usize {
        match &self.0 {
            Repr::Inline {
                ..
            } => 0,
            Repr::Long(bytes) => bytes.len(),
            Repr::Held(held) => held.as_bytes().len(),
        }
    }

    /// Whether the bytes are counted in a ledger's `held_out` ([`PackedCoin::held`]).
    pub fn is_held(&self) -> bool {
        matches!(self.0, Repr::Held(_))
    }
}

// Core: coins.h:61 Coin::Serialize; compressor.h:113 TxOutCompression
/// [`CoinRef::write`], written here for [`PackedCoin::from_coin`]: `CoinRef::write` is an upstream
/// generic, compiled into a codegen unit of its own, so whether `from_coin` inlines it depends on
/// how this crate's code is partitioned (CH2b c6's PF0 lost it, 7 more instructions per P2PKH
/// encode and 12 per long one). The script goes through [`write_script`] for the same reason. A
/// test pins the two encodings together.
///
/// # Panics
///
/// If `coin` is spent, as [`CoinRef::write`] does.
#[inline]
fn write_coin(coin: &CoinRef<'_>, w: &mut impl CoreWrite) {
    assert!(!coin.is_spent(), "Coin::Serialize: the coin is spent");
    write_varint(u64::from(coin.height() * 2 + u32::from(coin.is_coinbase())), w);
    write_varint(compress_amount(coin.value() as u64), w);
    write_script(&coin.script_pubkey(), w);
}

// Core: compressor.h:65 ScriptCompression::Ser
/// [`ScriptRef::write_compressed`], written here for [`write_coin`]: it is an upstream generic too,
/// and CH2b b4's PF0 lost its inline (12 more instructions per P2PKH encode, 22 per long one). Every
/// special form goes through one write of its code and bytes: with two write sites (upstream's
/// shape) `from_coin` kept one more store per encode, and with the raw writes merged six more.
#[inline]
fn write_script(script: &ScriptRef<'_>, w: &mut impl CoreWrite) {
    let (code, data) = if let Some(script) = script.as_script() {
        match compress_script(script) {
            Some(special) => special,
            None => return write_raw_script(script, w),
        }
    } else if let Some((code, data)) = script.as_special()
        && (code < 4
            || decompress_script_into(u32::from(code), data, &mut [0; MAX_SPECIAL_SCRIPT_LEN])
                .is_some())
    {
        (code, data)
    } else {
        // Only a code 4 or 5 whose x coordinate is off the curve gets here (a `ScriptRef` is always
        // one of the two forms): Core read it as an empty script.
        return write_raw_script(&[], w);
    };
    w.write(&[code]);
    w.write(data);
}

/// A script `compress_script` has no special form for: `VARINT(len + 6)` and the script.
#[inline]
fn write_raw_script(script: &[u8], w: &mut impl CoreWrite) {
    write_varint(u64::from((script.len() as u32).wrapping_add(SPECIAL_SCRIPTS)), w);
    w.write(script);
}

impl std::fmt::Debug for PackedCoin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let coin = self.as_ref();
        f.debug_struct("PackedCoin")
            .field("value", &coin.value())
            .field("height", &coin.height())
            .field("coinbase", &coin.is_coinbase())
            .field("len", &self.as_bytes().len())
            .finish()
    }
}

/// Decodes bytes a [`PackedCoin`] or a cache layer holds: they always decode.
pub(crate) fn decode_trusted(bytes: &[u8]) -> CoinRef<'_> {
    match CoinRef::decode(&mut &bytes[..]) {
        Ok(coin) => coin,
        Err(e) => unreachable!("cached coin bytes do not decode: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash_consensus::coins::{Coin, MAX_COIN_HEIGHT};
    use dash_consensus::primitives::TxOut;

    fn out(value: i64, script: &[u8]) -> TxOut {
        TxOut {
            value,
            script_pubkey: script.to_vec().into(),
        }
    }

    fn p2pkh() -> Vec<u8> {
        [&[0x76, 0xa9, 20][..], &[0x11; 20], &[0x88, 0xac]].concat()
    }

    fn p2sh() -> Vec<u8> {
        [&[0xa9, 20][..], &[0x22; 20], &[0x87]].concat()
    }

    /// The worst P2PKH and P2SH coins (a height code just below 2²⁷, an amount whose
    /// compression needs every bit) are inline; the bound is tight to 33 bytes.
    #[test]
    fn every_p2pkh_and_p2sh_coin_is_inline() {
        const MAX_MONEY: i64 = 21_000_000 * 100_000_000;
        let heights = [0, 1, 127, 128, (1 << 26) - 1, (1 << 26), (1 << 27) - 1];
        let values = [0, 1, 99_999_999, 123_456_789_012_345, MAX_MONEY - 1, MAX_MONEY];
        let mut longest = 0;
        for script in [p2pkh(), p2sh()] {
            for h in heights {
                for v in values {
                    for cb in [false, true] {
                        let c = PackedCoin::from_output(&out(v, &script), h, cb).unwrap();
                        assert!(c.is_inline(), "{h} {v}");
                        longest = longest.max(c.as_bytes().len());
                        assert_eq!(c.as_ref().value(), v);
                        assert_eq!(c.as_ref().height(), h);
                        assert_eq!(c.as_ref().is_coinbase(), cb);
                    }
                }
            }
        }
        assert_eq!(longest, 33);
    }

    #[test]
    fn long_coins_round_trip_and_match_cores_bytes() {
        for script in [
            vec![0x51; 35],
            vec![0x6a; 0],
            vec![0x00; 520],
            [&[33, 0x02][..], &[7; 32], &[0xac]].concat(),
        ] {
            let o = out(5_000, &script);
            let c = PackedCoin::from_output(&o, 1_000, false).unwrap();
            let core = Coin::from_output(&o, 1_000, false).serialize();
            assert_eq!(c.as_bytes(), &core[..]);
            assert_eq!(c.is_inline(), core.len() <= INLINE_MAX);
            assert_eq!(
                c.heap_bytes(),
                if c.is_inline() {
                    0
                } else {
                    core.len()
                }
            );
            assert_eq!(PackedCoin::from_coin(c.as_ref()), c);
            assert_eq!(PackedCoin::from_db(&core).unwrap(), c);
        }
    }

    /// Core's amount compression wraps for amounts far past the money range (from about 2^64/9 for
    /// an amount with no trailing zero, and for most negative ones), and a script over
    /// `MAX_SCRIPT_SIZE` decodes to a short
    /// unspendable one, so such a coin re-encodes to other bytes: exactly the bytes CS5a
    /// re-encodes it to (`packed_coin_roundtrip`'s rule).
    #[test]
    fn coins_that_do_not_round_trip_re_encode_as_cs5a_does() {
        let unspendable = vec![0x51; 10_001];
        for (v, script) in
            [(1 << 62, p2pkh()), (i64::MAX, p2pkh()), (-2, p2pkh()), (5, unspendable)]
        {
            let c = PackedCoin::from_output(&out(v, &script), 7, false).unwrap();
            let mut again = Vec::new();
            c.as_ref().write(&mut again);
            assert_ne!(again, c.as_bytes(), "{v}");
            assert_eq!(PackedCoin::from_coin(c.as_ref()).as_bytes(), &again[..], "{v}");
        }
    }

    /// `write_coin` writes [`CoinRef::write`]'s bytes: over heights, amounts in and past the money
    /// range and both coinbase flags. The coins come from outputs (P2PKH, P2SH, compressed P2PK
    /// with codes 2 and 3, uncompressed P2PK with codes 4 and 5, an uncompressed key off the curve
    /// and raw scripts) and from decoded bytes (codes 4 and 5 on and off the curve).
    #[test]
    fn write_coin_is_coin_ref_write() {
        const MAX_MONEY: i64 = 21_000_000 * 100_000_000;
        // The generator's x coordinate, on the curve; 0xff…ff is not.
        let gx = [
            0x79, 0xbe, 0x66, 0x7e, 0xf9, 0xdc, 0xbb, 0xac, 0x55, 0xa0, 0x62, 0x95, 0xce, 0x87,
            0x0b, 0x07, 0x02, 0x9b, 0xfc, 0xdb, 0x2d, 0xce, 0x28, 0xd9, 0x59, 0xf2, 0x81, 0x5b,
            0x16, 0xf8, 0x17, 0x98,
        ];
        // The generator's y (even, code 4) and the negated generator's y (odd, code 5).
        let gy = [
            0x48, 0x3a, 0xda, 0x77, 0x26, 0xa3, 0xc4, 0x65, 0x5d, 0xa4, 0xfb, 0xfc, 0x0e, 0x11,
            0x08, 0xa8, 0xfd, 0x17, 0xb4, 0x48, 0xa6, 0x85, 0x54, 0x19, 0x9c, 0x47, 0xd0, 0x8f,
            0xfb, 0x10, 0xd4, 0xb8,
        ];
        let neg_gy = [
            0xb7, 0xc5, 0x25, 0x88, 0xd9, 0x5c, 0x3b, 0x9a, 0xa2, 0x5b, 0x04, 0x03, 0xf1, 0xee,
            0xf7, 0x57, 0x02, 0xe8, 0x4b, 0xb7, 0x59, 0x7a, 0xab, 0xe6, 0x63, 0xb8, 0x2f, 0x6f,
            0x04, 0xef, 0x27, 0x77,
        ];
        let scripts = [
            p2pkh(),
            p2sh(),
            [&[33, 0x02][..], &gx, &[0xac]].concat(),
            [&[33, 0x03][..], &gx, &[0xac]].concat(),
            [&[65, 0x04][..], &gx, &gy, &[0xac]].concat(),
            [&[65, 0x04][..], &gx, &neg_gy, &[0xac]].concat(),
            [&[65, 0x04][..], &gx, &[0xff; 32], &[0xac]].concat(),
            vec![],
            vec![0x51; 35],
            vec![0x6a; 520],
            vec![0x51; 10_001],
        ];
        let codes: Vec<_> =
            scripts[2..7].iter().map(|s| compress_script(s).map(|(c, _)| c)).collect();
        assert_eq!(codes, [Some(2), Some(3), Some(4), Some(5), None]);
        let heights = [0, 1, 127, 128, (1 << 27) - 1, MAX_COIN_HEIGHT, u32::MAX];
        let values = [0, 1, 99_999_999, MAX_MONEY, 1 << 62, i64::MAX, -2, i64::MIN];
        let check = |coin: CoinRef<'_>| {
            let (mut want, mut got) = (Vec::new(), Vec::new());
            coin.write(&mut want);
            write_coin(&coin, &mut got);
            assert_eq!(got, want, "{coin:?}");
            want
        };
        for script in &scripts {
            for h in heights {
                for v in values {
                    for cb in [false, true] {
                        let o = out(v, script);
                        let bytes = check(CoinRef::from_output(&o, h, cb));
                        check(CoinRef::decode(&mut &bytes[..]).unwrap());
                    }
                }
            }
        }
        for code in [4, 5] {
            for x in [gx, [0xff; 32]] {
                let bytes = [&[0x02, 0x00, code][..], &x].concat();
                check(CoinRef::decode(&mut &bytes[..]).unwrap());
            }
        }
    }

    #[test]
    #[should_panic(expected = "Coin::Serialize: the coin is spent")]
    fn write_coin_asserts_a_spent_coin_as_coin_ref_write_does() {
        write_coin(&CoinRef::from_output(&out(-1, &p2pkh()), 1, false), &mut Vec::new());
    }

    #[test]
    fn a_null_output_is_out_of_range() {
        assert!(matches!(
            PackedCoin::from_output(&out(-1, &p2pkh()), 1, false),
            Err(CoinsError::ValueRange)
        ));
    }

    #[test]
    fn from_db_rejects_anything_but_exactly_one_coin() {
        assert!(matches!(
            PackedCoin::from_db(&[0x02, 0x09, 0x07]),
            Err(DecodeFailure::Malformed(_))
        ));
        assert!(matches!(PackedCoin::from_db(&[]), Err(DecodeFailure::Malformed(_))));
        let core = Coin::new(7, &[0x51], 3, false).serialize();
        assert_eq!(PackedCoin::from_db(&core).unwrap().as_bytes(), &core[..]);
        let mut trailing = core.clone();
        trailing.extend([0xee; 4]);
        assert_eq!(PackedCoin::from_db(&trailing), Err(DecodeFailure::TrailingBytes(4)));
    }
}
