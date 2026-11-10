// SPDX-License-Identifier: MIT

//! One block's basic filter: Core's `BlockFilter` (`src/blockfilter.h`).

use core::fmt;

use dash_primitives::{BlockHash, OutPoint, Transaction};
use dash_types::Numeric as _;

use crate::chain::{FilterHash, FilterHeader, filter_hash, filter_header};
use crate::elements::add_block_elements;
use crate::gcs::{self, FilterError, FilterQuery, GcsParams};

/// A block's basic filter: its encoded bytes and the parameters its block hash keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockFilter {
    params: GcsParams,
    encoded: Vec<u8>,
}

/// Why a filter could not be built from a block.
#[derive(Debug, PartialEq, Eq)]
pub enum BuildError<E> {
    /// The lookup of a spent output failed.
    Source(E),
    /// The filter does not encode.
    Filter(FilterError),
}

impl<E: fmt::Display> fmt::Display for BuildError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildError::Source(e) => write!(f, "spent output: {e}"),
            BuildError::Filter(e) => write!(f, "filter: {e}"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for BuildError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BuildError::Source(e) => Some(e),
            BuildError::Filter(e) => Some(e),
        }
    }
}

impl BlockFilter {
    /// Core's `BlockFilter(BASIC_FILTER, block_hash, filter, skip_decode_check)`: a filter
    /// received for the block with this hash. Unless `skip_decode_check`, it must hold exactly
    /// the elements its count says ([`gcs::check_encoded`]); the count itself is checked either
    /// way ([`gcs::read_count`]), as Core's constructor does.
    ///
    /// # Errors
    ///
    /// The error of [`gcs::check_encoded`], or of [`gcs::read_count`] when the decode is skipped.
    pub fn from_encoded(
        block_hash: &BlockHash,
        encoded: Vec<u8>,
        skip_decode_check: bool,
    ) -> Result<BlockFilter, FilterError> {
        let params = GcsParams::basic(&block_hash.to_lendian());
        if skip_decode_check {
            gcs::read_count(&encoded)?;
        } else {
            gcs::check_encoded(&params, &encoded)?;
        }
        Ok(BlockFilter {
            params,
            encoded,
        })
    }

    /// Core's `BlockFilter(BASIC_FILTER, block, block_undo)`: the filter of a block, whose
    /// first transaction is the coinbase. `script_for_coin` gives the script of an output the
    /// block spends (Core reads them from the undo data).
    ///
    /// # Errors
    ///
    /// [`BuildError::Source`] for the first error of `script_for_coin`.
    pub fn build<'a, S, E>(
        block_hash: &BlockHash,
        txs: impl IntoIterator<Item = &'a Transaction>,
        script_for_coin: impl FnMut(&OutPoint) -> Result<S, E>,
    ) -> Result<BlockFilter, BuildError<E>>
    where
        S: AsRef<[u8]>,
    {
        let params = GcsParams::basic(&block_hash.to_lendian());
        let mut elements = FilterQuery::new();
        add_block_elements(&mut elements, txs, script_for_coin).map_err(BuildError::Source)?;
        let encoded = gcs::encode(&params, &mut elements).map_err(BuildError::Filter)?;
        Ok(BlockFilter {
            params,
            encoded,
        })
    }

    /// The encoded filter, as `cfilter` carries it.
    pub fn encoded(&self) -> &[u8] {
        &self.encoded
    }

    /// Core's `BlockFilter::GetHash`.
    pub fn hash(&self) -> FilterHash {
        filter_hash(&self.encoded)
    }

    /// Core's `BlockFilter::ComputeHeader`.
    pub fn header(&self, prev_header: &FilterHeader) -> FilterHeader {
        filter_header(&self.hash(), prev_header)
    }

    /// Whether any element of `query` is in the filter ([`gcs::match_any`]).
    ///
    /// # Errors
    ///
    /// The error of [`gcs::match_any`].
    pub fn match_any(&self, query: &FilterQuery) -> Result<bool, FilterError> {
        gcs::match_any(&self.params, &self.encoded, query)
    }

    /// Whether every element of `query` is in the filter ([`gcs::match_all`]).
    ///
    /// # Errors
    ///
    /// The error of [`gcs::match_all`].
    pub fn match_all(&self, query: &FilterQuery) -> Result<bool, FilterError> {
        gcs::match_all(&self.params, &self.encoded, query)
    }
}
