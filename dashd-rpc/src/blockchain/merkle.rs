//! `getmerkleblocks`: the `merkleblock`s a bloom filter matches along the active chain.

use dash_consensus::primitives::{ReadErrorKind, read_compact_size};

use super::{BloomFilterData, NodeChain, block_read_error, ensure_chainman, parse_hash_v};
use crate::UniValue;
use crate::protocol::{RPC_INVALID_ADDRESS_OR_KEY, RPC_INVALID_PARAMETER, RpcError};
use crate::request::{get_int_i32, param};

/// `MAX_BLOOM_FILTER_SIZE`, in bytes.
// Core: common/bloom.h:20
const MAX_BLOOM_FILTER_SIZE: u64 = 36_000;

/// `MAX_HASH_FUNCS`.
// Core: common/bloom.h:21
const MAX_HASH_FUNCS: u32 = 50;

/// `MAX_HEADERS_UNCOMPRESSED_RESULT`: `count`'s default and upper bound.
// Core: validation.h:82
const MAX_HEADERS_UNCOMPRESSED_RESULT: i32 = 2000;

/// The wire size of the fields after `vData`: `nHashFuncs` and `nTweak` (`uint32_t`) and
/// `nFlags` (`uint8_t`).
const FILTER_TRAILER_SIZE: u64 = 4 + 4 + 1;

/// `ParseHex`: the bytes of pairs of hex digits, with whitespace allowed before each pair; the
/// empty vector for anything else (an odd digit, or another character).
// Core: util/strencodings.cpp:80-97 TryParseHex, util/strencodings.h:63-68
fn parse_hex_lenient(s: &str) -> Vec<u8> {
    let digit = |c: u8| char::from(c).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(s.len() / 2);
    let mut bytes = s.bytes();
    while let Some(c) = bytes.next() {
        // `IsSpace`: the C locale's whitespace.
        if matches!(c, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r') {
            continue;
        }
        match (digit(c), bytes.next().and_then(digit)) {
            (Some(hi), Some(lo)) => out.push(hi << 4 | lo),
            _ => return Vec::new(),
        }
    }
    out
}

/// A `std::ios_base::failure` thrown while reading the filter, as `ExecuteCommand` answers it:
/// libstdc++'s `what()` appends the `iostream` error category's message.
fn stream_error(kind: ReadErrorKind) -> RpcError {
    let what = match kind {
        ReadErrorKind::EndOfData => "DataStream::read(): end of data".to_owned(),
        kind => kind.to_string(),
    };
    RpcError::Runtime(format!("{what}: iostream error"))
}

/// "Filter is not within size constraints".
fn not_within_size_constraints() -> RpcError {
    RpcError::new(RPC_INVALID_PARAMETER, "Filter is not within size constraints")
}

/// `ssBloomFilter >> filter` after Core's precheck of the `vData` count, then
/// `IsWithinSizeConstraints`. Bytes after the filter are ignored.
///
/// The precheck reads the count without consuming it. A canonical count above
/// `MAX_BLOOM_FILTER_SIZE` answers as the unbounded reader did before `LIMITED_VECTOR`: the
/// size-constraints error when the whole `vData` and the fixed fields follow, else end of data.
/// A count it cannot read leaves the real read to fail the same way.
// Core: rpc/blockchain.cpp:850-886; common/bloom.h:78, :86; serialize.h:1018-1031
fn read_filter(bytes: &[u8]) -> Result<BloomFilterData, RpcError> {
    let mut prefix = bytes;
    if let Ok(size) = read_compact_size(&mut prefix, true)
        && size > MAX_BLOOM_FILTER_SIZE
    {
        let remaining = prefix.len() as u64;
        if remaining >= size && remaining - size >= FILTER_TRAILER_SIZE {
            return Err(not_within_size_constraints());
        }
        return Err(stream_error(ReadErrorKind::EndOfData));
    }

    let mut input = bytes;
    let size = read_compact_size(&mut input, true).map_err(stream_error)?;
    // `LimitedVectorFormatter`: unreachable after the precheck, which takes every canonical
    // count above the limit.
    if size > MAX_BLOOM_FILTER_SIZE {
        return Err(RpcError::Runtime("Vector length limit exceeded: iostream error".into()));
    }
    let mut take = |n: usize| -> Result<&[u8], RpcError> {
        if input.len() < n {
            return Err(stream_error(ReadErrorKind::EndOfData));
        }
        let (head, rest) = input.split_at(n);
        input = rest;
        Ok(head)
    };
    let data = take(size as usize)?.to_vec();
    let hash_funcs = u32::from_le_bytes(take(4)?.try_into().expect("four bytes"));
    let tweak = u32::from_le_bytes(take(4)?.try_into().expect("four bytes"));
    let flags = take(1)?[0];
    let filter = BloomFilterData {
        data,
        hash_funcs,
        tweak,
        flags,
    };
    // Core: common/bloom.cpp:93-96 CBloomFilter::IsWithinSizeConstraints
    if filter.data.len() as u64 > MAX_BLOOM_FILTER_SIZE || filter.hash_funcs > MAX_HASH_FUNCS {
        return Err(not_within_size_constraints());
    }
    Ok(filter)
}

/// `getmerkleblocks "filter" "blockhash" ( count )`: the filter is read and checked, then the
/// hash parsed and looked up, then `count` checked; the node walks the chain from there
/// ([`Chainman::merkle_blocks`](super::Chainman::merkle_blocks)).
// Core: rpc/blockchain.cpp:827-924
pub(super) fn getmerkleblocks(
    chain: &dyn NodeChain,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let chainman = ensure_chainman(chain)?;
    let filter = read_filter(&parse_hex_lenient(param(params, 0).get_str()?))?;
    let hash = parse_hash_v(param(params, 1), "blockhash")?;
    if chainman.lookup_block_index(&hash).is_none() {
        return Err(RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Block not found"));
    }
    let count = match param(params, 2) {
        v if v.is_null() => MAX_HEADERS_UNCOMPRESSED_RESULT,
        v => get_int_i32(v)?,
    };
    if count <= 0 || count > MAX_HEADERS_UNCOMPRESSED_RESULT {
        return Err(RpcError::new(RPC_INVALID_PARAMETER, "Count is out of range"));
    }
    let count = u32::try_from(count).expect("a count in range");
    let blocks = chainman.merkle_blocks(filter, &hash, count).map_err(block_read_error)?;
    let mut out = UniValue::new_array();
    for block in blocks {
        out.push_back(crate::descriptor::hex(&block).into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
