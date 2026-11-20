//! The P2P layer's [`CoinJoinObjects`] over the node's DSTX manager and wallet queue manager.

use std::sync::{Arc, Mutex, PoisonError};

use dash_coinjoin::dstx::{DstxScore, DstxValidation, txid};
use dash_coinjoin::queue::{MixingCounters, MixingMeta};
use dash_num::Hash256;
use dash_p2p::PeerId;
use dash_p2p_core::DsTx;
use dash_primitives::TxHash;
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashd_p2p::processing::seams::{CoinJoinObjects, DstxCheck};

use crate::dsq::DsqRuntime;
use crate::dstx::DstxRuntime;
use crate::env::{ChainView, DstxChainAdapter, MnLists};

/// Core's `m_dstxman` and `m_cj_walletman` queue reads as `net_processing.cpp` uses them.
///
/// Lock order:
/// - counters → `dstxes` → chain/list leaves;
/// - [`DsqRuntime`]: queues → counters;
/// - the counters mutex is shared only with `msghand` users, never with an RPC or other
///   non-`msghand` reader.
pub struct RuntimeObjects<C, M, Meta = MixingCounters> {
    dstx: Arc<DstxRuntime>,
    chain: Arc<C>,
    mns: Arc<M>,
    meta: Arc<Mutex<Meta>>,
    queues: Option<DsqRuntime<C, M, Meta>>,
}

impl<C, M, Meta> RuntimeObjects<C, M, Meta> {
    /// `queues` is the non-masternode queue runtime; a masternode passes `None`.
    pub fn new(
        dstx: Arc<DstxRuntime>,
        chain: Arc<C>,
        mns: Arc<M>,
        meta: Arc<Mutex<Meta>>,
        queues: Option<DsqRuntime<C, M, Meta>>,
    ) -> Self {
        Self {
            dstx,
            chain,
            mns,
            meta,
            queues,
        }
    }
}

/// Maps `ValidateDSTX`'s result to the P2P intake decision.
fn dstx_check(validation: DstxValidation) -> DstxCheck {
    match validation {
        DstxValidation::Stop(DstxScore::None) => DstxCheck::Stop,
        DstxValidation::Stop(score) => DstxCheck::Reject {
            score: Some(score.score()),
        },
        DstxValidation::Continue {
            priority_delta,
            ..
        } => DstxCheck::Accept {
            priority_delta,
        },
    }
}

impl<C, M, Meta> CoinJoinObjects for RuntimeObjects<C, M, Meta>
where
    C: ChainView + Send + Sync,
    M: MnLists + Send + Sync,
    Meta: MixingMeta + Send,
{
    fn enabled(&self) -> bool {
        true
    }

    // Core v24.0.0: net_processing.cpp:3642-3720,4930-4938.
    fn validate_dstx(&self, dstx: &DsTx, _from: PeerId) -> DstxCheck {
        let chain = DstxChainAdapter {
            chain: &*self.chain,
            mns: &*self.mns,
        };
        let mut meta = self.meta.lock().unwrap_or_else(PoisonError::into_inner);
        dstx_check(self.dstx.check(dstx, &txid(dstx.tx()), &mut *meta, &chain))
    }

    // Core v24.0.0: net_processing.cpp:4964-4967.
    fn add_dstx(&self, dstx: DsTx) {
        self.dstx.on_accepted(dstx);
    }

    // Core v24.0.0: net_processing.cpp:2984-2990.
    fn dstx_by_hash(&self, hash: &TxHash) -> Option<Vec<u8>> {
        let stored = self.dstx.get(hash)?;
        let mut bytes = Vec::new();
        stored.dstx.encode(&mut bytes);
        Some(bytes)
    }

    // Core v24.0.0: net_processing.cpp:3061-3066.
    fn queue_by_hash(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
        let dsq = self.queues.as_ref()?.get(&Hash256::from_lendian(*hash))?;
        let mut bytes = Vec::new();
        dsq.encode(&mut bytes);
        Some(bytes)
    }
}
