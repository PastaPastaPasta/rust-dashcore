//! Core's `CMerkleBlock` and `CPartialMerkleTree` (`merkleblock.cpp`): the `merkleblock` a
//! filtered-block request is answered with (P4b D8, D10).

use dash_consensus::hash::sha256d;
use dash_consensus::primitives::{Block, TxHash, TxType};
use dash_num::Hash256;
use dash_p2p_core::{MerkleBlock, PartialMerkleTree};
use dash_types::Numeric as _;

use super::bloom::BloomFilter;

/// `CMerkleBlock(block, filter)` (merkleblock.cpp:30-72): the header, the partial tree over the
/// matches, and `vMatchedTxn`, the matched transactions' positions and hashes in block order.
/// Matching updates `filter` as its flags say.
pub fn merkle_block(
    block: &Block,
    filter: &mut BloomFilter,
) -> (MerkleBlock, Vec<(usize, TxHash)>) {
    let mut matched = Vec::new();
    let mut hashes = Vec::with_capacity(block.txs().len());
    let mut matches = Vec::with_capacity(block.txs().len());
    for (i, tx) in block.txs().iter().enumerate() {
        let hash = tx.txid();
        let found = allowed_type(tx.is_special_tx_version(), tx.tx_type())
            && filter.is_relevant_and_update(tx);
        if found {
            matched.push((i, hash));
        }
        matches.push(found);
        hashes.push(*hash.as_bytes());
    }
    let msg = MerkleBlock {
        header: *block.header().raw(),
        txn: partial_tree(&hashes, &matches),
    };
    (msg, matched)
}

// Core: merkleblock.cpp:40-58 allowedTxTypes
/// Every transaction below the special version, and the special types Core lets a filter match:
/// not quorum commitments, MNHF signals or an unknown type.
fn allowed_type(special_version: bool, ty: TxType) -> bool {
    !special_version || !matches!(ty.to_base(), 6 | 7 | 13..)
}

/// `CalcTreeWidth(height)`: the number of nodes at `height` above the leaves.
fn width(n: usize, height: u32) -> usize {
    (n + (1 << height) - 1) >> height
}

// Core: merkleblock.cpp:74-92 CPartialMerkleTree::CalcHash
fn calc_hash(height: u32, pos: usize, txids: &[[u8; 32]]) -> [u8; 32] {
    if height == 0 {
        return txids[pos];
    }
    let left = calc_hash(height - 1, pos * 2, txids);
    let right = if pos * 2 + 1 < width(txids.len(), height - 1) {
        calc_hash(height - 1, pos * 2 + 1, txids)
    } else {
        left
    };
    let mut both = [0u8; 64];
    both[..32].copy_from_slice(&left);
    both[32..].copy_from_slice(&right);
    sha256d(&both)
}

struct Build<'a> {
    txids: &'a [[u8; 32]],
    matches: &'a [bool],
    bits: Vec<bool>,
    hashes: Vec<Hash256>,
}

impl Build<'_> {
    // Core: merkleblock.cpp:94-110 CPartialMerkleTree::TraverseAndBuild
    fn traverse(&mut self, height: u32, pos: usize) {
        let n = self.txids.len();
        let first = pos << height;
        let end = ((pos + 1) << height).min(n);
        let parent_of_match = self.matches[first.min(end)..end].iter().any(|&m| m);
        self.bits.push(parent_of_match);
        if height == 0 || !parent_of_match {
            self.hashes.push(Hash256::from_lendian(calc_hash(height, pos, self.txids)));
        } else {
            self.traverse(height - 1, pos * 2);
            if pos * 2 + 1 < width(n, height - 1) {
                self.traverse(height - 1, pos * 2 + 1);
            }
        }
    }
}

// Core: merkleblock.cpp:12-19 BitsToBytes, :150-162 CPartialMerkleTree::CPartialMerkleTree
/// The partial tree over `txids` that proves the ones `matches` marks.
///
/// # Panics
///
/// When `txids` is empty: a block always has its coinbase (Core asserts in `CalcHash`).
pub fn partial_tree(txids: &[[u8; 32]], matches: &[bool]) -> PartialMerkleTree {
    assert!(!txids.is_empty(), "a block has a coinbase (merkleblock.cpp:77)");
    let mut height = 0;
    while width(txids.len(), height) > 1 {
        height += 1;
    }
    let mut b = Build {
        txids,
        matches,
        bits: Vec::new(),
        hashes: Vec::new(),
    };
    b.traverse(height, 0);
    let mut flag_bytes = vec![0u8; b.bits.len().div_ceil(8)];
    for (p, &bit) in b.bits.iter().enumerate() {
        flag_bytes[p / 8] |= u8::from(bit) << (p % 8);
    }
    PartialMerkleTree {
        transactions: txids.len() as u32,
        hashes: b.hashes,
        flag_bytes,
    }
}

#[cfg(test)]
mod tests {
    use dash_consensus::merkle::compute_merkle_root;
    use dash_types::codec::BaseCodec as _;

    use super::super::bloom::vectors::{block, fixture, u256, unhex};
    use super::super::bloom::{BLOOM_UPDATE_ALL, BLOOM_UPDATE_P2PUBKEY_ONLY};
    use super::*;

    /// Core's `ExtractMatches` (merkleblock.cpp:112-148, 166-197): the root the tree proves and
    /// the matched hashes, or `None` where Core returns a null root.
    fn extract(t: &PartialMerkleTree) -> Option<([u8; 32], Vec<[u8; 32]>)> {
        let n = t.transactions as usize;
        if n == 0 || t.hashes.len() > n || t.flag_bytes.len() * 8 < t.hashes.len() {
            return None;
        }
        let bits: Vec<bool> =
            (0..t.flag_bytes.len() * 8).map(|p| t.flag_bytes[p / 8] >> (p % 8) & 1 == 1).collect();
        let mut height = 0;
        while width(n, height) > 1 {
            height += 1;
        }
        struct X<'a> {
            n: usize,
            bits: &'a [bool],
            hashes: &'a [Hash256],
            used_bits: usize,
            used_hashes: usize,
            matched: Vec<[u8; 32]>,
            bad: bool,
        }
        fn walk(x: &mut X<'_>, height: u32, pos: usize) -> [u8; 32] {
            let Some(&parent) = x.bits.get(x.used_bits) else {
                x.bad = true;
                return [0; 32];
            };
            x.used_bits += 1;
            if height == 0 || !parent {
                let Some(h) = x.hashes.get(x.used_hashes) else {
                    x.bad = true;
                    return [0; 32];
                };
                x.used_hashes += 1;
                if height == 0 && parent {
                    x.matched.push(*h.as_bytes());
                }
                return *h.as_bytes();
            }
            let left = walk(x, height - 1, pos * 2);
            let right = if pos * 2 + 1 < width(x.n, height - 1) {
                let r = walk(x, height - 1, pos * 2 + 1);
                x.bad |= r == left;
                r
            } else {
                left
            };
            sha256d(&[left, right].concat())
        }
        let mut x = X {
            n,
            bits: &bits,
            hashes: &t.hashes,
            used_bits: 0,
            used_hashes: 0,
            matched: Vec::new(),
            bad: false,
        };
        let root = walk(&mut x, height, 0);
        let ok = !x.bad
            && x.used_bits.div_ceil(8) == t.flag_bytes.len()
            && x.used_hashes == t.hashes.len();
        ok.then_some((root, x.matched))
    }

    fn check(b: &Block, filter: &mut BloomFilter, expected: &[(usize, &str)]) {
        let (msg, matched) = merkle_block(b, filter);
        assert_eq!(msg.header, *b.header().raw());
        let got: Vec<_> = matched.iter().map(|(i, h)| (*i, *h.as_bytes())).collect();
        let want: Vec<_> = expected.iter().map(|(i, h)| (*i, u256(h))).collect();
        assert_eq!(got, want);
        let (root, extracted) = extract(&msg.txn).expect("a well-formed tree");
        assert_eq!(root, *b.header().merkle_root().as_bytes());
        assert_eq!(extracted, want.iter().map(|(_, h)| *h).collect::<Vec<_>>());
    }

    // Core: src/test/bloom_tests.cpp merkle_block_2, merkle_block_2_with_update_none
    #[test]
    fn merkle_block_2() {
        let b = block(fixture!("merkle_block_2"));
        let first = (0, "0xe980fe9f792d014e73b95203dc1335c5f9ce19ac537a419e6df5b47aecb93b70");
        let rest = [
            (1, "0x28204cad1d7fc1d199e8ef4fa22f182de6258a3eaafe1bbe56ebdcacd3069a5f"),
            (2, "0x6b0f8a73a56c04b519f1883e8aafda643ba61a30bd1439969df21bea5f4e27e2"),
            (3, "0x3c1d7e82342158e4109df2e0b6348b6e84e403d8b4046d7007663ace63cddb23"),
        ];
        let key = unhex(
            "044a656f065871a353f216ca26cef8dde2f03e8c16202d2e8ad769f02032cb86a5eb5e56842e92e19141d60a01928f8dd2c875a390f67c1f6c94cfc617c0ea45af",
        );
        for (flags, after) in [(BLOOM_UPDATE_ALL, &rest[..]), (0, &[rest[0], rest[2]][..])] {
            let mut f = BloomFilter::new(10, 0.000_001, 0, flags);
            f.insert(&u256(first.1));
            check(&b, &mut f, &[first]);
            f.insert(&key);
            check(&b, &mut f, &[&[first][..], after].concat());
        }
    }

    // Core: src/test/bloom_tests.cpp merkle_block_3_and_serialize
    #[test]
    fn merkle_block_3_and_serialize() {
        let b = block(fixture!("merkle_block_3"));
        let mut f = BloomFilter::new(10, 0.000_001, 0, BLOOM_UPDATE_ALL);
        let only = (0, "0x63194f18be0af63f2c6bc9dc0f777cbefed3d9415c4af83f3ee3a3d669c00cb5");
        f.insert(&u256(only.1));
        check(&b, &mut f, &[only]);
        let (msg, _) = merkle_block(&b, &mut f);
        let mut wire = Vec::new();
        msg.encode(&mut wire);
        assert_eq!(wire, unhex(fixture!("merkle_block_3-merkleblock")));
    }

    // Core: src/test/bloom_tests.cpp merkle_block_4, _test_p2pubkey_only, _test_update_none
    #[test]
    fn merkle_block_4() {
        let b = block(fixture!("merkle_block_4"));
        let last = (6, "0x0a2a92f0bda4727d0a13eaddf4dd9ac6b5c61a1429e6b2b818f19b15df0ac154");
        let fourth = (3, "0x02981fa052f0481dbc5868f4fc2166035a10f27a03cfd2de67326471df5bc041");
        let mut f = BloomFilter::new(10, 0.000_001, 0, BLOOM_UPDATE_ALL);
        f.insert(&u256(last.1));
        check(&b, &mut f, &[last]);
        f.insert(&u256(fourth.1));
        check(&b, &mut f, &[fourth, last]);

        let generation = |f: &BloomFilter| {
            let outpoint = |h: &str| [&u256(h)[..], &0u32.to_le_bytes()].concat();
            (
                f.contains(&outpoint(
                    "0x147caa76786596590baa4e98f5d9f48b86c7765e489f7a6ff3360fe5c674360b",
                )),
                f.contains(&outpoint(fourth.1)),
            )
        };
        for (flags, expected) in [(BLOOM_UPDATE_P2PUBKEY_ONLY, (true, false)), (0, (false, false))]
        {
            let mut f = BloomFilter::new(10, 0.000_001, 0, flags);
            f.insert(&unhex("04eaafc2314def4ca98ac970241bcab022b9c1e1f4ea423a20f134c876f2c01ec0f0dd5b2e86e7168cefe0d81113c3807420ce13ad1357231a2252247d97a46a91"));
            f.insert(&unhex("b6efd80d99179f4f4ff6f4dd0a007d018c385d21"));
            merkle_block(&b, &mut f);
            assert_eq!(generation(&f), expected, "flags {flags}");
        }
    }

    // Core: src/test/pmt_tests.cpp pmt_test1 (a fixed LCG for Core's InsecureRand)
    #[test]
    fn partial_tree_round_trips() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rand = |bits: u32| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            if bits == 0 {
                0
            } else {
                (seed >> 33) & ((1 << bits) - 1)
            }
        };
        for n in [1usize, 4, 7, 17, 56, 100, 127, 256, 312, 513, 1000, 4095] {
            let txids: Vec<[u8; 32]> = (0..n as u32).map(|j| sha256d(&j.to_le_bytes())).collect();
            let (root, _) = compute_merkle_root(txids.clone());
            for att in 1..15 {
                let matches: Vec<bool> = (0..n).map(|_| rand(att / 2) == 0).collect();
                let t = partial_tree(&txids, &matches);
                let (got, matched) = extract(&t).expect("well formed");
                assert_eq!(got, root, "n {n} att {att}");
                let want: Vec<_> =
                    txids.iter().zip(&matches).filter(|(_, m)| **m).map(|(h, _)| *h).collect();
                assert_eq!(matched, want);
            }
        }
    }

    #[test]
    fn quorum_commitments_and_mnhf_never_match() {
        for (special, ty, allowed) in [
            (false, 6, true),
            (true, 0, true),
            (true, 5, true),
            (true, 6, false),
            (true, 7, false),
            (true, 9, true),
            (true, 12, true),
            (true, 13, false),
        ] {
            assert_eq!(allowed_type(special, TxType::from_base(ty)), allowed, "{special} {ty}");
        }
    }
}
