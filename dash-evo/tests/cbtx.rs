//! The CbTx proof, root checks and coinbase-ChainLock predicate (SB7) against Core's data.
//!
//! Provenance of `data/cbtx_testnet.json`: Dash Core v24 (`/Dash Core:24.0.0/`), the testnet
//! reference oracle, through `getblock <hash> 2` (the coinbase's raw hex, the block's txids and
//! merkle root, `cbTx`) and `gettxoutproof` (Core's `CMerkleBlock` for the coinbase alone, and
//! for the coinbase with the third transaction) at heights 900,000 (CbTx v2, before V20),
//! 905,101 (V20's second block, one transaction, `bestCLHeightDiff` 1) and 1,569,000 (CbTx v3,
//! 16 transactions). `/work/scratch/sb7/tools/fixtures.py` wrote it. The negative cases edit
//! those bytes and say which.
//!
//! Provenance of `data/qroot_mainnet.json`: the same Core on the mainnet reference oracle, read
//! with `getblockhash` and `getblock <hash> 2` only, for the `merkleRootQuorums` of 2,089,173 (it
//! mines one commitment each of two full non-rotated types, 400_60 and 400_85) and 2,089,194 (it
//! mines all 32 indices of the rotated 60_75). For each block: the active commitments at the
//! previous block (Core's `GetMinedAndActiveCommitmentsUntilBlock`, which `GetQcHashes` reads),
//! rebuilt from the type-6 transactions of the blocks below it by Core's rules (the newest
//! `signingActiveQuorumCount` non-null commitments of a non-rotated type by mined height, newest
//! first; the newest of each quorum index of the rotated type; 50_60, disabled from
//! `DIP0024QuorumsHeight`, below that height), each hashed (internal byte order) as
//! `SerializeHash` of the commitment bytes Core mined (the double SHA-256 of its type-6 payload
//! after the version and height); the block's type-6 transactions raw, in block order; and the
//! CbTx roots of both blocks. The previous block mines no commitment, so its CbTx root checks the
//! rebuilt set. `/work/scratch/sb7/tools/fetch_qroot.py` wrote it.
//!
//! The partial-merkle round trips port Core's `pmt_tests.cpp` (v24.0.0-rc.3): its tree builder
//! (`CPartialMerkleTree(vTxid, vMatch)`) is ported below to make the trees.

use std::sync::Arc;

use dash_consensus::hash::sha256d;
use dash_consensus::merkle::compute_merkle_root;
use dash_consensus::primitives::{MerkleRoot, Tx, TxHash, TxRef, decode_tx_exact};
use dash_consensus::validation::BlockValidationError;
use dash_evo::bls::BlsCtx;
use dash_evo::cbtx::{
    CbTxProofError, CoinbaseFacts, PartialMerkleTree, check_mn_list_root, check_quorums_root,
    nonnull_coinbase_chainlock, verify_cbtx_proof,
};
use dash_evo::sml::{
    BlockQc, QcHashes, QuorumRootParams, merkle_root_quorums, merkle_root_quorums_from_hashes,
};
use dash_params::LlmqType;
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::{CoinbaseCommitment, SpecialPayload};
use dash_types::Numeric;
use serde_json::Value;

fn fixtures() -> Value {
    serde_json::from_str(include_str!("data/cbtx_testnet.json")).unwrap()
}

fn block(height: u64) -> Value {
    fixtures()["blocks"].as_array().unwrap().iter().find(|b| b["height"] == height).unwrap().clone()
}

fn unhex(hex: &str) -> Vec<u8> {
    (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect()
}

fn tx(hex: &str) -> TxRef {
    Arc::new(decode_tx_exact(&unhex(hex)).unwrap().into_value())
}

fn compact_size(bytes: &mut &[u8]) -> usize {
    let (first, rest) = bytes.split_first().unwrap();
    *bytes = rest;
    let width = match first {
        0..=252 => return usize::from(*first),
        253 => 2,
        254 => 4,
        255 => 8,
    };
    let (n, rest) = bytes.split_at(width);
    *bytes = rest;
    n.iter().rev().fold(0, |acc, b| acc << 8 | usize::from(*b))
}

/// A `CMerkleBlock`'s header merkle root and partial merkle tree (`merkleblock.h:88`).
fn merkle_block(hex: &str) -> (MerkleRoot, PartialMerkleTree) {
    let bytes = unhex(hex);
    let merkle_root = MerkleRoot::from_lendian(bytes[36..68].try_into().unwrap());
    let mut rest = &bytes[80..];
    let transactions = u32::from_le_bytes(rest[..4].try_into().unwrap());
    rest = &rest[4..];
    let hashes = (0..compact_size(&mut rest))
        .map(|_| {
            let (hash, tail) = rest.split_at(32);
            rest = tail;
            hash.try_into().unwrap()
        })
        .collect();
    let flags_len = compact_size(&mut rest);
    assert_eq!(rest.len(), flags_len);
    (
        merkle_root,
        PartialMerkleTree {
            transactions,
            hashes,
            flags: rest.to_vec(),
        },
    )
}

#[test]
fn core_coinbase_proofs_verify() {
    for b in fixtures()["blocks"].as_array().unwrap() {
        let coinbase = tx(b["coinbase_hex"].as_str().unwrap());
        assert_eq!(coinbase.txid(), TxHash::from_hex(b["txids"][0].as_str().unwrap()).unwrap());
        let (merkle_root, tree) = merkle_block(b["coinbase_proof_hex"].as_str().unwrap());
        assert_eq!(merkle_root, MerkleRoot::from_hex(b["merkleroot"].as_str().unwrap()).unwrap());
        assert_eq!(
            verify_cbtx_proof(&tree, &merkle_root, &coinbase.txid()),
            Ok(()),
            "{}",
            b["height"]
        );
    }
}

#[test]
fn coinbase_proof_failures() {
    let b = block(1_569_000);
    let coinbase = tx(b["coinbase_hex"].as_str().unwrap());
    let (merkle_root, tree) = merkle_block(b["coinbase_proof_hex"].as_str().unwrap());

    // A proof matching a second transaction too, or another transaction alone.
    let (_, both) = merkle_block(b["two_tx_proof_hex"].as_str().unwrap());
    let extracted = both.extract_matches().unwrap();
    assert_eq!(extracted.root, merkle_root.to_lendian());
    assert_eq!(extracted.matches.iter().map(|m| m.1).collect::<Vec<_>>(), [0, 2]);
    assert_eq!(
        verify_cbtx_proof(&both, &merkle_root, &coinbase.txid()),
        Err(CbTxProofError::NotTheCoinbase)
    );
    let other = TxHash::from_hex(b["txids"][1].as_str().unwrap()).unwrap();
    assert_eq!(verify_cbtx_proof(&tree, &merkle_root, &other), Err(CbTxProofError::NotTheCoinbase));

    // Another block's root.
    let wrong_root = MerkleRoot::from_lendian([7; 32]);
    assert_eq!(
        verify_cbtx_proof(&tree, &wrong_root, &coinbase.txid()),
        Err(CbTxProofError::RootMismatch)
    );

    // A changed sibling hash moves the root.
    let mut changed = tree.clone();
    changed.hashes[1][0] ^= 1;
    assert_eq!(
        verify_cbtx_proof(&changed, &merkle_root, &coinbase.txid()),
        Err(CbTxProofError::RootMismatch)
    );

    // Each of Core's `ExtractMatches` failures.
    let malformed = |edit: &dyn Fn(&mut PartialMerkleTree)| {
        let mut bad = tree.clone();
        edit(&mut bad);
        assert_eq!(bad.extract_matches(), None);
        assert_eq!(
            verify_cbtx_proof(&bad, &merkle_root, &coinbase.txid()),
            Err(CbTxProofError::Malformed)
        );
    };
    malformed(&|t| t.transactions = 0);
    malformed(&|t| t.transactions = 2_000_000 / 60 + 1);
    malformed(&|t| t.hashes.push([1; 32])); // a hash left over
    malformed(&|t| {
        t.hashes.pop(); // the traversal runs past the hashes
    });
    malformed(&|t| t.flags.push(0)); // a whole byte of bits left over
    malformed(&|t| t.flags.clear()); // fewer bits than hashes
    malformed(&|t| t.transactions = 1); // one leaf: the remaining hashes are unused
}

/// Core's `CPartialMerkleTree(vTxid, vMatch)` (`merkleblock.cpp:96 TraverseAndBuild`), to build
/// trees for the round trips.
fn build(txids: &[[u8; 32]], matched: &[bool]) -> PartialMerkleTree {
    fn width(n: usize, height: u32) -> usize {
        (n + (1 << height) - 1) >> height
    }
    fn node(txids: &[[u8; 32]], height: u32, pos: usize) -> [u8; 32] {
        if height == 0 {
            return txids[pos];
        }
        let left = node(txids, height - 1, pos * 2);
        let right = if pos * 2 + 1 < width(txids.len(), height - 1) {
            node(txids, height - 1, pos * 2 + 1)
        } else {
            left
        };
        compute_merkle_root(vec![left, right]).0
    }
    fn walk(
        txids: &[[u8; 32]],
        matched: &[bool],
        height: u32,
        pos: usize,
        bits: &mut Vec<bool>,
        hashes: &mut Vec<[u8; 32]>,
    ) {
        let parent = (pos << height..((pos + 1) << height).min(txids.len())).any(|p| matched[p]);
        bits.push(parent);
        if height == 0 || !parent {
            hashes.push(node(txids, height, pos));
        } else {
            walk(txids, matched, height - 1, pos * 2, bits, hashes);
            if pos * 2 + 1 < width(txids.len(), height - 1) {
                walk(txids, matched, height - 1, pos * 2 + 1, bits, hashes);
            }
        }
    }
    let mut height = 0;
    while width(txids.len(), height) > 1 {
        height += 1;
    }
    let (mut bits, mut hashes) = (Vec::new(), Vec::new());
    walk(txids, matched, height, 0, &mut bits, &mut hashes);
    let mut flags = vec![0; bits.len().div_ceil(8)];
    for (i, bit) in bits.iter().enumerate() {
        flags[i / 8] |= u8::from(*bit) << (i % 8);
    }
    PartialMerkleTree {
        transactions: txids.len() as u32,
        hashes,
        flags,
    }
}

// Core: test/pmt_tests.cpp:33 pmt_test1, with a fixed pseudo-random source
#[test]
fn pmt_round_trips() {
    let mut seed = 0x5b7_u64;
    let mut next = move || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        seed
    };
    for n in [1usize, 4, 7, 17, 56, 100, 127, 256, 312, 513, 1000, 4095] {
        let txids: Vec<[u8; 32]> = (0..n)
            .map(|_| {
                let mut id = [0; 32];
                for chunk in id.chunks_mut(8) {
                    chunk.copy_from_slice(&next().to_le_bytes());
                }
                id
            })
            .collect();
        let (root, _) = compute_merkle_root(txids.clone());
        // Core's loop: match roughly 1/2^att of the transactions, att = 1..15.
        for att in 1..15 {
            let matched: Vec<bool> = (0..n).map(|_| next() % (1 << att) == 0).collect();
            let tree = build(&txids, &matched);
            let extracted = tree.extract_matches().unwrap();
            assert_eq!(extracted.root, root);
            let want: Vec<([u8; 32], u32)> =
                (0..n).filter(|&i| matched[i]).map(|i| (txids[i], i as u32)).collect();
            assert_eq!(extracted.matches, want);
        }
    }
}

// Core: test/pmt_tests.cpp:109 pmt_malleability
#[test]
fn pmt_malleability() {
    let mut txids: Vec<[u8; 32]> = (1..=10u8)
        .map(|i| {
            let mut id = [0; 32];
            id[0] = i;
            id
        })
        .collect();
    txids.push(txids[8]);
    txids.push(txids[9]);
    let matched =
        [false, false, false, false, false, false, false, false, false, true, true, false];
    assert_eq!(build(&txids, &matched).extract_matches(), None);
}

fn coinbase_payload(coinbase: &Tx) -> CoinbaseCommitment {
    match &coinbase.payload().unwrap().unwrap().payload {
        SpecialPayload::CoinbaseCommitment(cbtx) => cbtx.clone(),
        other => panic!("{other:?}"),
    }
}

/// `coinbase` with its extra payload edited.
fn with_payload(coinbase: &Tx, edit: impl FnOnce(&mut Vec<u8>)) -> TxRef {
    let mut raw = coinbase.to_mutable();
    edit(&mut raw.extra_payload);
    Arc::new(Tx::new(raw))
}

#[test]
fn nonnull_coinbase_chainlock_cases() {
    let b = block(1_569_000);
    let coinbase = tx(b["coinbase_hex"].as_str().unwrap());
    let cbtx = coinbase_payload(&coinbase);
    let facts = CoinbaseFacts::First(coinbase.clone());
    let h = 1_569_000;
    // At or after V20, under the basic scheme.
    let chainlock = |facts: &CoinbaseFacts, height| {
        nonnull_coinbase_chainlock(facts, height, true, BlsCtx::BASIC)
    };

    let (sig, diff) = chainlock(&facts, h).unwrap();
    assert_eq!(diff, 0);
    assert_eq!(sig.to_bytes(BlsCtx::BASIC.scheme()), cbtx.best_cl_signature.unwrap().to_bytes());
    assert_eq!(
        unhex(b["cbTx"]["bestCLSignature"].as_str().unwrap()),
        sig.to_bytes(BlsCtx::BASIC.scheme())
    );

    // Before V20, without the block's data or transactions, or with a first transaction that is
    // not a coinbase.
    assert!(nonnull_coinbase_chainlock(&facts, h, false, BlsCtx::BASIC).is_none());
    assert!(chainlock(&CoinbaseFacts::BlockDataUnavailable, h).is_none());
    assert!(chainlock(&CoinbaseFacts::NoTransactions, h).is_none());
    let second = CoinbaseFacts::First(tx(b["second_tx_hex"].as_str().unwrap()));
    assert!(chainlock(&second, h).is_none());

    // A CbTx at another height than the block's.
    assert!(chainlock(&facts, h + 1).is_none());

    // A version-2 CbTx (900,000) carries no ChainLock.
    let v2 = tx(block(900_000)["coinbase_hex"].as_str().unwrap());
    assert_eq!(coinbase_payload(&v2).version, 2);
    assert!(chainlock(&CoinbaseFacts::First(v2), 900_000).is_none());

    // Edits of the v3 payload: version(2) height(4) roots(64) diff(1) sig(96) balance(8).
    const SIG: std::ops::Range<usize> = 71..167;
    let null_sig = with_payload(&coinbase, |p| p[SIG].fill(0));
    assert!(chainlock(&CoinbaseFacts::First(null_sig), h).is_none());
    let trailing = with_payload(&coinbase, |p| p.push(0));
    assert!(chainlock(&CoinbaseFacts::First(trailing), h).is_none());

    // The difference must be below the height: a CbTx at height 1 with difference 1.
    let low = with_payload(&coinbase, |p| {
        p[2..6].copy_from_slice(&1i32.to_le_bytes());
        p[70] = 1;
    });
    assert!(chainlock(&CoinbaseFacts::First(low.clone()), 1).is_none());
    let ok = with_payload(&low, |p| p[2..6].copy_from_slice(&2i32.to_le_bytes()));
    assert_eq!(chainlock(&CoinbaseFacts::First(ok), 2).map(|r| r.1), Some(1));

    // V20's second block, from Core: difference 1 below height 905,101.
    let b = block(905_101);
    let first = CoinbaseFacts::First(tx(b["coinbase_hex"].as_str().unwrap()));
    assert_eq!(chainlock(&first, 905_101).map(|r| r.1), Some(1));
}

#[test]
fn cbtx_root_checks() {
    let b = block(1_569_000);
    let cbtx = coinbase_payload(&tx(b["coinbase_hex"].as_str().unwrap()));
    let mn_root = MerkleRoot::from_hex(b["cbTx"]["merkleRootMNList"].as_str().unwrap()).unwrap();
    let q_root = MerkleRoot::from_hex(b["cbTx"]["merkleRootQuorums"].as_str().unwrap()).unwrap();
    let other = MerkleRoot::from_lendian([9; 32]);
    let reason = |r: Result<(), BlockValidationError>| r.unwrap_err().reject_reason().to_owned();

    assert_eq!(check_mn_list_root(&cbtx, &mn_root), Ok(()));
    assert_eq!(reason(check_mn_list_root(&cbtx, &other)), "bad-cbtx-mnmerkleroot");
    assert_eq!(check_quorums_root(&cbtx, || Ok(q_root)), Ok(()));
    assert_eq!(reason(check_quorums_root(&cbtx, || Ok(other))), "bad-cbtx-quorummerkleroot");

    // A version-1 CbTx has no quorum root: nothing is computed.
    let mut v1 = cbtx.clone();
    v1.version = 1;
    assert_eq!(check_quorums_root(&v1, || unreachable!()), Ok(()));

    // Version 2, the first with a quorum root (testnet 900,000): it is computed and compared.
    let b = block(900_000);
    let v2 = coinbase_payload(&tx(b["coinbase_hex"].as_str().unwrap()));
    assert_eq!(v2.version, 2);
    let q_root = MerkleRoot::from_hex(b["cbTx"]["merkleRootQuorums"].as_str().unwrap()).unwrap();
    assert_eq!(check_quorums_root(&v2, || Ok(q_root)), Ok(()));
    assert_eq!(reason(check_quorums_root(&v2, || Ok(other))), "bad-cbtx-quorummerkleroot");
}

/// A block's type-6 transaction as `CalcCbTxMerkleRootQuorums` reads it, hashing the commitment
/// bytes as mined: `SerializeHash` of the payload after its version (2 bytes) and height (4).
fn block_qc(tx: &Tx) -> BlockQc {
    let Some(Ok(decoded)) = tx.payload() else {
        return BlockQc::BadPayload;
    };
    let SpecialPayload::QuorumCommitment(qc) = &decoded.payload else {
        panic!("not a commitment")
    };
    assert!(decoded.is_complete());
    if qc.commitment.is_null() {
        return BlockQc::Null;
    }
    BlockQc::Commitment {
        llmq_type: qc.commitment.llmq_type,
        quorum_index: qc.commitment.quorum_index.unwrap_or(0),
        hash: sha256d(&tx.extra_payload()[6..]),
    }
}

fn hash32(hex: &Value) -> [u8; 32] {
    unhex(hex.as_str().unwrap()).try_into().unwrap()
}

// Core: evo/cbtx.cpp:65 CalcCbTxMerkleRootQuorums
#[test]
fn quorums_root_of_mainnet_blocks() {
    let fixtures: Value = serde_json::from_str(include_str!("data/qroot_mainnet.json")).unwrap();
    let chain = ChainParamsBuilder::new(Network::Main, Release::V24_0).build().unwrap();
    let params = |llmq_type: LlmqType| {
        chain.consensus.llmq(llmq_type).map(|llmq| QuorumRootParams {
            signing_active_quorum_count: usize::from(llmq.signing_active_quorum_count),
            rotation: llmq.use_rotation,
        })
    };
    let llmq_type = |key: &str| LlmqType::from_base(key.parse().unwrap());
    for b in fixtures["blocks"].as_array().unwrap() {
        let height = &b["height"];
        let mut active = QcHashes::default();
        for (key, hashes) in b["active_plain"].as_object().unwrap() {
            for hash in hashes.as_array().unwrap() {
                active.push_plain(llmq_type(key), hash32(hash));
            }
        }
        for (key, by_index) in b["active_indexed"].as_object().unwrap() {
            for (index, hash) in by_index.as_object().unwrap() {
                active.insert_indexed(llmq_type(key), index.parse().unwrap(), hash32(hash));
            }
        }
        // The previous block mined no commitment, so the active set alone is its root.
        assert_eq!(b["prev_qc_commitments"], 0);
        let all =
            active.plain.values().flatten().chain(active.indexed.values().flat_map(|m| m.values()));
        let prev_root =
            MerkleRoot::from_hex(b["prev_merkleRootQuorums"].as_str().unwrap()).unwrap();
        assert_eq!(
            merkle_root_quorums_from_hashes(all.copied().collect()),
            Ok(prev_root),
            "{height}"
        );

        let qcs: Vec<BlockQc> = b["qc_txs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hex| block_qc(&tx(hex.as_str().unwrap())))
            .collect();
        let root = MerkleRoot::from_hex(b["merkleRootQuorums"].as_str().unwrap()).unwrap();
        assert_eq!(
            merkle_root_quorums(active.clone(), qcs.iter().copied(), params),
            Ok(root),
            "{height}"
        );

        // The vector tells the oldest commitment from the newest: popping the newest of a full
        // non-rotated type gives another root where the block mines one.
        for (&key, hashes) in &active.plain {
            let mined = qcs.iter().any(|qc| {
                matches!(qc, BlockQc::Commitment { llmq_type, .. } if llmq_type.to_base() == key)
            });
            if mined {
                let mut newest_last = active.clone();
                newest_last.plain.get_mut(&key).unwrap().reverse();
                assert_ne!(merkle_root_quorums(newest_last, qcs.iter().copied(), params), Ok(root));
                assert_eq!(
                    hashes.len(),
                    params(LlmqType::from_base(key)).unwrap().signing_active_quorum_count
                );
            }
        }
    }
}
