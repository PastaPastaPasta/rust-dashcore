//! `gettxoutproof` and `verifytxoutproof` (RP1b-1c), without a transaction index: the
//! `CMerkleBlock` proving transactions are in a block, and its check against the active chain.
//! The node finds the block, reads it and builds the proof ([`Chainman::txout_proof`]); the
//! proof's decoding and `ExtractMatches` are this module's.
//!
//! [`Chainman::txout_proof`]: super::Chainman::txout_proof

use std::collections::BTreeMap;
use std::sync::Arc;

use dash_consensus::hash::sha256d;
use dash_consensus::primitives::{ReadErrorKind, TxHash, decode_header, read_compact_size};
use dash_primitives::BlockHash;
use dash_types::Numeric as _;

use super::merkle::{parse_hex_lenient, stream_error};
use super::{NodeChain, ensure_chainman, get_hex, parse_hash_v};
use crate::UniValue;
use crate::descriptor::hex;
use crate::protocol::{
    RPC_DATABASE_ERROR, RPC_INTERNAL_ERROR, RPC_INVALID_ADDRESS_OR_KEY, RPC_INVALID_PARAMETER,
    RpcError,
};
use crate::request::param;
use crate::server::{Command, RpcServer};

/// Why `gettxoutproof` has no proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxOutProofError {
    /// The given block is not in the index: "Block not found".
    BlockNotFound,
    /// No block was given and no txid has an unspent output: "Transaction not yet in block".
    NotYetInBlock,
    /// `ReadBlockFromDisk` failed: "Can't read block from disk".
    Unreadable,
    /// The block lacks a txid: "Not all transactions found in specified or retrieved block".
    NotAllFound,
    /// A coin read failed or V has stopped (`AbortNode`): [`ABORTED`](super::ABORTED), as
    /// every command the stopped chainstate answers (RP1a D9).
    Aborted,
}

/// Registers `gettxoutproof` and `verifytxoutproof` on `server`.
pub fn register_txoutproof_commands(server: &RpcServer, chain: Arc<dyn NodeChain>) {
    let c = Arc::clone(&chain);
    server.append_command(Command::new("gettxoutproof", move |r| gettxoutproof(&*c, &r.params)));
    server.append_command(Command::new("verifytxoutproof", move |r| {
        verifytxoutproof(&*chain, &r.params)
    }));
}

/// Core's `gettxoutproof`: the txids as a `std::set<uint256>` (ordered by their bytes), then the
/// block, given or found from the first txid in that order with an unspent output (Core's
/// `AccessByTxid` over the coins tip). With no transaction index, `GetTransaction` finds nothing
/// else.
// Core: rpc/txoutproof.cpp:56-123
fn gettxoutproof(chain: &dyn NodeChain, params: &UniValue) -> Result<UniValue, RpcError> {
    // The argument check has refused any other type.
    let UniValue::Arr(txids) = param(params, 0) else {
        return Err(RpcError::Runtime(format!(
            "JSON value of type {} is not of expected type array",
            param(params, 0).value_type().name()
        )));
    };
    if txids.is_empty() {
        return Err(RpcError::new(RPC_INVALID_PARAMETER, "Parameter 'txids' cannot be empty"));
    }
    let mut set = BTreeMap::new();
    for v in txids {
        let txid: TxHash = parse_hash_v(v, "txid")?;
        if set.insert(*txid.as_bytes(), txid).is_some() {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                format!("Invalid parameter, duplicated txid: {}", v.get_str()?),
            ));
        }
    }
    let set: Vec<TxHash> = set.into_values().collect();
    let chainman = ensure_chainman(chain)?;
    let block = match param(params, 1) {
        v if v.is_null() => None,
        v => Some(parse_hash_v::<BlockHash>(v, "blockhash")?),
    };
    let proof = chainman.txout_proof(&set, block).map_err(|e| match e {
        TxOutProofError::BlockNotFound => {
            RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Block not found")
        }
        TxOutProofError::NotYetInBlock => {
            RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Transaction not yet in block")
        }
        TxOutProofError::Unreadable => {
            RpcError::new(RPC_INTERNAL_ERROR, "Can't read block from disk")
        }
        TxOutProofError::NotAllFound => RpcError::new(
            RPC_INVALID_ADDRESS_OR_KEY,
            "Not all transactions found in specified or retrieved block",
        ),
        TxOutProofError::Aborted => RpcError::new(RPC_DATABASE_ERROR, super::ABORTED),
    })?;
    Ok(hex(&proof).into())
}

/// `MaxBlockSize() / 60`: `ExtractMatches`' bound on `nTransactions`, 60 bytes being the
/// smallest transaction.
// Core: merkleblock.cpp:172; consensus/consensus.h:11-15
const MAX_PROOF_TRANSACTIONS: u32 = 2_000_000 / 60;

/// A `CMerkleBlock` as its serialization reads it: the header's bytes, `nTransactions`, `vHash`
/// and the flag bytes. The bits stay packed, eight to a byte, as Core's `std::vector<bool>`
/// `vBits` holds them, so a proof costs what it costs Core.
struct ProofRead {
    header: [u8; 80],
    transactions: u32,
    hashes: Vec<[u8; 32]>,
    flags: Vec<u8>,
}

impl ProofRead {
    /// `vBits.size()`: eight bits a flag byte.
    fn bit_count(&self) -> usize {
        self.flags.len() * 8
    }

    /// `vBits[p]`, bit `p % 8` of byte `p / 8`; `None` past the end.
    // Core: merkleblock.cpp:21-28 BytesToBits
    fn bit(&self, p: usize) -> Option<bool> {
        self.flags.get(p / 8).map(|b| b >> (p % 8) & 1 == 1)
    }
}

/// `ssMB >> merkleBlock`: Core's reads, with their `std::ios_base::failure`s. Bytes after the
/// proof are ignored.
// Core: merkleblock.h:61-71, :144-148; merkleblock.cpp:21-28 BytesToBits
fn read_proof(mut input: &[u8]) -> Result<ProofRead, RpcError> {
    let eod = || stream_error(ReadErrorKind::EndOfData);
    let mut take = |n: usize| -> Result<&[u8], RpcError> {
        let (head, rest) = input.split_at_checked(n).ok_or_else(eod)?;
        input = rest;
        Ok(head)
    };
    let header: [u8; 80] = take(80)?.try_into().expect("80 bytes");
    let transactions = u32::from_le_bytes(take(4)?.try_into().expect("4 bytes"));
    let count = read_compact_size(&mut input, true).map_err(stream_error)?;
    let mut hashes = Vec::new();
    for _ in 0..count {
        let (h, rest) = input.split_first_chunk::<32>().ok_or_else(eod)?;
        hashes.push(*h);
        input = rest;
    }
    let size = read_compact_size(&mut input, true).map_err(stream_error)?;
    let flags = usize::try_from(size).ok().and_then(|n| input.get(..n)).ok_or_else(eod)?;
    Ok(ProofRead {
        header,
        transactions,
        hashes,
        flags: flags.to_vec(),
    })
}

/// `CalcTreeWidth(height)`.
fn width(n: u32, height: u32) -> u32 {
    // `nTransactions` is at most `MAX_PROOF_TRANSACTIONS` here, so the sum cannot overflow.
    (n + (1 << height) - 1) >> height
}

/// `TraverseAndExtract`'s state: the bits and hashes used, the matches, and `fBad`.
struct Extract<'a> {
    proof: &'a ProofRead,
    bits_used: usize,
    hashes_used: usize,
    matches: Vec<[u8; 32]>,
    bad: bool,
}

impl Extract<'_> {
    // Core: merkleblock.cpp:112-148 CPartialMerkleTree::TraverseAndExtract
    fn traverse(&mut self, height: u32, pos: u32) -> [u8; 32] {
        let Some(parent_of_match) = self.proof.bit(self.bits_used) else {
            self.bad = true;
            return [0; 32];
        };
        self.bits_used += 1;
        if height == 0 || !parent_of_match {
            let Some(&hash) = self.proof.hashes.get(self.hashes_used) else {
                self.bad = true;
                return [0; 32];
            };
            self.hashes_used += 1;
            if height == 0 && parent_of_match {
                self.matches.push(hash);
            }
            return hash;
        }
        let left = self.traverse(height - 1, pos * 2);
        let right = if pos * 2 + 1 < width(self.proof.transactions, height - 1) {
            let right = self.traverse(height - 1, pos * 2 + 1);
            if right == left {
                self.bad = true;
            }
            right
        } else {
            left
        };
        let mut both = [0u8; 64];
        both[..32].copy_from_slice(&left);
        both[32..].copy_from_slice(&right);
        sha256d(&both)
    }
}

/// `ExtractMatches`: the root the tree proves and its matched hashes, the null root for a tree
/// that fails a check.
// Core: merkleblock.cpp:166-197 CPartialMerkleTree::ExtractMatches
fn extract_matches(proof: &ProofRead) -> ([u8; 32], Vec<[u8; 32]>) {
    let n = proof.transactions;
    if n == 0
        || n > MAX_PROOF_TRANSACTIONS
        || proof.hashes.len() > n as usize
        || proof.bit_count() < proof.hashes.len()
    {
        return ([0; 32], Vec::new());
    }
    let mut height = 0;
    while width(n, height) > 1 {
        height += 1;
    }
    let mut x = Extract {
        proof,
        bits_used: 0,
        hashes_used: 0,
        matches: Vec::new(),
        bad: false,
    };
    let root = x.traverse(height, 0);
    if x.bad
        || x.bits_used.div_ceil(8) != proof.bit_count().div_ceil(8)
        || x.hashes_used != proof.hashes.len()
    {
        return ([0; 32], x.matches);
    }
    (root, x.matches)
}

/// Core's `verifytxoutproof`: the matched txids of a proof whose tree proves its header's
/// merkle root, for a block on the active chain with that many transactions; `[]` for a proof
/// that proves another root or count.
// Core: rpc/txoutproof.cpp:146-174
fn verifytxoutproof(chain: &dyn NodeChain, params: &UniValue) -> Result<UniValue, RpcError> {
    let bytes = parse_hex_v(param(params, 0), "proof")?;
    let proof = read_proof(&bytes)?;
    let mut res = UniValue::new_array();
    let (root, matches) = extract_matches(&proof);
    let header = decode_header(&mut &proof.header[..]).expect("80 bytes are a header").into_value();
    if root != header.merkle_root().to_lendian() {
        return Ok(res);
    }
    let chainman = ensure_chainman(chain)?;
    let in_chain = chainman
        .header_facts(&header.hash())
        .filter(|f| f.confirmations >= 0 && f.n_tx != 0)
        .ok_or_else(|| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Block not found in chain"))?;
    if in_chain.n_tx == proof.transactions {
        for hash in &matches {
            res.push_back(get_hex(hash).into());
        }
    }
    Ok(res)
}

/// Core's `ParseHexV(v, name)`: a non-empty string of pairs of hex digits (anything else, a
/// non-string too, is refused with the string as given), as `ParseHex` reads it.
// Core: rpc/util.cpp:100-108; util/strencodings.cpp:69-78 IsHex
fn parse_hex_v(v: &UniValue, name: &str) -> Result<Vec<u8>, RpcError> {
    let s = if v.is_str() {
        v.get_str()?
    } else {
        ""
    };
    if s.is_empty() || s.len() % 2 != 0 || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            format!("{name} must be hexadecimal string (not '{s}')"),
        ));
    }
    Ok(parse_hex_lenient(s))
}

#[cfg(test)]
mod tests;
