//! Shared test fixtures: Core's test helpers (scripts, hashes, transactions), a coins view with a
//! mempool, and an operator key.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use dash_coinjoin::collateral::MempoolAccept;
use dash_coinjoin::denom::{COLLATERAL_AMOUNT, SMALLEST_DENOMINATION};
use dash_coinjoin::dstx::PoolLimits;
use dash_coinjoin::inouts::{CoinView, ViewCoin};
use dash_num::Hash256;
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_pkc::bls::core::BlsPkValue;
use dash_pkc::bls::{BlsScIetf, BlsSchemeKind, BlsSecretKey, BlsSigId};
use dash_primitives::{OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};
// `TxHash::from_lendian`.
pub use dash_types::Numeric;

/// Core's test `P2PKHScript(tag)`: `OP_DUP OP_HASH160 <20 x tag> OP_EQUALVERIFY OP_CHECKSIG`.
pub fn p2pkh(tag: u8) -> Vec<u8> {
    let mut s = vec![0x76, 0xa9, 0x14];
    s.extend([tag; 20]);
    s.extend([0x88, 0xac]);
    s
}

/// A P2PKH script per `n`, for tests that need more distinct scripts than a byte tag gives.
pub fn key(n: u32) -> Vec<u8> {
    let mut s = vec![0x76, 0xa9, 0x14];
    s.extend(n.to_le_bytes());
    s.extend([0xcc; 16]);
    s.extend([0x88, 0xac]);
    s
}

pub fn txout(value: i64, script: Vec<u8>) -> TxOut {
    TxOut {
        value,
        script_pubkey: script.into(),
    }
}

/// `uint256S("<low>")` for a one-byte value: `uint256::ONE` is `u256(1)`.
pub fn u256(low: u8) -> TxHash {
    let mut bytes = [0u8; 32];
    bytes[0] = low;
    TxHash::from_lendian(bytes)
}

pub fn outpoint(hash: TxHash, index: u32) -> OutPoint {
    OutPoint {
        hash,
        index,
    }
}

/// `CTxIn(prevout)`: an empty script and the final sequence.
pub fn txin(prevout: OutPoint) -> TxIn {
    TxIn {
        prevout,
        script_sig: Vec::new().into(),
        sequence: u32::MAX,
    }
}

/// `CMutableTransaction` with these inputs and outputs (version 2, no lock time).
pub fn tx(inputs: Vec<TxIn>, outputs: Vec<TxOut>) -> Transaction {
    Transaction {
        version: 2,
        tx_type: TxType::Spend,
        inputs,
        outputs,
        lock_time: 0,
        extra_payload: Vec::new(),
    }
}

/// The tests' clock: seconds since the epoch (October 2025).
pub const NOW: i64 = 1_760_000_000;

/// Pool limits of `min` to `max` participants.
pub fn limits(min: i32, max: i32) -> PoolLimits {
    PoolLimits {
        min_participants: min,
        max_participants: max,
    }
}

/// Core's mainnet pool limits (the unit tests' `TestingSetup` chain): 3 to 20 participants.
pub fn mainnet_limits() -> PoolLimits {
    PoolLimits::of(
        &ChainParamsBuilder::new(Network::Main, Release::V24_0).build().expect("Core's parameters"),
    )
}

/// The smallest denomination as Core's `AmountToDenomination` names it.
pub const SMALLEST: i32 = 1 << 4;

/// A coins view and mempool: confirmed and mempool coins, InstantSend locks, and the fee the
/// mempool's test-accept reports (`None`: it would reject).
#[derive(Default)]
pub struct Chain {
    pub coins: BTreeMap<OutPoint, ViewCoin>,
    pub locked: BTreeSet<TxHash>,
    pub accept_fee: Option<i64>,
    next: u32,
}

impl Chain {
    pub fn add_coin(&mut self, outpoint: OutPoint, out: TxOut, in_mempool: bool) {
        self.coins.insert(
            outpoint,
            ViewCoin {
                out,
                in_mempool,
            },
        );
    }

    /// A fresh confirmed coin of `value` paying to a fresh P2PKH script.
    pub fn fund(&mut self, value: i64) -> OutPoint {
        self.next += 1;
        let mut bytes = [0xf0u8; 32];
        bytes[..4].copy_from_slice(&self.next.to_le_bytes());
        let op = outpoint(TxHash::from_lendian(bytes), 0);
        self.add_coin(op, txout(value, key(0x0100_0000 + self.next)), false);
        op
    }

    /// A valid collateral: one funded input paying [`COLLATERAL_AMOUNT`] in fees (the mempool
    /// reports [`COLLATERAL_AMOUNT`] when it accepts).
    pub fn collateral(&mut self) -> Transaction {
        let input = self.fund(2 * COLLATERAL_AMOUNT);
        self.accept_fee = Some(COLLATERAL_AMOUNT);
        tx(vec![txin(input)], vec![txout(COLLATERAL_AMOUNT, key(0x0200_0000 + self.next))])
    }

    /// `count` fresh confirmed coins of the smallest denomination.
    pub fn smallest_coins(&mut self, count: usize) -> Vec<TxIn> {
        (0..count).map(|_| txin(self.fund(SMALLEST_DENOMINATION))).collect()
    }
}

impl CoinView for Chain {
    fn coin(&self, outpoint: &OutPoint) -> Option<ViewCoin> {
        self.coins.get(outpoint).cloned()
    }

    fn is_instantsend_locked(&self, txid: &TxHash) -> bool {
        self.locked.contains(txid)
    }
}

impl MempoolAccept for Chain {
    fn test_accept(&self, _tx: &Transaction) -> Option<i64> {
        self.accept_fee
    }
}

/// An operator key: Core's `CBLSSecretKey`, signing with `SignBasic`.
pub struct Operator {
    sk: BlsSecretKey<BlsScIetf>,
}

impl Operator {
    pub fn new(seed: u8) -> Self {
        Self {
            sk: BlsSecretKey::from_ikm(&[seed; 32]).expect("key"),
        }
    }

    /// `SignBasic(hash)`.
    pub fn sign(&self, hash: &Hash256) -> Vec<u8> {
        self.sk.sign_with(hash.as_bytes(), BlsSigId::Basic).to_bytes().to_vec()
    }

    /// `GetPubKey()` as the masternode list holds it.
    pub fn public(&self) -> BlsPkValue {
        BlsPkValue::from_canonical(&self.sk.public_key().to_bytes(), BlsSchemeKind::Basic)
            .expect("valid key")
    }
}
