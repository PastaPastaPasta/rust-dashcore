//! Mixing rounds: how many times a wallet's coin has been mixed, and whether it is fully mixed
//! (Core's `src/wallet/coinjoin.cpp`). The wallet is [`RoundsWallet`]; key-wallet and the
//! desktop wallet implement it.

use std::collections::BTreeMap;

use dash_primitives::{OutPoint, Transaction, TxHash, TxIn};

use crate::denom;

// Core: coinjoin/options.h:22 MAX_COINJOIN_ROUNDS
/// The most rounds a coin is counted to (before the random extra rounds).
pub const MAX_COINJOIN_ROUNDS: i32 = 16;
/// `DEFAULT_COINJOIN_ROUNDS`.
pub const DEFAULT_COINJOIN_ROUNDS: i32 = 4;
/// `COINJOIN_RANDOM_ROUNDS`: the most extra rounds a fully mixed coin may get.
pub const COINJOIN_RANDOM_ROUNDS: i32 = 3;

/// Rounds with Core's sentinels (`GetRealOutpointCoinJoinRounds`).
pub mod sentinel {
    /// The transaction is unknown to the wallet (not memoised).
    pub const UNKNOWN_TX: i32 = -1;
    /// The coin is not a denomination.
    pub const NOT_DENOMINATED: i32 = -2;
    /// The coin is a collateral amount.
    pub const COLLATERAL: i32 = -3;
    /// The output index is out of range.
    pub const OUT_OF_RANGE: i32 = -4;
    /// The computation of this coin is in progress (a cycle), Core's initial cache value.
    pub const IN_PROGRESS: i32 = -10;
}

/// The client's round options (`CCoinJoinClientOptions::GetRounds()`/`GetRandomRounds()`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RoundsOptions {
    /// `nCoinJoinRounds`.
    pub rounds: i32,
    /// `nCoinJoinRandomRounds`.
    pub random_rounds: i32,
}

impl Default for RoundsOptions {
    fn default() -> Self {
        Self {
            rounds: DEFAULT_COINJOIN_ROUNDS,
            random_rounds: COINJOIN_RANDOM_ROUNDS,
        }
    }
}

/// The wallet as the round count reads it.
pub trait RoundsWallet {
    /// `GetWalletTx(txid)->tx`.
    fn wallet_tx(&self, txid: &TxHash) -> Option<&Transaction>;

    /// `InputIsMine(txin)`.
    fn input_is_mine(&self, txin: &TxIn) -> bool;

    /// `CachedTxGetDebit(wtx, ISMINE_SPENDABLE) == CachedTxGetCredit(wtx, ISMINE_SPENDABLE)` for
    /// the wallet transaction `txid`: the wallet's coins in it all went to the wallet, no fee.
    fn debit_equals_credit(&self, txid: &TxHash) -> bool;
}

/// Core's `mapOutpointRoundsCache`: the rounds computed so far. Clear it
/// ([`RoundsCache::clear`], Core's `ClearCoinJoinRoundsCache`) when the wallet's transactions
/// change.
#[derive(Clone, Debug, Default)]
pub struct RoundsCache {
    rounds: BTreeMap<OutPoint, i32>,
}

impl RoundsCache {
    /// `ClearCoinJoinRoundsCache`.
    pub fn clear(&mut self) {
        self.rounds.clear();
    }

    // Core: wallet/coinjoin.cpp:261 CWallet::GetRealOutpointCoinJoinRounds
    /// `GetRealOutpointCoinJoinRounds(outpoint)`: how many mixing transactions lead to the coin
    /// through the wallet's own denominated coins, at most `MAX_COINJOIN_ROUNDS +
    /// random_rounds`, or a [`sentinel`].
    pub fn real_rounds<W: RoundsWallet + ?Sized>(
        &mut self,
        wallet: &W,
        options: RoundsOptions,
        outpoint: &OutPoint,
    ) -> i32 {
        self.real_rounds_at(wallet, options, outpoint, 0)
    }

    fn real_rounds_at<W: RoundsWallet + ?Sized>(
        &mut self,
        wallet: &W,
        options: RoundsOptions,
        outpoint: &OutPoint,
        depth: i32,
    ) -> i32 {
        let rounds_max = MAX_COINJOIN_ROUNDS + options.random_rounds;
        if depth >= rounds_max {
            return rounds_max - 1;
        }
        if let Some(&cached) = self.rounds.get(outpoint) {
            return cached;
        }
        self.rounds.insert(*outpoint, sentinel::IN_PROGRESS);

        let Some(tx) = wallet.wallet_tx(&outpoint.hash) else {
            // The wallet may learn of the transaction later and nothing would invalidate it.
            self.rounds.remove(outpoint);
            return sentinel::UNKNOWN_TX;
        };
        let rounds = self.compute(wallet, options, outpoint, tx, depth, rounds_max);
        self.rounds.insert(*outpoint, rounds);
        rounds
    }

    fn compute<W: RoundsWallet + ?Sized>(
        &mut self,
        wallet: &W,
        options: RoundsOptions,
        outpoint: &OutPoint,
        tx: &Transaction,
        depth: i32,
        rounds_max: i32,
    ) -> i32 {
        let Some(out) = usize::try_from(outpoint.index).ok().and_then(|i| tx.outputs.get(i)) else {
            return sentinel::OUT_OF_RANGE;
        };
        if denom::is_collateral_amount(out.value) {
            return sentinel::COLLATERAL;
        }
        if !denom::is_denominated_amount(out.value) {
            return sentinel::NOT_DENOMINATED;
        }
        // A denominated coin next to a non-denominated output, or from a transaction with a fee,
        // starts at 0.
        if tx.outputs.iter().any(|o| !denom::is_denominated_amount(o.value)) {
            return 0;
        }
        if !wallet.debit_equals_credit(&outpoint.hash) {
            return 0;
        }

        let output_denom = denom::amount_to_denomination(out.value);
        let mut shortest = None;
        for txin in &tx.inputs {
            if !wallet.input_is_mine(txin) {
                continue;
            }
            // Post-V24: an own input at another denomination makes this a promotion or demotion
            // output, which starts over. A wallet adds at most one entry per mixing transaction,
            // so deciding on the first such input is safe.
            let prev_denom = wallet
                .wallet_tx(&txin.prevout.hash)
                .and_then(|prev| {
                    usize::try_from(txin.prevout.index).ok().and_then(|i| prev.outputs.get(i))
                })
                .map(|prev_out| denom::amount_to_denomination(prev_out.value));
            if prev_denom.is_some_and(|d| d != output_denom) {
                return 0;
            }
            let n = self.real_rounds_at(wallet, options, &txin.prevout, depth + 1);
            if n >= 0 && shortest.is_none_or(|s| n < s) {
                shortest = Some(n);
            }
        }
        match shortest {
            Some(s) if s >= rounds_max - 1 => rounds_max,
            Some(s) => s + 1,
            // The first coin of its chain.
            None => 0,
        }
    }

    // Core: wallet/coinjoin.cpp:372 CWallet::GetCappedOutpointCoinJoinRounds
    /// `GetCappedOutpointCoinJoinRounds`: the real rounds, at most `options.rounds`.
    pub fn capped_rounds<W: RoundsWallet + ?Sized>(
        &mut self,
        wallet: &W,
        options: RoundsOptions,
        outpoint: &OutPoint,
    ) -> i32 {
        self.real_rounds(wallet, options, outpoint).min(options.rounds)
    }

    // Core: wallet/coinjoin.cpp:405 CWallet::IsFullyMixed
    /// `IsFullyMixed`: at least `options.rounds` rounds, and then mixed again with probability
    /// one half per round up to `options.rounds + options.random_rounds`, decided by the salted
    /// hash of the coin ([`stops_mixing`]).
    pub fn is_fully_mixed<W: RoundsWallet + ?Sized>(
        &mut self,
        wallet: &W,
        options: RoundsOptions,
        salt: &[u8; 32],
        outpoint: &OutPoint,
    ) -> bool {
        let rounds = self.real_rounds(wallet, options, outpoint);
        if rounds < options.rounds {
            return false;
        }
        rounds >= options.rounds.saturating_add(options.random_rounds)
            || stops_mixing(outpoint, salt)
    }
}

/// The coin's coin flip in `IsFullyMixed`: the first 8 bytes of `SHA256(ser(outpoint) ||
/// salt)`, little-endian, are odd. `salt` is the wallet's CoinJoin salt as Core stores the
/// `uint256` (its internal bytes).
pub fn stops_mixing(outpoint: &OutPoint, salt: &[u8; 32]) -> bool {
    let mut data = [0u8; 68];
    data[..32].copy_from_slice(outpoint.hash.as_bytes());
    data[32..36].copy_from_slice(&outpoint.index.to_le_bytes());
    data[36..].copy_from_slice(salt);
    let hash = dash_consensus::hash::sha256(&data);
    let mut first = [0u8; 8];
    first.copy_from_slice(&hash[..8]);
    u64::from_le_bytes(first) % 2 != 0
}
