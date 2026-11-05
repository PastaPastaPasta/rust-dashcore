//! EV4-serve against Dash Core v24.0.0-rc.3 (notes D9, D11): masternode lists imported from Core's
//! evodb records ([`EvoDbImage`]), their `merkleRootMNList` (full and cached) against every
//! block's CbTx, and the `mnlistdiff` the builder serves against rc.3's reply to the same
//! `getmnlistd`, byte for byte, in every serialization band.
//!
//! Fixtures: `tests/data/evodb/<test>.json.gz`, one per Core functional test chain, built by
//! `contrib/node/ev4-serve/capture.py` (provenance: `tests/data/evodb/PROVENANCE.md`).

use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::sync::Arc;

use dash_consensus::hash::sha256d;
use dash_consensus::primitives::{MerkleRoot, TxRef, decode_tx_exact};
use dash_coreimport::Hash256;
use dash_coreimport::evodb::{Ancestry, EvoDbImage};
use dash_dml::mnlistdiff::{
    BlockStatus, BlsSignatureBytes, MNLISTDIFF_CHAINLOCKS_PROTO_VERSION, MnListDiffRequest,
    MnListDiffSource, PeerOutcome, QuorumBase, QuorumKey, SMNLE_VERSIONED_PROTO_VERSION,
    SimplifiedMnListDiff, TargetBlock, build_mnlistdiff, check_request, write_sml_entry,
};
use dash_dml::{MnList, SmlRootCache, merkle_root_mn_list_full};
use dash_evo::bls::BlsCtx;
use dash_evo::cbtx::{CoinbaseFacts, nonnull_coinbase_chainlock};
use dash_evo::sml::merkle_root_mn_list_from_leaves;
use dash_primitives::{BlockHash, SpecialPayload};
use serde_json::Value;

/// Every chain's DIP3 height (`-dip3params=2:2`).
const DIP3_HEIGHT: i32 = 2;

/// The fixtures and each chain's V20 height: `-testactivationheight=v20@100`, or regtest's
/// default, `DIP0003Height` before `-dip3params` moves it (chainparams.cpp v24:818,828).
const CHAINS: &[(&str, i32)] = &[
    ("feature_asset_locks", 100),
    ("feature_masternode_payout_shares", 100),
    ("feature_masternode_shares", 100),
    ("feature_protx_version", 100),
    ("rpc_netinfo", 432),
];

struct Chain {
    name: &'static str,
    v20_height: i32,
    /// Block hashes by height, internal byte order.
    hashes: Vec<Hash256>,
    heights: HashMap<Hash256, i32>,
    coinbases: Vec<Vec<u8>>,
    txids: HashMap<Hash256, Vec<[u8; 32]>>,
    records: Vec<(Vec<u8>, Vec<u8>)>,
    image: EvoDbImage,
    /// `q_mc` rows: the mined commitment of each `(llmqType, quorumHash)`.
    commitments: HashMap<QuorumKey, Vec<u8>>,
    /// `(base, block, protocol version, payload)` of each rc.3 reply.
    replies: Vec<(Hash256, Hash256, i32, Vec<u8>)>,
}

impl Ancestry for Chain {
    fn header(&self, block: &Hash256) -> Option<(i32, Option<Hash256>)> {
        let height = *self.heights.get(block)?;
        let prev = usize::try_from(height - 1).ok().map(|h| self.hashes[h]);
        Some((height, prev))
    }
}

fn unhex(hex: &str) -> Vec<u8> {
    (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect()
}

/// A hash as RPC displays it, in internal byte order.
fn display_hash(hex: &str) -> Hash256 {
    let mut hash: Hash256 = unhex(hex).try_into().unwrap();
    hash.reverse();
    hash
}

fn load(name: &'static str, v20_height: i32) -> Chain {
    let path = format!("{}/tests/data/evodb/{name}.json.gz", env!("CARGO_MANIFEST_DIR"));
    let mut json = String::new();
    flate2::read::GzDecoder::new(std::fs::File::open(&path).unwrap())
        .read_to_string(&mut json)
        .unwrap();
    let fixture: Value = serde_json::from_str(&json).unwrap();
    let strings = |key: &str| -> Vec<String> {
        fixture[key].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_owned()).collect()
    };

    let hashes: Vec<Hash256> = strings("hashes").iter().map(|h| display_hash(h)).collect();
    let heights = hashes.iter().enumerate().map(|(h, hash)| (*hash, h as i32)).collect();
    let coinbases = strings("coinbases").iter().map(|c| unhex(c)).collect();
    let txids = fixture["txids"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(block, txids)| {
            let txids = txids.as_array().unwrap().iter().map(|t| display_hash(t.as_str().unwrap()));
            (display_hash(block), txids.collect())
        })
        .collect();

    let records: Vec<(Vec<u8>, Vec<u8>)> = fixture["evodb"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (unhex(r[0].as_str().unwrap()), unhex(r[1].as_str().unwrap())))
        .collect();
    // Core: llmq/blockprocessor.cpp:46 DB_MINED_COMMITMENT; the value is (qc, minedBlockHash).
    let commitments = records
        .iter()
        .filter(|(key, _)| key.starts_with(b"\x04q_mc"))
        .map(|(key, value)| {
            let quorum = (key[5], key[6..38].try_into().unwrap());
            (quorum, value[..value.len() - 32].to_vec())
        })
        .collect();
    let image = EvoDbImage::from_records(records.clone()).unwrap();

    let replies = fixture["replies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                display_hash(r[0].as_str().unwrap()),
                display_hash(r[1].as_str().unwrap()),
                r[2].as_i64().unwrap() as i32,
                unhex(r[3].as_str().unwrap()),
            )
        })
        .collect();
    Chain {
        name,
        v20_height,
        hashes,
        heights,
        coinbases,
        txids,
        records,
        image,
        commitments,
        replies,
    }
}

fn chains() -> impl Iterator<Item = Chain> {
    CHAINS.iter().map(|&(name, v20)| load(name, v20))
}

fn coinbase_tx(chain: &Chain, height: i32) -> TxRef {
    Arc::new(decode_tx_exact(&chain.coinbases[height as usize]).unwrap().into_value())
}

/// The block's CbTx `merkleRootMNList`. rpc_netinfo's first 199 blocks come from the test
/// framework's cache, mined before its `-dip3params=2:2` moved DIP3 below them, so they have none.
fn cbtx_mn_root(chain: &Chain, height: i32) -> Option<MerkleRoot> {
    match &coinbase_tx(chain, height).payload()?.unwrap().payload {
        SpecialPayload::CoinbaseCommitment(cbtx) => Some(cbtx.merkle_root_mn_list),
        other => panic!("{} {height}: {other:?}", chain.name),
    }
}

fn list_at(chain: &Chain, height: i32) -> MnList {
    chain.image.list_at(chain, &chain.hashes[height as usize], DIP3_HEIGHT).unwrap()
}

/// Every block's list, its root in full and through a cache walked forward, against its CbTx;
/// then the cache jumping between lists in a scrambled order.
#[test]
fn imported_lists_match_every_cbtx_mn_root() {
    for chain in chains() {
        let tip = chain.hashes.len() as i32 - 1;
        let lists: Vec<MnList> = (0..=tip).map(|h| list_at(&chain, h)).collect();
        let mut cache = SmlRootCache::new();
        for h in DIP3_HEIGHT..=tip {
            let list = &lists[h as usize];
            assert_eq!(list.height(), h, "{} {h}", chain.name);
            assert_eq!(list.block_hash().as_bytes(), &chain.hashes[h as usize]);
            let Some(expected) = cbtx_mn_root(&chain, h) else {
                assert!(chain.name == "rpc_netinfo" && h < 200, "{} {h}", chain.name);
                continue;
            };
            assert_eq!(merkle_root_mn_list_full(list).unwrap(), expected, "{} {h}", chain.name);
            assert_eq!(list.project_sml().merkle_root().unwrap(), expected, "{} {h}", chain.name);
            assert_eq!(cache.merkle_root(list).unwrap(), expected, "{} {h} cached", chain.name);
        }
        // A fixed scramble: each step jumps 7/13 of the chain on, so it goes back as often as on.
        let span = (tip - DIP3_HEIGHT + 1) as usize;
        let mut at = 0;
        for _ in 0..span {
            at = (at + span * 7 / 13 + 1) % span;
            let h = DIP3_HEIGHT + at as i32;
            let Some(expected) = cbtx_mn_root(&chain, h) else {
                continue;
            };
            assert_eq!(
                cache.merkle_root(&lists[h as usize]).unwrap(),
                expected,
                "{} jump to {h}",
                chain.name
            );
        }
    }
}

/// Quirk 17: the SML leaf is the SHA256d of the entry without `nVersion`, an empty list's root is
/// zero, and a mutated tree is invalid (`src/evo/simplifiedmns.h:72-75`,
/// `src/evo/simplifiedmns.cpp:42-49,126-144`). Checked against Core's CbTx roots: a one-entry
/// list's root is its leaf, and the empty list after DIP3 has the zero root. Entries of a list
/// have distinct proTxHashes, so no list's tree is mutated (EV4-serve D4a); the tree function
/// refuses one built from repeated leaves.
#[test]
fn quirk_17_sml_leaf_hash_and_root() {
    let (mut one, mut empty) = (0, 0);
    for chain in chains() {
        for h in DIP3_HEIGHT..chain.hashes.len() as i32 {
            let Some(root) = cbtx_mn_root(&chain, h) else {
                continue;
            };
            let list = list_at(&chain, h);
            match list.len() {
                0 => {
                    assert_eq!(root, MerkleRoot::from([0; 32]), "{} {h}", chain.name);
                    empty += 1;
                }
                1 => {
                    let entry = list.iter().next().unwrap().to_sml_entry();
                    // The band before SMNLE_VERSIONED writes the entry without nVersion.
                    let mut unversioned = Vec::new();
                    write_sml_entry(&entry, SMNLE_VERSIONED_PROTO_VERSION - 1, &mut unversioned);
                    let mut versioned = Vec::new();
                    write_sml_entry(&entry, SMNLE_VERSIONED_PROTO_VERSION, &mut versioned);
                    assert_eq!(root, MerkleRoot::from(sha256d(&unversioned)));
                    assert_ne!(root, MerkleRoot::from(sha256d(&versioned)));
                    assert_eq!(root, MerkleRoot::from(*entry.hash().as_bytes()));
                    one += 1;
                }
                _ => {}
            }
        }
    }
    assert!(one > 0 && empty > 0, "fixtures lack a one-entry ({one}) or empty ({empty}) list");
    let leaf = [7; 32];
    assert!(merkle_root_mn_list_from_leaves(vec![leaf, leaf]).is_err());
    assert!(merkle_root_mn_list_from_leaves(vec![[1; 32], [2; 32], leaf, leaf]).is_err());
}

/// Quirk 41: Core iterates a masternode list in immer's CHAMP order, not by proTxHash, so a
/// diff's `deletedMNs` follow the base list's CHAMP order (`src/evo/smldiff.cpp:144-149`,
/// `src/evo/deterministicmns.h:301-324`). rc.3's reply for rpc_netinfo's 204 -> 260 deletes two
/// masternodes, in that order and not in byte order; the full replies are pinned by
/// `mnlistdiff_matches_core_in_every_band`.
#[test]
fn quirk_41_mnlist_champ_order() {
    let chain = load("rpc_netinfo", 432);
    let (base, block) = (chain.hashes[204], chain.hashes[260]);
    let replies: Vec<&Vec<u8>> = chain
        .replies
        .iter()
        .filter(|(b, t, proto, _)| (*b, *t, *proto) == (base, block, 70242))
        .map(|(_, _, _, reply)| reply)
        .collect();
    let [reply] = replies[..] else {
        panic!("one 204 -> 260 reply at 70242")
    };

    // nVersion, baseBlockHash, blockHash, cbTxMerkleTree, cbTx, then deletedMNs (smldiff.h:64-79).
    let mut at = 2 + 32 + 32 + 4;
    at += 32 * read_compact(reply, &mut at);
    at += read_compact(reply, &mut at);
    let coinbase = &chain.coinbases[260];
    assert_eq!(&reply[at..at + coinbase.len()], &coinbase[..]);
    at += coinbase.len();
    let deleted: Vec<[u8; 32]> = (0..read_compact(reply, &mut at))
        .map(|i| reply[at + 32 * i..at + 32 * (i + 1)].try_into().unwrap())
        .collect();

    let (from, to) = (list_at(&chain, 204), list_at(&chain, 260));
    let champ: Vec<[u8; 32]> =
        from.core_order().iter().filter(|p| to.get(p).is_none()).map(|p| *p.as_bytes()).collect();
    assert_eq!(deleted.len(), 2);
    assert_eq!(deleted, champ);
    let mut sorted = deleted.clone();
    sorted.sort();
    assert_ne!(deleted, sorted, "Core's order is not byte order here");
}

/// The builder's inputs beyond the lists for one reply. The active quorum sets are LQ2's
/// (`GetMinedAndActiveCommitmentsUntilBlock`, notes D6): they come from the reply's own deleted
/// and new quorums, which is all a diff depends on (the base's set minus the target's, and the
/// reverse). The commitments come from evodb's `q_mc` rows, base heights from the chain, and
/// ChainLocks from the coinbases.
struct ReplySource<'a> {
    chain: &'a Chain,
    base: Hash256,
    block: Hash256,
    deleted: BTreeSet<QuorumKey>,
    new: BTreeSet<QuorumKey>,
}

impl MnListDiffSource for ReplySource<'_> {
    type Error = String;

    fn active_quorums(&self, block: &BlockHash) -> Result<BTreeSet<QuorumKey>, String> {
        let block = block.as_bytes();
        if *block == self.block {
            Ok(self.new.clone())
        } else if *block == self.base {
            Ok(self.deleted.clone())
        } else {
            Err(format!("unexpected block {block:?}"))
        }
    }

    fn mined_commitment(&self, quorum: &QuorumKey) -> Result<Option<Vec<u8>>, String> {
        Ok(self.chain.commitments.get(quorum).cloned())
    }

    fn quorum(&self, quorum: &QuorumKey) -> Result<Option<QuorumBase>, String> {
        let Some(commitment) = self.chain.commitments.get(quorum) else {
            return Ok(None);
        };
        // CFinalCommitment: nVersion, llmqType, quorumHash, then quorumIndex for the indexed
        // versions 2 and 4 (llmq/commitment.h v24:48-56).
        let version = u16::from_le_bytes([commitment[0], commitment[1]]);
        let quorum_index = match version {
            2 | 4 => i16::from_le_bytes([commitment[35], commitment[36]]),
            _ => 0,
        };
        let base_height = *self.chain.heights.get(&quorum.1).ok_or("quorum base not in chain")?;
        Ok(Some(QuorumBase {
            base_height,
            quorum_index,
        }))
    }

    fn coinbase_chainlock(
        &self,
        block: &BlockHash,
        height: i32,
    ) -> Result<Option<BlsSignatureBytes>, String> {
        if height < 0 {
            return Ok(None);
        }
        // The target's ancestor, walked back through the block index.
        let mut hash = *block.as_bytes();
        loop {
            let (at, prev) = self.chain.header(&hash).ok_or("block not in the index")?;
            if at == height {
                break;
            }
            match prev {
                Some(prev) if at > height => hash = prev,
                _ => return Ok(None),
            }
        }
        // The fixtures have one branch, so the ancestor is the active chain's block too.
        assert_eq!(self.chain.hashes[height as usize], hash);
        let facts = CoinbaseFacts::First(coinbase_tx(self.chain, height));
        let v20 = height >= self.chain.v20_height;
        Ok(nonnull_coinbase_chainlock(&facts, height as u32, v20, BlsCtx::BASIC).map(|(sig, _)| {
            let bytes: Vec<u8> = sig.to_bytes(BlsCtx::BASIC.scheme()).into();
            bytes.try_into().unwrap()
        }))
    }

    fn target_block(&self, block: &BlockHash) -> Option<TargetBlock> {
        Some(target_block(self.chain, block.as_bytes()))
    }
}

/// The target block's coinbase and txids, as `ReadBlockFromDisk` gives them.
fn target_block(chain: &Chain, block: &Hash256) -> TargetBlock {
    TargetBlock {
        coinbase: chain.coinbases[chain.heights[block] as usize].clone(),
        txids: chain.txids[block].clone(),
    }
}

fn read_compact(bytes: &[u8], at: &mut usize) -> usize {
    let first = bytes[*at];
    *at += 1;
    let width = match first {
        0xfd => 2,
        0xfe => 4,
        0xff => 8,
        n => return usize::from(n),
    };
    let mut value = [0; 8];
    value[..width].copy_from_slice(&bytes[*at..*at + width]);
    *at += width;
    u64::from_le_bytes(value) as usize
}

/// The quorums a reply deletes and adds, read after `at` (the end of its masternode list); each
/// new quorum's bytes must be its `q_mc` commitment.
fn reply_quorums(
    chain: &Chain,
    reply: &[u8],
    mut at: usize,
) -> (BTreeSet<QuorumKey>, BTreeSet<QuorumKey>) {
    let mut deleted = BTreeSet::new();
    for _ in 0..read_compact(reply, &mut at) {
        deleted.insert((reply[at], reply[at + 1..at + 33].try_into().unwrap()));
        at += 33;
    }
    let mut new = BTreeSet::new();
    for _ in 0..read_compact(reply, &mut at) {
        let quorum: QuorumKey = (reply[at + 2], reply[at + 3..at + 35].try_into().unwrap());
        let commitment = &chain.commitments[&quorum];
        assert_eq!(&reply[at..at + commitment.len()], &commitment[..], "{}", chain.name);
        at += commitment.len();
        new.insert(quorum);
    }
    (deleted, new)
}

/// Each rc.3 `mnlistdiff` reply, rebuilt from the imported lists and compared byte for byte.
#[test]
fn mnlistdiff_matches_core_in_every_band() {
    let mut bands = BTreeSet::new();
    let mut with_quorums = 0;
    let mut with_cl_sigs = 0;
    for chain in chains() {
        let mut lists: HashMap<Hash256, MnList> = HashMap::new();
        let mut list = |hash: &Hash256| {
            lists
                .entry(*hash)
                .or_insert_with(|| chain.image.list_at(&chain, hash, DIP3_HEIGHT).unwrap())
                .clone()
        };
        for (requested_base, block, proto, reply) in &chain.replies {
            // Core: evo/smldiff.cpp:162-169 (a null base is genesis).
            let base_block = if *requested_base == [0; 32] {
                chain.hashes[0]
            } else {
                *requested_base
            };
            let (base_list, block_list) = (list(&base_block), list(block));
            let height = chain.heights[block];
            let request = MnListDiffRequest {
                requested_base: BlockHash::from(*requested_base),
                base_block: BlockHash::from(base_block),
                base_list: &base_list,
                block: BlockHash::from(*block),
                list: &block_list,
                v20_active_after: height + 1 >= chain.v20_height,
            };

            // The diff without quorums ends with three empty vectors (two below the ChainLock
            // band); the reply's quorums start where they would.
            let prefix = build_list_half(&request, target_block(&chain, block)).to_bytes(*proto);
            let tail = if *proto >= MNLISTDIFF_CHAINLOCKS_PROTO_VERSION {
                3
            } else {
                2
            };
            let prefix = &prefix[..prefix.len() - tail];
            let what =
                format!("{} base {requested_base:02x?} block {height} proto {proto}", chain.name);
            assert_eq!(&reply[..prefix.len().min(reply.len())], prefix, "{what}: list half");
            let (deleted, new) = reply_quorums(&chain, reply, prefix.len());
            with_quorums += usize::from(!new.is_empty() || !deleted.is_empty());

            let source = ReplySource {
                chain: &chain,
                base: base_block,
                block: *block,
                deleted,
                new,
            };
            let diff = build_mnlistdiff(&request, &source).unwrap();
            with_cl_sigs += usize::from(!diff.quorums_cl_sigs.is_empty());
            assert_eq!(&diff.to_bytes(*proto), reply, "{what}");
            bands.insert(*proto);
        }
    }
    assert_eq!(
        bands.into_iter().collect::<Vec<_>>(),
        [70221, 70225, 70227, 70228, 70229, 70230, 70242]
    );
    assert!(with_quorums > 0 && with_cl_sigs > 0, "quorums {with_quorums}, CL sigs {with_cl_sigs}");
}

/// The diff's list half: what [`build_mnlistdiff`] gives with no quorums.
fn build_list_half(request: &MnListDiffRequest<'_>, target: TargetBlock) -> SimplifiedMnListDiff {
    struct NoQuorums(TargetBlock);
    impl MnListDiffSource for NoQuorums {
        type Error = String;
        fn active_quorums(&self, _: &BlockHash) -> Result<BTreeSet<QuorumKey>, String> {
            Ok(BTreeSet::new())
        }
        fn mined_commitment(&self, _: &QuorumKey) -> Result<Option<Vec<u8>>, String> {
            Ok(None)
        }
        fn quorum(&self, _: &QuorumKey) -> Result<Option<QuorumBase>, String> {
            Ok(None)
        }
        fn coinbase_chainlock(
            &self,
            _: &BlockHash,
            _: i32,
        ) -> Result<Option<BlsSignatureBytes>, String> {
            Ok(None)
        }
        fn target_block(&self, _: &BlockHash) -> Option<TargetBlock> {
            Some(self.0.clone())
        }
    }
    build_mnlistdiff(request, &NoQuorums(target)).unwrap()
}

/// rc.3's outcome for each request it refuses, from `contrib/node/ev4-serve/negative.py` on a
/// copy of this chain's datadir with one side block mined (PROVENANCE.md): it answers none of
/// them and scores each `Misbehaving(1)`. `check_request` gives the same refusal, text and
/// outcome; the two requests Core answered pass it.
#[test]
fn refusals_match_core() {
    let chain = load("feature_masternode_payout_shares", 100);
    // That node's `getchaintips`: a valid side block and a block the test invalidated.
    let side: HashMap<Hash256, i32> = [
        (display_hash("6ba025e952588919d510e4772502191f6c59591f2de1e9569b042dd8f66148d3"), 127),
        (display_hash("5fb40c8313c1a5690edb9f705e01f2b85bd7534b63ef07f55664648f024eb5ce"), 123),
    ]
    .into();
    let status = |hash: &Hash256| {
        let active = chain.heights.get(hash).map(|&height| (height, true));
        active.or(side.get(hash).map(|&height| (height, false))).map(|(height, in_active_chain)| {
            BlockStatus {
                height,
                in_active_chain,
                have_data: true,
            }
        })
    };
    let path = format!(
        "{}/tests/data/evodb/refusals-feature_masternode_payout_shares.jsonl",
        env!("CARGO_MANIFEST_DIR")
    );
    let (mut answered, mut refused) = (0, 0);
    for line in std::fs::read_to_string(path).unwrap().lines() {
        let case: Value = serde_json::from_str(line).unwrap();
        let name = case["case"].as_str().unwrap();
        let base = display_hash(case["base"].as_str().unwrap());
        let block = display_hash(case["block"].as_str().unwrap());
        let base_status = if base == [0; 32] {
            status(&chain.hashes[0])
        } else {
            status(&base)
        };
        let result = check_request::<String>(
            &BlockHash::from(base),
            base_status,
            &BlockHash::from(block),
            status(&block),
        );
        let log: Vec<&str> =
            case["log"].as_array().unwrap().iter().map(|l| l.as_str().unwrap()).collect();
        if case["answered"].as_bool().unwrap() {
            assert_eq!(result, Ok(()), "{name}");
            assert!(log.is_empty(), "{name}: {log:?}");
            answered += 1;
            continue;
        }
        let error = result.expect_err(name);
        let [line] = log[..] else {
            panic!("{name}: {log:?}")
        };
        let (score, text) = line.split_once(": getmnlistdiff failed for ").unwrap();
        assert!(score.starts_with("[net] Misbehaving: peer=") && score.ends_with("(0 -> 1)"));
        assert_eq!(text.split_once(". error=").unwrap().1, error.to_string(), "{name}");
        assert_eq!(error.peer_outcome(), PeerOutcome::Misbehaving(1), "{name}");
        refused += 1;
    }
    assert_eq!((answered, refused), (2, 7));
}

/// Core writes a snapshot every 576 blocks next to that block's diff. Without the diffs, the
/// snapshots alone give the same lists at their blocks.
#[test]
fn snapshot_rows_alone_give_their_lists() {
    let chain = load("feature_masternode_shares", 100);
    let snapshots = chain.records.iter().filter(|(key, _)| key.starts_with(b"\x06dmn_S3"));
    let image = EvoDbImage::from_records(snapshots.cloned()).unwrap();
    let mut listed = 0;
    for h in [576, 1152, 1728] {
        let block = chain.hashes[h];
        let alone = image.list_at(&chain, &block, DIP3_HEIGHT).unwrap();
        let full = list_at(&chain, h as i32);
        assert!(alone.is_equal(&full) && alone.len() == full.len(), "{h}");
        assert_eq!(alone.height(), h as i32);
        listed += full.len();
    }
    assert!(listed > 0);
}

/// A snapshot whose list names another block or height than its key, or whose registration
/// counter is not above its ids, is refused (EV4-serve D15; Sol r1 probe): such a list could
/// still match the CbTx root, and a diff would then carry the wrong block hash.
#[test]
fn mis_keyed_snapshots_are_refused() {
    use dash_coreimport::Error;

    // Sol's probe: key hash [1;32] at height 576, the empty list inside names [2;32] at 575.
    struct One;
    impl Ancestry for One {
        fn header(&self, block: &Hash256) -> Option<(i32, Option<Hash256>)> {
            (*block == [1; 32]).then_some((576, Some([0; 32])))
        }
    }
    let empty_list = |hash: [u8; 32], height: i32| {
        [&hash[..], &height.to_le_bytes(), &0u32.to_le_bytes(), &[0]].concat()
    };
    let key = [&b"\x06dmn_S3"[..], &[1; 32]].concat();
    for (value_hash, value_height) in [([2; 32], 575), ([2; 32], 576), ([1; 32], 575)] {
        let image = EvoDbImage::from_records([(key.clone(), empty_list(value_hash, value_height))])
            .unwrap();
        let reason = if value_hash != [1; 32] {
            "names block"
        } else {
            "at height"
        };
        match image.list_at(&One, &[1; 32], DIP3_HEIGHT) {
            Err(Error::Inconsistent(text)) => assert!(text.contains(reason), "{text}"),
            other => panic!("{reason}: {other:?}"),
        }
    }
    let image = EvoDbImage::from_records([(key, empty_list([1; 32], 576))]).unwrap();
    assert_eq!(image.list_at(&One, &[1; 32], DIP3_HEIGHT).unwrap().height(), 576);

    // Core's own snapshot rows, each header field rewritten in turn.
    let chain = load("feature_masternode_shares", 100);
    let (s3_key, s3) = chain
        .records
        .iter()
        .filter(|(key, _)| key.starts_with(b"\x06dmn_S3"))
        .find(|(_, value)| value[40] != 0)
        .map(|(key, value)| (key.clone(), value.clone()))
        .expect("a snapshot with masternodes");
    let block: Hash256 = s3_key[7..].try_into().unwrap();
    let read = |value: Vec<u8>| {
        EvoDbImage::from_records([(s3_key.clone(), value)]).unwrap().list_at(
            &chain,
            &block,
            DIP3_HEIGHT,
        )
    };
    let max_id = read(s3.clone()).unwrap().iter().map(|dmn| dmn.internal_id).max().unwrap() as u32;
    let rewritten = |at: std::ops::Range<usize>, bytes: &[u8]| {
        let mut value = s3.clone();
        value[at].copy_from_slice(bytes);
        value
    };
    // The lowest counter Core could have written is one above the highest id.
    assert!(read(rewritten(36..40, &(max_id + 1).to_le_bytes())).is_ok());
    let height = i32::from_le_bytes(s3[32..36].try_into().unwrap());
    for (value, reason) in [
        (rewritten(0..32, &chain.hashes[0]), "the list names block"),
        (rewritten(32..36, &(height - 1).to_le_bytes()), "the list is at height"),
        (rewritten(32..36, &(height + 1).to_le_bytes()), "the list is at height"),
        (rewritten(36..40, &max_id.to_le_bytes()), "not below the registered count"),
        (rewritten(36..40, &0u32.to_le_bytes()), "not below the registered count"),
    ] {
        match read(value) {
            Err(Error::Inconsistent(text)) => assert!(text.contains(reason), "{text}"),
            other => panic!("{reason}: {other:?}"),
        }
    }
}

/// Rows the importer refuses rather than read as a different list (EV4-serve D11, D15), and rows
/// it skips.
#[test]
fn damaged_and_legacy_rows_are_refused() {
    use dash_coreimport::Error;
    let chain = load("feature_masternode_payout_shares", 100);
    let key = |name: &[u8], block: &[u8]| [&[name.len() as u8][..], name, block].concat();
    let tip = *chain.hashes.last().unwrap();

    // Legacy diffs: v24 migrates them at startup.
    let legacy = EvoDbImage::from_records([(key(b"dmn_D3", &tip), vec![])]);
    assert!(matches!(legacy, Err(Error::Inconsistent(_))), "{legacy:?}");
    // A key whose suffix is not one block hash.
    let short = EvoDbImage::from_records([(key(b"dmn_D4", &tip[..31]), vec![])]);
    assert!(matches!(short, Err(Error::Malformed { .. })), "{short:?}");
    // Other names, empty keys and truncated names are other importers' rows.
    let skipped = EvoDbImage::from_records([
        (key(b"q_mc", &[0; 33]), vec![1]),
        (vec![], vec![]),
        (vec![9, b'd'], vec![]),
    ])
    .unwrap();
    assert_eq!(skipped.list_at(&chain, &tip, DIP3_HEIGHT).unwrap().len(), 0);

    // A diff with a byte after it (this chain has no snapshot row).
    let (d4_key, d4) = chain
        .records
        .iter()
        .find(|(key, _)| key.starts_with(b"\x06dmn_D4"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .unwrap();
    let block: Hash256 = d4_key[7..].try_into().unwrap();
    let trailing = EvoDbImage::from_records([(d4_key.clone(), [&d4[..], &[0]].concat())]).unwrap();
    let refused = trailing.list_at(&chain, &block, DIP3_HEIGHT);
    assert!(matches!(refused, Err(Error::Malformed { .. })), "{refused:?}");
    assert!(EvoDbImage::from_records([(d4_key, d4)]).unwrap().list_at(&chain, &block, 2).is_ok());

    // A diff on genesis, read with DIP3 at genesis.
    let genesis = EvoDbImage::from_records([(key(b"dmn_D4", &chain.hashes[0]), vec![])]).unwrap();
    let refused = genesis.list_at(&chain, &chain.hashes[0], 0);
    assert!(matches!(refused, Err(Error::Inconsistent(_))), "{refused:?}");
}
