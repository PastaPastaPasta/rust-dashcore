//! Property tests: every implementation this CPU runs agrees with the portable one, for any
//! input, any way of splitting it into writes, and any number of 64-byte double-hash inputs.

mod common;

use common::{implementations, sha256_split};
use dash_sha256::Implementation;
use proptest::prelude::*;

/// An input up to `max` bytes and the sizes of the writes that feed it.
fn input_and_splits(max: usize) -> impl Strategy<Value = (Vec<u8>, Vec<usize>)> {
    prop::collection::vec(any::<u8>(), 0..max).prop_flat_map(|input| {
        let len = input.len();
        (Just(input), prop::collection::vec(0..=len, 0..8)).prop_map(|(input, mut cuts)| {
            cuts.sort_unstable();
            let splits = cuts
                .first()
                .copied()
                .into_iter()
                .chain(cuts.windows(2).map(|w| w[1] - w[0]))
                .collect();
            (input, splits)
        })
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn sha256_matches_portable((input, splits) in input_and_splits(1500)) {
        let expected = sha256_split(Implementation::standard(), &input, &[]);
        for imp in implementations() {
            prop_assert_eq!(sha256_split(imp, &input, &[]), expected, "{}", imp.name());
            prop_assert_eq!(sha256_split(imp, &input, &splits), expected, "{} {:?}", imp.name(), &splits);
        }
    }

    #[test]
    fn sha256d64_matches_portable(input in prop::collection::vec(any::<[u8; 64]>(), 0..40)) {
        let mut expected = vec![[0; 32]; input.len()];
        Implementation::standard().sha256d64(&mut expected, &input);
        for imp in implementations() {
            let mut out = vec![[0; 32]; input.len()];
            imp.sha256d64(&mut out, &input);
            prop_assert_eq!(&out, &expected, "{}", imp.name());
            let mut level = input.as_flattened().as_chunks::<32>().0.to_vec();
            imp.sha256d64_in_place(&mut level);
            prop_assert_eq!(&level[..input.len()], &expected[..], "in place, {}", imp.name());
        }
    }
}
