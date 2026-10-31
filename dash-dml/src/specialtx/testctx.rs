//! The L0 context of EV1-3's checks (EV1 §6.1): a list, a model coins view, the buried
//! deployments by flag, mainnet's parameters and NetInfo rules, and builders of the special
//! transactions the checks read.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::Arc;

use dash_consensus::coins::{CoinRef, CoinsView};
use dash_consensus::ctx::{CheckpointView, Deployment, DeploymentView, bls_ctx_for_block};
use dash_consensus::primitives::Tx;
use dash_consensus::{BlockId, ConsensusCtx};
use dash_evo::ctx::EvoCtx;
use dash_evo::spork::{key_id_of, sign_compact};
use dash_params::{BuriedDeployment, ChainParams};
use dash_primitives::{
    BlockHash, CompactSig, InputsHash, OutPoint, Transaction, TxHash, TxIn, TxOut, TxType,
};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use secp256k1::{PublicKey, SecretKey};

use super::sig::{SerialVerifier, SigVerifier, inputs_hash_of};
use super::{CheckResult, SpecialTxCtx, SpecialTxRules, check_special_tx_inner};
use crate::netinfo::NetInfoRules;
use crate::testkit::MemChain;
use crate::testkit::replay::{CORE_NET_INFO, mainnet};
use crate::types::KeyId;
use crate::{Dmn, MnList};

/// A height past mainnet's DIP3 enforcement (1,047,200), and one between DIP3's activation
/// (1,028,160) and its enforcement.
pub(crate) const ENFORCED: u32 = 2_000_000;
pub(crate) const NOT_ENFORCED: u32 = 1_040_000;

/// A coins view of unspent outputs.
#[derive(Default)]
pub(crate) struct ModelCoins(pub BTreeMap<OutPoint, TxOut>);

impl ModelCoins {
    pub(crate) fn with(mut self, outpoint: OutPoint, out: TxOut) -> Self {
        self.0.insert(outpoint, out);
        self
    }
}

impl CoinsView for ModelCoins {
    type Error = Infallible;

    fn coin(&self, op: &OutPoint) -> Result<Option<CoinRef<'_>>, Infallible> {
        Ok(self.0.get(op).map(|out| CoinRef::from_output(out, 1, false)))
    }

    fn best_block(&self) -> Option<BlockHash> {
        None
    }
}

/// A view whose every read fails.
pub(crate) struct FailingCoins;

impl CoinsView for FailingCoins {
    type Error = &'static str;

    fn coin(&self, _: &OutPoint) -> Result<Option<CoinRef<'_>>, &'static str> {
        Err("read failure")
    }

    fn best_block(&self) -> Option<BlockHash> {
        None
    }
}

/// DIP3 and V19 by flag, whatever the block; no versionbits deployment.
struct Flags {
    dip3: bool,
    v19: bool,
    v20: bool,
}

impl Flags {
    fn active(&self, deployment: Deployment) -> bool {
        match deployment {
            Deployment::Buried(BuriedDeployment::Dip0003) => self.dip3,
            Deployment::Buried(BuriedDeployment::V19) => self.v19,
            Deployment::Buried(BuriedDeployment::Dip0008 | BuriedDeployment::V20) => self.v20,
            _ => false,
        }
    }
}

impl DeploymentView for Flags {
    fn active_after(&self, prev: Option<BlockId>, deployment: Deployment) -> bool {
        prev.is_some() && self.active(deployment)
    }

    fn active_at(&self, _: BlockId, deployment: Deployment) -> bool {
        self.active(deployment)
    }
}

pub(crate) struct NoCheckpoints;

impl CheckpointView for NoCheckpoints {
    fn highest_checkpoint_in_index(&self) -> Option<u32> {
        None
    }
}

/// What a check reads: mainnet, the block at `height` (`pindexPrev` at `height - 1`).
pub(crate) struct Fixture {
    pub list: MnList,
    pub coins: ModelCoins,
    pub rules: SpecialTxRules,
    pub dip3: bool,
    pub v19: bool,
    /// DIP0008 and V20 (off unless a test sets it): only `CheckCbTx` reads them.
    pub v20: bool,
    pub height: u32,
    pub check_sigs: bool,
    /// The serial verifier unless a test sets another.
    pub verifier: Box<dyn SigVerifier>,
    /// Mainnet's unless a test sets another.
    pub params: &'static ChainParams,
}

impl Fixture {
    /// V24 active (EVO_SHARES not), past DIP3 enforcement, an empty list and view, signatures
    /// not checked.
    pub(crate) fn v24() -> Fixture {
        Fixture {
            list: MnList::scratch(),
            coins: ModelCoins::default(),
            rules: SpecialTxRules {
                v24: true,
                evo_shares: false,
            },
            dip3: true,
            v19: true,
            v20: false,
            height: ENFORCED,
            check_sigs: false,
            verifier: Box::new(SerialVerifier),
            params: mainnet(),
        }
    }

    pub(crate) fn params(&self) -> &'static ChainParams {
        self.params
    }

    /// The list with `dmn` added.
    pub(crate) fn with_mn(mut self, dmn: Arc<Dmn>) -> Self {
        self.list.try_add(dmn, true).expect("a unique masternode");
        self
    }

    /// `f` of the context over `coins`.
    pub(crate) fn with_view<E: core::fmt::Debug, R>(
        &self,
        coins: &dyn CoinsView<Error = E>,
        f: impl FnOnce(&mut SpecialTxCtx<'_, E>) -> R,
    ) -> R {
        let chain = MemChain::new();
        let deployments = Flags {
            dip3: self.dip3,
            v19: self.v19,
            v20: self.v20,
        };
        let params = self.params();
        let cons = ConsensusCtx {
            params,
            height: self.height,
            prev: Some(chain.genesis()),
            chain: &chain,
            deployments: &deployments,
            checkpoints: &NoCheckpoints,
            bls: bls_ctx_for_block(params, self.height),
            median_time_past: 0,
            header_check_time: 0,
        };
        let mut cx = SpecialTxCtx {
            evo: EvoCtx {
                cons: &cons,
                check_sigs: self.check_sigs,
            },
            rules: self.rules,
            list: &self.list,
            coins,
            net_info: NetInfoRules::new(params, mainnet(), CORE_NET_INFO),
            verifier: self.verifier.as_ref(),
            quorum: None,
        };
        f(&mut cx)
    }

    /// `f` of the context over the fixture's view.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut SpecialTxCtx<'_, Infallible>) -> R) -> R {
        self.with_view(&self.coins, f)
    }

    /// `CheckSpecialTxInner` of `tx`.
    pub(crate) fn check(&self, tx: &Tx) -> CheckResult<Infallible> {
        self.with(|cx| check_special_tx_inner(tx, tx.payload(), cx))
    }
}

/// The payload's encoding.
#[allow(clippy::disallowed_methods, reason = "test payloads encoded into a transaction")]
pub(crate) fn encoded<T: BaseCodec>(payload: &T) -> Vec<u8> {
    let mut bytes = Vec::new();
    payload.encode(&mut bytes);
    bytes
}

/// A version-3 transaction of `tx_type` with `extra_payload`.
pub(crate) fn tx_of(
    tx_type: TxType,
    inputs: Vec<TxIn>,
    outputs: Vec<TxOut>,
    extra_payload: Vec<u8>,
) -> Tx {
    Tx::new(Transaction {
        version: 3,
        tx_type,
        inputs,
        outputs,
        lock_time: 0,
        extra_payload,
    })
}

/// [`tx_of`] with `payload` encoded.
pub(crate) fn special<T: BaseCodec>(
    tx_type: TxType,
    inputs: Vec<TxIn>,
    outputs: Vec<TxOut>,
    payload: &T,
) -> Tx {
    tx_of(tx_type, inputs, outputs, encoded(payload))
}

/// The input spending `(hash, index)`, with an empty scriptSig.
pub(crate) fn txin(hash: [u8; 32], index: u32) -> TxIn {
    TxIn {
        prevout: outpoint(hash, index),
        script_sig: Vec::new().into(),
        sequence: u32::MAX,
    }
}

pub(crate) fn outpoint(hash: [u8; 32], index: u32) -> OutPoint {
    OutPoint {
        hash: TxHash::from(hash),
        index,
    }
}

pub(crate) fn txout(value: i64, script: Vec<u8>) -> TxOut {
    TxOut {
        value,
        script_pubkey: script.into(),
    }
}

/// The `inputsHash` a payload must carry for `inputs`.
pub(crate) fn carried_inputs_hash(inputs: &[TxIn]) -> InputsHash {
    InputsHash::from_lendian(inputs_hash_of(inputs))
}

/// An ECDSA key from `seed` and its compressed key id.
pub(crate) fn signer(seed: u8) -> (SecretKey, KeyId) {
    let key = SecretKey::from_secret_bytes([seed; 32]).expect("a key");
    (key, KeyId::from(key_id_of(&PublicKey::from_secret_key(&key), true)))
}

/// A compact signature of `hash` by `key`.
pub(crate) fn sign(hash: &[u8; 32], key: &SecretKey) -> Vec<u8> {
    sign_compact(hash, key, true)
}

/// A provider signature (`CollateralSig`, `OwnerSig`) holding `bytes`, as the decoder reads it.
pub(crate) fn sig_of<T: BaseCodec>(bytes: &[u8]) -> T {
    T::decode(&mut encoded(&bytes.to_vec()).as_slice()).expect("a signature of any length decodes")
}

pub(crate) fn compact(sig: &[u8]) -> CompactSig {
    CompactSig::from_bytes(sig.try_into().expect("65 bytes"))
}
