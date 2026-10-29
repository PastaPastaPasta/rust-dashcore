//! Script checks with Core's caches (`validation.cpp`: `CScriptCheck` `:2106`, `CheckInputScripts`
//! `:2153-2238`) and what `ConnectBlock` reports for them in each mode (`:2649`, `:2749-2760`,
//! `:2778-2780`).
//!
//! CS1 provides the pure parts: [`InputCheck`], the serial [`check_input_scripts`] with Core's
//! failure classification, and [`TxSigData`]. This module adds the signature cache (through
//! [`CachingVerifier`]), the script-execution cache, and the queue: with a parallel [`Control`]
//! the inputs become [`ScriptCheck`]s on the queue, exactly where Core fills `pvChecks`.
//!
//! For `ConnectBlock` (CH3 D43), this module's [`check_input_scripts`](self::check_input_scripts)
//! comes in two halves, [`exec_cache_probe`] and then, on a miss, [`check_input_scripts_unprobed`],
//! so that the caller reads and copies the spent coins only after the probe, as Core does.
//!
//! [`check_input_scripts`]: dash_consensus::script::check_input_scripts

use std::sync::OnceLock;

use dash_consensus::Tx;
use dash_consensus::script::{
    CheckInputsError, DirectVerifier, InputCheck, InputScriptFailure, ScriptError, ScriptFlags,
    ScriptPolicy, TxSigData,
};
use dash_consensus::validation::{BlockValidationError, BlockValidationResult};

use super::{Check, Control};
use crate::sigcache::{CacheEntry, CachingVerifier, ScriptCaches, SigCache};

/// Core's `CScriptCheck`: one input's check as the queue runs it, verifying signatures through
/// the signature cache (storing or not, Core's `cacheStore`) or, with no cache, directly.
#[derive(Clone, Copy)]
pub struct ScriptCheck<'s> {
    input: InputCheck<'s>,
    sigcache: Option<&'s SigCache>,
    store: bool,
}

impl<'s> ScriptCheck<'s> {
    pub fn new(input: InputCheck<'s>, sigcache: Option<&'s SigCache>, store: bool) -> Self {
        Self {
            input,
            sigcache,
            store,
        }
    }

    // Core: validation.cpp:2106 CScriptCheck::operator()
    pub fn run(&self) -> Result<(), ScriptError> {
        match self.sigcache {
            Some(cache) => self.input.run(&CachingVerifier::new(cache, self.store)),
            None => self.input.run(&DirectVerifier),
        }
    }
}

impl Check for ScriptCheck<'_> {
    fn check(self) -> bool {
        self.run().is_ok()
    }
}

/// A transaction and its signature-hash data, bound together (D24): Core's `ptxTo` and
/// `txsdata[i]`, one slot of the caller's per-block arena.
///
/// The data can only be built from the slot's own transaction, so the scripts
/// [`check_input_scripts`] runs and the script-execution entry it reads and stores always
/// describe the same transaction, as in Core, where `CScriptCheck` builds its checker from its own
/// `ptxTo`. A slot is built from its transaction alone:
///
/// ```
/// # use std::sync::OnceLock;
/// # use dash_consensus::Tx;
/// # use dash_consensus::script::TxSigData;
/// # use dashd_chainstate::checkqueue::script::TxSlot;
/// fn bind<'s>(a: &'s Tx, b: &'s Tx) -> TxSlot<'s> {
///     // Another transaction's data can be built, but no slot accepts it.
///     let _a = OnceLock::from(TxSigData::new(a.raw()));
///     TxSlot::new(b)
/// }
/// ```
///
/// and there is no way to fill one from another transaction (the example above keeps this one's
/// imports and calls valid, so it fails only on the private fields):
///
/// ```compile_fail,E0451
/// # use std::sync::OnceLock;
/// # use dash_consensus::Tx;
/// # use dash_consensus::script::TxSigData;
/// # use dashd_chainstate::checkqueue::script::TxSlot;
/// fn mix<'s>(a: &'s Tx, b: &'s Tx) -> TxSlot<'s> {
///     TxSlot { tx: b, data: OnceLock::from(TxSigData::new(a.raw())) }
/// }
/// ```
pub struct TxSlot<'s> {
    tx: &'s Tx,
    /// Built on the first cache miss, where Core calls `txdata.Init`, and borrowed by the
    /// transaction's queued checks.
    data: OnceLock<TxSigData<'s>>,
}

impl<'s> TxSlot<'s> {
    pub fn new(tx: &'s Tx) -> Self {
        Self {
            tx,
            data: OnceLock::new(),
        }
    }

    pub fn tx(&self) -> &'s Tx {
        self.tx
    }

    /// Whether the signature-hash data was built (a cache hit or a coinbase builds none).
    pub fn is_initialised(&self) -> bool {
        self.data.get().is_some()
    }

    // Core: validation.cpp:2179 CheckInputScripts (txdata.Init)
    fn data(&self) -> &TxSigData<'s> {
        self.data.get_or_init(|| TxSigData::new(self.tx.raw()))
    }
}

/// What `CheckInputScripts` reads besides the transaction: its flags and policy masks, the caches
/// (`None` turns both off, for tests and twins), and Core's `cacheSigStore` and
/// `cacheFullScriptStore`.
///
/// `ConnectBlock` passes `fJustCheck` for both store flags: when it connects a block it consults
/// the caches without filling them (and soft-erases what it hits); `TestBlockValidity` stores.
/// The mempool stores signatures, then the transaction under the tip's block flags
/// (`CheckInputsFromMempoolAndCache`).
#[derive(Clone, Copy, Debug)]
pub struct ScriptOptions<'c> {
    pub flags: ScriptFlags,
    pub policy: ScriptPolicy,
    pub caches: Option<&'c ScriptCaches>,
    pub cache_sig_store: bool,
    pub cache_full_script_store: bool,
}

// Core: validation.cpp:2153 CheckInputScripts
/// Core's `CheckInputScripts` with its caches: `Ok` for a coinbase, or when the
/// script-execution cache holds the transaction under `flags` (soft-erased unless
/// `cache_full_script_store`); otherwise every input is checked. It is [`exec_cache_probe`], then
/// [`check_input_scripts_unprobed`] on its miss, which carries the entry: it is hashed once.
///
/// - With a parallel `queue` (Core's `pvChecks`), every input becomes a [`ScriptCheck`] on it and
///   this returns `Ok`; the queue's verdict is the caller's, and nothing is inserted in the
///   script-execution cache.
/// - Otherwise the inputs run here in order and the first failure is returned, classified by
///   CS1's [`check_input_scripts`] (its re-run without the policy's non-mandatory flags goes
///   through the same caching verifier, as Core's `check2` does). When all pass and
///   `cache_full_script_store`, the transaction is inserted.
///
/// `slot` is the transaction with its signature-hash data ([`TxSlot`]): the data is built here,
/// after a cache miss, where Core calls `txdata.Init`, and the queued checks borrow it.
/// `spent_script_pubkey(i)` is the script of the coin input `i` spends; the caller keeps the coins
/// alive for `'s` (Core copies each `CTxOut` into its check, because `UpdateCoins` spends them
/// before the queue runs).
///
/// # Errors
///
/// The first failing input (serial mode only), or [`CheckInputsError::Flags`] when `flags` sets
/// `CLEANSTACK` without `P2SH` (Core asserts).
///
/// [`check_input_scripts`]: dash_consensus::script::check_input_scripts
pub fn check_input_scripts<'s>(
    slot: &'s TxSlot<'s>,
    spent_script_pubkey: impl Fn(usize) -> &'s [u8],
    opts: &ScriptOptions<'s>,
    queue: Option<&Control<'_, 's, ScriptCheck<'s>>>,
) -> Result<(), CheckInputsError> {
    match exec_cache_probe(slot, opts) {
        Some(miss) => check_input_scripts_unprobed(miss, spent_script_pubkey, opts, queue),
        None => Ok(()),
    }
}

/// [`exec_cache_probe`]'s miss: the slot it probed, under the probe's flags, with its
/// script-execution entry (Core's `hashCacheEntry`) when the caches are on.
/// [`check_input_scripts_unprobed`] runs that slot's scripts and inserts that entry, so the two
/// always describe the same transaction and flags (D24), as Core's one local entry does. A miss
/// is used once.
pub struct ExecMiss<'s> {
    slot: &'s TxSlot<'s>,
    flags: ScriptFlags,
    entry: Option<(&'s ScriptCaches, CacheEntry)>,
}

/// The first half of [`check_input_scripts`] (CH3 D43): `None` when `CheckInputScripts` is done
/// before it reads a coin, because the transaction is a coinbase or the script-execution cache
/// holds it under `opts.flags`. A hit is soft-erased unless `opts.cache_full_script_store`
/// (Core's `contains(entry, !cacheFullScriptStore)`), once per call. On a miss, the caller reads
/// the spent coins and passes the [`ExecMiss`] to [`check_input_scripts_unprobed`], which does not
/// probe again, so a miss followed by the checks erases nothing and a hit is erased once.
///
/// `ConnectBlock` probes first so that a hit (a transaction the mempool validated) costs no coin
/// lookup and no script copy: Core probes before its first `AccessCoin` (`v:2184`).
// Core: validation.cpp:2159, :2165-2176 CheckInputScripts
pub fn exec_cache_probe<'s>(
    slot: &'s TxSlot<'s>,
    opts: &ScriptOptions<'s>,
) -> Option<ExecMiss<'s>> {
    let tx = slot.tx;
    if tx.is_coinbase() {
        return None;
    }
    let entry = exec_entry(tx, opts);
    (!exec_hit(entry, opts)).then_some(ExecMiss {
        slot,
        flags: opts.flags,
        entry,
    })
}

/// The caches and the transaction's script-execution entry, when the caches are on (Core's
/// `hashCacheEntry`).
// Core: validation.cpp:2170-2172 CheckInputScripts
#[inline]
fn exec_entry<'c>(tx: &Tx, opts: &ScriptOptions<'c>) -> Option<(&'c ScriptCaches, CacheEntry)> {
    opts.caches.map(|c| (c, c.exec.entry(tx.txid().as_bytes(), opts.flags)))
}

/// First check whether the scripts were already run under the same flags. This assumes the
/// spent coins are right: the txid commits to the outpoints, and an outpoint to its coin.
// Core: validation.cpp:2174-2176 CheckInputScripts
#[inline]
fn exec_hit(entry: Option<(&ScriptCaches, CacheEntry)>, opts: &ScriptOptions<'_>) -> bool {
    entry.is_some_and(|(caches, entry)| {
        caches.exec.cache().contains(entry, !opts.cache_full_script_store)
    })
}

/// The second half of [`check_input_scripts`] (CH3 D43), for [`exec_cache_probe`]'s `miss`: the
/// miss's slot's signature-hash data, then every input's check, queued on a parallel `queue` or
/// run here in order, then the insert of the miss's entry, only when the checks ran here and
/// `opts.cache_full_script_store` (Core's `cacheFullScriptStore && !pvChecks`). It never consults
/// the script-execution cache, and does not hash the entry again. `opts.flags` are the probe's.
///
/// # Errors
///
/// As [`check_input_scripts`].
// Core: validation.cpp:2178-2232 CheckInputScripts
pub fn check_input_scripts_unprobed<'s>(
    miss: ExecMiss<'s>,
    spent_script_pubkey: impl Fn(usize) -> &'s [u8],
    opts: &ScriptOptions<'s>,
    queue: Option<&Control<'_, 's, ScriptCheck<'s>>>,
) -> Result<(), CheckInputsError> {
    let ExecMiss {
        slot,
        flags,
        entry,
    } = miss;
    debug_assert_eq!(flags, opts.flags, "a miss is checked under the flags it was probed with");
    let tx = slot.tx;
    let txdata = slot.data();
    let sigcache = opts.caches.map(|c| &c.sig);
    match queue.filter(|q| q.is_parallel()) {
        Some(queue) => {
            let inputs = tx.inputs().len();
            let mut checks = Vec::with_capacity(inputs);
            for n_in in 0..inputs {
                let input = InputCheck::new(txdata, n_in, spent_script_pubkey(n_in), opts.flags)?;
                checks.push(ScriptCheck::new(input, sigcache, opts.cache_sig_store));
            }
            queue.add(checks);
        }
        None => {
            match sigcache {
                Some(cache) => dash_consensus::script::check_input_scripts(
                    txdata,
                    spent_script_pubkey,
                    opts.flags,
                    opts.policy,
                    &CachingVerifier::new(cache, opts.cache_sig_store),
                ),
                None => dash_consensus::script::check_input_scripts(
                    txdata,
                    spent_script_pubkey,
                    opts.flags,
                    opts.policy,
                    &DirectVerifier,
                ),
            }?;
            if opts.cache_full_script_store
                && let Some((caches, entry)) = entry
            {
                // Every script ran and passed, and the caller asked to cache the result.
                caches.exec.cache().insert(entry);
            }
        }
    }
    Ok(())
}

/// `ConnectBlock`'s report when a transaction's `CheckInputScripts` fails (serial mode, Core's
/// `-par=1`): `BLOCK_CONSENSUS` with the transaction's reject reason and debug message, which
/// for a script failure is `mandatory-script-verify-flag-failed (<error>)` and no debug message.
// Core: validation.cpp:2752-2758 ConnectBlock
pub fn input_scripts_failed(failure: InputScriptFailure) -> BlockValidationError {
    failure.into_tx_error().into_block(BlockValidationResult::Consensus)
}

/// `ConnectBlock`'s report when the check queue's verdict is false (parallel mode):
/// `BLOCK_CONSENSUS`, `block-validation-failed`, no debug message, whichever input failed.
// Core: validation.cpp:2778-2780 ConnectBlock
pub fn check_queue_failed() -> BlockValidationError {
    BlockValidationError::new(BlockValidationResult::Consensus, "block-validation-failed")
}

#[cfg(test)]
mod tests;
