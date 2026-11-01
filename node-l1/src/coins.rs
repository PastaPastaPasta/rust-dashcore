//! `diff_codec_primitives`, `diff_coin_decode` and `diff_undo_decode` (CS5a; A1 §4.4): the coins
//! database and undo codecs against Core's `oracle_coin_codec`, one comparison per operation.
//!
//! Every serialization compared here is the same at every pin, so each runs on whichever pin the
//! caller names.

use dash_consensus::coins::compress::{
    compress_amount, decompress_amount, read_compressed_script, write_compressed_script,
};
use dash_consensus::coins::{
    BlockUndoRef, Coin, CoinRef, TxUndo, TxUndoRef, coin_bytes, decode_block_undo,
};
use dash_consensus::primitives::TxOut;
use dash_consensus::primitives::canonical::write_output;
use dash_consensus::primitives::{
    read_compact_size, read_varint_u64, write_compact_size, write_varint,
};
use dash_oracle_sys::{CoinCodec, Oracle, Pin};

use crate::{Compared, Outcome, check, core_verdict, divergence, mismatched_verdicts};

/// Core's `MAX_MONEY`.
const MAX_MONEY: i64 = 21_000_000 * 100_000_000;

/// Core's answer to a decoding operation: its output and the bytes it consumed.
fn core_decode(op: CoinCodec, input: &[u8], pin: Pin) -> Result<(Vec<u8>, usize), bool> {
    core_verdict(Oracle::new(pin).coin_codec(op, input))
}

/// Core's answer to an encoding operation, which cannot fail on the inputs passed here.
fn core_encode(op: CoinCodec, input: &[u8], pin: Pin) -> Result<Vec<u8>, crate::Divergence> {
    Oracle::new(pin)
        .coin_codec(op, input)
        .map(|(out, _)| out)
        .map_err(|_| divergence("oracle error"))
}

fn u64_le(bytes: &[u8]) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.try_into().ok()?))
}

/// Runs one of our cursor readers over `input` the way the oracle's stream does: the value and
/// the bytes consumed.
fn ours<T, E>(
    input: &[u8],
    read: impl FnOnce(&mut &[u8]) -> Result<T, E>,
) -> Result<(T, usize), E> {
    let mut cursor = input;
    let value = read(&mut cursor)?;
    Ok((value, input.len() - cursor.len()))
}

/// Compares a decoding operation: the verdicts, then the consumed length and the decoded value
/// (as `same` judges it against Core's output).
fn compare_decode<T, E>(
    op: CoinCodec,
    input: &[u8],
    pin: Pin,
    read: impl FnOnce(&mut &[u8]) -> Result<T, E>,
    same: impl FnOnce(&T, &[u8]) -> Result<(), crate::Divergence>,
) -> Compared {
    match (core_decode(op, input, pin), ours(input, read)) {
        (Ok((core, core_consumed)), Ok((value, consumed))) => {
            check(consumed == core_consumed, "consumed")?;
            same(&value, &core)?;
            Ok(Outcome::BothAccept)
        }
        (core, ours) => mismatched_verdicts(core, ours),
    }
}

/// `ReadCompactSize` with its range check.
pub fn compare_compact_size_decode(input: &[u8], pin: Pin) -> Compared {
    compare_decode(
        CoinCodec::CompactSizeDecode,
        input,
        pin,
        |c| read_compact_size(c, true),
        |&n, core| check(u64_le(core) == Some(n), "compact size"),
    )
}

/// `WriteCompactSize`.
pub fn compare_compact_size_encode(n: u64, pin: Pin) -> Compared {
    let core = core_encode(CoinCodec::CompactSizeEncode, &n.to_le_bytes(), pin)?;
    let mut bytes = Vec::new();
    write_compact_size(n, &mut bytes);
    check(bytes == core, "compact size bytes")?;
    Ok(Outcome::BothAccept)
}

/// `VARINT` into a `uint64_t`.
pub fn compare_varint_decode(input: &[u8], pin: Pin) -> Compared {
    compare_decode(CoinCodec::VarIntDecode, input, pin, read_varint_u64, |&n, core| {
        check(u64_le(core) == Some(n), "varint")
    })
}

/// `VARINT` of a `uint64_t`.
pub fn compare_varint_encode(n: u64, pin: Pin) -> Compared {
    let core = core_encode(CoinCodec::VarIntEncode, &n.to_le_bytes(), pin)?;
    let mut bytes = Vec::new();
    write_varint(n, &mut bytes);
    check(bytes == core, "varint bytes")?;
    Ok(Outcome::BothAccept)
}

/// `CompressAmount`.
pub fn compare_amount_compress(n: u64, pin: Pin) -> Compared {
    let core = core_encode(CoinCodec::AmountCompress, &n.to_le_bytes(), pin)?;
    check(u64_le(&core) == Some(compress_amount(n)), "compressed amount")?;
    Ok(Outcome::BothAccept)
}

/// `DecompressAmount`.
pub fn compare_amount_decompress(n: u64, pin: Pin) -> Compared {
    let core = core_encode(CoinCodec::AmountDecompress, &n.to_le_bytes(), pin)?;
    check(u64_le(&core) == Some(decompress_amount(n)), "decompressed amount")?;
    Ok(Outcome::BothAccept)
}

/// `ScriptCompression` of a raw script.
pub fn compare_script_compress(script: &[u8], pin: Pin) -> Compared {
    let core = core_encode(CoinCodec::ScriptCompress, script, pin)?;
    let mut bytes = Vec::new();
    write_compressed_script(script, &mut bytes);
    check(bytes == core, "compressed script")?;
    Ok(Outcome::BothAccept)
}

/// Reading `ScriptCompression`.
pub fn compare_script_decompress(input: &[u8], pin: Pin) -> Compared {
    compare_decode(
        CoinCodec::ScriptDecompress,
        input,
        pin,
        read_compressed_script,
        |script, core| check(**script == *core, "decompressed script"),
    )
}

/// `CTxOut` serialization: what the oracle's coin operations carry.
fn txout_bytes(value: i64, script: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    write_output(value, script, &mut out);
    out
}

/// The oracle's coin layout: `u32 LE height | u8 coinbase | CTxOut`.
fn oracle_coin_input(height: u32, coinbase: bool, value: i64, script: &[u8]) -> Vec<u8> {
    let mut out = height.to_le_bytes().to_vec();
    out.push(u8::from(coinbase));
    out.extend(txout_bytes(value, script));
    out
}

fn oracle_coin(coin: &Coin) -> Vec<u8> {
    oracle_coin_input(coin.height(), coin.is_coinbase(), coin.value(), coin.script_pubkey())
}

/// `Coin::Serialize` of a coin built from these fields, through `Coin` and through `coin_bytes`
/// (the encoder of new coins). The height goes to Core unmasked. A spent coin (value −1) is
/// outside Core's domain (`Coin::Serialize` asserts), so it is not compared.
pub fn compare_coin_encode(
    height: u32,
    coinbase: bool,
    value: i64,
    script: &[u8],
    pin: Pin,
) -> Compared {
    if value == -1 {
        return Ok(Outcome::BothReject);
    }
    let core = core_encode(
        CoinCodec::CoinEncode,
        &oracle_coin_input(height, coinbase, value, script),
        pin,
    )?;
    check(Coin::new(value, script, height, coinbase).serialize() == core, "coin bytes")?;
    let out = TxOut {
        value,
        script_pubkey: script.to_vec().into(),
    };
    let mut bytes = Vec::new();
    coin_bytes(&out, height, coinbase, &mut bytes);
    check(bytes == core, "coin_bytes")?;
    Ok(Outcome::BothAccept)
}

/// `diff_coin_decode`: `Coin::Unserialize` of `input`. Compared: the verdict, the consumed
/// length, every field, and the re-serializations (owned and borrowed) against Core's
/// `Coin::Serialize` of its own decoded coin. Decoding never yields a spent coin, so Core's
/// encoder always accepts it.
pub fn compare_coin_decode(input: &[u8], pin: Pin) -> Compared {
    let outcome = compare_decode(CoinCodec::CoinDecode, input, pin, Coin::decode, |coin, core| {
        check(oracle_coin(coin) == core, "coin")
    })?;
    if outcome != Outcome::BothAccept {
        return Ok(outcome);
    }
    let view = CoinRef::decode(&mut &input[..]).map_err(|_| divergence("coin ref decode"))?;
    let coin = view.to_coin();
    let core = core_encode(CoinCodec::CoinEncode, &oracle_coin(&coin), pin)?;
    check(coin.serialize() == core, "coin re-encoding")?;
    // The borrowed decoder writes the same bytes without expanding a special form.
    let mut bytes = Vec::new();
    view.write(&mut bytes);
    check(bytes == core, "coin ref re-encoding")?;
    check(view.serialized_size() == core.len(), "coin ref size")?;
    // Database bytes to an undo entry: Core reads the entry back unchanged. A round trip is the
    // identity only for amounts in the money range, which every real coin has; the amount is
    // written by the same code as the database value, compared above for every amount.
    if (0..=MAX_MONEY).contains(&view.value()) {
        let mut entry = vec![1u8];
        view.write_undo(&mut entry);
        check(entry.len() == view.undo_size() + 1, "coin ref undo size")?;
        let (core_entry, _) = core_decode(CoinCodec::TxUndoRoundtrip, &entry, pin)
            .map_err(|_| divergence("coin ref undo entry"))?;
        check(core_entry == entry, "coin ref undo entry")?;
    }
    Ok(outcome)
}

/// `CTxUndo` read and written back.
pub fn compare_tx_undo(input: &[u8], pin: Pin) -> Compared {
    compare_decode(CoinCodec::TxUndoRoundtrip, input, pin, TxUndo::decode, |undo, core| {
        let mut bytes = Vec::new();
        undo.write(&mut bytes);
        check(bytes == core, "tx undo re-encoding")?;
        check(undo.serialized_size() == core.len(), "tx undo size")?;
        // The borrowed record writes the same bytes.
        let borrowed = TxUndoRef::decode(&mut &input[..])
            .map_err(|_| divergence("borrowed tx undo decode"))?;
        let mut entries = Vec::new();
        borrowed.write(&mut entries);
        check(entries == core, "borrowed tx undo re-encoding")
    })
}

/// `diff_undo_decode`: `CBlockUndo` read and written back. Compared: the verdict, the consumed
/// length, the re-encoding, `wire_is_canonical` against a byte comparison, and the rev-file
/// bytes. The same input is also read as one `CTxUndo`.
pub fn compare_block_undo(input: &[u8], pin: Pin) -> Compared {
    let core = core_decode(CoinCodec::BlockUndoRoundtrip, input, pin);
    let mut cursor = input;
    let outcome = match (core, decode_block_undo(&mut cursor)) {
        (Ok((core, consumed)), Ok(decoded)) => {
            let undo = decoded.value();
            let wire = decoded.wire();
            check(wire.len() == consumed, "consumed")?;
            let bytes = undo.core_bytes();
            check(bytes.as_slice() == core, "block undo re-encoding")?;
            check(undo.serialized_size() == core.len(), "block undo size")?;
            check(decoded.wire_is_canonical() == (core == wire), "wire_is_canonical")?;
            check(decoded.core_bytes().as_slice() == core, "core undo bytes")?;
            // The borrowed record reads as much and writes the same bytes.
            let mut cursor = input;
            let borrowed = BlockUndoRef::decode(&mut cursor)
                .map_err(|_| divergence("borrowed block undo decode"))?;
            check(input.len() - cursor.len() == consumed, "borrowed consumed")?;
            check(borrowed.core_bytes().as_slice() == core, "borrowed block undo re-encoding")?;
            check(borrowed.serialized_size() == core.len(), "borrowed block undo size")?;
            Outcome::BothAccept
        }
        (core, decoded) => mismatched_verdicts(core, decoded)?,
    };
    compare_tx_undo(input, pin)?;
    Ok(outcome)
}

/// The first `N` bytes of `bytes`, zero-padded.
fn le<const N: usize>(bytes: &[u8]) -> [u8; N] {
    let mut out = [0u8; N];
    let n = bytes.len().min(N);
    out[..n].copy_from_slice(&bytes[..n]);
    out
}

/// `diff_codec_primitives`: the first byte picks one of the oracle's twelve operations, and the
/// rest is its input. Integer operations take the next 8 bytes (zero-padded) as a little-endian
/// `u64`; coin encoding takes a `u32` height, a coinbase byte and an `i64` value the same way,
/// and the rest as the script.
pub fn compare_codec_primitives(bytes: &[u8], pin: Pin) -> Compared {
    let Some((&selector, input)) = bytes.split_first() else {
        return Ok(Outcome::BothReject);
    };
    let n = u64::from_le_bytes(le(input));
    match selector % 12 {
        0 => compare_compact_size_decode(input, pin),
        1 => compare_compact_size_encode(n, pin),
        2 => compare_varint_decode(input, pin),
        3 => compare_varint_encode(n, pin),
        4 => compare_amount_compress(n, pin),
        5 => compare_amount_decompress(n, pin),
        6 => compare_script_compress(input, pin),
        7 => compare_script_decompress(input, pin),
        8 => {
            let height = u32::from_le_bytes(le(input));
            let coinbase = input.get(4).is_some_and(|b| b & 1 == 1);
            let value = i64::from_le_bytes(le(input.get(5..).unwrap_or_default()));
            let script = input.get(13..).unwrap_or_default();
            compare_coin_encode(height, coinbase, value, script, pin)
        }
        9 => compare_coin_decode(input, pin),
        10 => compare_tx_undo(input, pin),
        _ => compare_block_undo(input, pin),
    }
}
