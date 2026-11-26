//! The coins admission reads: Core's `CCoinsViewMemPool` over `CoinsTip()`, and `PreChecks`'
//! input loop, which caches the inputs in `m_view` and then detaches its backend.

use std::collections::BTreeMap;
use std::convert::Infallible;

use dash_consensus::Tx;
use dash_consensus::coins::{Coin, CoinRef, CoinsView};
use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_primitives::{BlockHash, OutPoint};
use dashd_chainstate::chain::Fatal;
use dashd_chainstate::chain::seams::AtmpCoins;

use crate::api::MempoolRead;

/// Core's `MEMPOOL_HEIGHT`: the height a coin of an in-pool transaction has.
// Core: txmempool.h:60 at v24.0.0
pub const MEMPOOL_HEIGHT: u32 = 0x7FFF_FFFF;

/// The pool half of `CCoinsViewMemPool::GetCoin`: `Some` when `outpoint.txid` is in the pool
/// (the output at [`MEMPOOL_HEIGHT`], or `None` past its outputs, with no fallback to the base),
/// `None` when the base answers.
// Core: txmempool.cpp:1954-1975 at v24.0.0 CCoinsViewMemPool::GetCoin
pub fn pool_coin(pool: &dyn MempoolRead, outpoint: &OutPoint) -> Option<Option<Coin>> {
    let tx = pool.get(&outpoint.txid())?;
    Some(
        tx.outputs()
            .get(outpoint.index() as usize)
            .map(|out| Coin::from_output(out, MEMPOOL_HEIGHT, false)),
    )
}

/// `CCoinsViewMemPool::m_temp_added`: the outputs of a package's earlier transactions, for one
/// package call. It is never written to CoinsTip.
#[derive(Clone, Debug, Default)]
pub struct PackageCoins(BTreeMap<OutPoint, Coin>);

impl PackageCoins {
    /// `PackageAddTransaction`: each of `tx`'s outputs at [`MEMPOOL_HEIGHT`].
    // Core: txmempool.cpp:1977-1982 at v24.0.2
    pub fn add_transaction(&mut self, tx: &Tx) {
        let txid = tx.txid();
        for (n, out) in tx.outputs().iter().enumerate() {
            self.0
                .entry(OutPoint::new(txid, n as u32))
                .or_insert_with(|| Coin::from_output(out, MEMPOOL_HEIGHT, false));
        }
    }
}

/// `CCoinsViewMemPool::GetCoin` over `base` (Core's `base->GetCoin`, CoinsTip's caching read):
/// the package's coins, then the pool, then the base.
///
/// # Errors
///
/// The base's read fault.
// Core: txmempool.cpp:1954-1975 at v24.0.2 CCoinsViewMemPool::GetCoin
pub fn mempool_view_coin(
    package: &PackageCoins,
    pool: &dyn MempoolRead,
    outpoint: &OutPoint,
    base: &mut dyn FnMut(&OutPoint) -> Result<Option<Coin>, Fatal>,
) -> Result<Option<Coin>, Fatal> {
    if let Some(coin) = package.0.get(outpoint) {
        return Ok(Some(coin.clone()));
    }
    match pool_coin(pool, outpoint) {
        Some(coin) => Ok(coin),
        None => base(outpoint),
    }
}

/// `m_view` after `PreChecks` detached its backend: the inputs it cached and the best block it
/// pinned. A coin it did not cache reads as absent, as the dummy backend's does. There is one
/// per transaction, where Core shares `m_view` across a package: an input Core's `m_view` cached
/// whose source left the pool mid-package reaches Core's assert (`validation.cpp:557`), and is
/// `bad-txns-inputs-missingorspent` here.
#[derive(Clone, Debug, Default)]
pub struct InputView {
    coins: BTreeMap<OutPoint, Coin>,
    best_block: Option<BlockHash>,
}

impl InputView {
    /// The cached coin of `outpoint`.
    pub fn get(&self, outpoint: &OutPoint) -> Option<&Coin> {
        self.coins.get(outpoint)
    }
}

impl CoinsView for InputView {
    type Error = Infallible;

    fn coin(&self, outpoint: &OutPoint) -> Result<Option<CoinRef<'_>>, Infallible> {
        Ok(self.coins.get(outpoint).map(Coin::as_ref))
    }

    fn best_block(&self) -> Option<BlockHash> {
        self.best_block
    }
}

/// `PreChecks`' input loop and the backend detach (`v:972-1003`): for each input, records an
/// outpoint CoinsTip had not cached in `coins_to_uncache`, then reads it through the mempool view
/// (with `package`'s coins) into `m_view`. A missing input is `txn-already-known` when CoinsTip
/// has any of `tx`'s own outputs cached, otherwise `bad-txns-inputs-missingorspent`. Then
/// `m_view` pins CoinsTip's best block, which must be `tip` (Core asserts).
///
/// # Errors
///
/// A coins read fault, or Core's assert on the pinned best block.
// Core: validation.cpp:972-1003 at v24.0.2 MemPoolAccept::PreChecks
pub fn fetch_inputs(
    tx: &Tx,
    package: &PackageCoins,
    pool: &dyn MempoolRead,
    coins_tip: &mut dyn AtmpCoins,
    tip: BlockHash,
    coins_to_uncache: &mut Vec<OutPoint>,
) -> Result<Result<InputView, TxValidationError>, Fatal> {
    let mut view = InputView::default();
    for input in tx.inputs() {
        let prevout = input.prevout();
        if !coins_tip.have_in_cache(&prevout) {
            coins_to_uncache.push(prevout);
        }
        let coin = match view.coins.get(&prevout) {
            Some(coin) => Some(coin.clone()),
            None => mempool_view_coin(package, pool, &prevout, &mut |outpoint| {
                coins_tip.fetch(outpoint)
            })?,
        };
        let Some(coin) = coin else {
            return Ok(Err(missing_input(tx, coins_tip)));
        };
        view.coins.insert(prevout, coin);
    }
    view.best_block = coins_tip.best_block();
    if view.best_block != Some(tip) {
        return Err(Fatal::assert(format!(
            "PreChecks: m_view's best block {:?} is not the tip {tip:?}",
            view.best_block
        )));
    }
    Ok(Ok(view))
}

/// Are inputs missing because we already have the tx? (`v:981-989`)
fn missing_input(tx: &Tx, coins_tip: &dyn AtmpCoins) -> TxValidationError {
    let txid = tx.txid();
    let known =
        (0..tx.outputs().len()).any(|n| coins_tip.have_in_cache(&OutPoint::new(txid, n as u32)));
    if known {
        TxValidationError::new(TxValidationResult::Conflict, "txn-already-known")
    } else {
        TxValidationError::new(TxValidationResult::MissingInputs, "bad-txns-inputs-missingorspent")
    }
}
