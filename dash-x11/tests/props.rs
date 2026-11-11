//! Property tests: every implementation computes the same function, and X11 is the chain of its
//! stages.

mod common;

use common::implementations;
use dash_x11::{Implementation, Stage};
use proptest::prelude::*;

/// Every stage of every implementation gives the portable one's digest of `block`.
fn stages_match_portable(block: &[u8]) -> Result<(), TestCaseError> {
    for stage in Stage::ALL {
        let want = Implementation::standard().stage(stage, block);
        for imp in implementations() {
            prop_assert_eq!(imp.stage(stage, block), want, "{} {}", stage.name(), imp.name());
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// Every implementation this CPU runs gives the portable code's X11 hash.
    #[test]
    fn implementations_agree(data in prop::collection::vec(any::<u8>(), 0..400)) {
        let want = Implementation::standard().x11(&data);
        for imp in implementations() {
            prop_assert_eq!(imp.x11(&data), want, "{}", imp.name());
        }
        prop_assert_eq!(dash_x11::x11(&data), want);
    }

    /// X11 is BLAKE-512 of the input, then each stage of the previous digest, truncated.
    #[test]
    fn x11_is_the_chain(data in prop::collection::vec(any::<u8>(), 0..300)) {
        for imp in implementations() {
            let mut h = imp.stage(Stage::Blake, &data);
            for stage in &Stage::ALL[1..] {
                h = imp.stage(*stage, &h);
            }
            prop_assert_eq!(&imp.x11(&data)[..], &h[..32], "{}", imp.name());
        }
    }

    /// Every stage of every implementation agrees with the portable one on any 64-byte input.
    #[test]
    fn stages_agree(block in prop::collection::vec(any::<u8>(), 64)) {
        stages_match_portable(&block)?;
    }

    /// The same on inputs made of extreme bytes, where carries, reductions and saturations of
    /// the vector code are tightest.
    #[test]
    fn stages_agree_on_extreme_bytes(
        bytes in prop::collection::vec(prop::sample::select(vec![0x00u8, 0x01, 0x7f, 0x80, 0xfe, 0xff]), 64)
    ) {
        stages_match_portable(&bytes)?;
    }

    /// A one-bit change of an 80-byte header changes the hash.
    #[test]
    fn header_bit_flips(header in prop::array::uniform32(any::<u8>()), bit in 0usize..640) {
        let mut a = [0u8; 80];
        a[..32].copy_from_slice(&header);
        a[48..80].copy_from_slice(&header);
        let mut b = a;
        b[bit / 8] ^= 1 << (bit % 8);
        prop_assert_ne!(dash_x11::x11(&a), dash_x11::x11(&b));
    }
}

#[test]
#[should_panic(expected = "X11 chain stages hash 64 bytes")]
fn chain_stages_take_64_bytes() {
    Implementation::standard().stage(Stage::Bmw, &[0; 63]);
}

#[test]
fn stage_names_are_in_chain_order() {
    let names: Vec<_> = Stage::ALL.iter().map(|s| s.name()).collect();
    assert_eq!(
        names,
        [
            "blake", "bmw", "groestl", "skein", "jh", "keccak", "luffa", "cubehash", "shavite",
            "simd", "echo"
        ]
    );
}
