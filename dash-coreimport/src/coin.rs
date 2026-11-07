//! Core's `Coin` serialization and `CBlockUndo`.
//!
//! A coin is stored as `VARINT(height·2 + coinbase) ‖ VARINT(CompressAmount(value)) ‖
//! ScriptCompression(script)` (`coins.h` `Coin::Serialize`, `compressor.h`). Undo records use the
//! same layout with an extra dummy `VARINT` after the code when the height is non-zero
//! (`undo.h` `TxInUndoFormatter`).

use dashd_store::codec::write_varint;

use crate::error::{Error, Result};
use crate::tx::Cursor;

/// Core's `MAX_SCRIPT_SIZE`.
pub const MAX_SCRIPT_SIZE: usize = 10_000;
const OP_RETURN: u8 = 0x6a;
const OP_DUP: u8 = 0x76;
const OP_HASH160: u8 = 0xa9;
const OP_EQUAL: u8 = 0x87;
const OP_EQUALVERIFY: u8 = 0x88;
const OP_CHECKSIG: u8 = 0xac;
/// Script compression codes 0–5 are the special forms (`ScriptCompression::nSpecialScripts`).
const SPECIAL_SCRIPTS: u64 = 6;

/// Whether Core leaves the output out of the UTXO set (`CScript::IsUnspendable`).
// Core: script/script.h CScript::IsUnspendable
pub fn is_unspendable(script: &[u8]) -> bool {
    script.first() == Some(&OP_RETURN) || script.len() > MAX_SCRIPT_SIZE
}

/// Core's `CompressAmount`, wrapping on overflow like Core's `uint64_t` arithmetic (only a
/// corrupt amount overflows).
// Core: compressor.cpp CompressAmount
pub fn compress_amount(mut n: u64) -> u64 {
    if n == 0 {
        return 0;
    }
    let mut e = 0;
    while n.is_multiple_of(10) && e < 9 {
        n /= 10;
        e += 1;
    }
    use std::num::Wrapping as W;
    if e < 9 {
        let d = n % 10;
        n /= 10;
        (W(1) + (W(n) * W(9) + W(d) - W(1)) * W(10) + W(e)).0
    } else {
        (W(1) + (W(n) - W(1)) * W(10) + W(9)).0
    }
}

/// The kind of script compression a coin uses, for reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ScriptKind {
    P2pkh,
    P2sh,
    P2pkCompressed,
    P2pkUncompressed,
    Raw,
}

/// Appends Core's `ScriptCompression` of `script` and returns which form was used.
// Core: compressor.cpp CompressScript, IsToKeyID, IsToScriptID, IsToPubKey
pub fn compress_script(script: &[u8], out: &mut Vec<u8>) -> ScriptKind {
    let s = script;
    if s.len() == 25
        && s[0] == OP_DUP
        && s[1] == OP_HASH160
        && s[2] == 20
        && s[23] == OP_EQUALVERIFY
        && s[24] == OP_CHECKSIG
    {
        out.push(0x00);
        out.extend_from_slice(&s[3..23]);
        return ScriptKind::P2pkh;
    }
    if s.len() == 23 && s[0] == OP_HASH160 && s[1] == 20 && s[22] == OP_EQUAL {
        out.push(0x01);
        out.extend_from_slice(&s[2..22]);
        return ScriptKind::P2sh;
    }
    if s.len() == 35 && s[0] == 33 && s[34] == OP_CHECKSIG && (s[1] == 0x02 || s[1] == 0x03) {
        out.extend_from_slice(&s[1..34]);
        return ScriptKind::P2pkCompressed;
    }
    // An uncompressed key is only compressible when it is a valid curve point
    // (`CPubKey::IsFullyValid`).
    if s.len() == 67
        && s[0] == 65
        && s[66] == OP_CHECKSIG
        && s[1] == 0x04
        && secp256k1::PublicKey::from_slice(&s[1..66]).is_ok()
    {
        out.push(0x04 | (s[65] & 1));
        out.extend_from_slice(&s[2..34]);
        return ScriptKind::P2pkUncompressed;
    }
    write_varint(out, script.len() as u64 + SPECIAL_SCRIPTS);
    out.extend_from_slice(script);
    ScriptKind::Raw
}

/// Core's `Coin` serialization of an output created at `height`.
// Core: coins.h Coin::Serialize; compressor.h TxOutCompression
pub fn coin_bytes(
    height: u32,
    coinbase: bool,
    value: i64,
    script: &[u8],
    out: &mut Vec<u8>,
) -> ScriptKind {
    write_varint(out, u64::from(height) * 2 + u64::from(coinbase));
    // Core casts the (non-negative, consensus-checked) CAmount to uint64_t.
    write_varint(out, compress_amount(value as u64));
    compress_script(script, out)
}

/// Skips one `ScriptCompression` and returns its kind.
fn skip_compressed_script(c: &mut Cursor) -> Result<ScriptKind> {
    let code = u64::from(c.varint_u32()?);
    let (len, kind) = match code {
        0 => (20, ScriptKind::P2pkh),
        1 => (20, ScriptKind::P2sh),
        2 | 3 => (32, ScriptKind::P2pkCompressed),
        4 | 5 => (32, ScriptKind::P2pkUncompressed),
        n => ((n - SPECIAL_SCRIPTS) as usize, ScriptKind::Raw),
    };
    c.bytes(len)?;
    Ok(kind)
}

/// One spent coin from an undo record, in `Coin` layout (the dummy removed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoCoin {
    pub bytes: Vec<u8>,
    pub kind: ScriptKind,
}

/// Parses a `CBlockUndo`: one entry per non-coinbase transaction, each with one coin per input.
// Core: undo.h CBlockUndo, CTxUndo, TxInUndoFormatter
pub fn read_block_undo(bytes: &[u8]) -> Result<Vec<Vec<UndoCoin>>> {
    let mut c = Cursor::new(bytes);
    let n_tx = c.compact_size()?;
    // A `CTxUndo` is at least its 1-byte count, a coin at least 3 bytes (code, amount, script).
    let mut txs = Vec::with_capacity(c.capacity_for(n_tx, 1));
    for _ in 0..n_tx {
        let n_in = c.compact_size()?;
        let mut coins = Vec::with_capacity(c.capacity_for(n_in, 3));
        for _ in 0..n_in {
            let code_start = c.position();
            let code = c.varint_u32()?;
            let code_end = c.position();
            if code >> 1 > 0 {
                c.varint_u32()?; // the dummy version
            }
            let rest_start = c.position();
            c.varint()?; // compressed amount
            let kind = skip_compressed_script(&mut c)?;
            let mut coin = bytes[code_start..code_end].to_vec();
            coin.extend_from_slice(&bytes[rest_start..c.position()]);
            coins.push(UndoCoin {
                bytes: coin,
                kind,
            });
        }
        txs.push(coins);
    }
    if !c.remaining().is_empty() {
        return Err(Error::malformed(
            "block undo",
            format!("{} trailing bytes", c.remaining().len()),
        ));
    }
    Ok(txs)
}

/// Serializes a `CBlockUndo`: one entry per non-coinbase transaction (in block order), each with
/// the coins its inputs spent, given in `Coin` layout (as [`coin_bytes`] writes them). The
/// inverse of [`read_block_undo`].
// Core: undo.h CBlockUndo, CTxUndo, TxInUndoFormatter::Ser
pub fn write_block_undo<'a, T, C>(txs: T, out: &mut Vec<u8>) -> Result<()>
where
    T: ExactSizeIterator<Item = C>,
    C: ExactSizeIterator<Item = &'a [u8]>,
{
    write_compact_size(out, txs.len() as u64);
    for coins in txs {
        write_compact_size(out, coins.len() as u64);
        for coin in coins {
            let mut rest = coin;
            let code = dashd_store::codec::read_varint_u32(&mut rest)
                .map_err(|e| Error::malformed("coin", e.to_string()))?;
            out.extend_from_slice(&coin[..coin.len() - rest.len()]);
            if code >> 1 > 0 {
                // The dummy version byte older undo formats carried.
                out.push(0);
            }
            out.extend_from_slice(rest);
        }
    }
    Ok(())
}

/// Core's `WriteCompactSize`.
// Core: serialize.h WriteCompactSize
fn write_compact_size(out: &mut Vec<u8>, n: u64) {
    match n {
        0..=252 => out.push(n as u8),
        253..=0xffff => {
            out.push(253);
            out.extend((n as u16).to_le_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(254);
            out.extend((n as u32).to_le_bytes());
        }
        _ => {
            out.push(255);
            out.extend(n.to_le_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_undo_round_trips() {
        let mut a = Vec::new();
        coin_bytes(0, false, 1, &[0x51], &mut a);
        let mut b = Vec::new();
        coin_bytes(100_000, true, 50 * 100_000_000, &[OP_RETURN + 1; 300], &mut b);
        let txs: Vec<Vec<&[u8]>> = vec![vec![], vec![&a, &b], vec![&b]];
        let mut undo = Vec::new();
        write_block_undo(txs.iter().map(|t| t.iter().copied()), &mut undo).unwrap();
        let back = read_block_undo(&undo).unwrap();
        let back: Vec<Vec<Vec<u8>>> =
            back.into_iter().map(|t| t.into_iter().map(|c| c.bytes).collect()).collect();
        assert_eq!(back, vec![vec![], vec![a.clone(), b.clone()], vec![b]]);
        // Height 0: no dummy byte after the code.
        assert_eq!(&undo[..4], &[3, 0, 2, a[0]]);
        assert_eq!(undo[4], a[1]);
    }

    /// Core's `compress_amounts` test points (`src/test/compress_tests.cpp`).
    #[test]
    fn amounts_match_core() {
        const COIN: u64 = 100_000_000;
        const CENT: u64 = 1_000_000;
        assert_eq!(compress_amount(0), 0x0);
        assert_eq!(compress_amount(1), 0x1);
        assert_eq!(compress_amount(CENT), 0x7);
        assert_eq!(compress_amount(COIN), 0x9);
        assert_eq!(compress_amount(50 * COIN), 0x32);
        assert_eq!(compress_amount(21_000_000 * COIN), 0x1406f40);
        // A corrupt amount wraps as Core's `uint64_t` arithmetic does, instead of panicking.
        let n = u64::MAX;
        let wrapped = ((u128::from(n / 10) * 9 + u128::from(n % 10) - 1) * 10 + 1) as u64;
        assert_eq!(compress_amount(n), wrapped);
    }

    fn compress(script: &[u8]) -> (Vec<u8>, ScriptKind) {
        let mut out = Vec::new();
        let kind = compress_script(script, &mut out);
        (out, kind)
    }

    /// Core's `compress_script_to_*` tests (`src/test/compress_tests.cpp`).
    #[test]
    fn scripts_match_core() {
        let mut p2pkh = vec![OP_DUP, OP_HASH160, 20];
        p2pkh.extend([0u8; 20]);
        p2pkh.extend([OP_EQUALVERIFY, OP_CHECKSIG]);
        let (out, kind) = compress(&p2pkh);
        assert_eq!((out.len(), out[0], kind), (21, 0x00, ScriptKind::P2pkh));

        let mut p2sh = vec![OP_HASH160, 20];
        p2sh.extend([0u8; 20]);
        p2sh.push(OP_EQUAL);
        let (out, kind) = compress(&p2sh);
        assert_eq!((out.len(), out[0], kind), (21, 0x01, ScriptKind::P2sh));

        // The secp256k1 generator, compressed and uncompressed.
        let gx = "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
        let gy = "483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8";
        let unhex = |s: &str| -> Vec<u8> {
            (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
        };
        let mut p2pk = vec![33, 0x02];
        p2pk.extend(unhex(gx));
        p2pk.push(OP_CHECKSIG);
        let (out, kind) = compress(&p2pk);
        assert_eq!((out.len(), out[0], kind), (33, 0x02, ScriptKind::P2pkCompressed));
        assert_eq!(&out[1..], &p2pk[2..34]);

        let mut p2pk_u = vec![65, 0x04];
        p2pk_u.extend(unhex(gx));
        p2pk_u.extend(unhex(gy));
        p2pk_u.push(OP_CHECKSIG);
        let (out, kind) = compress(&p2pk_u);
        // gy is even.
        assert_eq!((out.len(), out[0], kind), (33, 0x04, ScriptKind::P2pkUncompressed));

        // An off-curve "uncompressed key" stays raw.
        let mut bad = p2pk_u.clone();
        bad[40] ^= 1;
        let (out, kind) = compress(&bad);
        assert_eq!((out[0] as usize, kind), (67 + 6, ScriptKind::Raw));
        assert_eq!(out.len(), 1 + 67);
    }

    #[test]
    fn unspendable() {
        assert!(is_unspendable(&[OP_RETURN, 1, 2]));
        assert!(is_unspendable(&vec![0; MAX_SCRIPT_SIZE + 1]));
        assert!(!is_unspendable(&vec![0; MAX_SCRIPT_SIZE]));
        assert!(!is_unspendable(&[]));
    }

    #[test]
    fn undo_drops_the_dummy() {
        // Two txs: one input at height 0 (no dummy), one at height 100 coinbase (dummy present).
        let mut undo = vec![2u8, 1];
        undo.extend([0x00, 0x09, 0x00]); // code 0, amount 0x09 (1 COIN), P2PKH
        undo.extend([0u8; 20]);
        undo.push(1);
        let mut code = Vec::new();
        write_varint(&mut code, 201);
        undo.extend(&code);
        undo.push(0x00); // dummy
        undo.extend([0x32, 0x07]); // 50 COIN, raw script of 1 byte
        undo.push(0x51);
        let txs = read_block_undo(&undo).unwrap();
        assert_eq!(txs.len(), 2);
        assert_eq!(txs[0][0].kind, ScriptKind::P2pkh);
        assert_eq!(txs[0][0].bytes.len(), 23);
        let mut want = code.clone();
        want.extend([0x32, 0x07, 0x51]);
        assert_eq!(txs[1][0].bytes, want);
        assert_eq!(txs[1][0].kind, ScriptKind::Raw);

        let mut built = Vec::new();
        coin_bytes(100, true, 50 * 100_000_000, &[0x51], &mut built);
        assert_eq!(built, want);
    }
}
