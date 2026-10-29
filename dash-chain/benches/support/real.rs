//! A-25's like-for-like pre-states (SB4 §16): real H-M headers around each accept window, on
//! the fixture's synthetic chain below them (`benches/data/README.md`).
//!
//! Core's harness preloads H-M up to the window (`--bench-pnbh`). Here `load` reads the
//! fixture's synthetic records below the slice, then the slice's real headers up to the window
//! as `VALID_TREE` records, as Core's preload leaves them. The window's rules only look back at
//! the real headers: DGW at 24, KGW at most `PastBlocksMax` (4,032), the median time at 11.

use std::sync::Arc;

use dash_chain::{
    AnchorPack, BlockId, BlockIndexRecord, ChainConfig, Evicting, HeaderBatch, HeaderChain,
    HeaderStore, LoadError, NoStore, Residency, Services, Status, Work,
};
use dash_clock::MockClock;
use dash_consensus::primitives::Header;
use dash_primitives::{BlockHash, BlockHeader};
use dash_types::Numeric as _;
use dash_types::codec::BaseCodec as _;

use super::fixture::{Setup, synthetic_hash};

/// The batch: one `headers` message.
pub const COUNT: u32 = 2_000;

/// H-M's heights `first..` from a committed slice.
pub struct Slice {
    first: u32,
    raw: Vec<u8>,
}

impl Slice {
    /// Heights 1,999,950..2,002,000: DGW's window at 2,000,000.
    pub fn dgw() -> Slice {
        Slice::read("mainnet-1999950-2002000.bin", 1_999_950)
    }

    /// Heights 15,900..22,000: KGW's window at 20,000.
    pub fn kgw() -> Slice {
        Slice::read("mainnet-15900-22000.bin", 15_900)
    }

    fn read(name: &str, first: u32) -> Slice {
        // Detection and its self-test run once, at a node's startup: not in a region.
        dash_x11::init();
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/benches/data/");
        let raw = std::fs::read(format!("{path}{name}")).expect("a committed slice");
        assert_eq!(raw.len() % 80, 0, "{name}: a partial header");
        Slice {
            first,
            raw,
        }
    }

    /// The serialized header at `height`.
    pub fn raw(&self, height: u32) -> &[u8; 80] {
        let k = (height - self.first) as usize;
        self.raw[80 * k..80 * k + 80].try_into().expect("in the slice")
    }

    pub fn header(&self, height: u32) -> BlockHeader {
        BlockHeader::decode(&mut &self.raw(height)[..]).expect("an 80-byte header")
    }

    /// The node's records of heights `0..from`: synthetic below the slice, real from its start,
    /// the lowest real header's parent rewritten to the synthetic one (`load` does not hash).
    pub fn node_records(
        &self,
        s: &Setup,
        from: u32,
    ) -> impl Iterator<Item = (BlockHash, Vec<u8>)> + use<> {
        let first = self.first;
        let real: Vec<(BlockHash, Vec<u8>)> = (first..from)
            .map(|h| {
                let mut header = self.header(h);
                let hash = Header::new(header).hash();
                if h == first {
                    header.prev_hash = synthetic_hash(first - 1);
                }
                let record = BlockIndexRecord {
                    height: h,
                    status: Status(Status::VALID_TREE),
                    n_tx: 0,
                    file: None,
                    data_pos: None,
                    undo_pos: None,
                    header,
                };
                (hash, record.to_bytes())
            })
            .collect();
        s.records(first).chain(real)
    }

    /// SPV's pack anchored at `from − 1`, with the reference node's chain work there (README).
    pub fn pack(&self, from: u32, chain_work: u128) -> AnchorPack {
        let headers: [BlockHeader; 25] = std::array::from_fn(|k| self.header(from - 25 + k as u32));
        AnchorPack {
            height: from - 1,
            hash: Header::new(headers[24]).hash(),
            chain_work: Work::from_u128(chain_work),
            headers,
        }
    }

    /// The batch at `from`, decoded (Core's harness passes `ProcessNewBlockHeaders` decoded
    /// headers too), with its serializations for the timed hashing.
    pub fn batch(&self, from: u32) -> (Vec<Header>, Vec<[u8; 80]>) {
        (from..from + COUNT).map(|h| (Header::new(self.header(h)), *self.raw(h))).unzip()
    }
}

/// The timed region: X11 of each header with `dash-x11`, which `ProcessNewBlockHeaders` does
/// inside (`validation.cpp:4736`), then `accept`. `Header::new` hashes with base-sdk's
/// `dash-pow` (~2.3× Core's instructions), so the headers are built outside the region and
/// `dash-x11` stands in for it (§16's risk row, until BS15's switch). `accept` checks the hashes
/// against the headers only under `debug_assertions`.
pub fn hash_and_accept<S, R>(
    chain: &mut HeaderChain<S, R>,
    headers: Vec<Header>,
    raw: &[[u8; 80]],
) -> Option<BlockId>
where
    S: HeaderStore,
    R: Residency,
{
    // `accept` reads the hashes only under `debug_assertions`: keep the hashing in the region.
    let hashes = std::hint::black_box(
        raw.iter().map(|r| BlockHash::from_lendian(dash_x11::x11(r))).collect(),
    );
    let r = chain
        .accept(
            HeaderBatch {
                headers,
                hashes,
            },
            None,
        )
        .expect("accept");
    assert!(r.stop.is_none(), "{:?}", r.stop);
    r.last_accepted
}

/// An anchored SPV chain at w = 10,000 (§11.1), with no store: the batch stays in the window.
pub fn spv_anchored(s: &Setup, pack: &AnchorPack) -> HeaderChain<NoStore, Evicting> {
    let cfg = ChainConfig::spv(&s.params, 10_000);
    HeaderChain::new_anchored(s.params.clone(), cfg, services(s), NoStore, pack)
        .expect("a valid pack")
}

/// [`Setup::load`] under the windows' services.
pub fn node_chain(
    s: &Setup,
    records: impl IntoIterator<Item = (BlockHash, Vec<u8>)>,
) -> HeaderChain {
    HeaderChain::load(
        s.params.clone(),
        s.node_config(),
        services(s),
        records.into_iter().map(Ok::<_, LoadError>),
        &s.pool,
    )
    .expect("the window's index loads")
}

/// The fixture's services with the clock past the windows: the DGW window's headers are later
/// than its clock, and would be too far in the future.
fn services(s: &Setup) -> Services {
    Services {
        clock: Arc::new(MockClock::at(2_000_000_000)),
        ..s.svc.clone()
    }
}
