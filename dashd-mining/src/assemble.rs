//! `CreateNewBlock`'s block below the readiness guard (G1): the header fields and the coinbase,
//! from the facts V reads in one turn ([`TipFacts`]). Nothing here reads the chain.

use std::sync::Arc;

use dash_consensus::primitives::{
    BlockHash, BlockHeader, Header, OutPoint, Transaction, Tx, TxHash, TxIn, TxOut, TxType,
};
use dash_consensus::script::ScriptNum;
use dash_consensus::script::opcodes::{OP_0, OP_1, OP_PUSHDATA1};
use dash_consensus::{Amount, Block};

/// `nBlockSize` and `nBlockSigOps` reserved for the coinbase (`resetBlock`,
/// `node/miner.cpp:103-114`): an empty mempool's block reports them unchanged.
pub(crate) const RESERVED_SIZE: u64 = 1000;
pub(crate) const RESERVED_SIGOPS: u64 = 100;

/// `CTransaction::CURRENT_VERSION` (`primitives/transaction.h:242`), the coinbase's version
/// below DIP3.
const CURRENT_VERSION: i16 = 2;

/// The block after the tip as V sees it in one turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TipFacts {
    pub prev: BlockHash,
    /// The new block's: the tip's plus one.
    pub height: u32,
    /// `ComputeBlockVersion(pindexPrev)`, or `-blockversion` off mainnet.
    pub version: i32,
    /// `block_time`'s.
    pub time: u32,
    /// `GetNextWorkRequired(pindexPrev, pblock)` at `time`.
    pub bits: u32,
    /// `GetBlockSubsidyInner(pindexPrev->nBits, pindexPrev->nHeight, fV20Active_context)`.
    pub subsidy: Amount,
}

/// The block's `nTime`: `NodeClock::now()` (`node/miner.cpp:216`), then `UpdateTime`'s raise to
/// `max(GetMedianTimePast() + 1, now)` when that is later (`:49-57`). Both stores truncate to
/// `uint32_t` as Core's do. Core reads the clock twice; one read gives the same time unless it
/// steps back between them.
pub(crate) fn block_time(now: i64, median_time_past: i64) -> u32 {
    let mut time = now as u32;
    let new_time = (median_time_past + 1).max(now);
    if i64::from(time) < new_time {
        time = new_time as u32;
    }
    time
}

/// The block `CreateNewBlock` assembles below DIP3 on an empty mempool: the header with
/// `nNonce` 0 and a null merkle root (`node/miner.cpp:343-347`), and the coinbase
/// (`:263-279`): one null-prevout input with scriptSig `CScript() << nHeight << OP_0`, one
/// output paying the subsidy (no fees) to `script`. `FillBlockPayments` adds nothing there: no
/// masternode list before DIP3, and no superblock without a governance trigger.
pub(crate) fn block(facts: &TipFacts, script: &[u8]) -> Block {
    let mut script_sig = Vec::with_capacity(7);
    push_int64(&mut script_sig, i64::from(facts.height));
    script_sig.push(OP_0);
    let coinbase = Transaction {
        version: CURRENT_VERSION,
        tx_type: TxType::new(0),
        inputs: vec![coinbase_input(script_sig)],
        outputs: vec![TxOut {
            value: facts.subsidy,
            script_pubkey: script.to_vec().into(),
        }],
        lock_time: 0,
        extra_payload: Vec::new(),
    };
    Block::new(header(facts), vec![Arc::new(Tx::new(coinbase))])
}

/// The coinbase's input: a null prevout (`vin[0].prevout.SetNull()`) and `script_sig`.
pub(crate) fn coinbase_input(script_sig: Vec<u8>) -> TxIn {
    TxIn {
        prevout: OutPoint {
            hash: TxHash::from([0; 32]),
            index: u32::MAX,
        },
        script_sig: script_sig.into(),
        sequence: u32::MAX,
    }
}

/// The header `CreateNewBlock` fills in: `nNonce` 0 and a null merkle root
/// (`node/miner.cpp:343-347`).
pub(crate) fn header(facts: &TipFacts) -> Header {
    Header::new(BlockHeader {
        version: facts.version,
        prev_hash: facts.prev,
        time: facts.time,
        bits: facts.bits,
        nonce: 0,
        ..BlockHeader::default()
    })
}

/// `CScript::push_int64` (`script/script.h:398-413`): `OP_1NEGATE` (`OP_1 - 2`) and
/// `OP_1`…`OP_16` for -1 and 1…16, `OP_0` for 0, else a push of `CScriptNum::serialize(n)`
/// ([`ScriptNum::encode`]).
fn push_int64(script: &mut Vec<u8>, n: i64) {
    if n == -1 || (1..=16).contains(&n) {
        script.push((n + i64::from(OP_1) - 1) as u8);
    } else if n == 0 {
        script.push(OP_0);
    } else {
        let num = ScriptNum::new(n).encode();
        debug_assert!(num.len() < usize::from(OP_PUSHDATA1));
        script.push(num.len() as u8);
        script.extend(num);
    }
}

#[cfg(test)]
mod tests;
