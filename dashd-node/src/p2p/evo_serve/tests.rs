use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use dash_chain::FlatFilePos;
use dash_consensus::BlockId;
use dash_consensus::primitives::Tx;
use dash_dml::MnList;
use dash_dml::mnlistdiff::PartialMerkleTree;
use dash_dml::testkit::numbered;
use dash_pkc::bls::{BlsEagerSig, BlsScIetf, BlsSchemeKind, BlsSchemeSource, BlsSecretKey};
use dash_primitives::{Transaction, TxType};
use dashd_chainstate::chain::genesis_block;
use dashd_p2p::processing::seams::{ChainAccess as _, HeaderBatch, TreeRead as _};

use super::super::amb;
use super::super::chain::NodeChain;
use super::*;
use crate::rpc::evo::sml::{QuorumReads, WorkBlock};
use crate::test_log::logged;

fn base() -> BlockHash {
    BlockHash::from([8; 32])
}

fn target() -> BlockHash {
    BlockHash::from([9; 32])
}

/// Every band edge: no `nVersion` below 70225, then smldiff.h:64-77 and simplifiedmns.h:74/86.
const BANDS: [i32; 7] = [70224, 70225, 70227, 70228, 70229, 70230, 70242];

fn block(byte: u8, height: u32) -> CapturedBlock {
    CapturedBlock {
        id: BlockId(u32::from(byte)),
        hash: BlockHash::from([byte; 32]),
        height,
        pos: Some(FlatFilePos {
            file: 1,
            pos: u32::from(byte) * 10,
        }),
    }
}

/// The work block at `height`, captured as `byte`'s.
fn work(byte: u8, height: i32, v20: bool) -> (i32, Option<WorkBlock>) {
    let block = block(byte, u32::try_from(height).unwrap());
    (
        height,
        Some(WorkBlock {
            block,
            v20,
        }),
    )
}

fn list(hash: BlockHash, height: i32, mns: std::ops::Range<u32>) -> Arc<MnList> {
    let mut list = MnList::new(hash, height, 0);
    for i in mns {
        list.try_add(numbered(i), true).unwrap();
    }
    Arc::new(list)
}

/// Pre-V20 data with no quorums: masternode 0 deleted, 2 and 3 added.
fn data(work_blocks: Vec<(i32, Option<WorkBlock>)>) -> DiffCapture {
    DiffCapture {
        requested_base: base(),
        base_block: base(),
        base_list: list(base(), 1, 0..2),
        list: list(target(), 2, 1..4),
        v20_active_after: false,
        target: block(9, 30),
        bls: BlsCtx::BASIC,
        quorums: QuorumReads {
            active: vec![Ok(BTreeSet::new()), Ok(BTreeSet::new())],
            work: work_blocks.into_iter().collect(),
            ..QuorumReads::default()
        },
    }
}

fn active_mut(data: &mut DiffCapture, at: usize) -> &mut BTreeSet<QuorumKey> {
    data.quorums.active[at].as_mut().unwrap()
}

/// Quorum `tag` whose work block is at `base_height - 8`.
fn add_quorum(data: &mut DiffCapture, tag: u8, base_height: i32) {
    let key = (1, [tag; 32]);
    active_mut(data, 1).insert(key);
    data.quorums.mined.insert(key, Ok(Some(vec![tag; 3])));
    data.quorums.bases.insert(
        key,
        Ok(Some(QuorumBase {
            base_height,
            quorum_index: 0,
        })),
    );
}

/// V20 data with a deleted quorum and new quorums 1 and 3 on the work block at 10 and 2 on the
/// one at 20.
fn data_with_quorums(work_blocks: Vec<(i32, Option<WorkBlock>)>) -> DiffCapture {
    let mut data = data(work_blocks);
    data.v20_active_after = true;
    active_mut(&mut data, 0).insert((1, [0x40; 32]));
    add_quorum(&mut data, 1, 18);
    add_quorum(&mut data, 2, 28);
    add_quorum(&mut data, 3, 18);
    data
}

fn two_work_blocks() -> Vec<(i32, Option<WorkBlock>)> {
    vec![work(1, 10, true), work(2, 20, true)]
}

fn serve(
    data: &DiffCapture,
    reader: &dyn EvoBlockReader,
    proto: i32,
) -> Result<Vec<u8>, ServeRefusal> {
    serve_captured(data, base(), target(), reader, proto)
}

/// One read: the captured id, position and hash, and the ChainLock height asked for.
type ReadCall = (BlockId, FlatFilePos, BlockHash, Option<i32>);

/// Blocks by hash: each block's coinbase and coinbase ChainLock are its hash byte repeated.
#[derive(Default)]
struct FakeReader {
    failed: BTreeSet<BlockHash>,
    pruned: BTreeSet<BlockHash>,
    calls: Mutex<Vec<ReadCall>>,
}

fn coinbase_of(hash: &BlockHash) -> Vec<u8> {
    vec![hash.as_bytes()[0]; 5]
}

fn txids_of(hash: &BlockHash) -> Vec<[u8; 32]> {
    vec![*hash.as_bytes(), [0xdd; 32]]
}

fn sig_of(hash: &BlockHash) -> ChainlockSignature {
    [hash.as_bytes()[0]; 96]
}

impl FakeReader {
    fn failing(hash: u8) -> Self {
        Self {
            failed: [BlockHash::from([hash; 32])].into(),
            ..Self::default()
        }
    }

    fn reads_of(&self, hash: u8) -> usize {
        let hash = BlockHash::from([hash; 32]);
        self.calls.lock().unwrap().iter().filter(|call| call.2 == hash).count()
    }
}

impl EvoBlockReader for FakeReader {
    fn read(
        &self,
        block: &CapturedBlock,
        chainlock_height: Option<i32>,
        _bls: BlsCtx,
    ) -> EvoBlockRead {
        self.calls.lock().unwrap().push((
            block.id,
            block.pos.unwrap(),
            block.hash,
            chainlock_height,
        ));
        if self.pruned.contains(&block.hash) {
            return EvoBlockRead::Pruned;
        }
        if self.failed.contains(&block.hash) {
            return EvoBlockRead::Failed;
        }
        EvoBlockRead::Block {
            target: TargetBlock {
                coinbase: coinbase_of(&block.hash),
                txids: txids_of(&block.hash),
            },
            chainlock: chainlock_height.map(|_| sig_of(&block.hash)),
        }
    }
}

/// The builder's own source over the same inputs, with no capture: `sigs` maps a work height
/// to its ChainLock.
struct Reference<'a> {
    data: &'a DiffCapture,
    sigs: BTreeMap<i32, Option<ChainlockSignature>>,
}

impl MnListDiffSource for Reference<'_> {
    type Error = String;

    fn active_quorums(&self, block: &BlockHash) -> Result<BTreeSet<QuorumKey>, String> {
        let at = usize::from(*block == self.data.target.hash);
        Ok(self.data.quorums.active[at].clone().unwrap())
    }

    fn mined_commitment(&self, quorum: &QuorumKey) -> Result<Option<Vec<u8>>, String> {
        Ok(self.data.quorums.mined.get(quorum).cloned().transpose().unwrap().flatten())
    }

    fn quorum(&self, quorum: &QuorumKey) -> Result<Option<QuorumBase>, String> {
        Ok(self.data.quorums.bases.get(quorum).cloned().transpose().unwrap().flatten())
    }

    fn coinbase_chainlock(
        &self,
        _block: &BlockHash,
        height: i32,
    ) -> Result<Option<BlsSignatureBytes>, String> {
        Ok(self.sigs.get(&height).copied().flatten())
    }

    fn target_block(&self, block: &BlockHash) -> Option<TargetBlock> {
        Some(TargetBlock {
            coinbase: coinbase_of(block),
            txids: txids_of(block),
        })
    }
}

fn reference(
    data: &DiffCapture,
    sigs: impl IntoIterator<Item = (i32, Option<ChainlockSignature>)>,
    proto: i32,
) -> Vec<u8> {
    let source = Reference {
        data,
        sigs: sigs.into_iter().collect(),
    };
    let request = MnListDiffRequest {
        requested_base: data.requested_base,
        base_block: data.base_block,
        base_list: &data.base_list,
        block: data.target.hash,
        list: &data.list,
        v20_active_after: data.v20_active_after,
    };
    build_mnlistdiff(&request, &source).unwrap().to_bytes(proto)
}

fn read_compact(bytes: &mut &[u8]) -> u64 {
    let (first, rest) = bytes.split_first().unwrap();
    *bytes = rest;
    let width = match first {
        0xfd => 2,
        0xfe => 4,
        0xff => 8,
        n => return u64::from(*n),
    };
    let (n, rest) = bytes.split_at(width);
    *bytes = rest;
    n.iter().rev().fold(0, |acc, b| (acc << 8) | u64::from(*b))
}

/// The `quorumsCLSigs` at the end of a 70242 reply, after `prefix`: the same reply with none.
fn decode_cl_sigs(bytes: &[u8], prefix: &[u8]) -> Vec<(ChainlockSignature, Vec<u16>)> {
    let head = &prefix[..prefix.len() - 1];
    assert_eq!(&bytes[..head.len()], head, "everything before quorumsCLSigs");
    let mut rest = &bytes[head.len()..];
    let mut sigs = Vec::new();
    for _ in 0..read_compact(&mut rest) {
        let (sig, tail) = rest.split_at(96);
        rest = tail;
        let n = read_compact(&mut rest);
        let indexes = (0..n)
            .map(|_| {
                let (index, tail) = rest.split_at(2);
                rest = tail;
                u16::from_le_bytes([index[0], index[1]])
            })
            .collect();
        sigs.push((sig.try_into().unwrap(), indexes));
    }
    assert!(rest.is_empty());
    sigs
}

/// The reply with `quorumsCLSigs` empty: the same data before V20.
fn without_cl_sigs(work_blocks: Vec<(i32, Option<WorkBlock>)>) -> Vec<u8> {
    let mut data = data_with_quorums(work_blocks);
    data.v20_active_after = false;
    serve(&data, &FakeReader::default(), 70242).unwrap()
}

#[test]
fn prune_race_is_unscored() {
    let reader = FakeReader {
        pruned: [target()].into(),
        ..FakeReader::default()
    };
    let refusal = serve(&data(Vec::new()), &reader, 70242).unwrap_err();
    assert_eq!(refusal.score, None);
    assert_eq!(
        refusal.text,
        format!(
            "block data for block {} is not available (pruned or below an unvalidated snapshot \
             base)",
            "09".repeat(32)
        )
    );
}

#[test]
fn real_io_failure_scores_one() {
    let refusal = serve(&data(Vec::new()), &FakeReader::failing(9), 70242).unwrap_err();
    assert_eq!(refusal.score, Some(1));
    assert_eq!(refusal.text, format!("failed to read block {} from disk", "09".repeat(32)));
}

/// Core reads the target last (smldiff.cpp:196-244): a quorum refusal wins over a failing
/// target read, and no block file is read.
#[test]
fn quorum_refusal_precedes_target_read_failure() {
    for missing in ["commitment", "quorum"] {
        let reader = FakeReader::failing(9);
        let mut data = data_with_quorums(two_work_blocks());
        let key = (1, [2; 32]);
        let text = if missing == "commitment" {
            data.quorums.mined.insert(key, Ok(None));
            "failed to build quorums diff"
        } else {
            data.quorums.bases.insert(key, Ok(None));
            "failed to build quorum chainlock info"
        };
        let refusal = serve(&data, &reader, 70242).unwrap_err();
        assert_eq!((refusal.score, refusal.text.as_str()), (Some(1), text), "{missing}");
        assert!(reader.calls.lock().unwrap().is_empty(), "{missing}");
    }
}

/// A storage fault the capture recorded is the builder's unscored `Source` error at the same
/// read, before any block file is read.
#[test]
fn a_captured_storage_fault_is_unscored_at_its_read() {
    let mut data = data_with_quorums(two_work_blocks());
    data.quorums.bases.insert((1, [2; 32]), Err(StoreError::read("disk")));
    let reader = FakeReader::default();
    let refusal = serve(&data, &reader, 70242).unwrap_err();
    assert_eq!((refusal.score, refusal.text), (None, StoreError::read("disk").to_string()));
    assert!(reader.calls.lock().unwrap().is_empty());
}

/// R3-1 and Opus M3: two work blocks for three new quorums, one failed read. Each block is read
/// once, the target last, and only the failed block's quorums get the null signature.
#[test]
fn a_failed_work_block_nulls_only_its_quorums() {
    let reader = FakeReader::failing(1);
    let bytes = serve(&data_with_quorums(two_work_blocks()), &reader, 70242).unwrap();
    let sigs = decode_cl_sigs(&bytes, &without_cl_sigs(two_work_blocks()));
    // New quorums in key order: 1 and 3 on the block at 10, 2 on the one at 20.
    assert_eq!(sigs.len(), 2, "{sigs:?}");
    assert!(sigs.contains(&([0; 96], vec![0, 2])), "{sigs:?}");
    assert!(sigs.contains(&([2; 96], vec![1])), "{sigs:?}");
    assert_eq!((reader.reads_of(1), reader.reads_of(2), reader.reads_of(9)), (1, 1, 1));
    assert_eq!(reader.calls.lock().unwrap().last().unwrap().2, target());
    assert_eq!(
        bytes,
        reference(&data_with_quorums(Vec::new()), [(10, None), (20, Some([2; 96]))], 70242)
    );
}

#[test]
fn work_blocks_with_signatures_keep_them() {
    let reader = FakeReader::default();
    let bytes = serve(&data_with_quorums(two_work_blocks()), &reader, 70242).unwrap();
    let sigs = decode_cl_sigs(&bytes, &without_cl_sigs(two_work_blocks()));
    assert!(sigs.contains(&([1; 96], vec![0, 2])), "{sigs:?}");
    assert!(sigs.contains(&([2; 96], vec![1])), "{sigs:?}");
}

/// `GetNonNullCoinbaseChainlock` checks V20 at the work block (cbtx.cpp:187): a work block
/// before V20 under a target after it is not read, and its quorums get the null signature.
#[test]
fn a_work_block_before_v20_is_not_read() {
    let reader = FakeReader::default();
    let blocks = vec![work(1, 10, false), work(2, 20, true)];
    let bytes = serve(&data_with_quorums(blocks.clone()), &reader, 70242).unwrap();
    let sigs = decode_cl_sigs(&bytes, &without_cl_sigs(blocks));
    assert!(sigs.contains(&([0; 96], vec![0, 2])), "{sigs:?}");
    assert!(sigs.contains(&([2; 96], vec![1])), "{sigs:?}");
    assert_eq!((reader.reads_of(1), reader.reads_of(2)), (0, 1));
}

/// Core's `nullptr` ancestor (a negative work height), which the capture holds unread: null
/// signature, no read.
#[test]
fn a_negative_work_height_gets_the_null_signature() {
    let mut data = data(vec![(-4, None)]);
    data.v20_active_after = true;
    add_quorum(&mut data, 1, 4);
    let reader = FakeReader::default();
    let bytes = serve(&data, &reader, 70242).unwrap();
    assert_eq!(&bytes[bytes.len() - 100..], [&[1][..], &[0; 96], &[1, 0, 0]].concat());
    assert_eq!(reader.calls.lock().unwrap().len(), 1, "the target only");
}

/// Opus m4: a work height the capture lacks fails closed as a block read.
#[test]
fn an_uncaptured_work_block_fails_closed() {
    let reader = FakeReader::default();
    let refusal = serve(&data_with_quorums(vec![work(2, 20, true)]), &reader, 70242).unwrap_err();
    assert_eq!(refusal.score, Some(1));
    assert_eq!(refusal.text, format!("failed to read block {} from disk", "09".repeat(32)));
    assert!(reader.calls.lock().unwrap().is_empty());
}

/// G8 review m1 / nit 2: a capture whose target, requested base, base block or target list is
/// not the request's fails closed as a block read of the requested target, with no block read.
#[test]
fn a_capture_of_another_request_fails_closed() {
    let mut other_target = data_with_quorums(two_work_blocks());
    other_target.target = block(7, 30);
    let mut other_base = data_with_quorums(two_work_blocks());
    other_base.requested_base = BlockHash::from([0; 32]);
    let mut other_base_block = data_with_quorums(two_work_blocks());
    other_base_block.base_block = BlockHash::from([7; 32]);
    let mut other_list = data_with_quorums(two_work_blocks());
    other_list.list = list(BlockHash::from([7; 32]), 2, 1..4);
    for data in [other_target, other_base, other_base_block, other_list] {
        let reader = FakeReader::default();
        let refusal = serve(&data, &reader, 70242).unwrap_err();
        assert_eq!(refusal.score, Some(1));
        assert_eq!(refusal.text, format!("failed to read block {} from disk", "09".repeat(32)));
        assert!(reader.calls.lock().unwrap().is_empty());
    }
}

/// The request check's other side: a null requested base is answered from genesis, whatever
/// block that is, and Core's default target list (a pre-DIP3 target) is the target's.
#[test]
fn a_null_base_and_a_default_list_are_the_request() {
    let mut data = data_with_quorums(two_work_blocks());
    data.requested_base = BlockHash::from([0; 32]);
    data.list = Arc::new(MnList::new(BlockHash::from([0; 32]), -1, 0));
    assert!(data.list.is_default());
    let reply =
        serve_captured(&data, BlockHash::from([0; 32]), target(), &FakeReader::default(), 70242);
    assert!(reply.is_ok(), "{reply:?}");
}

/// R2-1 step 5: the reads use only the captured ids, hashes and positions.
#[test]
fn only_the_captured_blocks_are_read() {
    let reader = FakeReader::default();
    let bytes = serve(&data_with_quorums(two_work_blocks()), &reader, 70242).unwrap();
    let captured = [(10, Some([1; 96])), (20, Some([2; 96]))];
    assert_eq!(bytes, reference(&data_with_quorums(Vec::new()), captured, 70242));
    let calls = reader.calls.lock().unwrap();
    let read: BTreeSet<_> =
        calls.iter().map(|call| (call.0, call.1.file, call.1.pos, call.2, call.3)).collect();
    let expected: BTreeSet<_> =
        [(block(1, 10), Some(10)), (block(2, 20), Some(20)), (block(9, 30), None)]
            .iter()
            .map(|(b, height)| (b.id, b.pos.unwrap().file, b.pos.unwrap().pos, b.hash, *height))
            .collect();
    assert_eq!(read, expected);
}

/// The reply is the builder's bytes at the peer's band, at every band edge.
#[test]
fn the_reply_is_the_builders_bytes_at_every_band() {
    let sigs = [(10, Some([1; 96])), (20, Some([2; 96]))];
    for proto in BANDS {
        let output =
            serve(&data_with_quorums(two_work_blocks()), &FakeReader::default(), proto).unwrap();
        assert_eq!(output, reference(&data_with_quorums(Vec::new()), sigs, proto), "{proto}");
    }
}

/// smldiff.h:66-73: at 70225 `nVersion` follows the cbTx, and below it there is none. The 70224
/// reply is the 70225 one with those two bytes removed.
#[test]
fn below_70225_the_reply_has_no_version() {
    let data = data_with_quorums(two_work_blocks());
    let reply = |proto| serve(&data, &FakeReader::default(), proto).unwrap();
    let (with, without) = (reply(70225), reply(70224));
    let mut tree = Vec::new();
    PartialMerkleTree::coinbase_only(&txids_of(&target())).encode(&mut tree);
    let cb_tx = coinbase_of(&target());
    let at = 64 + tree.len() + cb_tx.len();
    assert_eq!(&with[64..at], [tree, cb_tx].concat());
    assert_eq!(&with[at..at + 2], [1, 0]);
    assert_eq!(without, [&with[..at], &with[at + 2..]].concat());
}

/// A regtest block on `parent` whose coinbase is DIP0003's (`nVersion` 3, `nType` 5) with a
/// version-3 CbTx at `height` carrying `signature` as its best ChainLock, one block back.
fn cbtx_v3_block(
    p: &dash_params::ChainSetup,
    parent: dash_consensus::primitives::Header,
    height: u32,
    signature: [u8; 96],
) -> Arc<Block> {
    let plain = amb::branch(p, parent, height - 1, &[1], 0x2c);
    let cb = plain[0].txs()[0].to_mutable();
    let payload = [
        &3u16.to_le_bytes()[..],
        &height.to_le_bytes(),
        &[0; 64],
        &[0],
        &signature,
        &0i64.to_le_bytes(),
    ]
    .concat();
    let cb = Transaction::new(
        3,
        TxType::new(5),
        cb.inputs().clone(),
        cb.outputs().clone(),
        cb.lock_time(),
        payload,
    );
    let time = plain[0].header().raw().time();
    amb::mine(p, parent, time, 150, [vec![Arc::new(Tx::new(cb))]]).remove(0)
}

/// G8 review m2 and minor 1: `NodeBlockReader` over V's own block files. A V20 work block's
/// coinbase ChainLock is read at the work block's height under the view's scheme; at another
/// height it is none. A block without its data opens nothing and logs Core's
/// `OpenBlockFile failed` line, and is the prune race; another block at the captured position
/// logs the index read's hash-mismatch line and is a failed read.
#[test]
fn the_node_reader_reads_the_captured_block_files() {
    let dir = tempfile::tempdir().unwrap();
    let p = amb::regtest();
    let mut node = amb::node(dir.path(), &["-connect=0", "-dnsseed=0", "-fixedseeds=0"]);
    node.stop_net();
    let genesis = genesis_block(&p);
    let key = BlsSecretKey::<BlsScIetf>::from_ikm(&[7; 32]).unwrap();
    let signature = key.sign_message(b"bl-904").to_bytes();
    let legacy_signature = BlsEagerSig::decode(&signature, BlsSchemeSource::AlwaysBasic)
        .unwrap()
        .to_bytes(BlsSchemeKind::Legacy);
    assert_ne!(legacy_signature, signature);
    let work_block = cbtx_v3_block(&p, *genesis.header(), 1, signature);
    let legacy_block = cbtx_v3_block(&p, *genesis.header(), 1, legacy_signature);
    let other = amb::branch(&p, *genesis.header(), 0, &[1], 0x2d).remove(0);
    let mut files = node.take_test_block_files().expect("step 7");
    let work_pos = files.save_block(&work_block, 1, None).unwrap().unwrap();
    let legacy_pos = files.save_block(&legacy_block, 1, None).unwrap().unwrap();
    let other_pos = files.save_block(&other, 1, None).unwrap().unwrap();
    let chain = Arc::new(NodeChain::new(node.chain().unwrap().handle().clone(), Some(files)));
    for b in [&work_block, &legacy_block, &other] {
        let batch = HeaderBatch {
            headers: vec![*b.header()],
            hashes: vec![b.hash()],
        };
        assert!(chain.process_headers(batch, 7).expect("served").invalid.is_none());
    }
    let captured_at = |b: &Block, pos| CapturedBlock {
        id: chain.lookup(&b.hash()).expect("indexed"),
        hash: b.hash(),
        height: 1,
        pos: Some(pos),
    };
    let captured = captured_at(&work_block, work_pos);
    let captured_legacy = captured_at(&legacy_block, legacy_pos);
    let reader = NodeBlockReader {
        chain,
    };
    let read_from = |captured: &CapturedBlock, block: &Block, height, bls| {
        let coinbase = &block.txs()[0];
        let (read, lines) = logged(|| reader.read(captured, height, bls));
        assert!(lines.is_empty(), "{lines:?}");
        let EvoBlockRead::Block {
            target,
            chainlock,
        } = read
        else {
            panic!("the work block reads");
        };
        assert_eq!(target.coinbase, core_serialize(coinbase.as_ref()));
        assert_eq!(target.txids, [*coinbase.txid().as_bytes()]);
        chainlock
    };
    let read = |height, bls| read_from(&captured, &work_block, height, bls);
    assert_eq!(read(Some(1), BlsCtx::BASIC), Some(signature));
    assert_eq!(read(Some(2), BlsCtx::BASIC), None);
    assert_eq!(read(None, BlsCtx::BASIC), None);
    // Basic bytes are canonical only in the other scheme: the invalid object, no signature.
    assert_eq!(read(Some(1), BlsCtx::LEGACY), None);
    let legacy = read_from(&captured_legacy, &legacy_block, Some(1), BlsCtx::LEGACY);
    assert_eq!(legacy, Some(legacy_signature));

    let unread = CapturedBlock {
        pos: None,
        ..captured
    };
    let (read, lines) = logged(|| reader.read(&unread, Some(1), BlsCtx::BASIC));
    assert!(matches!(read, EvoBlockRead::Pruned));
    assert_eq!(
        lines,
        [(
            tracing::Level::ERROR,
            "ReadBlockFromDisk: OpenBlockFile failed for FlatFilePos(nFile=-1, nPos=0)".to_owned()
        )]
    );

    let moved = CapturedBlock {
        pos: Some(other_pos),
        ..captured
    };
    let (read, lines) = logged(|| reader.read(&moved, Some(1), BlsCtx::BASIC));
    assert!(matches!(read, EvoBlockRead::Failed));
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0].0, tracing::Level::ERROR, "{lines:?}");
    let mismatch = "ReadBlockFromDisk(CBlock&, CBlockIndex*): GetHash() doesn't match index for ";
    assert!(lines[0].1.starts_with(mismatch), "{lines:?}");
}

/// The node's source is RP2a-P2's capture: a request V's own view refuses comes back with
/// Core's text and score, and a valid one is answered from V's block files.
#[test]
fn the_node_source_serves_from_v() {
    let dir = tempfile::tempdir().unwrap();
    let p = amb::regtest();
    let mut node = amb::node(dir.path(), &["-connect=0", "-dnsseed=0", "-fixedseeds=0"]);
    node.stop_net();
    let main = amb::branch(&p, *genesis_block(&p).header(), 0, &[1, 1, 1], 0x2e);
    let v = node.chain().unwrap().handle().clone();
    for b in &main {
        v.process_block(Arc::clone(b), true, None, true).recv().expect("V");
    }
    let files = node.take_test_block_files().expect("step 7");
    let chain = Arc::new(NodeChain::new(v.clone(), Some(files)));
    let mut serve = NodeEvoServe::new(v, chain);

    let unknown = serve.mnlistdiff(base(), target(), 70242).unwrap_err();
    assert_eq!(
        (unknown.score, unknown.text),
        (Some(1), format!("block {} not found", "08".repeat(32)))
    );

    let null = BlockHash::from([0; 32]);
    let reply = serve.mnlistdiff(null, main[2].hash(), 70242).unwrap().expect("a reply");
    // `nVersion` 1, then the base hash as requested (smldiff.h:66-69).
    assert_eq!(&reply[..34], [&[1, 0][..], null.as_bytes()].concat());
    let coinbase = core_serialize(main[2].txs()[0].as_ref());
    assert!(reply.windows(coinbase.len()).any(|w| w == coinbase), "the target's coinbase");
}

struct FakeVSource {
    data: Mutex<Option<Result<DiffCapture, ServeRefusal>>>,
    on_v: AtomicUsize,
    caller: std::thread::ThreadId,
}

impl EvoVSource for FakeVSource {
    fn capture(
        &self,
        _on_v: &OnV<'_>,
        _query: &ChainQuery<'_>,
        requested_base: BlockHash,
        requested_block: BlockHash,
    ) -> Result<DiffCapture, ServeRefusal> {
        assert_ne!(std::thread::current().id(), self.caller, "on V's thread");
        assert_eq!((requested_base, requested_block), (base(), target()));
        self.on_v.fetch_add(1, Ordering::SeqCst);
        self.data.lock().unwrap().take().unwrap()
    }
}

/// The one-V-turn path through `with_sources` on a real V: capture on V, reads after it, the
/// builder's bytes out; a V-side refusal passes through as it is.
#[test]
fn node_evo_serve_captures_in_one_v_turn() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = amb::node(dir.path(), &["-connect=0", "-dnsseed=0"]);
    node.stop_net();
    let v = node.chain().unwrap().handle().clone();
    let refused = ServeRefusal {
        score: None,
        text: "list unavailable".into(),
    };
    for outcome in [Ok(data_with_quorums(two_work_blocks())), Err(refused.clone())] {
        let expect_ok = outcome.is_ok();
        let source = Arc::new(FakeVSource {
            data: Mutex::new(Some(outcome)),
            on_v: AtomicUsize::new(0),
            caller: std::thread::current().id(),
        });
        let reader = Arc::new(FakeReader::failing(1));
        let mut serve = NodeEvoServe::with_sources(
            v.clone(),
            Arc::clone(&source) as _,
            Arc::clone(&reader) as _,
        );
        let reply = serve.mnlistdiff(base(), target(), 70230);
        assert_eq!(source.on_v.load(Ordering::SeqCst), 1);
        if expect_ok {
            let sigs = [(10, None), (20, Some([2; 96]))];
            let expected = reference(&data_with_quorums(Vec::new()), sigs, 70230);
            assert_eq!(reply, Ok(Some(expected)));
            assert_eq!(reader.calls.lock().unwrap().len(), 3);
        } else {
            assert_eq!(reply, Err(refused.clone()));
            assert!(reader.calls.lock().unwrap().is_empty());
        }
    }
}
