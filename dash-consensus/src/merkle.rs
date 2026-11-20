//! Core's merkle root, with its mutation flag (CS0 §7).
//!
//! It sits at the crate root rather than under CS3's `block` module: [`crate::Block`] needs it
//! at construction, and `dash-evo` uses the same `ComputeMerkleRoot` for its roots.

use crate::hash;

/// Core's `ComputeMerkleRoot`: the root and whether the tree is mutated (CVE-2012-2459). An empty
/// input gives the zero hash.
///
/// "Mutated" is exactly Core's definition: on each level, before an odd level's last hash is
/// duplicated, the hashes at positions (0, 1), (2, 3), … are compared, and an equal pair sets
/// the flag. Overlapping pairs and the synthetic duplicate are never compared.
// Core: consensus/merkle.cpp:45 ComputeMerkleRoot (identical in v23.1.8)
pub fn compute_merkle_root(mut level: Vec<[u8; 32]>) -> ([u8; 32], bool) {
    let mut mutated = false;
    while level.len() > 1 {
        mutated |= level.as_chunks::<2>().0.iter().any(|[a, b]| a == b);
        if let Some(&last) = level.last()
            && level.len() % 2 == 1
        {
            level.push(last);
        }
        let pairs = hash::sha256d_64_in_place(&mut level);
        level.truncate(pairs);
    }
    (level.first().copied().unwrap_or([0; 32]), mutated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(n: u8) -> [u8; 32] {
        [n; 32]
    }

    fn pair_hash(a: [u8; 32], b: [u8; 32]) -> [u8; 32] {
        let mut both = [0u8; 64];
        both[..32].copy_from_slice(&a);
        both[32..].copy_from_slice(&b);
        hash::sha256d(&both)
    }

    #[test]
    fn small_trees_follow_the_definition() {
        let (a, b, c) = (leaf(1), leaf(2), leaf(3));
        assert_eq!(compute_merkle_root(vec![]), ([0; 32], false));
        assert_eq!(compute_merkle_root(vec![a]), (a, false));
        assert_eq!(compute_merkle_root(vec![a, b]), (pair_hash(a, b), false));
        let ab = pair_hash(a, b);
        let cc = pair_hash(c, c);
        assert_eq!(compute_merkle_root(vec![a, b, c]), (pair_hash(ab, cc), false));
    }
}
