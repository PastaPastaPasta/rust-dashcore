//! `packed_coin_roundtrip` (CH2 §6.5): the coins cache's value, [`PackedCoin`], against CS5a's
//! coin codec, which `diff_coin_decode` checks against Dash Core. The value is the same under
//! every release.
//!
//! The first byte picks the input's shape:
//! - even: a transaction output, `value (8, LE) ‖ height (4, LE) ‖ coinbase (1) ‖ script`. Its
//!   cache value must be CS5a's `Coin` bytes, inline exactly when they fit 34 bytes, re-encode as
//!   CS5a re-encodes the coin it decodes, and, where Core's compression keeps both the amount and
//!   the script, decode to the output and re-encode to the same bytes. The amount comes back
//!   exactly when `DecompressAmount(CompressAmount(v)) == v` (all of `0..=MAX_MONEY` and beyond,
//!   until the arithmetic wraps), and the script whenever it is at most `MAX_SCRIPT_SIZE` bytes (a
//!   longer one decodes to a short unspendable script);
//! - odd: a stored value. `PackedCoin::from_db` must accept exactly the bytes CS5a decodes
//!   completely, keep them, and re-encode them as CS5a re-encodes the coin it read.

use dash_consensus::coins::compress::{MAX_SCRIPT_SIZE, compress_amount, decompress_amount};
use dash_consensus::coins::{Coin, CoinRef};
use dash_consensus::primitives::TxOut;
use dashd_chainstate::coins::{DecodeFailure, INLINE_MAX, PackedCoin};
use node_l1::Divergence;

/// The height field's 31 bits.
const HEIGHT_MASK: u32 = (1 << 31) - 1;

fn diverged(field: &'static str) -> Divergence {
    Divergence {
        field,
    }
}

fn ensure(ok: bool, field: &'static str) -> Result<(), Divergence> {
    if ok {
        Ok(())
    } else {
        Err(diverged(field))
    }
}

pub(super) fn check(data: &[u8]) -> Result<(), Divergence> {
    match data.split_first() {
        Some((mode, rest)) if mode & 1 == 0 => output(rest),
        Some((_, rest)) => stored(rest),
        None => Ok(()),
    }
}

fn output(data: &[u8]) -> Result<(), Divergence> {
    let Some((head, script)) = data.split_at_checked(13) else {
        return Ok(());
    };
    let value = i64::from_le_bytes(head[..8].try_into().unwrap_or_default());
    let height = u32::from_le_bytes(head[8..12].try_into().unwrap_or_default());
    let coinbase = head[12] & 1 == 1;
    let out = TxOut {
        value,
        script_pubkey: script.to_vec().into(),
    };
    let Ok(packed) = PackedCoin::from_output(&out, height, coinbase) else {
        return ensure(value == -1, "from_output error");
    };
    ensure(value != -1, "null output accepted")?;

    let core = Coin::from_output(&out, height, coinbase).serialize();
    ensure(packed.as_bytes() == core, "coin bytes")?;
    ensure(packed.is_inline() == (core.len() <= INLINE_MAX), "inline boundary")?;
    ensure(
        packed.heap_bytes()
            == if packed.is_inline() {
                0
            } else {
                core.len()
            },
        "heap bytes",
    )?;
    ensure(PackedCoin::from_db(&core).as_ref() == Ok(&packed), "from_db")?;

    let coin = packed.as_ref();
    let mut again = Vec::new();
    coin.write(&mut again);
    ensure(PackedCoin::from_coin(coin).as_bytes() == again, "re-encode")?;
    ensure(coin.height() == height & HEIGHT_MASK, "height")?;
    ensure(coin.is_coinbase() == coinbase, "coinbase")?;
    let amount_kept = decompress_amount(compress_amount(value as u64)) == value as u64;
    let script_kept = script.len() <= MAX_SCRIPT_SIZE;
    if amount_kept {
        ensure(coin.value() == value, "value")?;
    }
    if script_kept {
        ensure(coin.script_pubkey().to_script().as_slice() == script, "script")?;
    }
    if amount_kept && script_kept {
        ensure(again == core, "round trip")?;
    }
    Ok(())
}

fn stored(data: &[u8]) -> Result<(), Divergence> {
    let mut rest = data;
    let decoded = CoinRef::decode(&mut rest);
    match (PackedCoin::from_db(data), decoded) {
        (Ok(packed), Ok(coin)) if rest.is_empty() => {
            ensure(packed.as_bytes() == data, "stored bytes")?;
            ensure(packed.is_inline() == (data.len() <= INLINE_MAX), "inline boundary")?;
            ensure(packed.as_ref() == coin, "decoded coin")?;
            let mut again = Vec::new();
            coin.write(&mut again);
            ensure(PackedCoin::from_coin(packed.as_ref()).as_bytes() == again, "re-encode")
        }
        (Err(DecodeFailure::TrailingBytes(n)), Ok(_)) => {
            ensure(n == rest.len() && n > 0, "trailing bytes")
        }
        (Err(DecodeFailure::Malformed(e)), Err(f)) => ensure(e == f, "decode error"),
        _ => Err(diverged("from_db verdict")),
    }
}
