//! Core's `CoinbaseChainLockReader`: the ChainLocks later coinbases embed, read from a fixed tip's
//! ancestry and its carriers' block data (`getchainlockbyheight`).
//!
//! Core keeps each carrier's result for the life of the process, keyed by block hash; this reader
//! loads every carrier it needs from its [`CarrierSource`] on each lookup, which only costs reads.

use std::collections::BTreeMap;
use std::fmt;

use dash_consensus::{Block, BlockId};
use dash_pkc::bls::BlsGlobalSig;
use dash_primitives::{BlockHash, SpecialPayload, TxType};
use dash_types::Numeric as _;

use super::CBTX_VERSION_CLSIG_AND_BALANCE;
use crate::bls::BlsCtx;

/// `MAX_HISTORICAL_CARRIER_LOOKUPS`: the distinct carrier heights one lookup may read.
// Core: chainlock/clsig.cpp:28
pub const MAX_HISTORICAL_CARRIER_LOOKUPS: usize = 16384;

/// The `std::runtime_error`s Core's reader throws, by their text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoricalClError {
    /// The carrier's block could not be read, or has no transactions.
    Unavailable,
    /// The block's transactions do not hash to its header's merkle root.
    Corrupted,
    /// The first transaction is not a version-3+ CbTx coinbase at the carrier's height.
    InvalidCoinbase,
    /// `bestCLHeightDiff` is not below the carrier's height.
    InvalidDiff,
    /// The signed height has no ancestor on the tip's chain.
    InvalidBlockHeight,
    /// The certificate's bytes are not a valid signature under the process flag.
    InvalidSignature,
    /// The certificate's bytes are canonical in neither scheme (`CBLSWrapper::Unserialize`).
    Malleable,
    /// `ShutdownRequested()`.
    Interrupted,
    /// [`MAX_HISTORICAL_CARRIER_LOOKUPS`] heights were already read.
    BudgetExhausted,
}

// Core: chainlock/clsig.cpp:150-171, :177-211; bls/bls.h:211-221
impl fmt::Display for HistoricalClError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            HistoricalClError::Unavailable => "Historical ChainLock block data unavailable",
            HistoricalClError::Corrupted => "Historical ChainLock block data corrupted",
            HistoricalClError::InvalidCoinbase => "Invalid historical ChainLock coinbase",
            HistoricalClError::InvalidDiff => "Invalid historical coinbase ChainLock height",
            HistoricalClError::InvalidBlockHeight => "Invalid historical ChainLock block height",
            HistoricalClError::InvalidSignature => "Invalid historical ChainLock signature",
            HistoricalClError::Malleable => "malleable BLS object",
            HistoricalClError::Interrupted => "ChainLock lookup interrupted",
            HistoricalClError::BudgetExhausted => "ChainLock lookup budget exhausted",
        })
    }
}

impl std::error::Error for HistoricalClError {}

/// What one carrier's coinbase says: its certificate's raw bytes and height difference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CarrierEntry {
    pub height_diff: u32,
    pub signature: [u8; 96],
}

impl CarrierEntry {
    /// Whether the coinbase carries a certificate: any of the 96 wire bytes is nonzero, whatever
    /// the BLS scheme.
    pub fn has_chainlock(&self) -> bool {
        self.signature.iter().any(|b| *b != 0)
    }
}

/// Core's `LoadCarrier(carrier, carrier_height)` over the block its `ReadBlockFromDisk` read
/// (`None` where the read failed), in Core's order of checks.
// Core: chainlock/clsig.cpp:128-174
pub fn load_carrier(
    block: Option<&Block>,
    carrier_height: i32,
) -> Result<CarrierEntry, HistoricalClError> {
    let block = block.ok_or(HistoricalClError::Unavailable)?;
    let coinbase = block.txs().first().ok_or(HistoricalClError::Unavailable)?;
    if block.merkle().0 != block.header().merkle_root() {
        return Err(HistoricalClError::Corrupted);
    }
    let cbtx = (coinbase.is_coinbase() && coinbase.tx_type() == TxType::CoinbaseCommitment)
        .then(|| coinbase.payload()?.ok())
        .flatten()
        .filter(|decoded| decoded.is_complete())
        .and_then(|decoded| match decoded.payload() {
            SpecialPayload::CoinbaseCommitment(cbtx) => Some(cbtx),
            _ => None,
        })
        .filter(|cbtx| {
            cbtx.version().to_base() >= CBTX_VERSION_CLSIG_AND_BALANCE
                && cbtx.height().to_u32().cast_signed() == carrier_height
        })
        .ok_or(HistoricalClError::InvalidCoinbase)?;
    let (Some(diff), Some(signature)) = (cbtx.best_cl_height_diff(), cbtx.best_cl_signature())
    else {
        return Err(HistoricalClError::InvalidCoinbase);
    };
    let height_diff = u32::try_from(diff).map_err(|_| HistoricalClError::InvalidCoinbase)?;
    if i64::from(height_diff) >= i64::from(carrier_height) {
        return Err(HistoricalClError::InvalidDiff);
    }
    let signature = signature.to_bytes();
    Ok(CarrierEntry {
        height_diff,
        signature,
    })
}

/// A block on the reader's fixed tip's chain. `id` is the source's own handle for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Carrier {
    pub height: i32,
    pub hash: BlockHash,
    pub id: BlockId,
}

/// What the reader reads: the fixed tip's ancestry, its blocks' data and the shutdown flag.
pub trait CarrierSource {
    /// The tip's ancestor at `height`: `None` when the height is negative or above the tip.
    fn ancestor(&self, height: i32) -> Option<Carrier>;

    /// `LoadCarrier(carrier, carrier.height)`: [`load_carrier`] over the block's data.
    fn load(&mut self, carrier: &Carrier) -> Result<CarrierEntry, HistoricalClError>;

    /// `ShutdownRequested()`.
    fn interrupted(&self) -> bool;
}

/// Core's `CoinbaseChainLock`: a certificate a carrier embeds, its signature kept as bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoricalCl {
    /// The signed height: `carrier.height - diff - 1`.
    pub height: i32,
    pub block_hash: BlockHash,
    pub carrier: Carrier,
    pub signature: [u8; 96],
}

impl HistoricalCl {
    /// `Signed()`'s signature: the bytes, when they decode as a valid signature under `bls`
    /// (Core's process flag now). A valid one re-encodes to the same bytes under that flag.
    ///
    /// # Errors
    ///
    /// [`HistoricalClError::Malleable`] or [`HistoricalClError::InvalidSignature`].
    // Core: chainlock/clsig.cpp:86-105, :203-211
    pub fn signed(&self, bls: BlsCtx) -> Result<[u8; 96], HistoricalClError> {
        let sig =
            BlsGlobalSig::decode(&self.signature).map_err(|_| HistoricalClError::Malleable)?;
        if !sig.get(bls).is_valid() {
            return Err(HistoricalClError::InvalidSignature);
        }
        Ok(self.signature)
    }
}

/// Core's `CoinbaseChainLockReader` over a fixed tip at `tip_height`, for one request.
pub struct CoinbaseClReader<S> {
    tip_height: i32,
    v20_height: i32,
    source: S,
    seen: BTreeMap<i32, Option<HistoricalCl>>,
}

impl<S: CarrierSource> CoinbaseClReader<S> {
    pub fn new(tip_height: i32, v20_height: i32, source: S) -> Self {
        CoinbaseClReader {
            tip_height,
            v20_height,
            source,
            seen: BTreeMap::new(),
        }
    }

    /// `Read(carrier_height)`: the certificate the coinbase at that height embeds, if any.
    ///
    /// # Errors
    ///
    /// What Core's `Read` throws.
    // Core: chainlock/clsig.cpp:177-201
    pub fn read(&mut self, carrier_height: i32) -> Result<Option<HistoricalCl>, HistoricalClError> {
        let Some(carrier) = self.source.ancestor(carrier_height) else {
            return Ok(None);
        };
        if carrier_height < self.v20_height {
            return Ok(None);
        }
        if let Some(seen) = self.seen.get(&carrier_height) {
            return Ok(*seen);
        }
        if self.source.interrupted() {
            return Err(HistoricalClError::Interrupted);
        }
        if self.seen.len() >= MAX_HISTORICAL_CARRIER_LOOKUPS {
            return Err(HistoricalClError::BudgetExhausted);
        }
        let entry = self.source.load(&carrier)?;
        let found = if entry.has_chainlock() {
            let height = i64::from(carrier_height) - i64::from(entry.height_diff) - 1;
            let height =
                i32::try_from(height).map_err(|_| HistoricalClError::InvalidBlockHeight)?;
            let signed =
                self.source.ancestor(height).ok_or(HistoricalClError::InvalidBlockHeight)?;
            Some(HistoricalCl {
                height,
                block_hash: signed.hash,
                carrier,
                signature: entry.signature,
            })
        } else {
            None
        };
        self.seen.insert(carrier_height, found);
        Ok(found)
    }

    /// `Find(minimum, maximum)`: the first certificate at or above `minimum`, if it is at most
    /// `maximum`. Certified heights never decrease along a validated chain, so the search probes
    /// exponentially from the first carrier, then bisects.
    ///
    /// # Errors
    ///
    /// What a [`Self::read`] it makes throws.
    // Core: chainlock/clsig.cpp:213-246
    pub fn find(
        &mut self,
        minimum: i32,
        maximum: i32,
    ) -> Result<Option<HistoricalCl>, HistoricalClError> {
        let tip_height = self.tip_height;
        if minimum < 0 || minimum > maximum || minimum >= tip_height {
            return Ok(None);
        }
        let first_carrier = (minimum + 1).max(self.v20_height);
        if first_carrier > tip_height {
            return Ok(None);
        }
        let (mut low, mut high, mut step) = (first_carrier, first_carrier, 1_i64);
        while self.below_minimum(high, minimum)? {
            if high == tip_height {
                return Ok(None);
            }
            low = high + 1;
            high = i32::try_from(i64::from(tip_height).min(i64::from(high) + step))
                .expect("at most the tip height");
            step *= 2;
        }
        while low < high {
            let middle = low + (high - low) / 2;
            if self.below_minimum(middle, minimum)? {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        Ok(self.read(low)?.filter(|e| e.height <= maximum))
    }

    /// `below_minimum(height)`: no certificate at `height`, or one below `minimum`.
    fn below_minimum(&mut self, height: i32, minimum: i32) -> Result<bool, HistoricalClError> {
        Ok(self.read(height)?.is_none_or(|e| e.height < minimum))
    }
}

#[cfg(test)]
mod tests;
