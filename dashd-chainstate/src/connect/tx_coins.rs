//! [`TxCoins`], CH3 §6.2's lever L1 (one resolve per input; CH3a D186–D188): the coins a
//! transaction spends, each read once through the block's overlay, for every later read of the
//! connect loop.

use std::cell::Cell;

use bumpalo::Bump;
use dash_consensus::Tx;
use dash_consensus::coins::{CoinRef, CoinsView};
use dash_consensus::primitives::{OutPoint, TxIn};
use dash_primitives::BlockHash;
use dashd_store::KvEngine;

use super::serial::read_failed;
use crate::chain::Fatal;
use crate::coins::BlockOverlay;

/// A transaction's spent coins as the overlay holds them once `HaveInputs` has cached them
/// (`coins.cpp:303-313`): read by `CheckTxInputs`, the sequence locks' `prevheights`, the sigop
/// count and the spent-script copy (`v:2698-2752`), where each of those used to resolve every input
/// through the overlay again. A coinbase has none ([`TxCoins::none`]).
///
/// Every read is by outpoint and gives what a read of the overlay would: the memo borrows the
/// overlay, so nothing can change it before the memo's last read, and `UpdateCoins` (`v:2767`),
/// the first change, comes after. With xcheck ([`SerialCore::set_xcheck`]) every read is compared
/// with a direct read of the overlay at the outpoint asked for, and a difference is fatal.
///
/// [`SerialCore::set_xcheck`]: super::SerialCore::set_xcheck
pub(super) struct TxCoins<'v, E: KvEngine> {
    overlay: &'v BlockOverlay<'v, E>,
    inputs: &'v [TxIn],
    /// The coins of the first inputs, up to and with the first that has none: `CheckTxInputs`
    /// rejects there, so no later input is read.
    coins: &'v [Option<CoinRef<'v>>],
    /// The input after the last one read: reads in input order find theirs at once.
    next: Cell<usize>,
    xcheck: bool,
    /// Planted L1: a read of input `i` ≥ 1 answers input `i − 1`'s coin.
    #[cfg(feature = "planted-bugs")]
    skew: bool,
}

impl<'v, E: KvEngine> TxCoins<'v, E> {
    /// The memo of `inputs`, the first `coins.len()` of which resolved to `coins`.
    fn new(
        overlay: &'v BlockOverlay<'v, E>,
        inputs: &'v [TxIn],
        coins: &'v [Option<CoinRef<'v>>],
        xcheck: bool,
    ) -> Self {
        debug_assert!(coins.len() <= inputs.len(), "a coin for each resolved input");
        TxCoins {
            overlay,
            inputs,
            coins,
            next: Cell::new(0),
            xcheck,
            #[cfg(feature = "planted-bugs")]
            skew: false,
        }
    }

    /// A coinbase's memo: it spends no coin.
    pub(super) fn none(overlay: &'v BlockOverlay<'v, E>) -> Self {
        Self::new(overlay, &[], &[], false)
    }

    /// Resolves the inputs of `tx`, not a coinbase, through `overlay` into `arena`, in order,
    /// stopping after the first with no coin.
    ///
    /// # Errors
    ///
    /// A failed read of the database ([`read_failed`]).
    pub(super) fn resolve(
        overlay: &'v BlockOverlay<'v, E>,
        tx: &'v Tx,
        arena: &'v Bump,
        xcheck: bool,
    ) -> Result<Self, Fatal> {
        let inputs = tx.inputs();
        let coins = arena.alloc_slice_fill_copy(inputs.len(), None);
        let mut resolved = 0;
        for (coin, input) in coins.iter_mut().zip(inputs) {
            *coin = overlay.coin(&input.prevout).map_err(read_failed)?;
            resolved += 1;
            if coin.is_none() {
                break;
            }
        }
        Ok(Self::new(overlay, inputs, &coins[..resolved], xcheck))
    }

    /// Arms planted L1 when `on` (the connector's `Planted::L1`).
    #[cfg(feature = "planted-bugs")]
    pub(super) fn planted(self, on: bool) -> Self {
        TxCoins {
            skew: on,
            ..self
        }
    }

    /// `AccessCoin` (`v:2184`, `v:2728`) of the input that spends `outpoint`, which
    /// `CheckTxInputs` found.
    ///
    /// # Errors
    ///
    /// No coin for it, or `outpoint` is no resolved input: a logic error.
    pub(super) fn access(&self, outpoint: &OutPoint) -> Result<CoinRef<'v>, Fatal> {
        self.read(outpoint)?.ok_or_else(|| Fatal::new("AccessCoin: a checked input is spent"))
    }

    /// The coin of the resolved input that spends `outpoint`, compared under xcheck with the
    /// overlay's coin at `outpoint`.
    fn read(&self, outpoint: &OutPoint) -> Result<Option<CoinRef<'v>>, Fatal> {
        let resolved = &self.inputs[..self.coins.len()];
        let next = self.next.get();
        let i = match resolved.get(next) {
            Some(input) if input.prevout == *outpoint => next,
            _ => resolved.iter().position(|input| input.prevout == *outpoint).ok_or_else(|| {
                Fatal::new(format!("TxCoins: {outpoint:?} is not a resolved input"))
            })?,
        };
        self.next.set(i + 1);
        #[cfg(feature = "planted-bugs")]
        let i = if self.skew {
            i.saturating_sub(1)
        } else {
            i
        };
        let coin = self.coins[i];
        if self.xcheck {
            let direct = self.overlay.coin(outpoint).map_err(read_failed)?;
            if direct != coin {
                return Err(Fatal::new(format!(
                    "consensusxcheck: the memo's coin for {outpoint:?} is {coin:?}, the overlay's \
                     {direct:?}"
                )));
            }
        }
        Ok(coin)
    }
}

impl<E: KvEngine> CoinsView for TxCoins<'_, E> {
    type Error = Fatal;

    /// The coin of the resolved input that spends `outpoint`. Any other outpoint is a logic error
    /// (CH3 §6.2 L1), never a missing coin.
    fn coin(&self, outpoint: &OutPoint) -> Result<Option<CoinRef<'_>>, Fatal> {
        self.read(outpoint)
    }

    fn best_block(&self) -> Option<BlockHash> {
        self.overlay.best_block()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dash_consensus::coins::ViewOr;
    use dash_consensus::primitives::{TxHash, TxOut};
    use dash_consensus::script::ScriptFlags;
    use dash_consensus::tx::{check_tx_inputs, transaction_sigop_count};

    use super::*;
    use crate::coins::model::spending;
    use crate::coins::{CoinSrc, CoinsBudget, CoinsCache, OutPointKey, Ratio, Salt};
    use crate::store::{GuardedEngine, ProbeEngine, RecordingSink};

    fn cache() -> CoinsCache<ProbeEngine> {
        let db = Arc::new(GuardedEngine::probe(Arc::new(RecordingSink::default())));
        CoinsCache::new(db, CoinsBudget::new(2 << 20, 0, Ratio::HALF, 0), Salt::fixed(7), None)
    }

    fn key(txid: u8, index: u32) -> OutPointKey {
        OutPointKey::new(&OutPoint {
            hash: TxHash::from([txid; 32]),
            index,
        })
    }

    fn out(value: i64) -> TxOut {
        TxOut {
            value,
            script_pubkey: vec![0x51].into(),
        }
    }

    /// The rejection of a `CheckTxInputs` that failed.
    fn rejection<E: std::fmt::Debug>(
        result: Result<i64, ViewOr<E, dash_consensus::TxValidationError>>,
    ) -> dash_consensus::TxValidationError {
        match result {
            Err(ViewOr::Reject(e)) => e,
            other => panic!("expected a rejection, got {other:?}"),
        }
    }

    /// Three coins: two in the tip at heights 10 and 20, and one the block created (the child's).
    fn fixture(cache: &mut CoinsCache<ProbeEngine>) -> BlockOverlay<'_, ProbeEngine> {
        for (k, value, height) in [(key(1, 0), 5, 10), (key(1, 1), 7, 20)] {
            let src = CoinSrc::Output {
                out: &out(value),
                height,
                coinbase: false,
            };
            cache.add(&k, src, false).unwrap();
        }
        let mut overlay = BlockOverlay::new(cache);
        let src = CoinSrc::Output {
            out: &out(11),
            height: 30,
            coinbase: false,
        };
        overlay.add(&key(2, 0), src, false).unwrap();
        overlay
    }

    #[test]
    fn reads_in_any_order_give_the_overlay_s_coins() {
        let mut cache = cache();
        let overlay = fixture(&mut cache);
        let tx = spending(&[key(1, 1), key(2, 0), key(1, 0)]);
        let arena = Bump::new();
        for xcheck in [false, true] {
            let coins = TxCoins::resolve(&overlay, &tx, &arena, xcheck).unwrap();
            for i in [0, 1, 2, 2, 0, 1, 1, 0] {
                let prevout = &tx.inputs()[i].prevout;
                let direct = overlay.coin(prevout).unwrap();
                assert!(direct.is_some());
                assert_eq!(coins.coin(prevout).unwrap(), direct, "input {i}");
                assert_eq!(coins.access(prevout).ok(), direct, "input {i}'s AccessCoin");
            }
            assert_eq!(check_tx_inputs(&tx, &coins, 100), Ok(5 + 7 + 11 - 1));
            assert_eq!(
                transaction_sigop_count(&tx, &coins, ScriptFlags::P2SH).unwrap(),
                transaction_sigop_count(&tx, &overlay, ScriptFlags::P2SH).unwrap()
            );
        }
    }

    #[test]
    fn the_memo_ends_at_the_first_input_without_a_coin() {
        let mut cache = cache();
        let overlay = fixture(&mut cache);
        let tx = spending(&[key(1, 0), key(9, 0), key(1, 1)]);
        let arena = Bump::new();
        let coins = TxCoins::resolve(&overlay, &tx, &arena, true).unwrap();
        assert_eq!(coins.coins.len(), 2);
        assert_eq!(
            rejection(check_tx_inputs(&tx, &coins, 100)),
            rejection(check_tx_inputs(&tx, &overlay, 100))
        );
        let missing = &tx.inputs()[1].prevout;
        assert_eq!(coins.coin(missing).unwrap(), None);
        assert_eq!(coins.access(missing).unwrap_err().0, "AccessCoin: a checked input is spent");
        // The input after it was never read.
        let after = &tx.inputs()[2].prevout;
        assert!(coins.coin(after).unwrap_err().0.contains("not a resolved input"));
        assert!(coins.access(after).unwrap_err().0.contains("not a resolved input"));
    }

    #[test]
    fn an_outpoint_the_transaction_does_not_spend_is_a_logic_error() {
        let mut cache = cache();
        let overlay = fixture(&mut cache);
        let tx = spending(&[key(1, 0)]);
        let arena = Bump::new();
        let coins = TxCoins::resolve(&overlay, &tx, &arena, false).unwrap();
        let other = key(1, 1).outpoint();
        assert!(overlay.coin(&other).unwrap().is_some());
        assert!(coins.coin(&other).unwrap_err().0.contains("not a resolved input"));
    }

    #[test]
    fn a_coinbase_has_no_coins() {
        let mut cache = cache();
        let overlay = fixture(&mut cache);
        // The null prevout makes it a coinbase.
        let tx = spending(&[key(0, u32::MAX)]);
        assert!(tx.is_coinbase());
        let coins = TxCoins::none(&overlay);
        assert_eq!(
            transaction_sigop_count(&tx, &coins, ScriptFlags::P2SH).unwrap(),
            tx.legacy_sigop_count()
        );
        assert!(coins.coin(&tx.inputs()[0].prevout).is_err());
    }

    /// xcheck's positive control: a memo that answers an input with another input's coin (a
    /// wrong outpoint-to-slot mapping, or a stale slot) is caught on both read paths, keyed by the
    /// outpoint asked for, and goes unnoticed without xcheck.
    #[test]
    fn xcheck_catches_a_memo_coin_that_is_not_the_overlay_s() {
        let mut cache = cache();
        let overlay = fixture(&mut cache);
        let tx = spending(&[key(1, 0), key(1, 1)]);
        let (first, second) = (&tx.inputs()[0].prevout, &tx.inputs()[1].prevout);
        let coin = overlay.coin(first).unwrap();
        assert_ne!(coin, overlay.coin(second).unwrap());
        // Input 1's slot holds input 0's coin.
        let skewed = [coin, coin];
        for xcheck in [false, true] {
            let coins = TxCoins::new(&overlay, tx.inputs(), &skewed, xcheck);
            let (by_view, by_access) = (coins.coin(second), coins.access(second));
            if xcheck {
                for e in [by_view.unwrap_err(), by_access.unwrap_err()] {
                    assert!(e.0.starts_with("consensusxcheck: the memo's coin for"), "{e}");
                }
            } else {
                assert_eq!(by_view.unwrap(), coin);
                assert_eq!(by_access.ok(), coin);
            }
            assert_eq!(coins.access(first).ok(), coin, "input 0's coin is the overlay's");
        }
    }
}
