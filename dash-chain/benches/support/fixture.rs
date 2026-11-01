//! The mainnet-shaped node index the benches run on (SB4 §16).
//!
//! The gate builds benches in detached worktrees, which never see the untracked H-M file
//! (`tests/data/headers/`), so this builds a mainnet-shaped chain instead: mainnet's parameters
//! and genesis, then H-M's length of headers at the genesis `nBits` whose hashes meet that target.
//! `load` checks proof of work against the stored hash and never hashes, so the chain's shape
//! (heights, skips, index size) is H-M's without X11.

use std::sync::{Arc, OnceLock};

use dash_chain::{
    BlockId, BlockIndexRecord, ChainConfig, FailStop, HeaderChain, LoadError, NoStore, Services,
    StaticSporks, Status,
};
use dash_clock::MockClock;
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dash_primitives::{BlockHash, BlockHeader};

/// H-M's length: heights 0..=2,551,766.
pub const MAINNET_HEADERS: u32 = 2_551_767;

pub type Records = Vec<(BlockHash, Vec<u8>)>;

struct NoFail;

impl FailStop for NoFail {
    fn read_failed(&self, what: &str) {
        panic!("bench store read failed: {what}");
    }

    fn is_tripped(&self) -> bool {
        false
    }
}

pub struct Setup {
    pub params: Arc<ChainSetup>,
    pub svc: Services,
    pub pool: rayon::ThreadPool,
}

impl Setup {
    /// One per process: its pool runs on the bench thread (`use_current_thread`), so with
    /// RAYON_NUM_THREADS=1 no worker exists to spin into a measured region, and a second such
    /// pool on the same thread would be refused.
    pub fn mainnet() -> &'static Setup {
        static SETUP: OnceLock<Setup> = OnceLock::new();
        SETUP.get_or_init(Setup::new)
    }

    fn new() -> Setup {
        let params = Arc::new(
            ChainParamsBuilder::new(Network::Main, Release::V24_0)
                .build()
                .expect("Core's parameters"),
        );
        let svc = Services {
            clock: Arc::new(MockClock::at(1_700_000_000)),
            sporks: Arc::new(StaticSporks::for_network(&params, None)),
            fail: Arc::new(NoFail),
        };
        // The host's pool (L5); the gate runs benches with RAYON_NUM_THREADS=1.
        let pool =
            rayon::ThreadPoolBuilder::new().use_current_thread().build().expect("a rayon pool");
        Setup {
            params,
            svc,
            pool,
        }
    }

    /// The node's default configuration.
    pub fn node_config(&self) -> ChainConfig {
        ChainConfig::node(&self.params)
    }

    /// `load` under the node's default configuration.
    pub fn load(&self, records: impl IntoIterator<Item = (BlockHash, Vec<u8>)>) -> HeaderChain {
        self.load_with(self.node_config(), records)
    }

    pub fn load_with(
        &self,
        cfg: ChainConfig,
        records: impl IntoIterator<Item = (BlockHash, Vec<u8>)>,
    ) -> HeaderChain {
        HeaderChain::load(
            self.params.clone(),
            cfg,
            self.svc.clone(),
            records.into_iter().map(Ok::<_, LoadError>),
            &self.pool,
        )
        .expect("the synthetic index loads")
    }

    /// The node's records of heights `0..n`, generated as `load` reads them: Core's genesis
    /// record, then headers on the genesis `nBits` 150 s apart, each fully validated with data
    /// and undo, as on a synced node.
    pub fn records(&self, n: u32) -> impl Iterator<Item = (BlockHash, Vec<u8>)> + use<> {
        let mut genesis: HeaderChain = HeaderChain::new_genesis(
            self.params.clone(),
            self.node_config(),
            self.svc.clone(),
            NoStore,
        )
        .expect("genesis");
        let write =
            genesis.node_index().expect("a node").take_dirty().pop().expect("the genesis record");
        let g = dash_consensus::params::genesis_block(&self.params).header;
        let genesis_hash = write.hash;
        std::iter::once((write.hash, write.bytes)).chain((1..n).map(move |h| {
            let header = BlockHeader {
                version: 0x2000_0000,
                prev_hash: match h {
                    1 => genesis_hash,
                    _ => synthetic_hash(h - 1),
                },
                time: g.time + 150 * h,
                bits: g.bits,
                nonce: h,
                ..BlockHeader::default()
            };
            let record = BlockIndexRecord {
                height: h,
                status: Status(Status::VALID_SCRIPTS | Status::HAVE_DATA | Status::HAVE_UNDO),
                n_tx: 1 + h % 64,
                file: Some((h / 1500) as i32),
                data_pos: Some(8 + (h % 1500) * 4096),
                undo_pos: Some(8 + (h % 1500) * 512),
                header,
            };
            (synthetic_hash(h), record.to_bytes())
        }))
    }
}

/// A unique hash below the target of mainnet's genesis `nBits` (0x1e0ffff0): its top three
/// little-endian bytes are zero. The low bytes are spread so the index's ReadLE64 key is not
/// sequential.
pub fn synthetic_hash(k: u32) -> BlockHash {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&u64::from(k).wrapping_mul(0x9e37_79b9_7f4a_7c15).to_le_bytes());
    b[8..12].copy_from_slice(&k.to_le_bytes());
    b[12..28].fill(0x5b);
    b[28] = 0x01;
    BlockHash::from(b)
}

/// A deterministic stream for the random cases.
pub struct Lcg(pub u64);

impl Lcg {
    pub fn below(&mut self, n: u32) -> u32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 32) % u64::from(n)) as u32
    }
}

/// A mainnet-sized node index with the active tip at its best header. Each bench builds its
/// fixture on its closure's first call: the shim calls a selected bench's closure once, criterion
/// once per sample.
pub fn mainnet_chain() -> (HeaderChain, BlockId) {
    mainnet_chain_with(Setup::mainnet().node_config())
}

/// [`mainnet_chain`] under `cfg` (a node configuration, e.g. with other feed capacities).
pub fn mainnet_chain_with(cfg: ChainConfig) -> (HeaderChain, BlockId) {
    let s = Setup::mainnet();
    let mut chain = s.load_with(cfg, s.records(MAINNET_HEADERS));
    let tip = chain.best_header();
    chain.init_active_tip(tip).expect("the tip is in the index");
    (chain, tip)
}
