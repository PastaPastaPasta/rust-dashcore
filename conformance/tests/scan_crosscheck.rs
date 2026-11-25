//! The scan's hand-parsed payload fields against `dashcore`'s decoders, over real testnet
//! blocks of the shared testnet oracle, opened read-only: the first two and the second- and
//! third-highest-numbered blk files, then, for a payload kind that sample lacks, the blocks
//! that carry it in the other files, highest-numbered file first. Skipped where that directory
//! is absent.

use std::fs::File;
use std::path::Path;

use dash_conformance::blkfile;
use dash_conformance::scan::payload::{parse_cbtx, parse_mnhf, parse_qc};
use dash_conformance::split::split_block;
use dashcore::blockdata::transaction::special_transaction::TransactionPayload as P;

const TESTNET_BLOCKS: &str = "/work/chains/testnet/data/testnet3/blocks";
const TESTNET_MAGIC: [u8; 4] = [0xce, 0xe2, 0xca, 0xff];

/// Core's `TRANSACTION_COINBASE`, `TRANSACTION_QUORUM_COMMITMENT` and `TRANSACTION_MNHF_SIGNAL`.
const CBTX: u16 = 5;
const QC: u16 = 6;
const MNHF: u16 = 7;

#[derive(Default)]
struct Counts {
    cbtx: u64,
    mnhf: u64,
    qc: u64,
    txs: u64,
}

impl Counts {
    fn lacks(&self, tx_type: u16) -> bool {
        match tx_type {
            CBTX => self.cbtx == 0,
            QC => self.qc == 0,
            MNHF => self.mnhf == 0,
            _ => false,
        }
    }

    fn complete(&self) -> bool {
        self.cbtx > 0 && self.mnhf > 0 && self.qc > 0
    }
}

/// Compares every special payload of one block; a block `dashcore` cannot decode is skipped.
fn compare_block(bytes: &[u8], n: &mut Counts) {
    let Ok(block) = dashcore::consensus::deserialize::<dashcore::Block>(bytes) else {
        return;
    };
    let spans = split_block(bytes).expect("a decodable block frames");
    assert_eq!(spans.txs.len(), block.txdata.len());
    for (span, tx) in spans.txs.iter().zip(&block.txdata) {
        n.txs += 1;
        let payload = span.payload.as_ref().map(|p| &bytes[p.clone()]);
        match tx.special_transaction_payload.as_ref() {
            Some(P::CoinbasePayloadType(c)) => {
                let mine = parse_cbtx(payload.unwrap()).expect("a CbTx");
                assert_eq!(mine.version, c.version);
                assert_eq!(mine.height as u32, c.height);
                assert_eq!(mine.has_cl, c.best_cl_height.is_some());
                if let Some(d) = c.best_cl_height {
                    assert_eq!(mine.best_cl_height_diff, d);
                }
                n.cbtx += 1;
            }
            Some(P::MnhfSignalPayloadType(m)) => {
                let mine = parse_mnhf(payload.unwrap()).expect("an MNHF signal");
                assert_eq!((mine.version, mine.bit), (m.version, m.version_bit));
                n.mnhf += 1;
            }
            Some(P::QuorumCommitmentPayloadType(q)) => {
                let mine = parse_qc(payload.unwrap()).expect("a commitment");
                // the payload is dashcore's, byte for byte; the version and type lie at
                // fixed offsets in it
                let raw = payload.unwrap();
                assert_eq!(dashcore::consensus::serialize(q), raw);
                assert_eq!(u16::from_le_bytes([raw[6], raw[7]]), mine.version);
                assert_eq!(raw[8], mine.llmq_type);
                n.qc += 1;
            }
            _ => {}
        }
    }
}

#[test]
fn hand_parsers_agree_with_dashcore_on_testnet_blocks() {
    let dir = Path::new(TESTNET_BLOCKS);
    if !dir.is_dir() {
        eprintln!("{TESTNET_BLOCKS} absent; skipped");
        return;
    }
    let key = blkfile::xor_key(dir).unwrap();
    let all = blkfile::list_files(dir).unwrap();
    // The highest-numbered file may still be written; past two files, take the last
    // complete-looking ones before it.
    let complete = &all[..all.len().saturating_sub(1)];
    let mut sample: Vec<_> = all.iter().take(2).cloned().collect();
    let newest: Vec<_> =
        complete.iter().rev().take(2).filter(|f| !sample.contains(f)).cloned().collect();
    sample.extend(newest);
    let mut n = Counts::default();
    let (records, _) = blkfile::index(&sample, key, TESTNET_MAGIC).unwrap();
    for f in &sample {
        let file = File::open(&f.1).unwrap();
        for r in records.iter().filter(|r| r.file == f.0) {
            compare_block(&blkfile::read_block(&file, r, key, TESTNET_MAGIC).unwrap(), &mut n);
        }
    }
    // MNHF signals are rare, so the sampled files may hold none: walk back through the other
    // complete files, highest-numbered first (blk numbering follows download order, not
    // height), and compare the blocks that carry a kind still missing. A block the splitter
    // cannot frame is compared too, so it fails as it would in the sample.
    let mut walked = 0;
    for f in complete.iter().rev().filter(|f| !sample.contains(f)) {
        if n.complete() {
            break;
        }
        walked += 1;
        let (records, _) = blkfile::index(std::slice::from_ref(f), key, TESTNET_MAGIC).unwrap();
        let file = File::open(&f.1).unwrap();
        for r in records.iter().rev() {
            let bytes = blkfile::read_block(&file, r, key, TESTNET_MAGIC).unwrap();
            let wanted = split_block(&bytes).map_or(true, |spans| {
                spans.txs.iter().any(|t| t.payload.is_some() && n.lacks(t.tx_type))
            });
            if wanted {
                compare_block(&bytes, &mut n);
            }
            if n.complete() {
                break;
            }
        }
    }
    let Counts {
        cbtx,
        mnhf,
        qc,
        txs,
    } = n;
    eprintln!(
        "compared {txs} txs: {cbtx} CbTx, {mnhf} MNHF, {qc} commitments ({walked} file(s) walked)"
    );
    assert!(n.complete(), "the oracle lacked a payload kind: {cbtx}/{mnhf}/{qc}");
}
