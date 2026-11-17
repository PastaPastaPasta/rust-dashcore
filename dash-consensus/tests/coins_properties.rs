//! Properties of the coin and undo codecs over generated wire data (CS5a).
//!
//! Output rules (CS0 §9.5): the generated seed's `Debug` is a placeholder, failures are not
//! persisted, there is no shrinking, and a failure names the property and the field only. When
//! `DASH_EMBARGO_DIR` is set, the seed goes to the private sink first.

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::use_debug,
    clippy::dbg_macro,
    clippy::print_stdout
)]

#[path = "support/coins_gen.rs"]
mod coins_gen;
#[path = "support/gen.rs"]
mod generate;

use dash_consensus::coins::compress::{
    MAX_SCRIPT_SIZE, compress_amount, decompress_amount, read_compressed_script,
    write_compressed_script,
};
use dash_consensus::coins::{BlockUndoRef, Coin, TxUndo, decode_block_undo};
use node_sink::sink::{OpaqueSeed, record_for_test};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;

use generate::{MAX_MONEY, Rng, mutated, proptest_config};

/// Fails the case naming `property` and `field`, after recording the seed privately.
fn fail(property: &'static str, field: &'static str, seed: OpaqueSeed) -> TestCaseError {
    let recorded = record_for_test(property, &seed.0.to_le_bytes(), "-", field);
    TestCaseError::fail(format!("{property}: {field} ({recorded})"))
}

fn check(ok: bool, field: &'static str) -> Result<(), &'static str> {
    if ok {
        Ok(())
    } else {
        Err(field)
    }
}

fn in_money_range(coin: &Coin) -> bool {
    (0..=MAX_MONEY).contains(&coin.value())
}

/// `Coin::Unserialize` of arbitrary bytes.
fn coin_properties(bytes: &[u8]) -> Result<(), &'static str> {
    let mut cursor = bytes;
    let Ok(coin) = Coin::decode(&mut cursor) else {
        check(cursor.len() == bytes.len(), "cursor moved on failure")?;
        return Ok(());
    };
    check(!coin.is_spent(), "decoded a spent coin")?;
    let serialized = coin.serialize();
    check(serialized.len() == coin.serialized_size(), "size")?;
    let mut again = &serialized[..];
    let decoded = Coin::decode(&mut again).map_err(|_| "re-decode")?;
    check(again.is_empty(), "re-decode consumed")?;
    check(decoded.height() == coin.height(), "height")?;
    check(decoded.is_coinbase() == coin.is_coinbase(), "coinbase")?;
    check(decoded.script_pubkey() == coin.script_pubkey(), "script")?;
    // Amount compression inverts only on the money range, so outside it neither Core's nor our
    // re-serialization is idempotent (the oracle comparisons check what such a value becomes).
    if in_money_range(&coin) {
        check(decoded == coin, "idempotence")?;
        check(decoded.serialize() == serialized, "idempotent bytes")?;
    }
    Ok(())
}

/// `CBlockUndo` (and `CTxUndo`) read from arbitrary bytes.
fn undo_properties(bytes: &[u8]) -> Result<(), &'static str> {
    let mut cursor = bytes;
    match TxUndo::decode(&mut cursor) {
        Ok(tx) => {
            let mut written = Vec::new();
            tx.write(&mut written);
            check(written.len() == tx.serialized_size(), "tx undo size")?;
        }
        Err(_) => check(cursor.len() == bytes.len(), "tx undo cursor moved on failure")?,
    }

    // The borrowed record decodes exactly where the owned one does, and writes the same bytes.
    let mut borrowed_cursor = bytes;
    let borrowed = BlockUndoRef::decode(&mut borrowed_cursor);
    let mut cursor = bytes;
    let Ok(decoded) = decode_block_undo(&mut cursor) else {
        check(cursor.len() == bytes.len(), "cursor moved on failure")?;
        check(borrowed.is_err(), "borrowed verdict")?;
        check(borrowed_cursor.len() == bytes.len(), "borrowed cursor moved on failure")?;
        return Ok(());
    };
    let borrowed = borrowed.map_err(|_| "borrowed verdict")?;
    check(borrowed_cursor.len() == cursor.len(), "borrowed consumed")?;
    check(borrowed.core_bytes() == decoded.value().core_bytes(), "borrowed core bytes")?;
    check(&borrowed.to_block_undo() == decoded.value(), "borrowed coins")?;
    let undo = decoded.value();
    let serialized = undo.core_bytes();
    check(serialized.len() == undo.serialized_size(), "size")?;
    check(cursor.len() == bytes.len() - decoded.wire().len(), "cursor")?;
    check(
        decoded.wire_is_canonical() == (serialized.as_slice() == decoded.wire()),
        "wire_is_canonical",
    )?;
    check(decoded.core_bytes() == serialized, "core bytes")?;
    if !undo.txs.iter().flat_map(|tx| &tx.coins).all(in_money_range) {
        return Ok(());
    }
    let mut again = serialized.as_slice();
    let again = decode_block_undo(&mut again).map_err(|_| "re-decode")?;
    check(again.wire_is_canonical(), "re-decode canonical")?;
    check(again.value().core_bytes() == serialized, "idempotence")?;
    Ok(())
}

/// Amount compression inverts on `0..=MAX_MONEY`, and decompression is inverted by compression
/// below 10^11, where `DecompressAmount` cannot overflow (Core's `compress_amounts` loops, over
/// random values).
fn amount_properties(rng: &mut Rng) -> Result<(), &'static str> {
    let amount = rng.below(MAX_MONEY as u64 + 1);
    check(decompress_amount(compress_amount(amount)) == amount, "amount round trip")?;
    let compressed = rng.below(100_000_000_000);
    check(compress_amount(decompress_amount(compressed)) == compressed, "compressed round trip")?;
    Ok(())
}

/// Any script up to `MAX_SCRIPT_SIZE` survives `ScriptCompression`.
fn script_properties(rng: &mut Rng) -> Result<(), &'static str> {
    let mut wire = Vec::new();
    coins_gen::compressed_script(rng, &mut wire);
    let Ok(script) = read_compressed_script(&mut &wire[..]) else {
        return Ok(());
    };
    check(script.len() <= MAX_SCRIPT_SIZE, "script size")?;
    let mut written = Vec::new();
    write_compressed_script(&script, &mut written);
    let mut cursor = &written[..];
    let again = read_compressed_script(&mut cursor).map_err(|_| "re-read")?;
    check(cursor.is_empty(), "re-read consumed")?;
    check(again == script, "script round trip")?;
    Ok(())
}

proptest! {
    #![proptest_config(proptest_config(1024))]

    #[test]
    fn coins(seed in any::<u64>().prop_map(OpaqueSeed)) {
        let bytes = mutated(seed.0, coins_gen::coin_bytes);
        coin_properties(&bytes).map_err(|field| fail("properties_coin", field, seed))?;
    }

    #[test]
    fn undo_records(seed in any::<u64>().prop_map(OpaqueSeed)) {
        let bytes = mutated(seed.0, coins_gen::block_undo_bytes);
        undo_properties(&bytes).map_err(|field| fail("properties_undo", field, seed))?;
        let bytes = mutated(seed.0, coins_gen::tx_undo_bytes);
        undo_properties(&bytes).map_err(|field| fail("properties_undo", field, seed))?;
    }

    #[test]
    fn compression(seed in any::<u64>().prop_map(OpaqueSeed)) {
        let mut rng = Rng::new(seed.0);
        amount_properties(&mut rng).map_err(|field| fail("properties_compression", field, seed))?;
        script_properties(&mut rng).map_err(|field| fail("properties_compression", field, seed))?;
    }
}
