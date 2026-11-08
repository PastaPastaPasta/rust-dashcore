//! [`VerifyScratch`]: VerifyDB's scratch view in production (CH5-1a scope 1; CH3 D36), the
//! [`ScratchView`] [`CoreConnector`](super::CoreConnector) builds for levels 3 and 4.
//!
//! Core's `CCoinsViewCache coins(&coinsview)` and `evoDb.BeginTransaction()`
//! (`validation.cpp:5053`, `:5066`), neither ever flushed or committed: a scratch coins layer
//! ([`CoinsCache::scratch`]) over the database at startup or over the attached tip on the
//! `verifychain` path, and a copy of the active evo layer ([`EvoHooks::scratch_layer`]). Dropping
//! the view rolls both back. The one thing it leaves behind is the RPC path's parent fills: a
//! read below the scratch layer is the tip's fetching read (DEC-981a, Sol r2 R2).
//!
//! Over the database, startup's view holds its engine's [`startup_verify`] region for the walk,
//! as Core's `coins` local spans it (`validation.cpp:5067` at v24.0.0): a read that fails on V's
//! thread meanwhile is recorded and returns, Core's throw to init's catch, or an evo hook's
//! catch inside its `try` (BL-787).
//!
//! [`startup_verify`]: crate::store::GuardedEngine::startup_verify

use dashd_store::KvEngine;

use super::connector::{ChainstateCell, lock};
use crate::chain::{EvoTxn, Fatal, ScratchCoins, ScratchParent, ScratchView};
use crate::coins::CoinsCache;
use crate::evo::EvoLayer;
use crate::store::StartupVerifyReads;

/// VerifyDB's scratch view over the coins cell `tip` (Core's `CoinsTip()`).
pub struct VerifyScratch<E: KvEngine + 'static> {
    coins: CoinsCache<E>,
    evo: ScratchEvo,
    tip: ChainstateCell<E>,
    /// Startup's region over the database, which nothing else holds open ([`ScratchParent::Db`]).
    region: Option<StartupVerifyReads<E>>,
}

impl<E: KvEngine + 'static> VerifyScratch<E> {
    /// The view over `tip`, its coins read through `parent`; `evo` the hooks' scratch layer, if
    /// they keep one.
    pub fn new(
        tip: &ChainstateCell<E>,
        parent: ScratchParent,
        evo: Option<EvoLayer>,
    ) -> Result<Self, Fatal> {
        let startup = parent == ScratchParent::Db;
        let parent = (!startup).then(|| tip.clone());
        let coins = CoinsCache::scratch(&*lock(tip)?, parent);
        let region = startup.then(|| coins.db_handle().startup_verify());
        Ok(Self {
            coins,
            evo: ScratchEvo(evo),
            tip: tip.clone(),
            region,
        })
    }
}

impl<E: KvEngine + 'static> ScratchView for VerifyScratch<E> {
    fn split(&mut self) -> (ScratchCoins<'_>, &mut dyn EvoTxn) {
        (ScratchCoins::new(&mut self.coins), &mut self.evo)
    }

    fn fits(&self) -> Result<bool, Fatal> {
        Ok(lock(&self.tip)?.verify_fits(&self.coins))
    }

    fn read_fault(&self) -> Option<String> {
        self.region.as_ref().and_then(StartupVerifyReads::fault)
    }

    fn read_caught(&self) -> bool {
        self.region.as_ref().is_some_and(StartupVerifyReads::caught)
    }
}

/// The view's evo transaction: the hooks' scratch layer, or none.
struct ScratchEvo(Option<EvoLayer>);

impl EvoTxn for ScratchEvo {
    fn layer(&mut self) -> Option<&mut EvoLayer> {
        self.0.as_mut()
    }
}
