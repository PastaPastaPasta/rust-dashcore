//! Proptest twins of the L1 targets (CS0 §9.3): every PR's smoke run of `diff_tx_decode`,
//! `diff_block_decode`, `diff_header_decode`, the coin targets (`diff_codec_primitives`,
//! `diff_coin_decode`, `diff_undo_decode`; CS5a) and the checks (`diff_check_transaction`,
//! `diff_check_block`, `diff_contextual_check_tx`, `diff_contextual_check_block_header`,
//! `diff_contextual_check_block`; CS3) and the coin-reading checks (`diff_check_tx_inputs`,
//! `diff_sequence_locks`; CS5b), versionbits (`diff_versionbits`; CS4) and `diff_bls_decode`
//! (BS11a-1) without libFuzzer and without private data.
//!
//! Output rules (CS0 §9.5): the seed's `Debug` is a placeholder, failures are not persisted,
//! there is no shrinking, and a failure names the target, the field and the pin. When
//! `DASH_EMBARGO_DIR` is set, the failing input goes to the private sink first.

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::use_debug,
    clippy::dbg_macro,
    clippy::print_stdout
)]

#[path = "support/checks_gen.rs"]
mod checks_gen;
#[path = "../../dash-consensus/tests/support/coins_gen.rs"]
mod coins_gen;
#[path = "support/inputs_gen.rs"]
mod inputs_gen;
#[path = "support/spec.rs"]
mod spec;

use dash_oracle_sys::Pin;
use node_l1::bls::compare_bls_decode;
use node_l1::{
    Compared, compare_block, compare_block_undo, compare_check_block, compare_check_transaction,
    compare_check_tx_inputs, compare_codec_primitives, compare_coin_decode,
    compare_contextual_check_block, compare_contextual_check_block_header,
    compare_contextual_check_tx, compare_header, compare_sequence_locks, compare_tx,
    compare_tx_exact, compare_versionbits,
};
use node_sink::sink::{OpaqueSeed, record_for_test};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;

use generate::{mutated, proptest_config};
use spec::generate;

/// Compares `bytes` on every linked pin; a divergence is recorded privately and fails the case.
fn twin(
    target: &'static str,
    compare: fn(&[u8], Pin) -> Compared,
    bytes: &[u8],
) -> Result<(), TestCaseError> {
    for pin in Pin::ALL {
        if let Err(divergence) = compare(bytes, pin) {
            let recorded = record_for_test(target, bytes, pin.name(), divergence.field);
            return Err(TestCaseError::fail(format!(
                "{target}: {divergence} differs on {} ({recorded})",
                pin.name()
            )));
        }
    }
    Ok(())
}

/// A `diff_versionbits` input: random bytes of a random length (every input of at least 8 bytes
/// is a scenario).
fn versionbits_bytes(rng: &mut generate::Rng) -> Vec<u8> {
    let len = 8 + rng.below(160) as usize;
    (0..len).map(|_| rng.next_u64() as u8).collect()
}

proptest! {
    #![proptest_config(proptest_config(512))]

    #[test]
    fn diff_tx_decode(seed in any::<u64>().prop_map(OpaqueSeed)) {
        let bytes = mutated(seed.0, generate::tx_bytes);
        twin("diff_tx_decode", compare_tx, &bytes)?;
        twin("diff_tx_decode_exact", compare_tx_exact, &bytes)?;
    }

    #[test]
    fn diff_block_decode(seed in any::<u64>().prop_map(OpaqueSeed)) {
        let bytes = mutated(seed.0, generate::block_bytes);
        twin("diff_block_decode", compare_block, &bytes)?;
    }

    #[test]
    fn diff_header_decode(seed in any::<u64>().prop_map(OpaqueSeed)) {
        let bytes = mutated(seed.0, generate::header_bytes);
        twin("diff_header_decode", compare_header, &bytes)?;
    }

    #[test]
    fn diff_codec_primitives(seed in any::<u64>().prop_map(OpaqueSeed)) {
        let bytes = mutated(seed.0, coins_gen::codec_primitives_bytes);
        twin("diff_codec_primitives", compare_codec_primitives, &bytes)?;
    }

    #[test]
    fn diff_coin_decode(seed in any::<u64>().prop_map(OpaqueSeed)) {
        let bytes = mutated(seed.0, coins_gen::coin_bytes);
        twin("diff_coin_decode", compare_coin_decode, &bytes)?;
    }

    #[test]
    fn diff_undo_decode(seed in any::<u64>().prop_map(OpaqueSeed)) {
        let bytes = mutated(seed.0, coins_gen::block_undo_bytes);
        twin("diff_undo_decode", compare_block_undo, &bytes)?;
    }

    #[test]
    fn diff_check_transaction(seed in any::<u64>().prop_map(OpaqueSeed)) {
        let bytes = mutated(seed.0, generate::tx_bytes);
        twin("diff_check_transaction", compare_check_transaction, &bytes)?;
    }

    #[test]
    fn diff_check_block(seed in any::<u64>().prop_map(OpaqueSeed)) {
        let bytes = checks_gen::check_block_bytes(&mut generate::Rng::new(seed.0));
        twin("diff_check_block", compare_check_block, &bytes)?;
    }

    #[test]
    fn diff_contextual_check_tx(seed in any::<u64>().prop_map(OpaqueSeed)) {
        let bytes = checks_gen::contextual_tx_bytes(&mut generate::Rng::new(seed.0));
        twin("diff_contextual_check_tx", compare_contextual_check_tx, &bytes)?;
    }

    #[test]
    fn diff_contextual_check_block_header(seed in any::<u64>().prop_map(OpaqueSeed)) {
        let bytes = checks_gen::contextual_header_bytes(&mut generate::Rng::new(seed.0));
        twin("diff_contextual_check_block_header", compare_contextual_check_block_header, &bytes)?;
    }

    #[test]
    fn diff_contextual_check_block(seed in any::<u64>().prop_map(OpaqueSeed)) {
        let bytes = checks_gen::contextual_block_bytes(&mut generate::Rng::new(seed.0));
        twin("diff_contextual_check_block", compare_contextual_check_block, &bytes)?;
    }

    #[test]
    fn diff_check_tx_inputs(seed in any::<u64>().prop_map(OpaqueSeed)) {
        let bytes = mutated(seed.0, inputs_gen::check_tx_inputs_bytes);
        twin("diff_check_tx_inputs", compare_check_tx_inputs, &bytes)?;
    }

    #[test]
    fn diff_sequence_locks(seed in any::<u64>().prop_map(OpaqueSeed)) {
        let bytes = inputs_gen::sequence_locks_bytes(&mut generate::Rng::new(seed.0));
        twin("diff_sequence_locks", compare_sequence_locks, &bytes)?;
    }

    #[test]
    fn diff_versionbits(seed in any::<u64>().prop_map(OpaqueSeed)) {
        let bytes = versionbits_bytes(&mut generate::Rng::new(seed.0));
        twin("diff_versionbits", compare_versionbits, &bytes)?;
    }

    #[test]
    fn diff_bls_decode(seed in any::<u64>().prop_map(OpaqueSeed)) {
        let mut rng = generate::Rng::new(seed.0);
        let len = rng.below(98) as usize + 1;
        twin("diff_bls_decode", compare_bls_decode, &rng.bytes(len))?;
    }
}
