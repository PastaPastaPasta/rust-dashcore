//! Checkpoints are known blocks a client can start syncing from instead of genesis.
//!
//! Each checkpoint holds its block header and the [`CHECKPOINT_HEADERS`] - 1 headers before
//! it, so the difficulty of the blocks after it can be checked from the first one on. The
//! genesis checkpoint is the exception: it holds only the genesis header and is never
//! encoded, as decoding always reads [`CHECKPOINT_HEADERS`] headers. An encoded checkpoint
//! also carries the absolute chain work at its block, as `dashd` reports it, and decoding
//! refuses one without it. The checkpoints of mainnet and testnet live in
//! `checkpoints/<network>.json`, which `build.rs` encodes for embedding. The
//! `generate-checkpoints` binary writes their headers but not `chain_work`, which is added by
//! hand with its source recorded in `checkpoints/PROVENANCE.md`.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::LazyLock;

use dash_chain::{AnchorPack, Work};
use dash_types::Numeric;
use dashcore::block::Header;
use dashcore::blockdata::constants::genesis_block;
use dashcore::consensus::{encode, Decodable, Encodable};
use dashcore::{BlockHash, Network};

use crate::bridge::headers::{block_hash_to_base, header_to_base};
use crate::client::{chain_setup, ClientConfig};
use crate::types::HashedBlockHeader;

/// Headers each checkpoint but genesis holds, the checkpoint block last.
pub const CHECKPOINT_HEADERS: usize = 25;

const MAINNET_CHECKPOINTS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/mainnet.checkpoints"));
const TESTNET_CHECKPOINTS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/testnet.checkpoints"));

// The embedded files are decoded and validated by the tests.
static MAINNET: LazyLock<CheckpointManager> = LazyLock::new(|| {
    CheckpointManager::new(
        decode_checkpoints(MAINNET_CHECKPOINTS, Network::Mainnet)
            .expect("embedded checkpoints must decode"),
    )
});
static TESTNET: LazyLock<CheckpointManager> = LazyLock::new(|| {
    CheckpointManager::new(
        decode_checkpoints(TESTNET_CHECKPOINTS, Network::Testnet)
            .expect("embedded checkpoints must decode"),
    )
});
static NO_CHECKPOINTS: CheckpointManager = CheckpointManager {
    checkpoints: BTreeMap::new(),
};

/// A known block and the [`CHECKPOINT_HEADERS`] - 1 headers before it, or the genesis block alone.
#[derive(Debug, Clone)]
pub struct Checkpoint {
    height: u32,
    headers: Vec<HashedBlockHeader>,
    chain_work: Option<Work>,
    network: Option<Network>,
}

impl Checkpoint {
    /// A checkpoint at `height` from `headers`, the last of which is the block at `height`.
    ///
    /// Returns `None` unless there are exactly [`CHECKPOINT_HEADERS`] headers that fit
    /// below `height`.
    pub fn new(height: u32, headers: Vec<HashedBlockHeader>) -> Option<Self> {
        let fits = headers.len() == CHECKPOINT_HEADERS && height >= CHECKPOINT_HEADERS as u32 - 1;

        fits.then_some(Self {
            height,
            headers,
            chain_work: None,
            network: None,
        })
    }

    /// The genesis block of `network`, alone.
    pub(crate) fn genesis(network: Network) -> Self {
        Self {
            height: 0,
            headers: vec![genesis_block(network).header.into()],
            chain_work: None,
            network: Some(network),
        }
    }

    /// Height of the checkpoint block.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Hash of the checkpoint block.
    pub fn hash(&self) -> &BlockHash {
        self.header().hash()
    }

    /// Header of the checkpoint block.
    pub fn header(&self) -> &HashedBlockHeader {
        self.headers.last().expect("a checkpoint holds headers")
    }

    /// All the headers of the checkpoint, oldest first, from [`Self::start_height`].
    pub fn headers(&self) -> &[HashedBlockHeader] {
        &self.headers
    }

    /// Height of the first of [`Self::headers`].
    pub fn start_height(&self) -> u32 {
        self.height + 1 - self.headers.len() as u32
    }

    /// Absolute chain work at the checkpoint block (Core's `chainwork`). Every embedded
    /// checkpoint but genesis has it; one built with [`Self::new`] does not.
    pub fn chain_work(&self) -> Option<Work> {
        self.chain_work
    }

    /// The checkpoint as a `dash_chain` anchor pack: its block's hash and chain work, and its
    /// [`CHECKPOINT_HEADERS`] headers oldest first, the checkpoint block last.
    ///
    /// `None` without chain work (genesis, or a checkpoint built with [`Self::new`]) and for a
    /// checkpoint `dash_chain` refuses as a pack because it is below DGW's window: under
    /// `pow_dgw_height + 24` of its network.
    pub fn anchor_pack(&self) -> Option<AnchorPack> {
        let chain_work = self.chain_work?;
        let params = chain_setup(&ClientConfig::new(self.network?)).ok()?;
        let dgw_height = params.consensus.pow_dgw_height.to_u32();
        if u64::from(self.height) < u64::from(dgw_height) + 24 {
            return None;
        }
        let headers: Vec<_> = self.headers.iter().map(|h| header_to_base(h.header())).collect();
        Some(AnchorPack {
            height: self.height,
            hash: block_hash_to_base(self.hash()),
            chain_work,
            headers: headers.try_into().ok()?,
        })
    }
}

impl Encodable for Checkpoint {
    fn consensus_encode<W: Write + ?Sized>(&self, writer: &mut W) -> Result<usize, std::io::Error> {
        let mut len = self.height.consensus_encode(writer)?;
        for header in &self.headers {
            len += header.header().consensus_encode(writer)?;
        }
        let chain_work = self.chain_work.ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "checkpoint without chain work")
        })?;
        len += chain_work.to_bendian().consensus_encode(writer)?;
        Ok(len)
    }
}

impl Decodable for Checkpoint {
    fn consensus_decode<R: Read + ?Sized>(reader: &mut R) -> Result<Self, encode::Error> {
        let height = u32::consensus_decode(reader)?;
        let headers = (0..CHECKPOINT_HEADERS)
            .map(|_| Header::consensus_decode(reader).map(HashedBlockHeader::from))
            .collect::<Result<Vec<_>, _>>()?;
        let chain_work = Work::from_bendian(<[u8; 32]>::consensus_decode(reader)?);
        if chain_work == Work::ZERO {
            return Err(encode::Error::ParseFailed("checkpoint chain work is zero"));
        }

        Self::new(height, headers)
            .map(|checkpoint| Self {
                chain_work: Some(chain_work),
                ..checkpoint
            })
            .ok_or(encode::Error::ParseFailed("checkpoint height below its headers"))
    }
}

/// Decode the checkpoints of `network` encoded one after another in `bytes`.
fn decode_checkpoints(
    mut bytes: &[u8],
    network: Network,
) -> Result<Vec<Checkpoint>, encode::Error> {
    let mut checkpoints = Vec::new();
    while !bytes.is_empty() {
        checkpoints.push(Checkpoint {
            network: Some(network),
            ..Checkpoint::consensus_decode(&mut bytes)?
        });
    }
    Ok(checkpoints)
}

/// The checkpoints of a network, by height.
pub struct CheckpointManager {
    checkpoints: BTreeMap<u32, Checkpoint>,
}

impl CheckpointManager {
    /// The checkpoints embedded for `network`. Networks without any (devnet, regtest)
    /// have none.
    pub fn for_network(network: Network) -> &'static Self {
        match network {
            Network::Mainnet => &MAINNET,
            Network::Testnet => &TESTNET,
            _ => &NO_CHECKPOINTS,
        }
    }

    pub(crate) fn new(checkpoints: Vec<Checkpoint>) -> Self {
        Self {
            checkpoints: checkpoints
                .into_iter()
                .map(|checkpoint| (checkpoint.height(), checkpoint))
                .collect(),
        }
    }

    /// The highest checkpoint at or below `height`.
    pub fn last_checkpoint_before_height(&self, height: u32) -> Option<&Checkpoint> {
        self.checkpoints.range(..=height).next_back().map(|(_, checkpoint)| checkpoint)
    }

    /// All the checkpoints, lowest first.
    pub fn checkpoints(&self) -> impl Iterator<Item = &Checkpoint> {
        self.checkpoints.values()
    }
}

#[cfg(test)]
#[path = "checkpoint_pack_tests.rs"]
mod pack_tests;

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;
    use crate::validation::{BlockHeaderValidator, Validator};

    fn checkpoints() -> Vec<Checkpoint> {
        [50_000, 100_000, 150_000].map(Checkpoint::dummy).to_vec()
    }

    fn with_chain_work(checkpoint: Checkpoint, chain_work: Work) -> Checkpoint {
        Checkpoint {
            chain_work: Some(chain_work),
            ..checkpoint
        }
    }

    fn encoded(checkpoint: &Checkpoint) -> Vec<u8> {
        let mut bytes = Vec::new();
        checkpoint.consensus_encode(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn new_requires_exactly_the_checkpoint_headers() {
        let headers = Checkpoint::dummy(100).headers().to_vec();

        assert!(Checkpoint::new(100, headers[1..].to_vec()).is_none());
        assert!(Checkpoint::new(23, headers.clone()).is_none());
        assert!(Checkpoint::new(24, headers).is_some());
    }

    #[test]
    fn checkpoint_block_is_the_last_header() {
        let checkpoint = Checkpoint::dummy(100);

        assert_eq!(checkpoint.start_height(), 76);
        assert_eq!(checkpoint.hash(), checkpoint.headers()[CHECKPOINT_HEADERS - 1].hash());
    }

    #[test]
    fn encoding_round_trips() {
        let checkpoints: Vec<Checkpoint> = checkpoints()
            .into_iter()
            .map(|c| {
                let work = Work::from(u64::from(c.height()) << 40);
                with_chain_work(c, work)
            })
            .collect();
        let bytes: Vec<u8> = checkpoints.iter().flat_map(encoded).collect();

        let decoded = decode_checkpoints(&bytes, Network::Mainnet).unwrap();

        assert_eq!(decoded.len(), checkpoints.len());
        for (decoded, checkpoint) in decoded.iter().zip(&checkpoints) {
            assert_eq!(decoded.height(), checkpoint.height());
            assert_eq!(decoded.hash(), checkpoint.hash());
            assert_eq!(decoded.chain_work(), checkpoint.chain_work());
        }
    }

    #[test]
    fn truncated_encoding_is_rejected() {
        let mut bytes = encoded(&with_chain_work(Checkpoint::dummy(100), Work::from(1u64)));
        bytes.pop();

        assert!(decode_checkpoints(&bytes, Network::Mainnet).is_err());
    }

    #[test]
    fn checkpoint_without_chain_work_is_not_encoded() {
        let mut bytes = Vec::new();

        assert!(Checkpoint::dummy(100).consensus_encode(&mut bytes).is_err());
    }

    #[test]
    fn zero_chain_work_is_rejected() {
        let bytes = encoded(&with_chain_work(Checkpoint::dummy(100), Work::ZERO));

        assert!(decode_checkpoints(&bytes, Network::Mainnet).is_err());
    }

    #[test_case(0, None)]
    #[test_case(49_999, None)]
    #[test_case(50_000, Some(50_000))]
    #[test_case(120_000, Some(100_000))]
    #[test_case(u32::MAX, Some(150_000))]
    fn last_checkpoint_before_height(height: u32, expected: Option<u32>) {
        let manager = CheckpointManager::new(checkpoints());

        assert_eq!(manager.last_checkpoint_before_height(height).map(Checkpoint::height), expected);
    }

    #[test]
    fn checkpoints_are_sorted_by_height() {
        let mut unsorted = checkpoints();
        unsorted.reverse();
        let manager = CheckpointManager::new(unsorted);

        let heights: Vec<u32> = manager.checkpoints().map(Checkpoint::height).collect();
        assert_eq!(heights, [50_000, 100_000, 150_000]);
    }

    #[test_case(Network::Mainnet)]
    #[test_case(Network::Testnet)]
    fn embedded_checkpoints_are_valid(network: Network) {
        let manager = CheckpointManager::for_network(network);
        assert!(manager.checkpoints().next().is_some(), "{network} must have checkpoints");

        for checkpoint in manager.checkpoints() {
            assert!(
                checkpoint.chain_work().is_some(),
                "checkpoint {} has no chain work",
                checkpoint.height()
            );
            let before = checkpoint.height() - CHECKPOINT_HEADERS as u32;
            BlockHeaderValidator::new(network, vec![], before)
                .validate(checkpoint.headers())
                .unwrap_or_else(|e| {
                    panic!("checkpoint {} of {network} is invalid: {e}", checkpoint.height())
                });
        }
    }

    // Hand-written, this checkpoint had the wrong time and bits.
    #[test]
    fn mainnet_checkpoint_750000_is_the_real_block() {
        let manager = CheckpointManager::for_network(Network::Mainnet);
        let checkpoint = manager.last_checkpoint_before_height(750_000).expect("checkpoint 750000");
        let header = checkpoint.header().header();

        assert_eq!(
            checkpoint.hash().to_string(),
            "00000000000000b4181bbbdddbae464ce11fede5d0292fb63fdede1e7c8ab21c"
        );
        assert_eq!(header.time, 1507424630);
        assert_eq!(header.bits.to_consensus(), 0x1a027884);
    }

    fn on_network(checkpoint: Checkpoint, network: Network) -> Checkpoint {
        Checkpoint {
            network: Some(network),
            ..with_chain_work(checkpoint, Work::from(1u64 << 40))
        }
    }

    #[test_case(Network::Mainnet)]
    #[test_case(Network::Testnet)]
    fn genesis_and_checkpoints_without_chain_work_have_no_pack(network: Network) {
        assert!(Checkpoint::genesis(network).anchor_pack().is_none());
        assert!(Checkpoint::dummy(100_000).anchor_pack().is_none());
    }

    #[test_case(Network::Mainnet, 34_140)]
    #[test_case(Network::Testnet, 4_002)]
    fn packs_start_at_the_top_of_dgw_window(network: Network, dgw_height: u32) {
        let first = dgw_height + 24;

        assert!(on_network(Checkpoint::dummy(first - 1), network).anchor_pack().is_none());
        assert!(on_network(Checkpoint::dummy(first), network).anchor_pack().is_some());
    }

    #[test]
    fn pack_holds_the_checkpoint_headers_oldest_first() {
        let checkpoint = on_network(Checkpoint::dummy(100_000), Network::Mainnet);

        let pack = checkpoint.anchor_pack().unwrap();

        assert_eq!(pack.height, 100_000);
        assert_eq!(pack.hash, block_hash_to_base(checkpoint.hash()));
        assert_eq!(Some(pack.chain_work), checkpoint.chain_work());
        for (header, hashed) in pack.headers.iter().zip(checkpoint.headers()) {
            assert_eq!(*header, header_to_base(hashed.header()));
        }
    }

    #[test_case(Network::Devnet)]
    #[test_case(Network::Regtest)]
    fn networks_without_embedded_checkpoints(network: Network) {
        assert!(CheckpointManager::for_network(network).checkpoints().next().is_none());
    }
}
