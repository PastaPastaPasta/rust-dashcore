//! Properties of the decoders and `core_serialize` over generated wire data (CS0 §9.1).
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

#[path = "support/gen.rs"]
mod generate;

use dash_consensus::primitives::core_serialize;
use dash_consensus::{Tx, decode_block, decode_header, decode_tx, decode_tx_exact};
use node_sink::sink::{OpaqueSeed, record_for_test};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;

use generate::{Rng, mutated, proptest_config};

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

/// The transaction properties for one input.
fn tx_properties(bytes: &[u8]) -> Result<(), &'static str> {
    let mut cursor = bytes;
    let Ok(decoded) = decode_tx(&mut cursor) else {
        check(cursor.len() == bytes.len(), "cursor moved on failure")?;
        return Ok(());
    };
    let tx = decoded.value();
    let serialized = core_serialize(tx);
    check(decoded.wire_is_canonical() == (serialized == decoded.wire()), "wire_is_canonical")?;
    check(tx.size() as usize == serialized.len(), "size")?;
    check(cursor.len() == bytes.len() - decoded.wire().len(), "cursor")?;

    // Idempotence, for every value.
    let again = decode_tx_exact(&serialized).map_err(|_| "re-decode")?;
    check(again.wire_is_canonical(), "re-decode canonical")?;
    check(core_serialize(again.value()) == serialized, "idempotence")?;
    check(again.value().instance_hash() == tx.instance_hash(), "instance hash")?;

    // Identity, where the received bytes were canonical.
    if decoded.wire_is_canonical() {
        check(again.value().raw() == tx.raw(), "identity")?;
    }

    // Tx::new agrees with the decoder.
    let rebuilt = Tx::new(tx.to_mutable());
    check(rebuilt.instance_hash() == tx.instance_hash(), "Tx::new instance hash")?;
    check(rebuilt.txid() == tx.txid(), "Tx::new txid")?;
    check(rebuilt.size() == tx.size(), "Tx::new size")?;
    Ok(())
}

/// The block properties for one input.
fn block_properties(bytes: &[u8]) -> Result<(), &'static str> {
    let mut cursor = bytes;
    let Ok(decoded) = decode_block(&mut cursor) else {
        check(cursor.len() == bytes.len(), "cursor moved on failure")?;
        return Ok(());
    };
    let block = decoded.value();
    let serialized = core_serialize(block);
    check(decoded.wire_is_canonical() == (serialized == decoded.wire()), "wire_is_canonical")?;
    check(block.size() as usize == serialized.len(), "size")?;
    check(decoded.core_bytes().as_slice() == serialized, "core bytes")?;
    let mut again = &serialized[..];
    let again = decode_block(&mut again).map_err(|_| "re-decode")?;
    check(again.wire_is_canonical(), "re-decode canonical")?;
    check(core_serialize(again.value()) == serialized, "idempotence")?;
    check(again.value().hash() == block.hash(), "hash")?;
    check(again.value().merkle() == block.merkle(), "merkle")?;
    Ok(())
}

fn header_properties(bytes: &[u8]) -> Result<(), &'static str> {
    let mut cursor = bytes;
    let Ok(decoded) = decode_header(&mut cursor) else {
        check(cursor.len() == bytes.len(), "cursor moved on failure")?;
        return Ok(());
    };
    check(decoded.wire_is_canonical(), "wire_is_canonical")?;
    check(core_serialize(decoded.value()) == decoded.wire(), "serialization")?;
    Ok(())
}

proptest! {
    #![proptest_config(proptest_config(1024))]

    #[test]
    fn transactions(seed in any::<u64>().prop_map(OpaqueSeed)) {
        let bytes = mutated(seed.0, generate::tx_bytes);
        tx_properties(&bytes).map_err(|field| fail("properties_tx", field, seed))?;
    }

    #[test]
    fn blocks(seed in any::<u64>().prop_map(OpaqueSeed)) {
        let bytes = mutated(seed.0, generate::block_bytes);
        block_properties(&bytes).map_err(|field| fail("properties_block", field, seed))?;
    }

    #[test]
    fn headers(seed in any::<u64>().prop_map(OpaqueSeed)) {
        let bytes = mutated(seed.0, generate::header_bytes);
        header_properties(&bytes).map_err(|field| fail("properties_header", field, seed))?;
    }

    /// The decoders never panic on arbitrary bytes.
    #[test]
    fn arbitrary_bytes(seed in any::<u64>().prop_map(OpaqueSeed)) {
        let mut rng = Rng::new(seed.0);
        let len = rng.below(300) as usize;
        let bytes = rng.bytes(len);
        tx_properties(&bytes).map_err(|field| fail("properties_bytes", field, seed))?;
        block_properties(&bytes).map_err(|field| fail("properties_bytes", field, seed))?;
        header_properties(&bytes).map_err(|field| fail("properties_bytes", field, seed))?;
    }
}

/// The generators reach the decoders' success paths often enough to test them.
#[test]
fn generators_produce_decodable_inputs() {
    let (mut txs, mut blocks) = (0, 0);
    for seed in 0..2000u64 {
        let mut rng = Rng::new(seed);
        let wire = generate::tx_bytes(&mut rng);
        let bytes = generate::mutate(&mut rng, wire);
        txs += usize::from(decode_tx(&mut &bytes[..]).is_ok());
        let wire = generate::block_bytes(&mut rng);
        let bytes = generate::mutate(&mut rng, wire);
        blocks += usize::from(decode_block(&mut &bytes[..]).is_ok());
    }
    eprintln!("decodable: {txs} transactions, {blocks} blocks of 2000");
    assert!(txs >= 600 && blocks >= 300, "too few decodable inputs");
}
