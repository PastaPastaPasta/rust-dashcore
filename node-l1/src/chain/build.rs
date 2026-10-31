//! Block and transaction construction for `diff_chain` (O2b §3.2, D18): regtest blocks from
//! CS0's primitives, as CRASH's `crash/synth.rs` builds them, so that the generator needs no
//! oracle codec (the binding refuses them on a chain session's thread, O2b-2 D1).
//!
//! Outputs are anyone-can-spend `OP_TRUE` unless a mutation operator asks for one of the
//! [`Lock`] puzzles, whose failures need no key: a false result, a non-DER signature, a valid
//! DER signature that does not verify, a non-null `CHECKMULTISIG` dummy, a CLTV or CSV lock.

use std::sync::{Arc, LazyLock};

use dash_consensus::pow::{PowParams, check_proof_of_work};
use dash_consensus::primitives::{
    Block, BlockHash, BlockHeader, Header, MerkleRoot, OutPoint, Transaction, Tx, TxHash, TxIn,
    TxOut, TxType, core_serialize,
};
use dash_consensus::subsidy::{SubsidyParams, block_subsidy_inner, genesis_reward};
use dash_num::CompactTarget;
use dash_params::{ChainParams, ChainParamsBuilder, NetworkKind, Release};

/// Regtest's proof-of-work limit; every block the generator indexes has it (O2b-3 P3).
pub const REGTEST_BITS: u32 = 0x207f_ffff;
/// Regtest's genesis time and hash (internal byte order).
pub const GENESIS_TIME: u32 = 1_417_713_337;
/// Regtest's block spacing (`nPowTargetSpacing`).
pub const SPACING: u32 = 150;
/// The header version with the BIP9 top bits.
pub const VERSION_BITS_TOP: i32 = 0x2000_0000;
/// Regtest testdummy's bit (`chainparams.cpp`, `DEPLOYMENT_TESTDUMMY`).
pub const TESTDUMMY_BIT: u32 = 28;
/// Coinbase maturity.
pub const MATURITY: u32 = 100;
/// `MAX_FUTURE_BLOCK_TIME` (chain.h): a header more than this past the adjusted time is
/// `time-too-new`.
pub const MAX_FUTURE: i64 = 2 * 60 * 60;

pub const OP_0: u8 = 0x00;
pub const OP_1: u8 = 0x51;
pub const OP_RETURN: u8 = 0x6a;
pub const OP_DROP: u8 = 0x75;
pub const OP_EQUAL: u8 = 0x87;
pub const OP_CHECKSIG: u8 = 0xac;
pub const OP_CHECKMULTISIG: u8 = 0xae;
pub const OP_CLTV: u8 = 0xb1;
pub const OP_CSV: u8 = 0xb2;

/// secp256k1's generator, compressed: a valid key no signature here verifies against.
const G: [u8; 33] = [
    0x02, 0x79, 0xbe, 0x66, 0x7e, 0xf9, 0xdc, 0xbb, 0xac, 0x55, 0xa0, 0x62, 0x95, 0xce, 0x87, 0x0b,
    0x07, 0x02, 0x9b, 0xfc, 0xdb, 0x2d, 0xce, 0x28, 0xd9, 0x59, 0xf2, 0x81, 0x5b, 0x16, 0xf8, 0x17,
    0x98,
];

/// How an output is locked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lock {
    /// `OP_TRUE`: an empty scriptSig spends it.
    True,
    /// `OP_2 OP_EQUAL`: the spend pushes `OP_3`, so the script ends false.
    False,
    /// `<G> OP_CHECKSIG`: the spend's signature is not DER (`SCRIPT_ERR_SIG_DER`).
    NonDer,
    /// `<G> OP_CHECKSIG`: a valid low-S DER signature that does not verify (false result).
    BadSig,
    /// `OP_0 OP_0 OP_CHECKMULTISIG` with a non-null dummy (`SCRIPT_ERR_SIG_NULLDUMMY`).
    NullDummy,
    /// `<n> OP_CHECKLOCKTIMEVERIFY OP_DROP OP_TRUE`.
    Cltv(u32),
    /// `<n> OP_CHECKSEQUENCEVERIFY OP_DROP OP_TRUE`.
    Csv(u32),
    /// A P2SH of `OP_2 OP_EQUAL`: the spend pushes `OP_3` and the redeem script.
    P2shFalse,
}

impl Lock {
    /// The scriptPubKey.
    pub fn script_pubkey(self) -> Vec<u8> {
        match self {
            Lock::True => vec![OP_1],
            Lock::False => vec![0x52, OP_EQUAL],
            Lock::NonDer | Lock::BadSig => {
                let mut s = push(&G);
                s.push(OP_CHECKSIG);
                s
            }
            Lock::NullDummy => vec![OP_0, OP_0, OP_CHECKMULTISIG],
            Lock::Cltv(n) => [script_num(i64::from(n)), vec![OP_CLTV, OP_DROP, OP_1]].concat(),
            Lock::Csv(n) => [script_num(i64::from(n)), vec![OP_CSV, OP_DROP, OP_1]].concat(),
            Lock::P2shFalse => {
                let redeem = [0x52, OP_EQUAL];
                let mut s = vec![0xa9, 0x14];
                s.extend(hash160(&redeem));
                s.push(OP_EQUAL);
                s
            }
        }
    }

    /// The scriptSig that spends it (and fails, for every lock but `True`, `Cltv` and `Csv`).
    pub fn script_sig(self) -> Vec<u8> {
        match self {
            Lock::True | Lock::Cltv(_) | Lock::Csv(_) => Vec::new(),
            Lock::False => vec![0x53],
            // Not DER: a bare 0x30 byte and the hash type.
            Lock::NonDer => push(&[0x30, 0x01]),
            // r = 1, s = 1, SIGHASH_ALL: DER, low S, never a signature of anything.
            Lock::BadSig => push(&[0x30, 0x06, 0x02, 0x01, 0x01, 0x02, 0x01, 0x01, 0x01]),
            Lock::NullDummy => vec![OP_1],
            Lock::P2shFalse => [vec![0x53], push(&[0x52, OP_EQUAL])].concat(),
        }
    }
}

/// A minimal data push.
pub fn push(data: &[u8]) -> Vec<u8> {
    let mut s = Vec::with_capacity(data.len() + 2);
    match data.len() {
        n @ 0..=75 => s.push(n as u8),
        n @ 76..=255 => s.extend([0x4c, n as u8]),
        n => {
            s.push(0x4d);
            s.extend((n as u16).to_le_bytes());
        }
    }
    s.extend(data);
    s
}

/// `CScript() << n` (`push_int64`): `OP_0`, `OP_1NEGATE`, `OP_1`..`OP_16`, else a minimal
/// `CScriptNum` push.
pub fn script_num(n: i64) -> Vec<u8> {
    match n {
        0 => vec![OP_0],
        -1 => vec![0x4f],
        1..=16 => vec![OP_1 + (n as u8) - 1],
        _ => push(&script_num_bytes(n)),
    }
}

/// `CScriptNum::serialize`.
pub fn script_num_bytes(n: i64) -> Vec<u8> {
    let neg = n < 0;
    let mut abs = n.unsigned_abs();
    let mut out = Vec::new();
    while abs > 0 {
        out.push((abs & 0xff) as u8);
        abs >>= 8;
    }
    match out.last().copied() {
        Some(last) if last & 0x80 != 0 => out.push(if neg {
            0x80
        } else {
            0
        }),
        Some(last) if neg => {
            let i = out.len() - 1;
            out[i] = last | 0x80;
        }
        _ => {}
    }
    out
}

fn hash160(data: &[u8]) -> [u8; 20] {
    use bitcoin_hashes::hash160;
    hash160::Hash::hash(data).to_byte_array()
}

/// Regtest's parameters (V24), shared by the subsidy and the proof of work.
pub fn regtest() -> &'static ChainParams {
    static PARAMS: LazyLock<ChainParams> = LazyLock::new(|| {
        ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
            .build()
            .expect("regtest's parameters")
    });
    &PARAMS
}

fn pow() -> &'static PowParams {
    static POW: LazyLock<PowParams> = LazyLock::new(|| {
        PowParams::from_chain_params(regtest()).expect("regtest's proof-of-work parameters")
    });
    &POW
}

fn subsidy_params() -> &'static SubsidyParams {
    static P: LazyLock<SubsidyParams> = LazyLock::new(|| {
        let chain = regtest();
        let reward = genesis_reward(&chain.genesis).expect("regtest's genesis reward");
        SubsidyParams::from_chain_params(chain, reward).expect("regtest's subsidy parameters")
    });
    &P
}

/// The subsidy of the block at `height` (> 0), all blocks at regtest's limit and V20 inactive
/// (raised to 2000 in every M3 schedule).
pub fn subsidy(height: u32) -> i64 {
    let prev = i32::try_from(height - 1).expect("an M3 height");
    block_subsidy_inner(REGTEST_BITS, prev, false, subsidy_params())
        .expect("a subsidy below the budget start")
}

/// The genesis block's hash (internal order).
pub fn genesis_hash() -> [u8; 32] {
    *Header::new(regtest().genesis.header).hash().as_bytes()
}

/// One input: the coin spent, its scriptSig and sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Input {
    pub prevout: ([u8; 32], u32),
    pub script_sig: Vec<u8>,
    pub sequence: u32,
}

impl Input {
    /// An input with the final sequence.
    pub fn new(prevout: ([u8; 32], u32), script_sig: Vec<u8>) -> Input {
        Input {
            prevout,
            script_sig,
            sequence: u32::MAX,
        }
    }
}

/// A transaction before serialization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxPlan {
    pub version: i16,
    pub tx_type: u16,
    pub inputs: Vec<Input>,
    pub outputs: Vec<(i64, Vec<u8>)>,
    pub lock_time: u32,
    pub extra_payload: Vec<u8>,
}

impl TxPlan {
    pub fn simple(inputs: Vec<Input>, outputs: Vec<(i64, Vec<u8>)>) -> TxPlan {
        TxPlan {
            version: 2,
            tx_type: 0,
            inputs,
            outputs,
            lock_time: 0,
            extra_payload: Vec::new(),
        }
    }

    fn raw(&self) -> Transaction {
        Transaction {
            version: self.version,
            tx_type: TxType::new(self.tx_type),
            inputs: self
                .inputs
                .iter()
                .map(|i| TxIn {
                    prevout: OutPoint {
                        hash: TxHash::from(i.prevout.0),
                        index: i.prevout.1,
                    },
                    script_sig: i.script_sig.clone().into(),
                    sequence: i.sequence,
                })
                .collect(),
            outputs: self
                .outputs
                .iter()
                .map(|(value, script)| TxOut {
                    value: *value,
                    script_pubkey: script.clone().into(),
                })
                .collect(),
            lock_time: self.lock_time,
            extra_payload: self.extra_payload.clone(),
        }
    }

    pub fn tx(&self) -> Arc<Tx> {
        Arc::new(Tx::new(self.raw()))
    }

    pub fn txid(&self) -> [u8; 32] {
        *self.tx().txid().as_bytes()
    }
}

/// A coinbase at `height` paying `value` to `OP_TRUE`, with the BIP34 height push and a tag
/// that makes every generated block unique.
pub fn coinbase(height: u32, value: i64, tag: u64) -> TxPlan {
    let mut script_sig = script_num(i64::from(height));
    script_sig.extend(push(&tag.to_le_bytes()));
    TxPlan::simple(
        vec![Input::new(([0; 32], u32::MAX), script_sig)],
        vec![(value, Lock::True.script_pubkey())],
    )
}

/// `MAX_SCRIPT_ELEMENT_SIZE`: the largest push.
const MAX_PUSH: usize = 520;
/// A [`fat`] data output's pushes: a 9,938-byte script, under `MAX_SCRIPT_SIZE`.
const FAT_PUSHES: usize = 19;
/// What a [`fat`] transaction leaves its block's coinbase.
pub const FAT_FEE: i64 = 1_000;

/// CH3 §7.5's `tx=fat` (its eviction case, DEC-500): `input`, worth `value`, spent to an
/// `OP_TRUE` change of `value - FAT_FEE` first, then to `data` outputs of nothing, each
/// `OP_RETURN` and nineteen 520-byte pushes. They are unspendable, so they add no coins, only
/// bytes. A builder rather than an intent shape: bits 3–4 hold the four shapes, and a fifth would
/// change what the fuzz corpora mean.
pub fn fat(input: Input, value: i64, data: usize) -> TxPlan {
    let script = [vec![OP_RETURN], push(&[0; MAX_PUSH]).repeat(FAT_PUSHES)].concat();
    let mut outputs = vec![(value - FAT_FEE, Lock::True.script_pubkey())];
    outputs.extend(std::iter::repeat_n((0, script), data));
    TxPlan::simple(vec![input], outputs)
}

/// A block before serialization; [`BlockPlan::build`] grinds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockPlan {
    pub prev: [u8; 32],
    pub version: i32,
    pub time: u32,
    pub bits: u32,
    pub txs: Vec<TxPlan>,
    /// Set the merkle root to this instead of the computed one.
    pub merkle_override: Option<[u8; 32]>,
}

/// A built block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Built {
    pub hash: [u8; 32],
    pub header: [u8; 80],
    pub bytes: Vec<u8>,
    /// Each transaction's txid, in block order.
    pub txids: Vec<[u8; 32]>,
}

impl BlockPlan {
    /// Serialize, at the first nonce whose hash meets `bits`, or regtest's limit when `bits`
    /// is easier than it or not a target at all (negative or overflowing compacts meet none): a
    /// `bad-diffbits` block still passes the proof-of-work check before the contextual one.
    pub fn build(&self) -> Built {
        let txs: Vec<Arc<Tx>> = self.txs.iter().map(TxPlan::tx).collect();
        let mut header = BlockHeader {
            version: self.version,
            prev_hash: BlockHash::from(self.prev),
            merkle_root: MerkleRoot::from([0; 32]),
            time: self.time,
            bits: self.bits,
            nonce: 0,
        };
        header.merkle_root = match self.merkle_override {
            Some(root) => MerkleRoot::from(root),
            None => Block::new(Header::new(header), txs.clone()).merkle().0,
        };
        let target = CompactTarget::new(self.bits.min(REGTEST_BITS));
        let block = loop {
            let block = Block::new(Header::new(header), txs.clone());
            if check_proof_of_work(block.hash().as_bytes(), target, pow()) {
                break block;
            }
            header.nonce += 1;
        };
        let bytes = core_serialize(&block);
        let mut head = [0u8; 80];
        head.copy_from_slice(&bytes[..80]);
        Built {
            hash: *block.hash().as_bytes(),
            header: head,
            txids: txs.iter().map(|t| *t.txid().as_bytes()).collect(),
            bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_numbers_are_minimal() {
        assert_eq!(script_num(0), vec![0]);
        assert_eq!(script_num(16), vec![0x60]);
        assert_eq!(script_num(17), vec![1, 17]);
        assert_eq!(script_num(128), vec![2, 0x80, 0]);
        assert_eq!(script_num(255), vec![2, 0xff, 0]);
        assert_eq!(script_num(256), vec![2, 0, 1]);
        assert_eq!(script_num(-2), vec![1, 0x82]);
    }

    #[test]
    fn a_built_block_meets_regtest_and_reads_back() {
        let plan = BlockPlan {
            prev: genesis_hash(),
            version: VERSION_BITS_TOP,
            time: GENESIS_TIME + SPACING,
            bits: REGTEST_BITS,
            txs: vec![coinbase(1, subsidy(1), 7)],
            merkle_override: None,
        };
        let built = plan.build();
        let mut cursor = &built.bytes[..];
        let decoded = dash_consensus::decode_block(&mut cursor).expect("decodes");
        assert_eq!(decoded.value().hash().as_bytes(), &built.hash);
        assert_eq!(built.txids.len(), 1);
    }

    #[test]
    fn a_fat_transaction_is_its_change_and_its_data() {
        let tx = fat(Input::new(([1; 32], 0), Vec::new()), 50_000, 10).tx();
        let raw = tx.raw();
        assert_eq!(raw.outputs[0].value, 50_000 - FAT_FEE);
        assert_eq!(raw.outputs[0].script_pubkey.as_bytes(), Lock::True.script_pubkey());
        assert_eq!(raw.outputs.len(), 11);
        for out in &raw.outputs[1..] {
            let script = out.script_pubkey.as_bytes();
            assert_eq!((out.value, script.len(), script[0]), (0, 9_938, OP_RETURN));
        }
        assert!(core_serialize(&*tx).len() > 10 * 9_938);
    }
}
