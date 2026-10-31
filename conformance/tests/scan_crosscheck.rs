//! The scan's hand-parsed payload fields against `dashcore`'s decoders, over real testnet
//! blocks: the first and the last blk files of the shared testnet oracle, opened read-only.
//! Skipped where that directory is absent.

use std::fs::File;
use std::path::Path;

use dash_conformance::blkfile;
use dash_conformance::scan::payload::{parse_cbtx, parse_mnhf, parse_qc};
use dash_conformance::split::split_block;
use dashcore::blockdata::transaction::special_transaction::TransactionPayload as P;

const TESTNET_BLOCKS: &str = "/work/chains/testnet/data/testnet3/blocks";
const TESTNET_MAGIC: [u8; 4] = [0xce, 0xe2, 0xca, 0xff];

#[test]
fn hand_parsers_agree_with_dashcore_on_testnet_blocks() {
    let dir = Path::new(TESTNET_BLOCKS);
    if !dir.is_dir() {
        eprintln!("{TESTNET_BLOCKS} absent; skipped");
        return;
    }
    let key = blkfile::xor_key(dir).unwrap();
    let all = blkfile::list_files(dir).unwrap();
    // The newest file may still be written; take the last complete-looking ones before it.
    let mut files: Vec<_> = all.iter().take(2).cloned().collect();
    files.extend(all.iter().rev().skip(1).take(2).cloned());
    let (records, _) = blkfile::index(&files, key, TESTNET_MAGIC).unwrap();
    let (mut cbtx, mut mnhf, mut qc, mut txs) = (0u64, 0u64, 0u64, 0u64);
    for f in &files {
        let file = File::open(&f.1).unwrap();
        for r in records.iter().filter(|r| r.file == f.0) {
            let bytes = blkfile::read_block(&file, r, key, TESTNET_MAGIC).unwrap();
            let Ok(block) = dashcore::consensus::deserialize::<dashcore::Block>(&bytes) else {
                continue;
            };
            let spans = split_block(&bytes).expect("a decodable block frames");
            assert_eq!(spans.txs.len(), block.txdata.len());
            for (span, tx) in spans.txs.iter().zip(&block.txdata) {
                txs += 1;
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
                        cbtx += 1;
                    }
                    Some(P::MnhfSignalPayloadType(m)) => {
                        let mine = parse_mnhf(payload.unwrap()).expect("an MNHF signal");
                        assert_eq!((mine.version, mine.bit), (m.version, m.version_bit));
                        mnhf += 1;
                    }
                    Some(P::QuorumCommitmentPayloadType(q)) => {
                        let mine = parse_qc(payload.unwrap()).expect("a commitment");
                        // the payload is dashcore's, byte for byte; the version and type lie at
                        // fixed offsets in it
                        let raw = payload.unwrap();
                        assert_eq!(dashcore::consensus::serialize(q), raw);
                        assert_eq!(u16::from_le_bytes([raw[6], raw[7]]), mine.version);
                        assert_eq!(raw[8], mine.llmq_type);
                        qc += 1;
                    }
                    _ => {}
                }
            }
        }
    }
    eprintln!("compared {txs} txs: {cbtx} CbTx, {mnhf} MNHF, {qc} commitments");
    assert!(cbtx > 0 && mnhf > 0 && qc > 0, "the sample lacked a payload kind: {cbtx}/{mnhf}/{qc}");
}
