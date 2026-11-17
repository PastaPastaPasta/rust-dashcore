use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use dash_dml::testkit::numbered;

use super::*;

fn base() -> BlockHash {
    BlockHash::from([8; 32])
}

fn target() -> BlockHash {
    BlockHash::from([9; 32])
}
const BANDS: [i32; 6] = [70225, 70227, 70228, 70229, 70230, 70242];

fn block(byte: u8, pos: u32) -> CapturedBlock {
    CapturedBlock {
        id: BlockId(u32::from(byte)),
        hash: BlockHash::from([byte; 32]),
        data_pos: Some(BlockPos {
            file: 1,
            pos,
        }),
        have_data: true,
    }
}

fn work(byte: u8, height: i32, v20_active_at: bool) -> CapturedWorkBlock {
    CapturedWorkBlock {
        block: block(byte, u32::from(byte) * 10),
        height,
        v20_active_at,
    }
}

fn list(hash: BlockHash, height: i32, mns: std::ops::Range<u32>) -> Arc<MnList> {
    let mut list = MnList::new(hash, height, 0);
    for i in mns {
        list.try_add(numbered(i), true).unwrap();
    }
    Arc::new(list)
}

/// Pre-V20 data with no quorums: masternode 0 deleted, 2 and 3 added.
fn data(work_blocks: Vec<CapturedWorkBlock>) -> EvoVData {
    EvoVData {
        requested_base: base(),
        base_block: base(),
        block: target(),
        base_list: list(base(), 1, 0..2),
        target_list: list(target(), 2, 1..4),
        v20_active_after: false,
        bls: BlsCtx::BASIC,
        base_quorums: BTreeSet::new(),
        target_quorums: BTreeSet::new(),
        commitments: BTreeMap::new(),
        quorum_bases: BTreeMap::new(),
        captures: CapturedEvoReads::new(block(9, 90), work_blocks),
    }
}

/// Quorum `tag` whose work block is at `base_height - 8`.
fn add_quorum(data: &mut EvoVData, tag: u8, base_height: i32) {
    let key = (1, [tag; 32]);
    data.target_quorums.insert(key);
    data.commitments.insert(key, vec![tag; 3]);
    data.quorum_bases.insert(
        key,
        QuorumBase {
            base_height,
            quorum_index: 0,
        },
    );
}

/// V20 data with a deleted quorum and new quorums 1 and 3 on the work block at 10 and 2 on the
/// one at 20.
fn data_with_quorums(work_blocks: Vec<CapturedWorkBlock>) -> EvoVData {
    let mut data = data(work_blocks);
    data.v20_active_after = true;
    data.base_quorums.insert((1, [0x40; 32]));
    add_quorum(&mut data, 1, 18);
    add_quorum(&mut data, 2, 28);
    add_quorum(&mut data, 3, 18);
    data
}

fn two_work_blocks() -> Vec<CapturedWorkBlock> {
    vec![work(1, 10, true), work(2, 20, true)]
}

type ReadHook = Box<dyn FnOnce() + Send>;

/// One read: the captured id, position and hash, and the ChainLock height asked for.
type ReadCall = (BlockId, BlockPos, BlockHash, Option<i32>);

/// Blocks by hash: each block's coinbase and coinbase ChainLock are its hash byte repeated.
#[derive(Default)]
struct FakeReader {
    failed: BTreeSet<BlockHash>,
    pruned: BTreeSet<BlockHash>,
    calls: Mutex<Vec<ReadCall>>,
    before_first_read: Mutex<Option<ReadHook>>,
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
        if let Some(hook) = self.before_first_read.lock().unwrap().take() {
            hook();
        }
        self.calls.lock().unwrap().push((
            block.id,
            block.data_pos.unwrap(),
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
    data: &'a EvoVData,
    sigs: BTreeMap<i32, Option<ChainlockSignature>>,
    target: BlockHash,
}

impl MnListDiffSource for Reference<'_> {
    type Error = String;

    fn active_quorums(&self, block: &BlockHash) -> Result<BTreeSet<QuorumKey>, String> {
        Ok(if *block == self.data.block {
            self.data.target_quorums.clone()
        } else {
            self.data.base_quorums.clone()
        })
    }

    fn mined_commitment(&self, quorum: &QuorumKey) -> Result<Option<Vec<u8>>, String> {
        Ok(self.data.commitments.get(quorum).cloned())
    }

    fn quorum(&self, quorum: &QuorumKey) -> Result<Option<QuorumBase>, String> {
        Ok(self.data.quorum_bases.get(quorum).copied())
    }

    fn coinbase_chainlock(
        &self,
        _block: &BlockHash,
        height: i32,
    ) -> Result<Option<BlsSignatureBytes>, String> {
        Ok(self.sigs.get(&height).copied().flatten())
    }

    fn target_block(&self, _block: &BlockHash) -> Option<TargetBlock> {
        Some(TargetBlock {
            coinbase: coinbase_of(&self.target),
            txids: txids_of(&self.target),
        })
    }
}

fn reference(
    data: &EvoVData,
    sigs: impl IntoIterator<Item = (i32, Option<ChainlockSignature>)>,
    proto: i32,
) -> Vec<u8> {
    reference_at(data, sigs, data.block, proto)
}

fn reference_at(
    data: &EvoVData,
    sigs: impl IntoIterator<Item = (i32, Option<ChainlockSignature>)>,
    target: BlockHash,
    proto: i32,
) -> Vec<u8> {
    let source = Reference {
        data,
        sigs: sigs.into_iter().collect(),
        target,
    };
    let request = MnListDiffRequest {
        requested_base: data.requested_base,
        base_block: data.base_block,
        base_list: &data.base_list,
        block: data.block,
        list: &data.target_list,
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
fn without_cl_sigs(work_blocks: Vec<CapturedWorkBlock>) -> Vec<u8> {
    let mut data = data_with_quorums(work_blocks);
    data.v20_active_after = false;
    serve_captured(data, &FakeReader::default(), 70242).unwrap()
}

#[test]
fn captures_each_work_block_once() {
    let reads = CapturedEvoReads::new(
        block(9, 90),
        [work(1, 10, true), work(2, 20, true), work(1, 10, true)],
    );
    assert_eq!(reads.work_blocks, [work(1, 10, true), work(2, 20, true)]);
}

#[test]
fn prune_race_is_unscored() {
    let reader = FakeReader {
        pruned: [target()].into(),
        ..FakeReader::default()
    };
    let refusal = serve_captured(data(Vec::new()), &reader, 70242).unwrap_err();
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
    let refusal = serve_captured(data(Vec::new()), &FakeReader::failing(9), 70242).unwrap_err();
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
            data.commitments.remove(&key);
            "failed to build quorums diff"
        } else {
            data.quorum_bases.remove(&key);
            "failed to build quorum chainlock info"
        };
        let refusal = serve_captured(data, &reader, 70242).unwrap_err();
        assert_eq!((refusal.score, refusal.text.as_str()), (Some(1), text), "{missing}");
        assert!(reader.calls.lock().unwrap().is_empty(), "{missing}");
    }
}

/// R3-1 and Opus M3: two work blocks for three new quorums, one failed read. Each block is read
/// once, the target last, and only the failed block's quorums get the null signature.
#[test]
fn a_failed_work_block_nulls_only_its_quorums() {
    let reader = FakeReader::failing(1);
    let bytes = serve_captured(data_with_quorums(two_work_blocks()), &reader, 70242).unwrap();
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
    let bytes = serve_captured(data_with_quorums(two_work_blocks()), &reader, 70242).unwrap();
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
    let bytes = serve_captured(data_with_quorums(blocks.clone()), &reader, 70242).unwrap();
    let sigs = decode_cl_sigs(&bytes, &without_cl_sigs(blocks));
    assert!(sigs.contains(&([0; 96], vec![0, 2])), "{sigs:?}");
    assert!(sigs.contains(&([2; 96], vec![1])), "{sigs:?}");
    assert_eq!((reader.reads_of(1), reader.reads_of(2)), (0, 1));
}

/// Core's `nullptr` ancestor (a negative work height) is never captured: null signature, no
/// read.
#[test]
fn a_negative_work_height_gets_the_null_signature() {
    let mut data = data(Vec::new());
    data.v20_active_after = true;
    add_quorum(&mut data, 1, 4);
    let reader = FakeReader::default();
    let bytes = serve_captured(data, &reader, 70242).unwrap();
    assert_eq!(&bytes[bytes.len() - 100..], [&[1][..], &[0; 96], &[1, 0, 0]].concat());
    assert_eq!(reader.calls.lock().unwrap().len(), 1, "the target only");
}

/// Opus m4: a work height the capture lacks, or holds twice, fails closed as a block read.
#[test]
fn an_uncaptured_work_block_fails_closed() {
    let twice = vec![work(1, 10, true), work(3, 10, true), work(2, 20, true)];
    for blocks in [vec![work(2, 20, true)], twice] {
        let reader = FakeReader::default();
        let refusal = serve_captured(data_with_quorums(blocks), &reader, 70242).unwrap_err();
        assert_eq!(refusal.score, Some(1));
        assert_eq!(refusal.text, format!("failed to read block {} from disk", "09".repeat(32)));
        assert!(reader.calls.lock().unwrap().is_empty());
    }
}

/// R2-1 step 5: the chain moves between V and the reads. The reads use only the captured
/// hashes and positions, so the bytes are the captured blocks', not the new active chain's.
#[test]
fn a_reorg_after_capture_serves_the_captured_blocks() {
    let active = Arc::new(Mutex::new(BTreeMap::from([
        (10, BlockHash::from([1; 32])),
        (20, BlockHash::from([2; 32])),
        (30, target()),
    ])));
    let moved = Arc::clone(&active);
    let reader = FakeReader {
        before_first_read: Mutex::new(Some(Box::new(move || {
            let mut chain = moved.lock().unwrap();
            for (height, byte) in [(10, 0x51), (20, 0x52), (30, 0x59)] {
                chain.insert(height, BlockHash::from([byte; 32]));
            }
        }))),
        ..FakeReader::default()
    };
    let bytes = serve_captured(data_with_quorums(two_work_blocks()), &reader, 70242).unwrap();
    assert_eq!(active.lock().unwrap()[&30], BlockHash::from([0x59; 32]), "the chain moved");
    let captured = [(10, Some([1; 96])), (20, Some([2; 96]))];
    assert_eq!(bytes, reference(&data_with_quorums(Vec::new()), captured, 70242));
    let after = [(10, Some([0x51; 96])), (20, Some([0x52; 96]))];
    let new_chain =
        reference_at(&data_with_quorums(Vec::new()), after, BlockHash::from([0x59; 32]), 70242);
    assert_ne!(bytes, new_chain);
    let calls = reader.calls.lock().unwrap();
    let key = |id: BlockId, pos: BlockPos, hash: BlockHash| (id, pos.file, pos.pos, hash);
    let read: BTreeSet<_> = calls.iter().map(|call| key(call.0, call.1, call.2)).collect();
    let expected: BTreeSet<_> = [block(1, 10), block(2, 20), block(9, 90)]
        .iter()
        .map(|b| key(b.id, b.data_pos.unwrap(), b.hash))
        .collect();
    assert_eq!(read, expected);
}

/// The reply is the builder's bytes at the peer's band, at every band edge.
#[test]
fn the_reply_is_the_builders_bytes_at_every_band() {
    let sigs = [(10, Some([1; 96])), (20, Some([2; 96]))];
    for proto in BANDS {
        let output =
            serve_captured(data_with_quorums(two_work_blocks()), &FakeReader::default(), proto)
                .unwrap();
        assert_eq!(output, reference(&data_with_quorums(Vec::new()), sigs, proto), "{proto}");
    }
}

/// The node's source before RP2a-P2: a checked request ends silently, with no V turn.
#[test]
fn the_unwired_source_ends_silently() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = super::super::amb::node(dir.path(), &["-connect=0", "-dnsseed=0"]);
    node.stop_net();
    let v = node.chain().unwrap().handle().clone();
    let chain = Arc::new(super::super::chain::NodeChain::new(v.clone(), None));
    let mut serve = NodeEvoServe::new(v, chain);
    assert_eq!(serve.mnlistdiff(base(), target(), 70242), Ok(None));
}

struct FakeVSource {
    data: Mutex<Option<Result<EvoVData, ServeRefusal>>>,
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
    ) -> Result<EvoVData, ServeRefusal> {
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
    let mut node = super::super::amb::node(dir.path(), &["-connect=0", "-dnsseed=0"]);
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
