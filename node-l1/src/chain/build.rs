//! Block and transaction construction for `diff_chain` (O2b §3.2, D18): regtest blocks from
//! CS0's primitives, as CRASH's `crash/synth.rs` builds them, so that the generator needs no
//! oracle codec (the binding refuses them on a chain session's thread, O2b-2 D1).
//!
//! Outputs are anyone-can-spend `OP_TRUE` unless a mutation operator asks for one of the
//! [`Lock`] puzzles, whose failures need no key: a false result, a non-DER signature, a valid
//! DER signature that does not verify, a non-null `CHECKMULTISIG` dummy, a CLTV or CSV lock. L3's
//! limit families add the sigop fillers ([`Lock::Sigops`], [`legacy_sigops`]) and the exact-size
//! filler ([`fill_tx`], [`fill_block`]).

use std::sync::{Arc, LazyLock};

use dash_consensus::pow::{PowParams, check_proof_of_work};
use dash_consensus::primitives::{
    Block, BlockHash, BlockHeader, Header, MerkleRoot, OutPoint, Transaction, Tx, TxHash, TxIn,
    TxOut, TxType, core_serialize, decode_block, decode_tx_exact,
};
use dash_consensus::subsidy::{SubsidyParams, block_subsidy_inner, genesis_reward};
use dash_num::CompactTarget;
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dash_types::Numeric;

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
pub const OP_VERIFY: u8 = 0x69;
pub const OP_RETURN: u8 = 0x6a;
pub const OP_DROP: u8 = 0x75;
pub const OP_DUP: u8 = 0x76;
pub const OP_EQUAL: u8 = 0x87;
pub const OP_EQUALVERIFY: u8 = 0x88;
pub const OP_NOT: u8 = 0x91;
pub const OP_HASH160: u8 = 0xa9;
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
    /// A P2SH of `OP_TRUE`: the spend pushes the redeem script. Standard on both sides of a
    /// spend, so dashd's mempool takes the spend back at a rollback (a10-4 review r2-m1).
    P2shTrue,
    /// `OP_DUP OP_HASH160 <hash160(G)> OP_EQUALVERIFY OP_CHECKSIG`: the spend pushes
    /// [`BadSig`](Lock::BadSig)'s signature and `G`, so the script ends false.
    P2pkhBadSig,
    /// A P2SH of `<G> OP_CHECKSIG`: the spend pushes [`BadSig`](Lock::BadSig)'s signature and
    /// the redeem script.
    P2shBadSig,
    /// `<G> OP_CHECKSIG OP_NOT` with [`NonDer`](Lock::NonDer)'s signature: true before DERSIG
    /// (the failed check is negated), `SCRIPT_ERR_SIG_DER` from it.
    NonDerNot,
    /// The sigop filler (plan rev1 §4): a P2SH of [`sigop_redeem`]`(k)`, `k` ≤ [`SIGOPS_MAX`].
    /// The spend pushes only the redeem script and succeeds; its accurate count is `k`.
    Sigops(u8),
}

impl Lock {
    /// The scriptPubKey.
    pub fn script_pubkey(self) -> Vec<u8> {
        match self {
            Lock::True => vec![OP_1],
            Lock::False => vec![0x52, OP_EQUAL],
            Lock::NonDer | Lock::BadSig => checksig_g(),
            Lock::NullDummy => vec![OP_0, OP_0, OP_CHECKMULTISIG],
            Lock::Cltv(n) => [script_num(i64::from(n)), vec![OP_CLTV, OP_DROP, OP_1]].concat(),
            Lock::Csv(n) => [script_num(i64::from(n)), vec![OP_CSV, OP_DROP, OP_1]].concat(),
            Lock::P2shFalse => p2sh(&[0x52, OP_EQUAL]),
            Lock::P2shTrue => p2sh(&[OP_1]),
            Lock::P2pkhBadSig => {
                let mut s = vec![OP_DUP, OP_HASH160];
                s.extend(push(&hash160(&G)));
                s.extend([OP_EQUALVERIFY, OP_CHECKSIG]);
                s
            }
            Lock::P2shBadSig => p2sh(&checksig_g()),
            Lock::NonDerNot => [checksig_g(), vec![OP_NOT]].concat(),
            Lock::Sigops(k) => p2sh(&sigop_redeem(k)),
        }
    }

    /// The scriptSig that spends it (and fails, for every lock but `True`, `Cltv`, `Csv`,
    /// `P2shTrue` and `Sigops`, and `NonDerNot` before DERSIG).
    pub fn script_sig(self) -> Vec<u8> {
        match self {
            Lock::True | Lock::Cltv(_) | Lock::Csv(_) => Vec::new(),
            Lock::False => vec![0x53],
            Lock::NonDer | Lock::NonDerNot => push(&NON_DER_SIG),
            Lock::BadSig => push(&BAD_SIG),
            Lock::NullDummy => vec![OP_1],
            Lock::P2shFalse => [vec![0x53], push(&[0x52, OP_EQUAL])].concat(),
            Lock::P2shTrue => push(&[OP_1]),
            Lock::P2pkhBadSig => [push(&BAD_SIG), push(&G)].concat(),
            Lock::P2shBadSig => [push(&BAD_SIG), push(&checksig_g())].concat(),
            Lock::Sigops(k) => push(&sigop_redeem(k)),
        }
    }
}

/// Not DER: a bare 0x30 byte and the hash type.
const NON_DER_SIG: [u8; 2] = [0x30, 0x01];
/// r = 1, s = 1, SIGHASH_ALL: DER, low S, never a signature of anything.
const BAD_SIG: [u8; 9] = [0x30, 0x06, 0x02, 0x01, 0x01, 0x02, 0x01, 0x01, 0x01];

/// `<G> OP_CHECKSIG`.
fn checksig_g() -> Vec<u8> {
    [push(&G), vec![OP_CHECKSIG]].concat()
}

/// The most repetitions one [`sigop_redeem`] holds: 13 × 38 + 1 = 495 bytes and 39 counted
/// opcodes, under `MAX_SCRIPT_ELEMENT_SIZE` (520) and `MAX_OPS_PER_SCRIPT` (201).
pub const SIGOPS_MAX: u8 = 13;

/// `(OP_0 <G> OP_CHECKSIG OP_NOT OP_VERIFY) × k OP_TRUE`: each empty signature fails its check
/// without an error (an empty signature passes the DER and strict-encoding checks), `OP_NOT`
/// makes it true, so the script succeeds with `k` accurate sigops.
pub fn sigop_redeem(k: u8) -> Vec<u8> {
    assert!(k <= SIGOPS_MAX, "{k} sigops in one redeem script");
    let one = [vec![OP_0], checksig_g(), vec![OP_NOT, OP_VERIFY]].concat();
    [one.repeat(usize::from(k)), vec![OP_1]].concat()
}

/// The legacy filler: `OP_CHECKSIG × k`, for an output that is counted (legacy and accurate
/// alike, one per opcode) and never executed.
pub fn legacy_sigops(k: usize) -> Vec<u8> {
    vec![OP_CHECKSIG; k]
}

/// `OP_HASH160 <hash160(redeem)> OP_EQUAL`.
fn p2sh(redeem: &[u8]) -> Vec<u8> {
    let mut s = vec![0xa9, 0x14];
    s.extend(hash160(redeem));
    s.push(OP_EQUAL);
    s
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
pub fn regtest() -> &'static ChainSetup {
    static PARAMS: LazyLock<ChainSetup> = LazyLock::new(|| {
        ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
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
        let reward = genesis_reward(&dash_consensus::params::genesis_block(chain))
            .expect("regtest's genesis reward");
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
    *Header::new(dash_consensus::params::genesis_block(regtest()).header).hash().as_bytes()
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
    /// A decoded transaction's plan, which serializes back to its bytes.
    pub fn of(tx: &Tx) -> TxPlan {
        let raw = tx.raw();
        TxPlan {
            version: raw.version,
            tx_type: raw.tx_type.to_base(),
            inputs: raw
                .inputs
                .iter()
                .map(|i| Input {
                    prevout: (*i.prevout.hash.as_bytes(), i.prevout.index),
                    script_sig: i.script_sig.as_bytes().to_vec(),
                    sequence: i.sequence,
                })
                .collect(),
            outputs: raw
                .outputs
                .iter()
                .map(|o| (o.value, o.script_pubkey.as_bytes().to_vec()))
                .collect(),
            lock_time: raw.lock_time,
            extra_payload: raw.extra_payload.clone(),
        }
    }

    /// One serialized transaction's plan (Core's `DecodeTx`: trailing bytes are an error).
    pub fn decode(bytes: &[u8]) -> Result<TxPlan, String> {
        let tx = decode_tx_exact(bytes).map_err(|e| format!("a tx that does not decode: {e:?}"))?;
        Ok(TxPlan::of(&tx.into_value()))
    }

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

/// The longest [`fill_tx`] data script: `MAX_SCRIPT_SIZE`.
const FILL_MAX: usize = 10_000;
/// An output's bytes besides its script: the value and the script's compact size.
const fn output_cost(script: usize) -> usize {
    8 + compact_size_len(script) + script
}

pub const fn compact_size_len(n: usize) -> usize {
    match n {
        0..=252 => 1,
        253..=0xffff => 3,
        0x1_0000..=0xffff_ffff => 5,
        _ => 9,
    }
}

/// A transaction's serialized size.
pub fn tx_size(tx: &TxPlan) -> usize {
    core_serialize(&*tx.tx()).len()
}

/// The exact-size filler (plan rev1 §4): `tx` with data outputs of nothing appended, each
/// `OP_RETURN` and `OP_0`s (no sigops, at most `FILL_MAX` bytes), so that it serializes to
/// exactly `target` bytes. Output costs of 262 and 263 bytes do not exist (the script's compact
/// size grows by two at 253), so the split avoids them; the result is re-measured. A target 1 to
/// 9 bytes over the bare transaction is an error: no data output is that small.
pub fn fill_tx(tx: &TxPlan, target: usize) -> Result<TxPlan, String> {
    let base = tx_size(tx);
    let budget = target
        .checked_sub(base)
        .ok_or_else(|| format!("a {base}-byte transaction is over the {target}-byte target"))?;
    if budget == 0 {
        return Ok(tx.clone());
    }
    let max = output_cost(FILL_MAX);
    let mut costs = vec![max; budget.div_ceil(max).max(1)];
    let last = budget - max * (costs.len() - 1);
    *costs.last_mut().expect("one output") = last;
    let n = costs.len();
    if !(10..=261).contains(&last) && !(264..=max).contains(&last) {
        // Move bytes from the output before, or split a lone output in two.
        let take = if last < 10 {
            20
        } else {
            10
        };
        if n > 1 {
            costs[n - 2] -= take;
            costs[n - 1] += take;
        } else {
            costs = vec![last / 2, last - last / 2];
        }
    }
    if costs.iter().any(|c| !(10..=261).contains(c) && !(264..=max).contains(c)) {
        return Err(format!("{budget} bytes do not split into data outputs"));
    }
    if tx.outputs.len() + costs.len() > 252 {
        return Err(format!(
            "{} outputs change the output count's size",
            tx.outputs.len() + costs.len()
        ));
    }
    let mut out = tx.clone();
    for c in costs {
        let script = if c <= 261 {
            c - 9
        } else {
            c - 11
        };
        out.outputs.push((0, [vec![OP_RETURN], vec![OP_0; script - 1]].concat()));
    }
    match tx_size(&out) {
        size if size == target => Ok(out),
        size => Err(format!("filled to {size} bytes, not {target}")),
    }
}

/// [`fill_tx`] on `block.txs[index]` so that the block serializes to exactly `target` bytes.
pub fn fill_block(block: &BlockPlan, index: usize, target: usize) -> Result<BlockPlan, String> {
    let others: usize =
        block.txs.iter().enumerate().filter(|(i, _)| *i != index).map(|(_, tx)| tx_size(tx)).sum();
    let fixed = 80 + compact_size_len(block.txs.len()) + others;
    let tx_target = target.checked_sub(fixed).ok_or_else(|| {
        format!("{fixed} bytes besides tx {index} are over the {target}-byte target")
    })?;
    let mut out = block.clone();
    out.txs[index] = fill_tx(&block.txs[index], tx_target)?;
    match out.build().bytes.len() {
        size if size == target => Ok(out),
        size => Err(format!("filled block is {size} bytes, not {target}")),
    }
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
    /// The plan of a serialized block (L3's Core templates, plan §3.2): its header fields and
    /// transactions, the merkle root recomputed by [`build`](Self::build). Trailing bytes are an
    /// error, unlike Core's `DecodeHexBlk`: a template is exactly one block.
    pub fn from_block(bytes: &[u8]) -> Result<BlockPlan, String> {
        let mut cursor = bytes;
        let block = decode_block(&mut cursor)
            .map_err(|e| format!("a block that does not decode: {e:?}"))?
            .into_value();
        if !cursor.is_empty() {
            return Err(format!("{} bytes after the block", cursor.len()));
        }
        let header = block.header();
        let txs = block.txs().iter().map(|tx| TxPlan::of(tx)).collect();
        Ok(BlockPlan {
            prev: *header.prev_hash().as_bytes(),
            version: header.version(),
            time: header.time(),
            bits: header.bits(),
            txs,
            merkle_override: None,
        })
    }

    /// Serialize, at the first nonce whose hash meets `bits`, or regtest's limit when `bits`
    /// is easier than it or not a target at all (negative or overflowing compacts meet none): a
    /// `bad-diffbits` block still passes the proof-of-work check before the contextual one.
    pub fn build(&self) -> Built {
        let target = CompactTarget::new(self.bits.min(REGTEST_BITS));
        self.assemble(|hash| check_proof_of_work(hash, target, pow()), u32::MAX)
            .expect("regtest's limit is met within 2^32 nonces")
    }

    /// Serialize, at the first nonce whose hash meets the header's own `bits` by Core's
    /// `CheckProofOfWork` (`grind`), or at nonce 0 with no proof (O10-3's unground draws, whose
    /// expected class is high-hash). `Err` when no nonce under 2^24 meets the bits.
    pub fn finish(&self, grind: bool) -> Result<Built, String> {
        const TRIES: u32 = 1 << 24;
        match grind {
            true => self.assemble(|hash| meets_bits(hash, self.bits), TRIES),
            false => self.assemble(|_| true, 1),
        }
        .ok_or_else(|| format!("no nonce under {TRIES} meets bits {:08x}", self.bits))
    }

    /// The block at the first nonce under `tries` whose hash `meets`.
    fn assemble(&self, meets: impl Fn(&[u8; 32]) -> bool, tries: u32) -> Option<Built> {
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
        let block = loop {
            let block = Block::new(Header::new(header), txs.clone());
            if meets(block.hash().as_bytes()) {
                break block;
            }
            if header.nonce == tries - 1 {
                return None;
            }
            header.nonce += 1;
        };
        let bytes = core_serialize(&block);
        let mut head = [0u8; 80];
        head.copy_from_slice(&bytes[..80]);
        Some(Built {
            hash: *block.hash().as_bytes(),
            header: head,
            txids: txs.iter().map(|t| *t.txid().as_bytes()).collect(),
            bytes,
        })
    }
}

/// Whether `hash` meets `bits` by Core's `CheckProofOfWork` on regtest (pow.cpp:239): a
/// positive, non-overflowing, nonzero target no easier than powLimit, and the hash under it.
pub fn meets_bits(hash: &[u8; 32], bits: u32) -> bool {
    check_proof_of_work(hash, CompactTarget::new(bits), pow())
}

#[cfg(test)]
mod tests {
    use dash_consensus::script::parse::get_op;
    use dash_consensus::script::sigops::{legacy_sigop_count, script_sigop_count_with_sig};

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
        let decoded = decode_block(&mut cursor).expect("decodes");
        assert_eq!(decoded.value().hash().as_bytes(), &built.hash);
        assert_eq!(built.txids.len(), 1);
    }

    /// A built block read back into a plan rebuilds to the same bytes (the nonce ground from 0
    /// again), and its transactions keep every field.
    #[test]
    fn a_block_reads_back_into_its_plan() {
        let mut spend = TxPlan::simple(
            vec![Input {
                prevout: ([3; 32], 2),
                script_sig: vec![0x51, 0x52],
                sequence: 7,
            }],
            vec![(1_234, Lock::Csv(5).script_pubkey()), (0, vec![OP_RETURN, 1, 9])],
        );
        spend.version = 3;
        spend.tx_type = 1;
        spend.lock_time = 99;
        spend.extra_payload = vec![1, 2, 3];
        let plan = BlockPlan {
            prev: genesis_hash(),
            version: VERSION_BITS_TOP,
            time: GENESIS_TIME + SPACING,
            bits: REGTEST_BITS,
            txs: vec![coinbase(1, subsidy(1), 7), spend],
            merkle_override: None,
        };
        let built = plan.build();
        let back = BlockPlan::from_block(&built.bytes).expect("reads back");
        assert_eq!(back, plan);
        assert_eq!(back.build(), built);
        let mut trailing = built.bytes.clone();
        trailing.push(0);
        assert!(BlockPlan::from_block(&trailing).is_err());
        assert!(BlockPlan::from_block(&built.bytes[..81]).is_err());
    }

    /// The sigop filler's exact shape and counts: 38 bytes and 3 counted opcodes per repetition,
    /// `k` accurate sigops through its P2SH spend, and none for the legacy count of either side.
    #[test]
    fn the_sigop_filler_counts_exactly() {
        for k in 0..=SIGOPS_MAX {
            let redeem = sigop_redeem(k);
            assert_eq!(redeem.len(), 38 * usize::from(k) + 1);
            // MAX_OPS_PER_SCRIPT counts every opcode above OP_16.
            let (mut pc, mut counted) = (0, 0);
            while let Some(op) = get_op(&redeem, &mut pc) {
                counted += usize::from(op.opcode > 0x60);
            }
            assert_eq!((pc, counted), (redeem.len(), 3 * usize::from(k)));
            let lock = Lock::Sigops(k);
            let (spk, sig) = (lock.script_pubkey(), lock.script_sig());
            assert_eq!(script_sigop_count_with_sig(&spk, &sig), u32::from(k));
            let spend = TxPlan::simple(vec![Input::new(([1; 32], 0), sig)], vec![(1, spk)]);
            assert_eq!(legacy_sigop_count(spend.tx().raw()), 0);
        }
        assert_eq!(sigop_redeem(SIGOPS_MAX).len(), 495);
        let out = TxPlan::simple(Vec::new(), vec![(0, legacy_sigops(7))]);
        assert_eq!(legacy_sigop_count(out.tx().raw()), 7);
    }

    /// The bad-signature puzzles: P2PKH and P2SH spends that push `BAD_SIG`, and the negated
    /// non-DER check.
    #[test]
    fn the_signature_puzzles_have_their_shapes() {
        let p2pkh = Lock::P2pkhBadSig.script_pubkey();
        assert_eq!(
            (p2pkh.len(), &p2pkh[..3], &p2pkh[23..]),
            (25, &[OP_DUP, OP_HASH160, 20][..], &[OP_EQUALVERIFY, OP_CHECKSIG][..])
        );
        assert_eq!(Lock::P2pkhBadSig.script_sig(), [push(&BAD_SIG), push(&G)].concat());
        assert_eq!(Lock::P2shBadSig.script_pubkey(), p2sh(&checksig_g()));
        assert_eq!(
            script_sigop_count_with_sig(
                &Lock::P2shBadSig.script_pubkey(),
                &Lock::P2shBadSig.script_sig()
            ),
            1
        );
        assert_eq!(
            Lock::NonDerNot.script_pubkey(),
            [Lock::NonDer.script_pubkey(), vec![OP_NOT]].concat()
        );
        assert_eq!(Lock::NonDerNot.script_sig(), Lock::NonDer.script_sig());
    }

    /// Every target from the bare transaction up, across the compact-size steps of a script
    /// (253) and of a whole output split, fills exactly; below the bare size, or within 9 bytes
    /// of it, is an error.
    #[test]
    fn the_filler_hits_every_target() {
        let tx = TxPlan::simple(vec![Input::new(([2; 32], 1), Vec::new())], vec![(5, vec![OP_1])]);
        let base = tx_size(&tx);
        assert!(fill_tx(&tx, base - 1).is_err());
        assert_eq!(fill_tx(&tx, base).expect("bare"), tx);
        assert!((1..10).all(|extra| fill_tx(&tx, base + extra).is_err()));
        for extra in
            (10..700).chain([10_010, 10_011, 10_020, 10_270, 10_271, 10_272, 10_273, 20_022])
        {
            let filled = fill_tx(&tx, base + extra).unwrap_or_else(|e| panic!("+{extra}: {e}"));
            assert_eq!(tx_size(&filled), base + extra);
            assert_eq!(filled.outputs[..1], tx.outputs[..]);
            for (value, script) in &filled.outputs[1..] {
                assert_eq!((*value, script[0]), (0, OP_RETURN));
                assert!(script.len() <= FILL_MAX && script[1..].iter().all(|&b| b == OP_0));
            }
        }
        for size in [99_999, 100_000, 100_001, 1_000_000, 1_000_001] {
            assert_eq!(tx_size(&fill_tx(&tx, size).expect("fills")), size);
        }
    }

    #[test]
    fn a_block_fills_to_its_target() {
        let spend =
            TxPlan::simple(vec![Input::new(([2; 32], 1), Vec::new())], vec![(5, vec![OP_1])]);
        let plan = BlockPlan {
            prev: genesis_hash(),
            version: VERSION_BITS_TOP,
            time: GENESIS_TIME + SPACING,
            bits: REGTEST_BITS,
            txs: vec![coinbase(1, subsidy(1), 7), spend],
            merkle_override: None,
        };
        for size in [999_999, 1_000_000, 1_000_001] {
            assert_eq!(fill_block(&plan, 1, size).expect("fills").build().bytes.len(), size);
        }
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
