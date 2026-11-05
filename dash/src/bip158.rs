// Written in 2019 by Tammas Blummer.
// SPDX-License-Identifier: CC0-1.0

// This module was largely copied from https://github.com/rust-bitcoin/murmel/blob/master/src/blockfilter.rs
// on 11. June 2019 which is licensed under Apache, that file specifically
// was written entirely by Tamas Blummer, who is re-licensing its contents here as CC0.

//! BIP 158 Compact Block Filters for Light Clients.
//!
//! This module implements a structure for compact filters on block data, for
//! use in the BIP 157 light client protocol. The filter construction proposed
//! is an alternative to Bloom filters, as used in BIP 37, that minimizes filter
//! size by using Golomb-Rice coding for compression.
//!
//! The codec, the Dash element set (including the special-transaction payload fields of Dash Core
//! v24) and the filter-header chain live in the `dash-filters` crate (SB9), which the node and
//! SPV share. What is here are the dashcore types and signatures that existed before the move,
//! converting to its base-sdk types and calling it.
//!
//! ### Relevant BIPS
//!
//! * [BIP 157 - Client Side Block Filtering](https://github.com/bitcoin/bips/blob/master/bip-0157.mediawiki)
//! * [BIP 158 - Compact Block Filters for Light Clients](https://github.com/bitcoin/bips/blob/master/bip-0158.mediawiki)
//!
//! # Examples
//!
//! ```ignore
//! fn get_script_for_coin(coin: &OutPoint) -> Result<ScriptBuf, BlockFilterError> {
//!   // get utxo ...
//! }
//!
//! // create a block filter for a block (server side)
//! let filter = BlockFilter::new_script_filter(&block, get_script_for_coin)?;
//!
//! // or create a filter from known raw data
//! let filter = BlockFilter::new(content);
//!
//! // read and evaluate a filter
//!
//! let query: Iterator<Item=ScriptBuf> = // .. some scripts you care about
//! if filter.match_any(&block_hash, &mut query.map(|s| s.as_bytes())) {
//!   // get this block
//! }
//!  ```
//!

use core::cmp;
use core::fmt::{self, Display, Formatter};

use dash_filters::{self as filters, FilterError, GcsParams};
use dash_num::Hash256;
use dash_primitives::Transaction as BaseTransaction;
use dash_types::Numeric as _;
use dash_types::codec::BaseCodec as _;
use hashes::Hash;
use internals::write_err;

use crate::blockdata::block::Block;
use crate::blockdata::script::Script;
use crate::blockdata::transaction::OutPoint;
use crate::blockdata::transaction::special_transaction::TransactionPayload;
use crate::consensus::encode::serialize;
use crate::hash_types::{BlockHash, FilterHash, FilterHeader};
use crate::io;
use crate::prelude::*;

pub use dash_filters::FilterQuery;

/// Errors for blockfilter.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Missing UTXO, cannot calculate script filter.
    UtxoMissing(OutPoint),
    /// IO error reading or writing binary serialization of the filter.
    Io(io::Error),
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter) -> Result<(), fmt::Error> {
        match *self {
            Error::UtxoMissing(ref coin) => write!(f, "unresolved UTXO {}", coin),
            Error::Io(ref e) => write_err!(f, "IO error"; e),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        use self::Error::*;

        match self {
            UtxoMissing(_) => None,
            Io(e) => Some(e),
        }
    }
}

impl From<io::Error> for Error {
    fn from(io: io::Error) -> Self {
        Error::Io(io)
    }
}

impl From<FilterError> for Error {
    fn from(e: FilterError) -> Self {
        Error::Io(match e {
            FilterError::Truncated => io::Error::from(io::ErrorKind::UnexpectedEof),
            e => io::Error::new(io::ErrorKind::InvalidData, e),
        })
    }
}

fn to_hash256(bytes: [u8; 32]) -> Hash256 {
    Hash256::from_lendian(bytes)
}

fn to_filter_header(header: Hash256) -> FilterHeader {
    FilterHeader::from_byte_array(header.to_lendian())
}

/// A block filter, as described by BIP 158.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockFilter {
    /// Golomb encoded filter
    pub content: Vec<u8>,
}

impl FilterHash {
    /// Computes the filter header from a filter hash and previous filter header.
    pub fn filter_header(&self, previous_filter_header: &FilterHeader) -> FilterHeader {
        to_filter_header(filters::filter_header(
            &to_hash256(self.to_byte_array()),
            &to_hash256(previous_filter_header.to_byte_array()),
        ))
    }
}

impl BlockFilter {
    /// Creates a new filter from pre-computed data.
    pub fn new(content: &[u8]) -> BlockFilter {
        BlockFilter {
            content: content.to_vec(),
        }
    }

    /// Computes a SCRIPT_FILTER that contains spent and output scripts, and the fields of the
    /// special transactions Dash Core's filter carries.
    pub fn new_script_filter<M, S>(block: &Block, script_for_coin: M) -> Result<BlockFilter, Error>
    where
        M: Fn(&OutPoint) -> Result<S, Error>,
        S: Borrow<Script>,
    {
        let mut out = Vec::new();
        let mut writer = BlockFilterWriter::new(&mut out, block);

        writer.add_output_scripts();
        writer.add_special_tx_elements();
        writer.add_input_scripts(script_for_coin)?;
        writer.finish()?;

        Ok(BlockFilter {
            content: out,
        })
    }

    /// Computes this filter's ID in a chain of filters (see [BIP 157]).
    ///
    /// [BIP 157]: <https://github.com/bitcoin/bips/blob/master/bip-0157.mediawiki#Filter_Headers>
    pub fn filter_header(&self, previous_filter_header: &FilterHeader) -> FilterHeader {
        to_filter_header(filters::filter_header(
            &filters::filter_hash(&self.content),
            &to_hash256(previous_filter_header.to_byte_array()),
        ))
    }

    /// Returns true if any element of `query` matches against this [`BlockFilter`].
    pub fn match_any(&self, block_hash: &BlockHash, query: &FilterQuery) -> Result<bool, Error> {
        let params = GcsParams::basic(&block_hash.to_byte_array());
        Ok(filters::match_any(&params, &self.content, query)?)
    }

    /// Returns true if all elements of `query` match against this [`BlockFilter`].
    pub fn match_all(&self, block_hash: &BlockHash, query: &FilterQuery) -> Result<bool, Error> {
        let params = GcsParams::basic(&block_hash.to_byte_array());
        Ok(filters::match_all(&params, &self.content, query)?)
    }
}

/// Compiles and writes a block filter.
pub struct BlockFilterWriter<'a, W> {
    block: &'a Block,
    writer: GcsFilterWriter<'a, W>,
}

impl<'a, W: io::Write> BlockFilterWriter<'a, W> {
    /// Creates a new [`BlockFilterWriter`] from `block`.
    pub fn new(writer: &'a mut W, block: &'a Block) -> BlockFilterWriter<'a, W> {
        let params = GcsParams::basic(&block.block_hash().to_byte_array());
        BlockFilterWriter {
            block,
            writer: GcsFilterWriter::with_params(writer, Some(params)),
        }
    }

    /// Adds output scripts of the block to filter (excluding OP_RETURN scripts).
    pub fn add_output_scripts(&mut self) {
        for transaction in &self.block.txdata {
            for output in &transaction.output {
                if !output.script_pubkey.is_op_return() {
                    self.add_element(output.script_pubkey.as_bytes());
                }
            }
        }
    }

    /// Adds the fields of the block's special transactions that Dash Core's filter carries
    /// (`ExtractSpecialTxFilterElements`): collateral outpoints, owner and voting key ids, payout
    /// scripts, ProTx hashes and asset-lock credit outputs.
    pub fn add_special_tx_elements(&mut self) {
        for transaction in &self.block.txdata {
            // The coinbase and quorum commitment payloads, which are in nearly every block, add
            // nothing, so they are not re-serialized.
            if matches!(
                transaction.special_transaction_payload,
                None | Some(
                    TransactionPayload::CoinbasePayloadType(_)
                        | TransactionPayload::QuorumCommitmentPayloadType(_)
                )
            ) {
                continue;
            }
            // dash-filters reads base-sdk values; a transaction it cannot decode adds nothing,
            // as a payload Core's `GetTxPayload` rejects does.
            if let Ok(tx) = BaseTransaction::decode(&mut serialize(transaction).as_slice()) {
                filters::elements::add_special_tx_elements(&mut self.writer.elements, &tx);
            }
        }
    }

    /// Adds consumed output scripts of a block to filter.
    pub fn add_input_scripts<M, S>(&mut self, script_for_coin: M) -> Result<(), Error>
    where
        M: Fn(&OutPoint) -> Result<S, Error>,
        S: Borrow<Script>,
    {
        for script in self
            .block
            .txdata
            .iter()
            .skip(1) // skip coinbase
            .flat_map(|t| t.input.iter().map(|i| &i.previous_output))
            .map(script_for_coin)
        {
            let script = script?;
            self.add_element(script.borrow().as_bytes());
        }
        Ok(())
    }

    /// Adds an arbitrary element to filter.
    pub fn add_element(&mut self, data: &[u8]) {
        self.writer.add_element(data);
    }

    /// Writes the block filter.
    pub fn finish(&mut self) -> Result<usize, io::Error> {
        self.writer.finish()
    }
}

/// Reads and interprets a block filter.
pub struct BlockFilterReader {
    reader: GcsFilterReader,
}

impl BlockFilterReader {
    /// Creates a new [`BlockFilterReader`] from `block_hash`.
    pub fn new(block_hash: &BlockHash) -> BlockFilterReader {
        BlockFilterReader {
            reader: GcsFilterReader::with_params(Some(GcsParams::basic(
                &block_hash.to_byte_array(),
            ))),
        }
    }

    /// Returns true if any element of `query` matches against this [`BlockFilterReader`].
    pub fn match_any<R>(&self, reader: &mut R, query: &FilterQuery) -> Result<bool, Error>
    where
        R: io::Read + ?Sized,
    {
        self.reader.match_any(reader, query)
    }

    /// Returns true if all elements of `query` match against this [`BlockFilterReader`].
    pub fn match_all<R>(&self, reader: &mut R, query: &FilterQuery) -> Result<bool, Error>
    where
        R: io::Read + ?Sized,
    {
        self.reader.match_all(reader, query)
    }
}

/// The parameters of a filter whose keys, `m` and `p` came in as plain numbers: `None` for a `p`
/// above 63, which the old reader and writer could not shift by either.
fn gcs_params(k0: u64, k1: u64, m: u64, p: u8) -> Option<GcsParams> {
    GcsParams::new(k0, k1, p, m).ok()
}

fn invalid_params() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "unsupported GCS filter parameters")
}

/// An `io::Read` as the byte source of `dash-filters`' matchers: one single-byte `read_exact` per
/// byte the match needs, as the bit stream reader of this module always read.
struct IoSource<'a, R: ?Sized>(&'a mut R);

impl<R: io::Read + ?Sized> filters::ByteSource for IoSource<'_, R> {
    type Error = Error;

    fn next_byte(&mut self) -> Result<u8, Error> {
        let mut byte = [0u8];
        self.0.read_exact(&mut byte)?;
        Ok(byte[0])
    }
}

/// Golomb-Rice encoded filter reader.
pub struct GcsFilterReader {
    params: Option<GcsParams>,
}

impl GcsFilterReader {
    /// Creates a new [`GcsFilterReader`] with specific seed to siphash.
    pub fn new(k0: u64, k1: u64, m: u64, p: u8) -> GcsFilterReader {
        GcsFilterReader::with_params(gcs_params(k0, k1, m, p))
    }

    fn with_params(params: Option<GcsParams>) -> GcsFilterReader {
        GcsFilterReader {
            params,
        }
    }

    fn params(&self) -> Result<GcsParams, Error> {
        self.params.ok_or_else(|| invalid_params().into())
    }

    /// Returns true if any element of `query` matches against this [`GcsFilterReader`]. It reads
    /// the filter's count and then only the values up to the one that decides the result, never
    /// beyond the filter, so a stream with further filters, or with nothing more yet, is not
    /// disturbed. A count that cannot be read, because the stream failed on it too, is no elements.
    pub fn match_any<R>(&self, reader: &mut R, query: &FilterQuery) -> Result<bool, Error>
    where
        R: io::Read + ?Sized,
    {
        filters::match_any_from(&self.params()?, IoSource(reader), query)
    }

    /// Returns true if all elements of `query` match against this [`GcsFilterReader`], reading as
    /// [`GcsFilterReader::match_any`] does.
    pub fn match_all<R>(&self, reader: &mut R, query: &FilterQuery) -> Result<bool, Error>
    where
        R: io::Read + ?Sized,
    {
        filters::match_all_from(&self.params()?, IoSource(reader), query)
    }
}

/// Golomb-Rice encoded filter writer.
pub struct GcsFilterWriter<'a, W> {
    writer: &'a mut W,
    params: Option<GcsParams>,
    elements: FilterQuery,
}

impl<'a, W: io::Write> GcsFilterWriter<'a, W> {
    /// Creates a new [`GcsFilterWriter`] wrapping a generic writer, with specific seed to siphash.
    pub fn new(writer: &'a mut W, k0: u64, k1: u64, m: u64, p: u8) -> GcsFilterWriter<'a, W> {
        GcsFilterWriter::with_params(writer, gcs_params(k0, k1, m, p))
    }

    fn with_params(writer: &'a mut W, params: Option<GcsParams>) -> GcsFilterWriter<'a, W> {
        GcsFilterWriter {
            writer,
            params,
            elements: FilterQuery::new(),
        }
    }

    /// Adds data to the filter.
    pub fn add_element(&mut self, element: &[u8]) {
        if !element.is_empty() {
            self.elements.push(element);
        }
    }

    /// Writes the filter to the wrapped writer.
    pub fn finish(&mut self) -> Result<usize, io::Error> {
        let params = self.params.ok_or_else(invalid_params)?;
        // BIP158 counts distinct elements; the encoder folds the duplicates.
        let encoded = filters::encode(&params, &mut self.elements)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        self.writer.write_all(&encoded)?;
        Ok(encoded.len())
    }
}

// The bit streams below are not used by the filter codec any more (dash-filters has its own bit
// reader and writer); they stay for callers of these public types.

/// Bitwise stream reader.
pub struct BitStreamReader<'a, R> {
    buffer: [u8; 1],
    offset: u8,
    reader: &'a mut R,
}

impl<'a, R: io::Read> BitStreamReader<'a, R> {
    /// Creates a new [`BitStreamReader`] that reads bitwise from a given `reader`.
    pub fn new(reader: &'a mut R) -> BitStreamReader<'a, R> {
        BitStreamReader {
            buffer: [0u8],
            reader,
            offset: 8,
        }
    }

    /// Reads nbit bits, returning the bits in a `u64` starting with the rightmost bit.
    ///
    /// # Examples
    /// ```
    /// # use dashcore::bip158::BitStreamReader;
    /// # let data = vec![0xff];
    /// # let mut input = data.as_slice();
    /// let mut reader = BitStreamReader::new(&mut input); // input contains all 1's
    /// let res = reader.read(1).expect("read failed");
    /// assert_eq!(res, 1_u64);
    /// ```
    pub fn read(&mut self, mut nbits: u8) -> Result<u64, io::Error> {
        if nbits > 64 {
            return Err(io::Error::other("can not read more than 64 bits at once"));
        }
        let mut data = 0u64;
        while nbits > 0 {
            if self.offset == 8 {
                self.reader.read_exact(&mut self.buffer)?;
                self.offset = 0;
            }
            let bits = cmp::min(8 - self.offset, nbits);
            data <<= bits;
            data |= ((self.buffer[0] << self.offset) >> (8 - bits)) as u64;
            self.offset += bits;
            nbits -= bits;
        }
        Ok(data)
    }
}

/// Bitwise stream writer.
pub struct BitStreamWriter<'a, W> {
    buffer: [u8; 1],
    offset: u8,
    writer: &'a mut W,
}

impl<'a, W: io::Write> BitStreamWriter<'a, W> {
    /// Creates a new [`BitStreamWriter`] that writes bitwise to a given `writer`.
    pub fn new(writer: &'a mut W) -> BitStreamWriter<'a, W> {
        BitStreamWriter {
            buffer: [0u8],
            writer,
            offset: 0,
        }
    }

    /// Writes nbits bits from data.
    pub fn write(&mut self, data: u64, mut nbits: u8) -> Result<usize, io::Error> {
        if nbits > 64 {
            return Err(io::Error::other("can not write more than 64 bits at once"));
        }
        let mut wrote = 0;
        while nbits > 0 {
            let bits = cmp::min(8 - self.offset, nbits);
            self.buffer[0] |= ((data << (64 - nbits)) >> (64 - 8 + self.offset)) as u8;
            self.offset += bits;
            nbits -= bits;
            if self.offset == 8 {
                wrote += self.flush()?;
            }
        }
        Ok(wrote)
    }

    /// flush bits not yet written.
    pub fn flush(&mut self) -> Result<usize, io::Error> {
        if self.offset > 0 {
            self.writer.write_all(&self.buffer)?;
            self.buffer[0] = 0u8;
            self.offset = 0;
            Ok(1)
        } else {
            Ok(0)
        }
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashMap;

    use serde_json::Value;

    use super::*;
    use crate::ScriptBuf;
    use crate::consensus::encode::deserialize;
    use crate::hash_types::BlockHash;
    use crate::internal_macros::hex;

    /// Golomb encoding parameter as in BIP-158, see also https://gist.github.com/sipa/576d5f09c3b86c3b1b75598d799fc845
    const P: u8 = filters::BASIC_FILTER_P;
    const M: u64 = filters::BASIC_FILTER_M;

    #[test]
    fn test_blockfilters() {
        // test vectors from: https://github.com/dashpay/dash/blob/436872aaec86edad3c6243aecd2549453608d0c1/src/test/data/blockfilters.json
        let data = include_str!("../tests/data/blockfilters.json");

        let testdata = serde_json::from_str::<Value>(data).unwrap().as_array().unwrap().clone();
        for t in testdata.iter().skip(1) {
            let block_hash = t.get(1).unwrap().as_str().unwrap().parse::<BlockHash>().unwrap();
            let block: Block = deserialize(&hex!(t.get(2).unwrap().as_str().unwrap())).unwrap();
            assert_eq!(block.block_hash(), block_hash);
            let scripts = t.get(3).unwrap().as_array().unwrap();
            let previous_filter_header =
                t.get(4).unwrap().as_str().unwrap().parse::<FilterHeader>().unwrap();
            let filter_content = hex!(t.get(5).unwrap().as_str().unwrap());
            let filter_header =
                t.get(6).unwrap().as_str().unwrap().parse::<FilterHeader>().unwrap();

            let mut txmap = HashMap::new();
            let mut si = scripts.iter();
            for tx in block.txdata.iter().skip(1) {
                for input in tx.input.iter() {
                    txmap.insert(
                        input.previous_output,
                        ScriptBuf::from(hex!(si.next().unwrap().as_str().unwrap())),
                    );
                }
            }

            let filter = BlockFilter::new_script_filter(&block, |o| {
                if let Some(s) = txmap.get(o) {
                    Ok(s.clone())
                } else {
                    Err(Error::UtxoMissing(*o))
                }
            })
            .unwrap();

            let test_filter = BlockFilter::new(filter_content.as_slice());

            assert_eq!(test_filter.content, filter.content);

            let block_hash = &block.block_hash();
            let all: FilterQuery =
                txmap.values().filter(|s| !s.is_empty()).map(|s| s.as_bytes()).collect();
            assert!(filter.match_all(block_hash, &all).unwrap());

            for script in txmap.values() {
                if !script.is_empty() {
                    let query: FilterQuery = core::iter::once(script.as_bytes()).collect();
                    assert!(filter.match_any(block_hash, &query).unwrap());
                }
            }

            assert_eq!(filter_header, filter.filter_header(&previous_filter_header));
        }
    }

    #[test]
    fn test_filter() {
        let mut patterns = BTreeSet::new();

        patterns.insert(hex!("000000"));
        patterns.insert(hex!("111111"));
        patterns.insert(hex!("222222"));
        patterns.insert(hex!("333333"));
        patterns.insert(hex!("444444"));
        patterns.insert(hex!("555555"));
        patterns.insert(hex!("666666"));
        patterns.insert(hex!("777777"));
        patterns.insert(hex!("888888"));
        patterns.insert(hex!("999999"));
        patterns.insert(hex!("aaaaaa"));
        patterns.insert(hex!("bbbbbb"));
        patterns.insert(hex!("cccccc"));
        patterns.insert(hex!("dddddd"));
        patterns.insert(hex!("eeeeee"));
        patterns.insert(hex!("ffffff"));

        let mut out = Vec::new();
        {
            let mut writer = GcsFilterWriter::new(&mut out, 0, 0, M, P);
            for p in &patterns {
                writer.add_element(p.as_slice());
            }
            writer.finish().unwrap();
        }

        let bytes = out;

        {
            let query: FilterQuery =
                [hex!("abcdef"), hex!("eeeeee")].iter().map(|v| v.as_slice()).collect();
            let reader = GcsFilterReader::new(0, 0, M, P);
            assert!(reader.match_any(&mut bytes.as_slice(), &query).unwrap());
        }
        {
            let query: FilterQuery =
                [hex!("abcdef"), hex!("123456")].iter().map(|v| v.as_slice()).collect();
            let reader = GcsFilterReader::new(0, 0, M, P);
            assert!(!reader.match_any(&mut bytes.as_slice(), &query).unwrap());
        }
        {
            let reader = GcsFilterReader::new(0, 0, M, P);
            let query: FilterQuery = patterns.iter().map(|v| v.as_slice()).collect();
            assert!(reader.match_all(&mut bytes.as_slice(), &query).unwrap());
        }
        {
            let reader = GcsFilterReader::new(0, 0, M, P);
            let mut query: FilterQuery = patterns.iter().map(|v| v.as_slice()).collect();
            query.push(&hex!("abcdef"));
            assert!(!reader.match_all(&mut bytes.as_slice(), &query).unwrap());
        }
    }

    #[test]
    fn test_bit_stream() {
        let mut out = Vec::new();
        {
            let mut writer = BitStreamWriter::new(&mut out);
            writer.write(0, 1).unwrap(); // 0
            writer.write(2, 2).unwrap(); // 10
            writer.write(6, 3).unwrap(); // 110
            writer.write(11, 4).unwrap(); // 1011
            writer.write(1, 5).unwrap(); // 00001
            writer.write(32, 6).unwrap(); // 100000
            writer.write(7, 7).unwrap(); // 0000111
            writer.flush().unwrap();
        }
        let bytes = out;
        assert_eq!(
            "01011010110000110000000001110000",
            format!("{:08b}{:08b}{:08b}{:08b}", bytes[0], bytes[1], bytes[2], bytes[3])
        );
        {
            let mut input = bytes.as_slice();
            let mut reader = BitStreamReader::new(&mut input);
            assert_eq!(reader.read(1).unwrap(), 0);
            assert_eq!(reader.read(2).unwrap(), 2);
            assert_eq!(reader.read(3).unwrap(), 6);
            assert_eq!(reader.read(4).unwrap(), 11);
            assert_eq!(reader.read(5).unwrap(), 1);
            assert_eq!(reader.read(6).unwrap(), 32);
            assert_eq!(reader.read(7).unwrap(), 7);
            // 4 bits remained
            assert!(reader.read(5).is_err());
        }
    }

    /// The filter of `elements` with keys (0, 0), as the old writer wrote it.
    fn written(m: u64, p: u8, elements: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut writer = GcsFilterWriter::new(&mut out, 0, 0, m, p);
        for element in elements {
            writer.add_element(element);
        }
        let wrote = writer.finish().unwrap();
        assert_eq!(wrote, out.len());
        out
    }

    /// A stream that has `data` and then no more yet: `WouldBlock`, as a socket with the rest of
    /// the message still on the wire.
    struct ThenWouldBlock<'a>(&'a [u8]);

    impl io::Read for ThenWouldBlock<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.0.is_empty() {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            io::Read::read(&mut self.0, buf)
        }
    }

    fn is_would_block(e: &Error) -> bool {
        matches!(e, Error::Io(e) if e.kind() == io::ErrorKind::WouldBlock)
    }

    fn query(elements: &[&[u8]]) -> FilterQuery {
        elements.iter().copied().collect()
    }

    #[test]
    fn a_matching_filter_needs_no_end_of_stream() {
        let hello = query(&[b"hello"]);
        let other = query(&[b"other"]);
        let bytes = written(M, P, &[b"hello"]);
        let reader = GcsFilterReader::new(0, 0, M, P);
        // The whole filter is there and then the stream stalls: the match must not wait for more.
        assert!(reader.match_any(&mut ThenWouldBlock(&bytes), &hello).unwrap());
        assert!(reader.match_all(&mut ThenWouldBlock(&bytes), &hello).unwrap());
        assert!(!reader.match_any(&mut ThenWouldBlock(&bytes), &other).unwrap());
        assert!(!reader.match_all(&mut ThenWouldBlock(&bytes), &other).unwrap());
        // A stream that stalls inside the filter is the caller's error, as before.
        let cut = &bytes[..bytes.len() - 1];
        let err = reader.match_any(&mut ThenWouldBlock(cut), &hello).unwrap_err();
        assert!(is_would_block(&err), "{err:?}");
        // The block filter reader reads the same way.
        let hash = BlockHash::from_byte_array([7; 32]);
        let mut out = Vec::new();
        let mut writer =
            GcsFilterWriter::with_params(&mut out, Some(GcsParams::basic(&hash.to_byte_array())));
        writer.add_element(b"hello");
        writer.finish().unwrap();
        let block_reader = BlockFilterReader::new(&hash);
        assert!(block_reader.match_any(&mut ThenWouldBlock(&out), &hello).unwrap());
        assert!(block_reader.match_all(&mut ThenWouldBlock(&out), &hello).unwrap());
    }

    #[test]
    fn a_match_stops_reading_at_the_matching_value() {
        let elements: [&[u8]; 4] = [b"one", b"two", b"three", b"four"];
        let bytes = written(M, P, &elements);
        let reader = GcsFilterReader::new(0, 0, M, P);
        // Some element is decided before the filter ends: the stream is left with bytes, and a
        // stream that stalls right after them still matches.
        let left: Vec<usize> = elements
            .iter()
            .map(|e| {
                let mut stream = bytes.as_slice();
                assert!(reader.match_any(&mut stream, &query(&[e])).unwrap());
                stream.len()
            })
            .collect();
        let early = left.iter().copied().max().unwrap();
        assert!(early > 0, "{left:?}");
        let e = elements[left.iter().position(|&l| l == early).unwrap()];
        let cut = &bytes[..bytes.len() - early];
        assert!(reader.match_any(&mut ThenWouldBlock(cut), &query(&[e])).unwrap());
    }

    #[test]
    fn concatenated_filters_are_not_consumed() {
        let first = written(M, P, &[b"hello"]);
        let second = written(M, P, &[b"world", b"hello"]);
        let mut both = first.clone();
        both.extend_from_slice(&second);
        let reader = GcsFilterReader::new(0, 0, M, P);

        let mut stream = both.as_slice();
        assert!(reader.match_any(&mut stream, &query(&[b"hello"])).unwrap());
        assert_eq!(stream, second.as_slice());
        // Matching every element of the second one takes the stream to its end.
        assert!(reader.match_all(&mut stream, &query(&[b"world", b"hello"])).unwrap());
        assert!(stream.is_empty());

        // A miss on a one-element filter takes exactly that filter too.
        let mut stream = both.as_slice();
        assert!(!reader.match_any(&mut stream, &query(&[b"nothing"])).unwrap());
        assert_eq!(stream, second.as_slice());
    }

    #[test]
    fn an_empty_query_reads_only_the_count() {
        let reader = GcsFilterReader::new(0, 0, M, P);
        let empty = FilterQuery::new();
        // A count of one, and then a body that cannot be read.
        let mut stream = ThenWouldBlock(&[1]);
        assert!(!reader.match_any(&mut stream, &empty).unwrap());
        assert!(stream.0.is_empty());
        let mut stream = ThenWouldBlock(&[1, 0xff]);
        assert!(reader.match_all(&mut stream, &empty).unwrap());
        assert_eq!(stream.0, [0xff]);
        // A count that cannot be read is no elements, as before.
        let hello = query(&[b"hello"]);
        assert!(!reader.match_any(&mut ThenWouldBlock(&[]), &hello).unwrap());
        assert!(!reader.match_any(&mut ThenWouldBlock(&[0xfd, 1, 0]), &hello).unwrap());
    }

    #[test]
    fn the_generic_parameter_domain_is_kept() {
        // What the old writer wrote for one element with these parameters: sizes in bytes.
        for (m, p, size) in [(M, 33, 6), (1u64 << 32, 19, 567)] {
            let bytes = written(m, p, &[b"hello"]);
            assert_eq!(bytes.len(), size, "m {m} p {p}");
            let hello = query(&[b"hello"]);
            let reader = GcsFilterReader::new(0, 0, m, p);
            assert!(reader.match_any(&mut bytes.as_slice(), &hello).unwrap(), "m {m} p {p}");
            assert!(reader.match_all(&mut bytes.as_slice(), &hello).unwrap(), "m {m} p {p}");
        }
        // A shift the old code could not make is refused, not wrapped.
        let reader = GcsFilterReader::new(0, 0, M, 64);
        let hello = query(&[b"hello"]);
        assert!(matches!(reader.match_any(&mut [1u8, 0].as_slice(), &hello), Err(Error::Io(_))));
    }
}
