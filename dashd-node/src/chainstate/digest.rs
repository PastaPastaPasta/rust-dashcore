use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use dash_args::args::abs_path_for_config_val;
use dash_args::{ArgsManager, RuntimeError};
use dash_chain::{BlockRef, ChainReader};
use dash_consensus::coins::MuHash3072;
use dash_consensus::coins::stats::{CoinStatsAccumulator, CoinStatsTotals};
use dash_consensus::subsidy::{SubsidyParams, block_subsidy};
use dash_consensus::{Amount, BlockId, HeaderCtx};
use dash_params::{ChainSetup, Network};
use dash_statedigest::{Chain, FileKind, Header, Writer};
use dashd_chainstate::chain::genesis_block;
use dashd_chainstate::coins::CoinsTable;
use dashd_chainstate::connect::{DigestLog, DigestSink, MuHashAt};
use dashd_chainstate::store::{AbortSink as _, GuardedSnapshot, Meta};
use dashd_store::KvEngine;
use dashd_store::flatfile::FlatFs;

use super::Chainstate;
use crate::{FULL_VERSION, InitFailure};

pub(crate) fn log_path(args: &ArgsManager) -> Result<Option<PathBuf>, RuntimeError> {
    let path = args.get_path_arg("-statedigestlog", Path::new(""))?;
    if path.as_os_str().is_empty() {
        return Ok(None);
    }
    abs_path_for_config_val(args, &path, true).map(Some)
}

pub(crate) fn open_log(args: &ArgsManager) -> Result<Option<File>, InitFailure> {
    log_path(args)?
        .map(|path| {
            OpenOptions::new().create(true).append(true).open(&path).map_err(|_| {
                InitFailure::Error(format!("cannot open {} for -statedigestlog", path.display()))
            })
        })
        .transpose()
}

/// Scans the recovered coins into a digest accumulator.
///
/// The historical `bogo_size` total is deliberately left at zero: it is not seeded and is not
/// emitted by the StateDigest records. The emitted `stats.utxo_size_inc` comes from each block's
/// own statistics instead.
fn scan_seed<E: KvEngine>(
    snap: &GuardedSnapshot<'_, E>,
    tip: Option<BlockRef>,
    subsidy: Amount,
) -> Result<CoinStatsAccumulator, String> {
    let stored = Meta::read(snap).map_err(|e| e.to_string())?.tip;
    if stored != tip.map(|t| t.hash) {
        return Err("the coins snapshot does not match the recovered tip".to_owned());
    }
    let mut muhash = MuHash3072::new();
    let mut totals = CoinStatsTotals::default();
    for item in CoinsTable::default().cursor(snap) {
        let (key, coin) = item.map_err(|e| e.to_string())?;
        let coin = coin.as_ref();
        muhash.insert_coin(&key.outpoint(), &coin);
        totals.transaction_output_count = totals
            .transaction_output_count
            .checked_add(1)
            .ok_or_else(|| "coin count overflow".to_owned())?;
        totals.total_amount = totals
            .total_amount
            .checked_add(coin.value())
            .ok_or_else(|| "coin amount overflow".to_owned())?;
    }
    if tip.is_none() && totals.transaction_output_count != 0 {
        return Err("coins exist without a best block".to_owned());
    }
    totals.total_unspendable_amount = subsidy
        .checked_sub(totals.total_amount)
        .ok_or_else(|| "unspendable amount overflow".to_owned())?;
    // Historical offsets cancel in block_info; the unclaimed-reward balance must remain zero.
    totals.total_subsidy = totals.total_unspendable_amount;
    Ok(CoinStatsAccumulator::from_parts(muhash, totals))
}

fn seed_at(
    params: &ChainSetup,
    reader: &ChainReader,
    tip: Option<BlockRef>,
) -> Result<Option<(BlockId, Amount)>, String> {
    let Some(tip) = tip else {
        return Ok(None);
    };
    let id = reader.lookup(&tip.hash).ok_or("the coins tip is missing from the index")?;
    if id != tip.id {
        return Err("the coins tip id does not match the index".to_owned());
    }
    let entry = reader.entry(id).ok_or("the coins tip is not resident")?;
    if entry.height != tip.height {
        return Err("the coins tip height does not match the index".to_owned());
    }
    let genesis = genesis_block(params);
    let mut total: Amount = genesis
        .txs()
        .first()
        .ok_or("genesis has no coinbase")?
        .outputs()
        .iter()
        .map(|out| out.value())
        .sum();
    let subsidy_params =
        SubsidyParams::from_chain_params(params, total).map_err(|e| e.to_string())?;
    let v20_height = u32::try_from(subsidy_params.config().v20_height)
        .map_err(|_| "the V20 height exceeds uint32")?;
    let mut at = id;
    loop {
        let entry = reader.entry(at).ok_or("a coins-tip ancestor is missing from the index")?;
        let Some(prev) = entry.prev else {
            if entry.hash != genesis.hash() || entry.height != 0 {
                return Err("the coins-tip ancestry does not reach genesis".to_owned());
            }
            break;
        };
        let parent = reader.entry(prev).ok_or("a coins-tip parent is missing from the index")?;
        if parent.height.checked_add(1) != Some(entry.height) {
            return Err("the coins-tip ancestor heights do not match".to_owned());
        }
        let parent_header = HeaderCtx {
            height: parent.height,
            hash: parent.hash,
            time: parent.header.time(),
            bits: dash_types::Numeric::from_base(parent.header.bits()),
            version: parent.header.version(),
            mtp: parent.mtp,
            chain_work: parent.chain_work,
        };
        let subsidy =
            block_subsidy(Some(&parent_header), entry.height >= v20_height, &subsidy_params)
                .map_err(|e| e.to_string())?;
        total = total.checked_add(subsidy).ok_or("subsidy total overflow")?;
        at = prev;
    }
    Ok(Some((id, total)))
}

impl<E: KvEngine, F: FlatFs> Chainstate<E, F> {
    pub(crate) fn seed_digest(
        &self,
        params: &ChainSetup,
        reader: &ChainReader,
        file: File,
    ) -> Result<Box<dyn DigestSink>, InitFailure> {
        let tip = self.coins_best_block();
        let seed = seed_at(params, reader, tip)
            .and_then(|at| {
                let snapshot = self.engine.snapshot();
                scan_seed(&snapshot, tip, at.map_or(0, |(_, total)| total)).map(|stats| (at, stats))
            })
            .map_err(|detail| {
                if self.engine.latch().tripped().is_none() {
                    self.abort_path().abort_node(
                        &format!("statedigest: seeding the digest log: {detail}"),
                        None,
                    );
                }
                InitFailure::Reported
            })?;
        let opened = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| InitFailure::Exception(e.to_string()))?;
        let header = Header {
            file: FileKind::Digest,
            src: Writer::Rdashd,
            ver: FULL_VERSION.as_bytes().to_vec(),
            chain: match params.network {
                Network::Main => Chain::Main,
                Network::Testnet3 => Chain::Test,
                Network::Regtest => Chain::Regtest,
                Network::Devnet => Chain::Devnet,
            },
            run: format!("{}-{}", process::id(), opened.as_micros()).into_bytes(),
        };
        let (at, stats) = seed;
        let log = match at {
            Some((id, _)) => DigestLog::seeded(file, header, stats, id),
            None => DigestLog::new(file, header),
        }
        .map_err(|error| {
            self.abort_path().abort_node(&error.0, None);
            InitFailure::Reported
        })?;
        Ok(Box::new(log.with_muhash_at(MuHashAt::Every)))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dash_clock::{Clock, MockClock};
    use dash_consensus::coins::is_unspendable;
    use dash_consensus::coins::{BlockUndo, Coin};
    use dash_consensus::primitives::{
        Header as BlockHeader, OutPoint, Transaction, TxHash, TxIn, TxOut, TxType,
    };
    use dash_consensus::{Block, Tx};
    use dash_params::{ChainParamsBuilder, Release};
    use dashd_chainstate::chain::{MemBlockFiles, RecordingAbort};
    use dashd_chainstate::coins::OutPointKey;
    use dashd_chainstate::store::{
        CHAINSTATE_SCHEMA, GlobalFailStop, GuardedEngine, RecordingSink,
    };
    use dashd_store::{Commit, MemoryEngine};

    use super::*;
    use crate::chain::Chain;
    use crate::chain::tests::{fresh, regtest};
    use crate::init::{test_args, test_args_with_conf};

    #[test]
    fn digest_paths_follow_core_arguments() {
        for flags in [
            &[][..],
            &["-statedigestlog"],
            &["-statedigestlog="],
            &["-nostatedigestlog"],
            &["-statedigestlog=earlier", "-nostatedigestlog"],
        ] {
            assert_eq!(log_path(&test_args(flags)).unwrap(), None, "{flags:?}");
        }
        for (flags, name) in [
            (vec!["-statedigestlog=0"], "0"),
            (vec!["-statedigestlog=earlier", "-statedigestlog=later"], "later"),
            (vec!["-nostatedigestlog=0"], "1"),
        ] {
            let args = test_args(&flags);
            assert_eq!(log_path(&args).unwrap(), Some(args.get_data_dir_net().unwrap().join(name)));
        }
        let args = test_args_with_conf(&["-statedigestlog=cli"], Some("statedigestlog=config\n"));
        assert_eq!(log_path(&args).unwrap(), Some(args.get_data_dir_net().unwrap().join("cli")));
        let args = test_args(&["-statedigestlog=/absolute/digest.jsonl"]);
        assert_eq!(log_path(&args).unwrap(), Some(PathBuf::from("/absolute/digest.jsonl")));
        let args = test_args(&["-testnet", "-statedigestlog=relative"]);
        assert_eq!(
            log_path(&args).unwrap(),
            Some(args.get_data_dir_net().unwrap().join("relative"))
        );
    }

    fn block(height: u8, outputs: &[(Amount, u8)]) -> Block {
        let params = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
        let input = TxIn::new(
            OutPoint::new(TxHash::from([0; 32]), u32::MAX),
            vec![1, height, 0x51].into(),
            u32::MAX,
        );
        let outputs = outputs
            .iter()
            .map(|&(value, script)| TxOut::from_duffs(value, vec![script].into()))
            .collect();
        let tx =
            Arc::new(Tx::new(Transaction::new(1, TxType::new(0), vec![input], outputs, 0, vec![])));
        let mut header = genesis_block(&params).header().raw().to_owned();
        let txs = vec![tx];
        header.set_merkle_root(Block::new(BlockHeader::new(header), txs.clone()).merkle().0);
        Block::new(BlockHeader::new(header), txs)
    }

    fn database() -> GuardedEngine<MemoryEngine> {
        GuardedEngine::new(
            MemoryEngine::new(&CHAINSTATE_SCHEMA),
            GlobalFailStop::new(),
            Arc::new(RecordingSink::default()),
        )
    }

    #[test]
    fn scanned_seed_matches_uninterrupted_accumulator_after_burn_and_unclaimed_reward() {
        let db = database();
        let blocks = [
            block(0, &[(50, 0x51)]),
            block(1, &[(45, 0x51), (3, 0x6a)]),
            block(2, &[(49, 0x51)]),
            block(3, &[(44, 0x51), (4, 0x6a)]),
        ];
        let tip = BlockRef {
            id: BlockId(2),
            hash: blocks[2].hash(),
            height: 2,
        };
        let mut uninterrupted = CoinStatsAccumulator::new();
        let mut batch = db.batch();
        for (height, block) in blocks[..3].iter().enumerate() {
            uninterrupted.append_block(height as u32, block, &BlockUndo::default(), 50).unwrap();
            if height == 0 {
                continue;
            }
            let tx = &block.txs()[0];
            for (index, out) in tx.outputs().iter().enumerate() {
                if !is_unspendable(out.script_pubkey().as_bytes()) {
                    let key = OutPointKey::new(&OutPoint::new(tx.txid(), index as u32));
                    CoinsTable::default().put(
                        &mut batch,
                        &key,
                        &Coin::from_output(out, height as u32, true).serialize(),
                    );
                }
            }
        }
        Meta::put_tip(&mut batch, &tip.hash);
        db.write(batch, Commit::Buffered).unwrap();
        let mut seeded = scan_seed(&db.snapshot(), Some(tip), 150).unwrap();
        assert_eq!(uninterrupted.totals().total_unspendables_scripts, 3);
        assert_eq!(uninterrupted.totals().total_unspendables_unclaimed_rewards, 3);
        assert_eq!(seeded.totals().total_unspendable_amount, 56);
        for (height, block) in blocks.iter().enumerate().skip(3) {
            let before = *uninterrupted.totals();
            let seed_before = *seeded.totals();
            uninterrupted.append_block(height as u32, block, &BlockUndo::default(), 50).unwrap();
            seeded.append_block(height as u32, block, &BlockUndo::default(), 50).unwrap();
            assert_eq!(
                seeded.totals().total_unspendable_amount,
                uninterrupted.totals().total_unspendable_amount
            );
            assert_eq!(
                seeded.totals().block_info(Some(&seed_before)),
                uninterrupted.totals().block_info(Some(&before))
            );
            assert_eq!(seeded.finalize(), uninterrupted.finalize());
        }
        for height in (1..=3).rev() {
            seeded
                .reverse_block(height, &blocks[height as usize], &BlockUndo::default(), 50)
                .unwrap();
            uninterrupted
                .reverse_block(height, &blocks[height as usize], &BlockUndo::default(), 50)
                .unwrap();
            assert_eq!(
                seeded.totals().total_unspendable_amount,
                uninterrupted.totals().total_unspendable_amount
            );
            assert_eq!(seeded.finalize(), uninterrupted.finalize());
        }
    }

    #[test]
    fn indexed_seed_subsidy_matches_connector_across_v20_and_decline() {
        use dash_chain::HeaderBatch;
        use dash_consensus::header::HeaderParams;
        use dash_consensus::pow::check_proof_of_work;
        use dash_params::ChainOption;
        use dash_primitives::BlockHeader as RawHeader;
        use dashd_chainstate::connect::block_subsidy_after;

        let params = Arc::new(
            ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
                .arg(ChainOption::Dip3Params, "1:1")
                .arg(ChainOption::TestActivationHeight, "v20@100")
                .build()
                .unwrap(),
        );
        assert_eq!(params.consensus.subsidy_halving_interval, 150);
        let genesis = genesis_block(&params);
        let reward: Amount = genesis.txs()[0].outputs().iter().map(|out| out.value()).sum();
        let subsidy = SubsidyParams::from_chain_params(&params, reward).unwrap();
        let clock: Arc<dyn Clock> =
            Arc::new(MockClock::at(i64::from(genesis.header().raw().time()) + 86_400));
        let abort = Arc::new(RecordingAbort::default());
        let mut chain =
            Chain::start(fresh(&params, &clock, &abort, Box::new(MemBlockFiles::default())))
                .unwrap();
        crate::chain::tests::startup_over(
            &mut chain,
            Box::new(dashd_chainstate::chain::NullConnector),
            Box::new(dashd_chainstate::chain::NoFlush),
        );
        crate::chain::tests::imported(&mut chain);
        let hp = HeaderParams::from_chain_params(&params).unwrap();
        let mut headers = vec![*genesis.header()];
        for height in 1..=152 {
            let parent = headers.last().unwrap();
            let header = (0..)
                .map(|nonce| {
                    BlockHeader::new(RawHeader::new(
                        0x2000_0000,
                        parent.hash(),
                        parent.raw().merkle_root(),
                        parent.raw().time() + 150,
                        0x207f_ffff,
                        nonce,
                    ))
                })
                .find(|header| {
                    check_proof_of_work(
                        header.hash().as_bytes(),
                        dash_types::Numeric::from_base(0x207f_ffff),
                        hp.pow(),
                    )
                })
                .unwrap();
            let accepted = chain
                .handle()
                .process_headers(
                    HeaderBatch {
                        hashes: vec![header.hash()],
                        headers: vec![header],
                    },
                    None,
                )
                .recv()
                .unwrap();
            assert!(accepted.stop.is_none(), "height {height}: {:?}", accepted.stop);
            headers.push(header);
        }
        let hashes: Vec<_> = headers.iter().map(|header| header.hash()).collect();
        let per_block = chain
            .handle()
            .query(move |q| {
                let reader = q.chain.reader();
                hashes
                    .iter()
                    .map(|hash| {
                        let prev = reader.entry(reader.lookup(hash).unwrap()).unwrap().prev;
                        block_subsidy_after(&reader, &q.deployments, prev, &subsidy).unwrap()
                    })
                    .collect::<Vec<_>>()
            })
            .recv()
            .unwrap();
        let reader = chain.handle().reader();
        let mut total = 0;
        for (height, header) in headers.iter().enumerate() {
            let id = reader.lookup(&header.hash()).unwrap();
            total += per_block[height];
            let tip = BlockRef {
                id,
                hash: header.hash(),
                height: height as u32,
            };
            assert_eq!(
                seed_at(&params, reader, Some(tip)).unwrap(),
                Some((id, total)),
                "height {height}"
            );
        }
        assert_eq!(per_block[0], reward);
        assert_ne!(per_block[99], per_block[100], "V20 must change the subsidy");
        assert!(per_block[151] < per_block[150], "the first decline must be exercised");
        chain.stop().unwrap();
    }

    #[test]
    fn seed_refuses_missing_and_mismatched_index_tips() {
        let params = regtest();
        let genesis = genesis_block(&params);
        let clock: Arc<dyn Clock> =
            Arc::new(MockClock::at(i64::from(genesis.header().raw().time()) + 60));
        let abort = Arc::new(RecordingAbort::default());
        let mut chain =
            Chain::start(fresh(&params, &clock, &abort, Box::new(MemBlockFiles::default())))
                .unwrap();
        assert!(seed_at(&params, chain.handle().reader(), None).unwrap().is_none());
        let tip = BlockRef {
            id: chain.handle().reader().lookup(&genesis.hash()).unwrap(),
            hash: genesis.hash(),
            height: 0,
        };
        assert_eq!(
            seed_at(&params, chain.handle().reader(), Some(tip)).unwrap().unwrap().1,
            5_000_000_000
        );
        let wrong_id = BlockRef {
            id: BlockId(tip.id.0 + 1),
            ..tip
        };
        assert!(
            seed_at(&params, chain.handle().reader(), Some(wrong_id))
                .unwrap_err()
                .contains("id does not match")
        );
        let wrong_height = BlockRef {
            height: 1,
            ..tip
        };
        assert!(
            seed_at(&params, chain.handle().reader(), Some(wrong_height))
                .unwrap_err()
                .contains("height does not match")
        );
        let missing = BlockRef {
            id: tip.id,
            hash: [1; 32].into(),
            height: 0,
        };
        assert!(
            seed_at(&params, chain.handle().reader(), Some(missing))
                .unwrap_err()
                .contains("missing from the index")
        );
        chain.stop().unwrap();
    }

    #[test]
    fn seed_refuses_missing_mismatched_and_corrupt_coin_state() {
        let db = database();
        let tip = BlockRef {
            id: BlockId(0),
            hash: genesis_block(
                &ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap(),
            )
            .hash(),
            height: 0,
        };
        assert!(scan_seed(&db.snapshot(), Some(tip), 50).unwrap_err().contains("does not match"));
        let key = OutPointKey::new(&OutPoint::new(TxHash::from([1; 32]), 0));
        let mut batch = db.batch();
        CoinsTable::default().put(&mut batch, &key, &Coin::new(1, &[0x51], 1, true).serialize());
        db.write(batch, Commit::Buffered).unwrap();
        assert!(scan_seed(&db.snapshot(), None, 0).unwrap_err().contains("without a best block"));
        let mut batch = db.batch();
        Meta::put_tip(&mut batch, &tip.hash);
        CoinsTable::default().put(&mut batch, &key, &[0xff]);
        db.write(batch, Commit::Buffered).unwrap();
        assert!(scan_seed(&db.snapshot(), Some(tip), 50).is_err());
        assert!(db.latch().tripped().is_some());
    }
}
