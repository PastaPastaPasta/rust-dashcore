//! CH7b-2 C16 (PLAN §5.3, T-TN): testnet connects through its first non-null commitments.
//!
//! `CH7B2_TESTNET_WINDOW` names a directory whose `blocks/` holds a testnet datadir's block files
//! (a reflink copy of O8's stopped node's). The node connects genesis to [`H_END`] under
//! `-assumevalid=0`, so every block runs its script checks and with them the CbTx's root checks
//! (`fCheckCbTxMerkleRoots`, v24.0.0-rc.4 `validation.cpp:2669-2671`): step 8's MN-list root at
//! exactly the heights whose coinbase is a CbTx (`evo/specialtxman.cpp:984-996`), step 9's quorum
//! root at exactly those whose CbTx is version 2 or more (`:1003-1011`; none below DIP0008's
//! 78,800), and none skipped. The first non-null commitment, an LLMQ_50_60 one mined at 7,522
//! (DIP0003 at 7,000), connects with its members, and two of its DKG intervals past it. A CbTx's
//! roots commit to the MN list and the mined quorums Core built, so each connect is a
//! differential against the chain Core accepted. Past the first rotated cycle (base 769,824), the
//! heights from the first block that mines a rotated commitment are step 9 over rotated rows.
//!
//! Without the variable the test says it skipped. `CH7B2_TESTNET_TO=<height>` runs it further:
//! the one-off run to 905,099, the last block before V20's CbTx ChainLocks, whose step 10 needs
//! CH7b-3's verify.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::PathBuf;

use dash_consensus::Block;
use dash_consensus::primitives::{decode_block, decode_header};
use dash_primitives::QuorumHash;
use dashd_chainstate::chain::OnV;
use dashd_chainstate::evo::gates::counts_under;

use super::*;

/// The first non-null testnet commitment's block: LLMQ_50_60 (`dkgInterval` 24), at 7,522.
const FIRST_QC: (u32, &str) =
    (7_522, "000000001c500ea39146e911f9f7630a389af7c603ab48d89b58b5d0b424b43d");
/// [`FIRST_QC`] and two of its type's DKG intervals.
const H_END: (u32, &str) =
    (7_570, "00000000b5f7ec21d990e84ab593a3a5e42185237ebf27bdd32a08769edaa34c");
/// A height by which testnet's first rotated cycle, based at 769,824, has mined its commitments.
const ROTATED_BY: u32 = 769_824 + 288;
/// The indexed (rotated) commitment versions, legacy and basic BLS (`llmq/commitment.h:48-51`).
const INDEXED: [u16; 2] = [2, 4];

/// Where a located block's bytes are.
struct Located {
    file: PathBuf,
    offset: u64,
    size: usize,
}

impl Located {
    fn read(&self) -> Arc<Block> {
        let mut f = File::open(&self.file).unwrap();
        f.seek(SeekFrom::Start(self.offset)).unwrap();
        let mut raw = vec![0; self.size];
        f.read_exact(&mut raw).unwrap();
        let block = decode_block(&mut raw.as_slice()).expect("a block");
        Arc::new(block.into_value())
    }
}

/// The chain from genesis to the first block met at height `to`, located in `blocks`'s
/// `blk?????.dat` files, read in file order: each record's header is hashed and linked to its
/// parent, so the heights follow from genesis.
fn locate(blocks: &Path, magic: [u8; 4], genesis: BlockHash, to: u32) -> Vec<(BlockHash, Located)> {
    assert!(!blocks.join("xor.dat").exists(), "an obfuscated copy");
    let mut files: Vec<PathBuf> = std::fs::read_dir(blocks)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_str().unwrap();
            name.starts_with("blk") && name.ends_with(".dat")
        })
        .collect();
    files.sort();
    let mut height = HashMap::from([(genesis, 0_u32)]);
    let mut parent: HashMap<BlockHash, BlockHash> = HashMap::new();
    let mut at: HashMap<BlockHash, Located> = HashMap::new();
    let mut waiting: HashMap<BlockHash, Vec<BlockHash>> = HashMap::new();
    let mut target = None;
    'files: for file in files {
        let bytes = std::fs::read(&file).unwrap();
        let mut i = 0;
        while i + 8 <= bytes.len() && bytes[i..i + 4] != [0; 4] {
            assert_eq!(bytes[i..i + 4], magic, "{}@{i}: a record", file.display());
            let size = u32::from_le_bytes(bytes[i + 4..i + 8].try_into().unwrap()) as usize;
            let offset = i + 8;
            i = offset + size;
            let Some(raw) = bytes.get(offset..offset + 80) else {
                break;
            };
            let header = decode_header(&mut &raw[..]).expect("a header").into_value();
            let (hash, prev) = (header.hash(), header.prev_hash());
            let file = file.clone();
            at.insert(
                hash,
                Located {
                    file,
                    offset: offset as u64,
                    size,
                },
            );
            parent.insert(hash, prev);
            let Some(&h) = height.get(&prev) else {
                waiting.entry(prev).or_default().push(hash);
                continue;
            };
            // `hash` and every block waiting on it, at their heights.
            let mut linked = vec![(hash, h + 1)];
            while let Some((hash, h)) = linked.pop() {
                height.insert(hash, h);
                if h == to {
                    target = Some(hash);
                    break 'files;
                }
                let children = waiting.remove(&hash).unwrap_or_default();
                linked.extend(children.into_iter().map(|c| (c, h + 1)));
            }
        }
    }
    let mut chain = vec![target.unwrap_or_else(|| panic!("no block at {to}"))];
    while chain.len() <= to as usize {
        chain.push(parent[chain.last().unwrap()]);
    }
    assert_eq!(chain.last(), Some(&genesis), "linked to genesis");
    chain.reverse();
    // Genesis's own record is in the files too.
    chain.into_iter().map(|hash| (hash, at.remove(&hash).expect("located"))).collect()
}

/// The coinbase's CbTx version: `None` for a coinbase that is no CbTx (`nVersion` 3, type 5).
fn cbtx_version(block: &Block) -> Option<u16> {
    let cb = block.txs()[0].to_mutable();
    let special = cb.version >= 3 && cb.tx_type == TxType::new(5);
    special.then(|| u16::from_le_bytes(cb.extra_payload[..2].try_into().unwrap()))
}

/// The LLMQ type, quorum hash and commitment version of each of `block`'s non-null commitments
/// (type 6): its payload's version and height, then the commitment's version, type and quorum
/// hash (`llmq/commitment.h:95-110`, `:158-161`).
fn non_null_commitments(block: &Block) -> Vec<(dash_params::LlmqType, QuorumHash, u16)> {
    let commitment = |tx: &Arc<Tx>| {
        let tx = tx.to_mutable();
        let p = &tx.extra_payload;
        let null = p.len() < 192 || p[p.len() - 192..].iter().all(|&b| b == 0);
        (tx.version >= 3 && tx.tx_type == TxType::new(6) && !null).then(|| {
            let version = u16::from_le_bytes(p[6..8].try_into().unwrap());
            let hash: [u8; 32] = p[9..41].try_into().unwrap();
            (dash_params::LlmqType::new(p[8]), QuorumHash::from(hash), version)
        })
    };
    block.txs().iter().filter_map(commitment).collect()
}

/// Where `got` and `want` part, for an assertion's message: a run to 905,099 records some 900,000
/// heights.
fn parting(got: &[u32], want: &[u32]) -> String {
    let at = got.iter().zip(want).position(|(g, w)| g != w).unwrap_or(got.len().min(want.len()));
    let (g, w) = (got.get(at), want.get(at));
    format!("{} heights, want {}; at #{at} {g:?}, want {w:?}", got.len(), want.len())
}

/// PLAN §5.3's T-TN.
#[test]
fn testnet_connects_through_its_first_non_null_commitments() {
    let Some(window) = std::env::var_os("CH7B2_TESTNET_WINDOW").map(PathBuf::from) else {
        eprintln!(
            "\n*** SKIPPED testnet_connects_through_its_first_non_null_commitments: \
             CH7B2_TESTNET_WINDOW is not set. Set it to a directory whose blocks/ holds a \
             testnet datadir's block files (PLAN §5.3). ***\n"
        );
        return;
    };
    let to = std::env::var("CH7B2_TESTNET_TO").map_or(H_END.0, |t| t.parse().expect("a height"));
    let probe = tempfile::tempdir().unwrap();
    let flags = ["-testnet", "-printtoconsole=0"];
    let p = NodeSut::new(probe.path(), &flags, MOCKTIME, ui()).unwrap().params.chain.clone();
    let genesis = dashd_chainstate::chain::genesis_block(&p).hash();
    let chain = locate(&window.join("blocks"), p.message_start, genesis, to);
    let hash = |h: u32| chain[h as usize].0;
    for (h, want) in [FIRST_QC, H_END].into_iter().filter(|&(h, _)| h <= to) {
        assert_eq!(hash(h).to_string(), want, "the located block at {h}");
    }

    // A day past the last block, so none is too far ahead of the clock.
    let last = chain.last().unwrap().1.read();
    let mocktime = i64::from(last.header().time()) + 86_400;
    let dir = tempfile::tempdir().unwrap();
    let argv = ["-testnet", "-printtoconsole=0", "-assumevalid=0"];
    let mut sut = NodeSut::new(dir.path(), &argv, mocktime, ui()).expect("accepted");
    imported(&mut sut);
    let (mut s8, mut s9, mut first, mut first_rotated) = (Vec::new(), Vec::new(), None, None);
    for (h, (hash, located)) in (0_u32..).zip(&chain).skip(1) {
        let b = located.read();
        assert_eq!(b.hash(), *hash, "{h}: the located bytes");
        if let Some(version) = cbtx_version(&b) {
            s8.push(h);
            if version >= 2 {
                s9.push(h);
            }
        }
        let commitments = non_null_commitments(&b);
        if first.is_none() {
            first = commitments.first().map(|&(llmq_type, quorum, _)| (h, (llmq_type, quorum)));
        }
        if first_rotated.is_none() && commitments.iter().any(|c| INDEXED.contains(&c.2)) {
            first_rotated = Some(h);
        }
        let r = sut.chain().unwrap().process_block(b, true, None, true).recv();
        let r = r.expect("V replies");
        let connected = r.accepted && r.state.is_valid() && view_tip(&sut) == *hash;
        assert!(connected, "{h}: accepted {}, {}, tip {}", r.accepted, r.state, view_tip(&sut));
    }
    assert_eq!(view_tip(&sut), hash(to));

    let roots = counts_under(dir.path()).expect("the node's cells").root_checks();
    assert!(roots.mn_list == s8, "S8: {}", parting(&roots.mn_list, &s8));
    assert!(roots.quorums == s9, "S9: {}", parting(&roots.quorums, &s9));
    let unchecked = &roots.unchecked;
    assert!(
        unchecked.is_empty(),
        "{} judged without the flag, from {:?}",
        unchecked.len(),
        unchecked.first()
    );
    // Step 9 over rotated rows: each CbTx's quorum root from the first rotated commitment's block.
    if to >= ROTATED_BY {
        assert!(first_rotated.is_some(), "a rotated commitment by {ROTATED_BY}");
    }
    let rotated = |s: &[u32]| -> Vec<u32> {
        s.iter().copied().filter(|&h| first_rotated.is_some_and(|r| h >= r)).collect()
    };
    let (s9_rotated, checked_rotated) = (rotated(&s9), rotated(&roots.quorums));
    assert!(
        checked_rotated == s9_rotated,
        "S9 rotated: {}",
        parting(&checked_rotated, &s9_rotated)
    );
    eprintln!(
        "T-TN to {to} ({}): S8 {} heights from {:?}, S9 {} from {:?}, rotated {} from {:?}",
        hash(to),
        s8.len(),
        s8.first(),
        s9.len(),
        s9.first(),
        s9_rotated.len(),
        first_rotated
    );

    // The first non-null commitment, through route B at the tip: mined at its height, with its
    // members.
    let (mined_at, (llmq_type, quorum_hash)) = first.expect("a non-null commitment");
    assert_eq!(mined_at, FIRST_QC.0);
    let quorum = sut.chain().unwrap().on_v(move |o: &OnV<'_>, q: &ChainQuery<'_>| {
        let quorum = q.bind(o).quorum(llmq_type, &quorum_hash).map_err(|e| e.to_string());
        quorum.map(|got| got.map(|q| (q.mined_block, q.members.len())))
    });
    let (mined, members) = quorum.recv().expect("served").expect("no fault").expect("mined");
    assert_eq!(mined, hash(FIRST_QC.0));
    assert!(members > 0, "a non-null commitment's quorum has members");
    eprintln!(
        "T-TN: the first non-null commitment, {llmq_type:?} at {mined_at}: {members} members"
    );
}
