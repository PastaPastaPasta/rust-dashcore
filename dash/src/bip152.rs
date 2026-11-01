// SPDX-License-Identifier: CC0-1.0

//! BIP152 Compact Blocks
//!
//! Implementation of compact blocks data structure and algorithms.
//!

use core::convert::{TryFrom, TryInto};
use core::{convert, fmt};
use std::error;

use hashes::{Hash, sha256, siphash24};
use internals::{impl_array_newtype, impl_bytes_newtype};

use crate::consensus::encode::{self, Decodable, Encodable, VarInt};
use crate::internal_macros::impl_consensus_encoding;
use crate::prelude::*;
use crate::{Block, BlockHash, Transaction, block, io};

/// A BIP-152 error
#[derive(Clone, PartialEq, Eq, Debug, Copy, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Error {
    /// An unknown version number was used.
    UnknownVersion,
    /// The prefill slice provided was invalid.
    InvalidPrefill,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Error::UnknownVersion => write!(f, "an unknown version number was used"),
            Error::InvalidPrefill => write!(f, "the prefill slice provided was invalid"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        use self::Error::*;

        match *self {
            UnknownVersion | InvalidPrefill => None,
        }
    }
}

/// A [PrefilledTransaction] structure is used in [HeaderAndShortIds] to
/// provide a list of a few transactions explicitly.
#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord, Hash)]
pub struct PrefilledTransaction {
    /// The index of the transaction in the block.
    ///
    /// This field is differentially encoded relative to the previous
    /// prefilled transaction as described as follows:
    ///
    /// > Several uses of CompactSize below are "differentially encoded". For
    /// > these, instead of using raw indexes, the number encoded is the
    /// > difference between the current index and the previous index, minus one.
    /// > For example, a first index of 0 implies a real index of 0, a second
    /// > index of 0 thereafter refers to a real index of 1, etc.
    pub idx: u16,
    /// The actual transaction.
    pub tx: Transaction,
}

impl convert::AsRef<Transaction> for PrefilledTransaction {
    fn as_ref(&self) -> &Transaction {
        &self.tx
    }
}

impl Encodable for PrefilledTransaction {
    #[inline]
    fn consensus_encode<S: io::Write + ?Sized>(&self, mut s: &mut S) -> Result<usize, io::Error> {
        Ok(VarInt(self.idx as u64).consensus_encode(&mut s)? + self.tx.consensus_encode(&mut s)?)
    }
}

impl Decodable for PrefilledTransaction {
    #[inline]
    fn consensus_decode<D: io::Read + ?Sized>(
        mut d: &mut D,
    ) -> Result<PrefilledTransaction, encode::Error> {
        // Core reads `COMPACTSIZE(obj.index)` of a `uint16_t`: `ReadCompactSize` with its range
        // check, then the type's bound.
        let idx = encode::read_compact_size_len(&mut d)?;
        let idx = u16::try_from(idx)
            .map_err(|_| encode::Error::ParseFailed("BIP152 prefilled tx index out of bounds"))?;
        let tx = Transaction::consensus_decode(&mut d)?;
        Ok(PrefilledTransaction {
            idx,
            tx,
        })
    }
}

/// Short transaction IDs are used to represent a transaction without sending a full 256-bit hash.
#[derive(PartialEq, Eq, Clone, Copy, Hash, Default, PartialOrd, Ord)]
pub struct ShortId([u8; 6]);
impl_array_newtype!(ShortId, u8, 6);
impl_bytes_newtype!(ShortId, 6);

impl ShortId {
    /// Calculate the SipHash24 keys used to calculate short IDs.
    pub fn calculate_siphash_keys(header: &block::Header, nonce: u64) -> (u64, u64) {
        // 1. single-SHA256 hashing the block header with the nonce appended (in little-endian)
        let h = {
            let mut engine = sha256::Hash::engine();
            header.consensus_encode(&mut engine).expect("engines don't error");
            nonce.consensus_encode(&mut engine).expect("engines don't error");
            sha256::Hash::from_engine(engine)
        };

        // 2. Running SipHash-2-4 with the input being the transaction ID and the keys (k0/k1)
        // set to the first two little-endian 64-bit integers from the above hash, respectively.
        (
            u64::from_le_bytes(h[0..8].try_into().expect("8 byte slice")),
            u64::from_le_bytes(h[8..16].try_into().expect("8 byte slice")),
        )
    }

    /// Calculate the short ID with the given (w)txid and using the provided SipHash keys.
    pub fn with_siphash_keys<T: AsRef<[u8]>>(txid: &T, siphash_keys: (u64, u64)) -> ShortId {
        // 2. Running SipHash-2-4 with the input being the transaction ID and the keys (k0/k1)
        // set to the first two little-endian 64-bit integers from the above hash, respectively.
        let hash = siphash24::siphash(siphash_keys.0, siphash_keys.1, txid.as_ref());

        // 3. Dropping the 2 most significant bytes from the SipHash output to make it 6 bytes.
        let mut id = ShortId([0; 6]);
        id.0.copy_from_slice(&hash.to_le_bytes()[0..6]);
        id
    }
}

impl Encodable for ShortId {
    #[inline]
    fn consensus_encode<S: io::Write + ?Sized>(&self, s: &mut S) -> Result<usize, io::Error> {
        self.0.consensus_encode(s)
    }
}

impl Decodable for ShortId {
    #[inline]
    fn consensus_decode<D: io::Read + ?Sized>(d: &mut D) -> Result<ShortId, encode::Error> {
        Ok(ShortId(Decodable::consensus_decode(d)?))
    }
}

/// A [HeaderAndShortIds] structure is used to relay a block header, the short
/// transactions IDs used for matching already-available transactions, and a
/// select few transactions which we expect a peer may be missing.
#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord, Hash)]
pub struct HeaderAndShortIds {
    /// The header of the block being provided.
    pub header: block::Header,
    ///  A nonce for use in short transaction ID calculations.
    pub nonce: u64,
    ///  The short transaction IDs calculated from the transactions
    ///  which were not provided explicitly in prefilled_txs.
    pub short_ids: Vec<ShortId>,
    ///  Used to provide the coinbase transaction and a select few
    ///  which we expect a peer may be missing.
    pub prefilled_txs: Vec<PrefilledTransaction>,
}

impl Encodable for HeaderAndShortIds {
    #[inline]
    fn consensus_encode<S: io::Write + ?Sized>(&self, s: &mut S) -> Result<usize, io::Error> {
        let mut len = self.header.consensus_encode(s)?;
        len += self.nonce.consensus_encode(s)?;
        len += self.short_ids.consensus_encode(s)?;
        len += self.prefilled_txs.consensus_encode(s)?;
        Ok(len)
    }
}

impl Decodable for HeaderAndShortIds {
    #[inline]
    fn consensus_decode_from_finite_reader<D: io::Read + ?Sized>(
        d: &mut D,
    ) -> Result<HeaderAndShortIds, encode::Error> {
        let compact = HeaderAndShortIds {
            header: Decodable::consensus_decode_from_finite_reader(d)?,
            nonce: Decodable::consensus_decode_from_finite_reader(d)?,
            short_ids: Decodable::consensus_decode_from_finite_reader(d)?,
            prefilled_txs: Decodable::consensus_decode_from_finite_reader(d)?,
        };
        // Core, once the message is read (`CBlockHeaderAndShortTxIDs::SERIALIZE_METHODS`).
        if compact.short_ids.len() + compact.prefilled_txs.len() > usize::from(u16::MAX) {
            return Err(encode::Error::ParseFailed("indexes overflowed 16 bits"));
        }
        Ok(compact)
    }
}

impl HeaderAndShortIds {
    /// Create a new [HeaderAndShortIds] from a full block.
    ///
    /// The version number must be 1, the only compact block version Dash Core speaks
    /// (`CMPCTBLOCKS_VERSION`): Dash has no BIP144, so BIP152's version 2 (witness
    /// transactions, wtxid short IDs) does not exist.
    ///
    /// The `prefill` slice indicates which transactions should be prefilled in
    /// the block. It should contain the indexes in the block of the txs to
    /// prefill. It must be ordered. 0 should not be included as the
    /// coinbase tx is always prefilled.
    ///
    /// > Nodes SHOULD NOT use the same nonce across multiple different blocks.
    pub fn from_block(
        block: &Block,
        nonce: u64,
        version: u32,
        mut prefill: &[usize],
    ) -> Result<HeaderAndShortIds, Error> {
        if version != 1 {
            return Err(Error::UnknownVersion);
        }

        let siphash_keys = ShortId::calculate_siphash_keys(&block.header, nonce);

        let mut prefilled = Vec::with_capacity(prefill.len() + 1); // +1 for coinbase tx
        let mut short_ids = Vec::with_capacity(block.txdata.len() - prefill.len());
        let mut last_prefill = 0;
        for (idx, tx) in block.txdata.iter().enumerate() {
            // Check if we should prefill this tx.
            let prefill_tx = if prefill.first() == Some(&idx) {
                prefill = &prefill[1..];
                true
            } else {
                idx == 0 // Always prefill coinbase.
            };

            if prefill_tx {
                let diff_idx = idx - last_prefill;
                last_prefill = idx + 1;
                prefilled.push(PrefilledTransaction {
                    idx: diff_idx as u16,
                    // As encoded in "tx" messages sent in response to getdata MSG_TX.
                    tx: tx.clone(),
                });
            } else {
                short_ids.push(ShortId::with_siphash_keys(
                    // Dash Core hashes the full serialization (`GetInstanceHash()`), which
                    // differs from the txid for version 2+ asset unlocks; its mempool is
                    // indexed the same way.
                    &tx.instance_hash().to_raw_hash(),
                    siphash_keys,
                ));
            }
        }

        if !prefill.is_empty() {
            return Err(Error::InvalidPrefill);
        }

        Ok(HeaderAndShortIds {
            header: block.header,
            nonce,
            // Provide coinbase prefilled.
            prefilled_txs: prefilled,
            short_ids,
        })
    }
}

/// A [BlockTransactionsRequest] structure is used to list transaction indexes
/// in a block being requested.
#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord, Hash)]
pub struct BlockTransactionsRequest {
    ///  The blockhash of the block which the transactions being requested are in.
    pub block_hash: BlockHash,
    ///  The indexes of the transactions being requested in the block.
    ///
    ///  Core holds these as `uint16_t`: decoding refuses an index above [`u16::MAX`], and encoding
    ///  refuses indexes that are not increasing. See [`BlockTransactionsRequest::consensus_encode()`]
    pub indexes: Vec<u64>,
}

impl Encodable for BlockTransactionsRequest {
    /// Writes the indexes differentially, as Core's `DifferenceFormatter::Ser`.
    ///
    /// # Errors
    ///
    /// Like Core ("differential value overflow"), an error if an index is lower than the one
    /// before it plus one, or is above [`u16::MAX`] (Core's indexes are `uint16_t`, so it would
    /// reject the bytes).
    fn consensus_encode<S: io::Write + ?Sized>(&self, mut s: &mut S) -> Result<usize, io::Error> {
        let mut len = self.block_hash.consensus_encode(&mut s)?;
        // Manually encode indexes because they are differentially encoded VarInts.
        len += VarInt(self.indexes.len() as u64).consensus_encode(&mut s)?;
        let mut shift: u64 = 0;
        for idx in &self.indexes {
            if *idx < shift || *idx > u64::from(u16::MAX) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "differential value overflow",
                ));
            }
            len += VarInt(*idx - shift).consensus_encode(&mut s)?;
            shift = *idx + 1;
        }
        Ok(len)
    }
}

impl Decodable for BlockTransactionsRequest {
    /// Reads the request as Core's `BlockTransactionsRequest` does: the count and every
    /// differential are `ReadCompactSize` (32 MiB range), and an absolute index above `uint16_t`
    /// max is "differential value overflow" (`DifferenceFormatter::Unser`, `blockencodings.h`).
    fn consensus_decode<D: io::Read + ?Sized>(
        mut d: &mut D,
    ) -> Result<BlockTransactionsRequest, encode::Error> {
        let block_hash = BlockHash::consensus_decode(&mut d)?;
        let count = encode::read_compact_size_len(&mut d)?;
        // Each index is above the one before and none exceeds `u16::MAX`, so no more than 65,536
        // can be read before the overflow: that bounds the allocation, not the count.
        let max_indexes = u64::from(u16::MAX) + 1;
        let mut indexes = Vec::with_capacity(count.min(max_indexes) as usize);
        let mut shift: u64 = 0;
        for _ in 0..count {
            let differential = encode::read_compact_size_len(&mut d)?;
            // At most 65,536 + 32 MiB: no overflow.
            shift += differential;
            if shift > u64::from(u16::MAX) {
                return Err(encode::Error::ParseFailed("differential value overflow"));
            }
            indexes.push(shift);
            shift += 1;
        }
        Ok(BlockTransactionsRequest {
            block_hash,
            indexes,
        })
    }
}

/// A transaction index is requested that is out of range from the
/// corresponding block.
#[derive(Clone, PartialEq, Eq, Debug, Copy, PartialOrd, Ord, Hash)]
pub struct TxIndexOutOfRangeError(u64);

impl fmt::Display for TxIndexOutOfRangeError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "a transaction index is requested that is \
            out of range from the corresponding block: {}",
            self.0,
        )
    }
}

impl error::Error for TxIndexOutOfRangeError {}

/// A [BlockTransactions] structure is used to provide some of the transactions
/// in a block, as requested.
#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord, Hash)]
pub struct BlockTransactions {
    ///  The blockhash of the block which the transactions being provided are in.
    pub block_hash: BlockHash,
    ///  The transactions provided.
    pub transactions: Vec<Transaction>,
}
impl_consensus_encoding!(BlockTransactions, block_hash, transactions);

impl BlockTransactions {
    /// Construct a [BlockTransactions] from a [BlockTransactionsRequest] and
    /// the corresponding full [Block] by providing all requested transactions.
    pub fn from_request(
        request: &BlockTransactionsRequest,
        block: &Block,
    ) -> Result<BlockTransactions, TxIndexOutOfRangeError> {
        Ok(BlockTransactions {
            block_hash: request.block_hash,
            transactions: {
                let mut txs = Vec::with_capacity(request.indexes.len());
                for idx in &request.indexes {
                    if *idx >= block.txdata.len() as u64 {
                        return Err(TxIndexOutOfRangeError(*idx));
                    }
                    txs.push(block.txdata[*idx as usize].clone());
                }
                txs
            },
        })
    }
}

#[cfg(test)]
mod test {

    use super::*;
    use crate::blockdata::locktime::absolute;
    use crate::blockdata::script::ScriptBuf;
    use crate::blockdata::transaction::Transaction;
    use crate::blockdata::transaction::outpoint::OutPoint;
    use crate::blockdata::transaction::txin::TxIn;
    use crate::blockdata::transaction::txout::TxOut;
    use crate::blockdata::witness::Witness;
    use crate::consensus::encode::{deserialize, serialize};
    use crate::hash_types::{TxMerkleNode, Txid};
    use crate::pow::CompactTarget;

    fn dummy_tx(nonce: &[u8]) -> Transaction {
        Transaction {
            version: 1,
            lock_time: absolute::LockTime::from_consensus(2).to_consensus_u32(),
            input: vec![TxIn {
                previous_output: OutPoint::new(Txid::hash(nonce), 0),
                script_sig: ScriptBuf::new(),
                sequence: 1,
                witness: Witness::new(),
            }],
            output: vec![TxOut {
                value: 1,
                script_pubkey: ScriptBuf::new(),
            }],
            special_transaction_payload: None,
        }
    }

    fn dummy_block() -> Block {
        Block {
            header: block::Header {
                version: block::Version::ONE,
                prev_blockhash: BlockHash::from_byte_array([0; 32]),
                merkle_root: TxMerkleNode::hash(&[1]),
                time: 2,
                bits: CompactTarget::from_consensus(3),
                nonce: 4,
            },
            txdata: vec![dummy_tx(&[2]), dummy_tx(&[3]), dummy_tx(&[4])],
        }
    }

    /// Width, fill and zero padding behave like an integer, and precision does
    /// not cut digits.
    #[test]
    fn short_id_hex_pads_like_an_integer() {
        let id: ShortId = "abcd00000001".parse().unwrap();
        assert_eq!(format!("{}", id), "abcd00000001");
        assert_eq!(format!("{:?}", id), "abcd00000001");
        assert_eq!(format!("{:#X}", id), "0xABCD00000001");
        assert_eq!(format!("{:16}", id), "    abcd00000001");
        assert_eq!(format!("{:016x}", id), "0000abcd00000001");
        assert_eq!(format!("{:.4x}", id), "abcd00000001");
    }

    #[test]
    fn test_header_and_short_ids_from_block() {
        let block = dummy_block();

        let compact = HeaderAndShortIds::from_block(&block, 42, 1, &[]).unwrap();
        assert_eq!(compact.nonce, 42);
        assert_eq!(compact.short_ids.len(), 2);
        assert_eq!(compact.prefilled_txs.len(), 1);
        assert_eq!(compact.prefilled_txs[0].idx, 0);
        assert_eq!(&compact.prefilled_txs[0].tx, &block.txdata[0]);

        let compact = HeaderAndShortIds::from_block(&block, 42, 1, &[0, 1, 2]).unwrap();
        let idxs = compact.prefilled_txs.iter().map(|t| t.idx).collect::<Vec<_>>();
        assert_eq!(idxs, vec![0, 0, 0]);

        let compact = HeaderAndShortIds::from_block(&block, 42, 1, &[2]).unwrap();
        let idxs = compact.prefilled_txs.iter().map(|t| t.idx).collect::<Vec<_>>();
        assert_eq!(idxs, vec![0, 1]);
    }

    #[test]
    fn short_ids_use_the_instance_hash() {
        use crate::blockdata::transaction::special_transaction::TransactionPayload;

        // An asset unlock with a version 2 payload: its txid leaves out the quorum signature.
        let mut payload = vec![7u8; 145];
        payload[0] = 2;
        let mut unlock = dummy_tx(&[5]);
        unlock.version = 3;
        unlock.special_transaction_payload = Some(TransactionPayload::RawPayloadType(9, payload));
        assert_ne!(unlock.txid(), unlock.instance_hash());
        let mut block = dummy_block();
        block.txdata.push(unlock.clone());

        let compact = HeaderAndShortIds::from_block(&block, 42, 1, &[]).unwrap();
        let keys = ShortId::calculate_siphash_keys(&block.header, 42);
        let expected = ShortId::with_siphash_keys(&unlock.instance_hash().to_raw_hash(), keys);
        assert_eq!(compact.short_ids.last(), Some(&expected));
        assert_ne!(ShortId::with_siphash_keys(&unlock.txid().to_raw_hash(), keys), expected);
    }

    /// BIP152 version 2 is the witness (BIP144) variant, which Dash does not have: Core speaks
    /// compact blocks version 1 only.
    #[test]
    fn only_compact_block_version_1_exists() {
        let block = dummy_block();
        for version in [0, 2, 3] {
            assert!(matches!(
                HeaderAndShortIds::from_block(&block, 42, version, &[]),
                Err(Error::UnknownVersion)
            ));
        }
        assert!(HeaderAndShortIds::from_block(&block, 42, 1, &[]).is_ok());
    }

    #[test]
    fn test_getblocktx_differential_encoding_de_and_serialization() {
        let testcases = vec![
            // differentially encoded VarInts, indices
            (vec![4, 0, 5, 1, 10], vec![0, 6, 8, 19]),
            (vec![1, 0], vec![0]),
            (vec![5, 0, 0, 0, 0, 0], vec![0, 1, 2, 3, 4]),
            (vec![3, 1, 1, 1], vec![1, 3, 5]),
            (vec![3, 0, 0, 253, 0, 1], vec![0, 1, 258]), // .., 253, 0, 1] == VarInt(256)
        ];
        let deser_errorcases = vec![
            vec![2, 255, 254, 255, 255, 255, 255, 255, 255, 255, 0], // .., 255, 254, .., 255] == VarInt(u64::MAX-1)
            vec![1, 255, 255, 255, 255, 255, 255, 255, 255, 255], // .., 255, 255, .., 255] == VarInt(u64::MAX)
        ];
        for testcase in testcases {
            {
                // test deserialization
                let mut raw: Vec<u8> = [0u8; 32].to_vec();
                raw.extend(testcase.0.clone());
                let btr: BlockTransactionsRequest = deserialize(&raw.to_vec()).unwrap();
                assert_eq!(testcase.1, btr.indexes);
            }
            {
                // test serialization
                let raw: Vec<u8> = serialize(&BlockTransactionsRequest {
                    block_hash: Hash::all_zeros(),
                    indexes: testcase.1,
                });
                let mut expected_raw: Vec<u8> = [0u8; 32].to_vec();
                expected_raw.extend(testcase.0);
                assert_eq!(expected_raw, raw);
            }
        }
        for errorcase in deser_errorcases {
            {
                // test that we return Err() if deserialization fails (and don't panic)
                let mut raw: Vec<u8> = [0u8; 32].to_vec();
                raw.extend(errorcase);
                assert!(deserialize::<BlockTransactionsRequest>(&raw.to_vec()).is_err());
            }
        }
    }

    /// Indexes that cannot be written differentially are an error, as Core's "differential value
    /// overflow" in `DifferenceFormatter::Ser`, and not a panic or a wrapped value.
    #[test]
    fn getblocktxn_encoding_refuses_indexes_that_are_not_increasing() {
        for indexes in [vec![u64::MAX], vec![1, 0], vec![5, 5], vec![0, 3, 2]] {
            let mut out = Vec::new();
            let err = BlockTransactionsRequest {
                block_hash: Hash::all_zeros(),
                indexes: indexes.clone(),
            }
            .consensus_encode(&mut out)
            .expect_err("indexes must be refused");
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{:?}", indexes);
        }
        // The neighbours are fine.
        for indexes in [vec![0, 1], vec![1, 3, 65_535]] {
            let mut out = Vec::new();
            BlockTransactionsRequest {
                block_hash: Hash::all_zeros(),
                indexes,
            }
            .consensus_encode(&mut out)
            .unwrap();
        }
    }

    /// Core holds the indexes as `uint16_t`, so it rejects bytes with an absolute index above
    /// 65,535: the encoder must not emit them. The highest index round-trips.
    #[test]
    fn getblocktxn_encoding_refuses_indexes_core_cannot_represent() {
        let request = |indexes: Vec<u64>| BlockTransactionsRequest {
            block_hash: Hash::all_zeros(),
            indexes,
        };

        let top = request(vec![65_535]);
        let raw = serialize(&top);
        assert_eq!(deserialize::<BlockTransactionsRequest>(&raw).unwrap(), top);
        assert_eq!(BlockTransactionsRequest::consensus_decode(&mut &raw[..]).unwrap(), top);

        for indexes in [vec![65_536], vec![0, 65_536], vec![u64::MAX - 1], vec![u64::MAX]] {
            let err = request(indexes.clone())
                .consensus_encode(&mut Vec::new())
                .expect_err("Core cannot represent the index");
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{:?}", indexes);
        }
    }

    /// A `getblocktxn` payload with the given count and raw (already encoded) differentials.
    fn getblocktxn(count: &[u8], differentials: &[u8]) -> Vec<u8> {
        let mut raw = vec![0u8; 32];
        raw.extend_from_slice(count);
        raw.extend_from_slice(differentials);
        raw
    }

    fn decode_getblocktxn(raw: &[u8]) -> Result<BlockTransactionsRequest, encode::Error> {
        // The streaming entry point, as the message reader uses it, and the slice one agree.
        let streamed = BlockTransactionsRequest::consensus_decode(&mut &raw[..]);
        let sliced = deserialize::<BlockTransactionsRequest>(raw);
        assert_eq!(streamed.is_ok(), sliced.is_ok());
        streamed
    }

    /// Core's `BlockTransactionsRequest` reads the count as `ReadCompactSize` with no other limit,
    /// so a request with 50,000 indexes (more than the heuristic allowed) is accepted.
    #[test]
    fn getblocktxn_accepts_the_request_core_accepts_with_50000_indexes() {
        let request = decode_getblocktxn(&getblocktxn(&[0xfd, 0x50, 0xc3], &vec![0u8; 50_000]))
            .expect("Core accepts 50,000 indexes");
        assert_eq!(request.indexes.len(), 50_000);
        assert_eq!(request.indexes[49_999], 49_999);
    }

    /// The absolute index is a `uint16_t` in Core, so 65,535 is the highest one, on both sides.
    #[test]
    fn getblocktxn_absolute_index_is_bounded_by_u16() {
        // Differential 65,535 as the first index: absolute index 65,535.
        let request = decode_getblocktxn(&getblocktxn(&[1], &[0xfd, 0xff, 0xff])).unwrap();
        assert_eq!(request.indexes, vec![65_535]);
        // Differential 65,536: absolute index 65,536.
        let err = decode_getblocktxn(&getblocktxn(&[1], &[0xfe, 0x00, 0x00, 0x01, 0x00]))
            .expect_err("65,536 overflows uint16_t");
        assert!(
            matches!(err, encode::Error::ParseFailed("differential value overflow")),
            "{:?}",
            err
        );
        // 65,535 followed by a zero differential is absolute 65,536 as well.
        let err = decode_getblocktxn(&getblocktxn(&[2], &[0xfd, 0xff, 0xff, 0x00]))
            .expect_err("the second index is 65,536");
        assert!(matches!(err, encode::Error::ParseFailed("differential value overflow")));
        // 65,536 consecutive indexes end at 65,535; one more does not fit.
        let ok =
            decode_getblocktxn(&getblocktxn(&[0xfe, 0x00, 0x00, 0x01, 0x00], &vec![0; 65_536]))
                .unwrap();
        assert_eq!(ok.indexes.last(), Some(&65_535));
        let err =
            decode_getblocktxn(&getblocktxn(&[0xfe, 0x01, 0x00, 0x01, 0x00], &vec![0; 65_537]))
                .expect_err("the 65,537th index is 65,536");
        assert!(matches!(err, encode::Error::ParseFailed("differential value overflow")));
    }

    /// The count and every differential are `ReadCompactSize`: a value above `MAX_SIZE` is refused
    /// at the value, and one at `MAX_SIZE` is read (to an overflow or end of data).
    #[test]
    fn getblocktxn_counts_and_differentials_follow_read_compact_size() {
        // Count 0x02000001 is refused before any index is read.
        let err = decode_getblocktxn(&getblocktxn(&[0xfe, 0x01, 0x00, 0x00, 0x02], &[]))
            .expect_err("count above MAX_SIZE");
        assert!(matches!(err, encode::Error::OversizedVectorAllocation { .. }), "{:?}", err);
        // Count 0x02000000 is in range: it runs out of data, and allocates only a bounded vector.
        let err = decode_getblocktxn(&getblocktxn(&[0xfe, 0x00, 0x00, 0x00, 0x02], &[]))
            .expect_err("no indexes follow");
        assert!(matches!(err, encode::Error::Io(_)), "{:?}", err);
        // A huge count fails at the first index above 65,535, whichever comes first.
        let err = decode_getblocktxn(&getblocktxn(
            &[0xfe, 0x00, 0x00, 0x00, 0x02],
            &[0xfd, 0xff, 0xff, 0xfd, 0xff, 0xff],
        ))
        .expect_err("the second index overflows");
        assert!(matches!(err, encode::Error::ParseFailed("differential value overflow")));
        // A differential of MAX_SIZE is in range for the read but overflows the index...
        let err = decode_getblocktxn(&getblocktxn(&[1], &[0xfe, 0x00, 0x00, 0x00, 0x02]))
            .expect_err("differential 0x02000000");
        assert!(matches!(err, encode::Error::ParseFailed("differential value overflow")));
        // ... and one above it is not a valid CompactSize at all.
        let err = decode_getblocktxn(&getblocktxn(&[1], &[0xfe, 0x01, 0x00, 0x00, 0x02]))
            .expect_err("differential 0x02000001");
        assert!(matches!(err, encode::Error::OversizedVectorAllocation { .. }), "{:?}", err);
    }

    /// `PrefilledTransaction` indexes are `COMPACTSIZE(uint16_t)` in Core.
    #[test]
    fn prefilled_transaction_index_is_a_compact_size_bounded_by_u16() {
        let tx = serialize(&dummy_tx(&[7]));
        let with_index = |index: &[u8]| {
            let mut raw = index.to_vec();
            raw.extend_from_slice(&tx);
            raw
        };
        let ok = deserialize::<PrefilledTransaction>(&with_index(&[0xfd, 0xff, 0xff])).unwrap();
        assert_eq!(ok.idx, 65_535);
        let err = deserialize::<PrefilledTransaction>(&with_index(&[0xfe, 0x00, 0x00, 0x01, 0x00]))
            .expect_err("65,536 overflows uint16_t");
        assert!(matches!(
            err,
            encode::Error::ParseFailed("BIP152 prefilled tx index out of bounds")
        ));
        let err = deserialize::<PrefilledTransaction>(&with_index(&[0xfe, 0x01, 0x00, 0x00, 0x02]))
            .expect_err("index above MAX_SIZE");
        assert!(matches!(err, encode::Error::OversizedVectorAllocation { .. }), "{:?}", err);
    }

    /// Core refuses a compact block whose short ids and prefilled transactions together are more
    /// than `uint16_t` can count ("indexes overflowed 16 bits").
    #[test]
    fn compact_block_transaction_count_is_bounded_by_u16() {
        let block = dummy_block();
        let mut compact = HeaderAndShortIds::from_block(&block, 42, 1, &[]).unwrap();
        // 1 prefilled transaction plus 65,534 short ids is 65,535: the largest accepted.
        compact.short_ids = vec![ShortId([0; 6]); 65_534];
        let raw = serialize(&compact);
        assert_eq!(deserialize::<HeaderAndShortIds>(&raw).unwrap(), compact);
        assert_eq!(HeaderAndShortIds::consensus_decode(&mut &raw[..]).unwrap(), compact);
        // One more is 65,536.
        compact.short_ids.push(ShortId([0; 6]));
        let raw = serialize(&compact);
        for err in [
            deserialize::<HeaderAndShortIds>(&raw).expect_err("65,536 transactions"),
            HeaderAndShortIds::consensus_decode(&mut &raw[..]).expect_err("65,536 transactions"),
        ] {
            assert!(matches!(err, encode::Error::ParseFailed("indexes overflowed 16 bits")));
        }
    }
}
