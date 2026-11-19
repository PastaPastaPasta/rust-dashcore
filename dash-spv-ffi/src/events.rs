//! Translates the SPV client's events into calls to the C callback tables.
//!
//! The `#[repr(C)]` tables and callback types stay in `callbacks.rs`, where cbindgen reads them.

use crate::{
    dash_spv_ffi_sync_progress_destroy, FFIAccountBalance, FFIBlockNeeded, FFIBroadcastStatus,
    FFIChainlockedTxid, FFIClientErrorCallback, FFIDerivedAddress, FFIEventCallbacks,
    FFIExtEventCallbacks, FFINetworkEventCallbacks, FFIOutPoint, FFIProgressCallback,
    FFISyncEventCallbacks, FFISyncProgress, FFIWalletEventCallbacks,
};
use dash_spv::network::NetworkEvent;
use dash_spv::EventHandler;
use dash_spv::{SyncEvent, SyncProgress};
use hex_conservative::DisplayHex;
use key_wallet_ffi::managed_account::FFITransactionRecord;
use key_wallet_ffi::types::FFIBalance;
use key_wallet_manager::WalletEvent;
use std::ffi::CString;
use std::ptr;
use std::sync::{Arc, PoisonError, RwLock};

/// The extension tables in effect.
pub(crate) struct ExtTables {
    pub(crate) ext: FFIExtEventCallbacks,
}

/// The event handler of a client: the frozen tables passed to `dash_spv_ffi_client_new`, and
/// the extension tables a caller may replace at any time.
pub(crate) struct FfiEventSink {
    frozen: FFIEventCallbacks,
    ext: RwLock<Arc<ExtTables>>,
}

impl FfiEventSink {
    pub(crate) fn new(frozen: FFIEventCallbacks) -> Self {
        Self {
            frozen,
            ext: RwLock::new(Arc::new(ExtTables {
                ext: FFIExtEventCallbacks::default(),
            })),
        }
    }

    /// Returns the extension tables in effect. The lock is released before it returns, so a
    /// callback may replace the tables while it runs.
    pub(crate) fn ext(&self) -> Arc<ExtTables> {
        Arc::clone(&self.ext.read().unwrap_or_else(PoisonError::into_inner))
    }

    pub(crate) fn set_ext(&self, ext: FFIExtEventCallbacks) {
        *self.ext.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(ExtTables {
            ext,
        });
    }

    #[cfg_attr(not(test), expect(dead_code, reason = "no event maps to it yet"))]
    pub(crate) fn chain_reorganized(&self, fork_height: u32, disconnected: u32, connected: u32) {
        let tables = self.ext();
        if let Some(cb) = tables.ext.on_chain_reorganized {
            cb(fork_height, disconnected, connected, tables.ext.user_data);
        }
    }
}

impl EventHandler for FfiEventSink {
    fn on_sync_event(&self, event: &SyncEvent) {
        self.frozen.on_sync_event(event);
    }

    fn on_network_event(&self, event: &NetworkEvent) {
        self.frozen.on_network_event(event);
    }

    fn on_progress(&self, progress: &SyncProgress) {
        self.frozen.on_progress(progress);
    }

    fn on_wallet_event(&self, event: &WalletEvent) {
        self.frozen.on_wallet_event(event);
    }

    fn on_error(&self, error: &str) {
        self.frozen.on_error(error);
    }
}

impl FFIProgressCallback {
    /// Dispatch a progress update to the callback.
    ///
    /// Creates an FFISyncProgress from the Rust progress, calls the callback,
    /// then cleans up all allocated memory.
    pub fn dispatch(&self, progress: &dash_spv::SyncProgress) {
        if let Some(cb) = self.on_progress {
            // Clone the progress to get an owned SyncProgress for conversion
            let owned_progress = progress.clone();
            let ffi_progress = Box::new(FFISyncProgress::from(owned_progress));
            let ptr = Box::into_raw(ffi_progress);

            // Call the callback
            cb(ptr as *const FFISyncProgress, self.user_data);

            // Clean up the progress and all its nested pointers
            unsafe {
                dash_spv_ffi_sync_progress_destroy(ptr);
            }
        }
    }
}

impl FFISyncEventCallbacks {
    /// Dispatch a SyncEvent to the appropriate callback.
    pub fn dispatch(&self, event: &dash_spv::SyncEvent) {
        use dash_spv::SyncEvent;

        match event {
            SyncEvent::SyncStart {
                identifier,
            } => {
                if let Some(cb) = self.on_sync_start {
                    cb((*identifier).into(), self.user_data);
                }
            }
            SyncEvent::BlockHeadersStored {
                tip_height,
            } => {
                if let Some(cb) = self.on_block_headers_stored {
                    cb(*tip_height, self.user_data);
                }
            }
            SyncEvent::BlockHeaderSyncComplete {
                tip_height,
                ..
            } => {
                if let Some(cb) = self.on_block_header_sync_complete {
                    cb(*tip_height, self.user_data);
                }
            }
            SyncEvent::FilterHeadersStored {
                start_height,
                end_height,
                tip_height,
                ..
            } => {
                if let Some(cb) = self.on_filter_headers_stored {
                    cb(*start_height, *end_height, *tip_height, self.user_data);
                }
            }
            SyncEvent::FilterHeadersSyncComplete {
                tip_height,
            } => {
                if let Some(cb) = self.on_filter_headers_sync_complete {
                    cb(*tip_height, self.user_data);
                }
            }
            SyncEvent::FiltersStored {
                start_height,
                end_height,
            } => {
                if let Some(cb) = self.on_filters_stored {
                    cb(*start_height, *end_height, self.user_data);
                }
            }
            SyncEvent::FiltersSyncComplete {
                tip_height,
            } => {
                if let Some(cb) = self.on_filters_sync_complete {
                    cb(*tip_height, self.user_data);
                }
            }
            SyncEvent::BlocksNeeded {
                blocks,
            } => {
                if let Some(cb) = self.on_blocks_needed {
                    let ffi_blocks: Vec<FFIBlockNeeded> = blocks
                        .keys()
                        .map(|key| FFIBlockNeeded {
                            height: key.height(),
                            hash: *key.hash().as_byte_array(),
                        })
                        .collect();
                    cb(ffi_blocks.as_ptr(), ffi_blocks.len() as u32, self.user_data);
                }
            }
            SyncEvent::BlockProcessed {
                block_hash,
                height,
                new_scripts,
                confirmed_txids,
                ..
            } => {
                if let Some(cb) = self.on_block_processed {
                    let hash_bytes = block_hash.as_byte_array();
                    let txid_bytes: Vec<[u8; 32]> =
                        confirmed_txids.iter().map(|txid| *txid.as_byte_array()).collect();
                    let total_new_scripts: usize = new_scripts.values().map(|v| v.len()).sum();
                    cb(
                        *height,
                        hash_bytes as *const [u8; 32],
                        total_new_scripts as u32,
                        txid_bytes.as_ptr(),
                        txid_bytes.len() as u32,
                        self.user_data,
                    );
                }
            }
            SyncEvent::MasternodeStateUpdated {
                height,
                ..
            } => {
                if let Some(cb) = self.on_masternode_state_updated {
                    cb(*height, self.user_data);
                }
            }
            SyncEvent::ChainLockReceived {
                chain_lock,
                validated,
            } => {
                if let Some(cb) = self.on_chainlock_received {
                    let hash_bytes = chain_lock.block_hash.as_byte_array();
                    let sig_bytes = chain_lock.signature.as_bytes();
                    cb(
                        chain_lock.block_height,
                        hash_bytes as *const [u8; 32],
                        sig_bytes as *const [u8; 96],
                        *validated,
                        self.user_data,
                    );
                }
            }
            SyncEvent::InstantLockReceived {
                instant_lock,
                validated,
            } => {
                if let Some(cb) = self.on_instantlock_received {
                    let txid_bytes = instant_lock.txid.as_byte_array();
                    let serialized = dashcore::consensus::serialize(instant_lock);
                    cb(
                        txid_bytes as *const [u8; 32],
                        serialized.as_ptr(),
                        serialized.len(),
                        *validated,
                        self.user_data,
                    );
                }
            }
            SyncEvent::ManagerError {
                manager,
                error,
            } => {
                if let Some(cb) = self.on_manager_error {
                    let c_error = CString::new(error.as_str()).unwrap_or_default();
                    cb((*manager).into(), c_error.as_ptr(), self.user_data);
                }
            }
            SyncEvent::ForkDetected {
                ..
            }
            | SyncEvent::HeadersBackfillNeeded {
                ..
            } => {}
            SyncEvent::SyncComplete {
                header_tip,
                cycle,
            } => {
                if let Some(cb) = self.on_sync_complete {
                    cb(*header_tip, *cycle, self.user_data);
                }
            }
            SyncEvent::TransactionBroadcastResult {
                txid,
                result,
            } => {
                if let Some(cb) = self.on_transaction_broadcast_result {
                    use dash_spv::BroadcastResult;
                    let txid_bytes = txid.as_byte_array();
                    let (status, relayed_by) = match result {
                        BroadcastResult::Accepted {
                            relayed_by,
                        } => (FFIBroadcastStatus::Accepted, *relayed_by as u32),
                        BroadcastResult::Uncertain => (FFIBroadcastStatus::Uncertain, 0),
                    };
                    cb(txid_bytes as *const [u8; 32], status, relayed_by, self.user_data);
                }
            }
        }
    }
}

impl FFINetworkEventCallbacks {
    /// Dispatch a NetworkEvent to the appropriate callback.
    pub fn dispatch(&self, event: &dash_spv::network::NetworkEvent) {
        use dash_spv::network::NetworkEvent;

        match event {
            NetworkEvent::PeerConnected(address) => {
                if let Some(cb) = self.on_peer_connected {
                    let c_addr = CString::new(address.to_string()).unwrap_or_default();
                    cb(c_addr.as_ptr(), self.user_data);
                }
            }
            NetworkEvent::PeerDisconnected(address) => {
                if let Some(cb) = self.on_peer_disconnected {
                    let c_addr = CString::new(address.to_string()).unwrap_or_default();
                    cb(c_addr.as_ptr(), self.user_data);
                }
            }
            NetworkEvent::PeersUpdated {
                addresses,
                best_height,
            } => {
                if let Some(cb) = self.on_peers_updated {
                    cb(addresses.len() as u32, *best_height, self.user_data);
                }
            }
        }
    }
}

impl FFIClientErrorCallback {
    /// Dispatch a client error to the callback.
    pub fn dispatch(&self, error: &str) {
        if let Some(cb) = self.on_error {
            let c_error = CString::new(error).unwrap_or_default();
            cb(c_error.as_ptr(), self.user_data);
        }
    }
}

impl EventHandler for FFIEventCallbacks {
    fn on_sync_event(&self, event: &SyncEvent) {
        self.sync.dispatch(event);
    }

    fn on_network_event(&self, event: &NetworkEvent) {
        self.network.dispatch(event);
    }

    fn on_progress(&self, progress: &SyncProgress) {
        self.progress.dispatch(progress);
    }

    fn on_wallet_event(&self, event: &WalletEvent) {
        self.wallet.dispatch(event);
    }

    fn on_error(&self, error: &str) {
        self.error.dispatch(error);
    }
}

impl FFIWalletEventCallbacks {
    /// Dispatch a WalletEvent to the appropriate callback.
    pub fn dispatch(&self, event: &WalletEvent) {
        match event {
            WalletEvent::TransactionsSwept {
                wallet_id,
                txids,
                superseded_by,
                // Deliberately not forwarded across the C ABI. #962
                // established that `OnTransactionsSweptCallback` has no
                // safely-degrading insertion point: a C consumer declaring
                // the function pointer by hand keeps compiling against a
                // grown parameter list and reads shifted arguments. Rust
                // consumers of `WalletEvent` get the field directly; the C
                // surface stays byte-for-byte identical until a consumer
                // needs it there, at which point the addition must be a
                // flagged breaking change like #962's.
                winner_mined_height: _,
                released_outpoints,
                balance,
                account_balances,
            } => {
                if let Some(cb) = self.on_transactions_swept {
                    let wallet_id_hex = wallet_id.to_lower_hex_string();
                    let c_wallet_id = CString::new(wallet_id_hex).unwrap_or_default();
                    let raw_txids: Vec<[u8; 32]> =
                        txids.iter().map(|t| t.to_byte_array()).collect();
                    let raw_superseded_by = superseded_by.to_byte_array();
                    let ffi_released_outpoints = FFIOutPoint::from_slice(released_outpoints);
                    let released_outpoints_ptr = if ffi_released_outpoints.is_empty() {
                        ptr::null()
                    } else {
                        ffi_released_outpoints.as_ptr()
                    };
                    let ffi_balance = FFIBalance::from(*balance);
                    let ffi_account_balances = FFIAccountBalance::from_map(account_balances);
                    let account_balances_ptr = if ffi_account_balances.is_empty() {
                        ptr::null()
                    } else {
                        ffi_account_balances.as_ptr()
                    };

                    cb(
                        c_wallet_id.as_ptr(),
                        raw_txids.as_ptr(),
                        raw_txids.len(),
                        &raw_superseded_by as *const [u8; 32],
                        released_outpoints_ptr,
                        ffi_released_outpoints.len(),
                        &ffi_balance as *const FFIBalance,
                        account_balances_ptr,
                        ffi_account_balances.len() as u32,
                        self.user_data,
                    );

                    drop(ffi_account_balances);
                    drop(ffi_released_outpoints);
                } else {
                    // Deliberately loud: every other wallet callback is
                    // additive, so a consumer that leaves this one unset keeps
                    // transactions the wallet has already dropped.
                    tracing::warn!(
                        wallet_id = %wallet_id.to_lower_hex_string(),
                        swept = txids.len(),
                        %superseded_by,
                        "no on_transactions_swept callback set; the consumer will keep \
                         mirroring transactions the wallet removed"
                    );
                }
            }
            WalletEvent::ChainTruncated {
                wallet_id,
                height,
                txids,
                unconfirmed_txids,
                restored_outpoints,
                balance,
                account_balances,
            } => {
                if let Some(cb) = self.on_chain_truncated {
                    let c_wallet_id =
                        CString::new(wallet_id.to_lower_hex_string()).unwrap_or_default();
                    let raw_txids: Vec<[u8; 32]> =
                        txids.iter().map(|t| t.to_byte_array()).collect();
                    let txids_ptr = if raw_txids.is_empty() {
                        ptr::null()
                    } else {
                        raw_txids.as_ptr()
                    };
                    let raw_unconfirmed_txids: Vec<[u8; 32]> =
                        unconfirmed_txids.iter().map(|t| t.to_byte_array()).collect();
                    let unconfirmed_txids_ptr = if raw_unconfirmed_txids.is_empty() {
                        ptr::null()
                    } else {
                        raw_unconfirmed_txids.as_ptr()
                    };
                    let ffi_restored_outpoints = FFIOutPoint::from_slice(restored_outpoints);
                    let restored_outpoints_ptr = if ffi_restored_outpoints.is_empty() {
                        ptr::null()
                    } else {
                        ffi_restored_outpoints.as_ptr()
                    };
                    let ffi_balance = FFIBalance::from(*balance);
                    let ffi_account_balances = FFIAccountBalance::from_map(account_balances);
                    let account_balances_ptr = if ffi_account_balances.is_empty() {
                        ptr::null()
                    } else {
                        ffi_account_balances.as_ptr()
                    };

                    cb(
                        c_wallet_id.as_ptr(),
                        *height,
                        txids_ptr,
                        raw_txids.len(),
                        unconfirmed_txids_ptr,
                        raw_unconfirmed_txids.len(),
                        restored_outpoints_ptr,
                        ffi_restored_outpoints.len(),
                        &ffi_balance as *const FFIBalance,
                        account_balances_ptr,
                        ffi_account_balances.len() as u32,
                        self.user_data,
                    );
                } else {
                    tracing::warn!(
                        wallet_id = %wallet_id.to_lower_hex_string(),
                        height,
                        "no on_chain_truncated callback set; the consumer will keep \
                         mirroring transactions the wallet dropped after a fork"
                    );
                }
            }
            WalletEvent::TransactionDetected {
                wallet_id,
                record,
                balance,
                account_balances,
                addresses_derived,
            } => {
                if let Some(cb) = self.on_transaction_detected {
                    let wallet_id_hex = wallet_id.to_lower_hex_string();
                    let c_wallet_id = CString::new(wallet_id_hex).unwrap_or_default();
                    let ffi_record = FFITransactionRecord::from(record.as_ref());
                    let ffi_balance = FFIBalance::from(*balance);
                    let ffi_account_balances = FFIAccountBalance::from_map(account_balances);
                    let ffi_addresses_derived = FFIDerivedAddress::from_slice(addresses_derived);
                    let account_balances_ptr = if ffi_account_balances.is_empty() {
                        ptr::null()
                    } else {
                        ffi_account_balances.as_ptr()
                    };
                    let addresses_derived_ptr = if ffi_addresses_derived.is_empty() {
                        ptr::null()
                    } else {
                        ffi_addresses_derived.as_ptr()
                    };

                    cb(
                        c_wallet_id.as_ptr(),
                        &ffi_record as *const FFITransactionRecord,
                        &ffi_balance as *const FFIBalance,
                        account_balances_ptr,
                        ffi_account_balances.len() as u32,
                        addresses_derived_ptr,
                        ffi_addresses_derived.len() as u32,
                        self.user_data,
                    );

                    drop(ffi_account_balances);
                    drop(ffi_addresses_derived);
                }
            }
            WalletEvent::TransactionInstantLocked {
                wallet_id,
                txid,
                instant_lock,
                balance,
                account_balances,
            } => {
                if let Some(cb) = self.on_transaction_instant_locked {
                    let wallet_id_hex = wallet_id.to_lower_hex_string();
                    let c_wallet_id = CString::new(wallet_id_hex).unwrap_or_default();
                    let txid_bytes = *txid.as_byte_array();
                    let islock_bytes = dashcore::consensus::serialize(instant_lock);
                    let ffi_balance = FFIBalance::from(*balance);
                    let ffi_account_balances = FFIAccountBalance::from_map(account_balances);
                    let account_balances_ptr = if ffi_account_balances.is_empty() {
                        ptr::null()
                    } else {
                        ffi_account_balances.as_ptr()
                    };

                    cb(
                        c_wallet_id.as_ptr(),
                        &txid_bytes as *const [u8; 32],
                        islock_bytes.as_ptr(),
                        islock_bytes.len(),
                        &ffi_balance as *const FFIBalance,
                        account_balances_ptr,
                        ffi_account_balances.len() as u32,
                        self.user_data,
                    );

                    drop(ffi_account_balances);
                }
            }
            WalletEvent::BlockProcessed {
                wallet_id,
                height,
                inserted,
                updated,
                matured,
                balance,
                account_balances,
                addresses_derived,
                chain_lock,
            } => {
                if let Some(cb) = self.on_block_processed {
                    let wallet_id_hex = wallet_id.to_lower_hex_string();
                    let c_wallet_id = CString::new(wallet_id_hex).unwrap_or_default();
                    let ffi_inserted: Vec<FFITransactionRecord> =
                        inserted.iter().map(FFITransactionRecord::from).collect();
                    let ffi_updated: Vec<FFITransactionRecord> =
                        updated.iter().map(FFITransactionRecord::from).collect();
                    let ffi_matured: Vec<FFITransactionRecord> =
                        matured.iter().map(FFITransactionRecord::from).collect();
                    let ffi_balance = FFIBalance::from(*balance);
                    let ffi_account_balances = FFIAccountBalance::from_map(account_balances);
                    let ffi_addresses_derived = FFIDerivedAddress::from_slice(addresses_derived);

                    // Pass a null pointer when an array is empty so C/Swift
                    // consumers that null-check before reading don't see a
                    // non-null dangling pointer paired with a zero count.
                    let inserted_ptr = if ffi_inserted.is_empty() {
                        ptr::null()
                    } else {
                        ffi_inserted.as_ptr()
                    };
                    let updated_ptr = if ffi_updated.is_empty() {
                        ptr::null()
                    } else {
                        ffi_updated.as_ptr()
                    };
                    let matured_ptr = if ffi_matured.is_empty() {
                        ptr::null()
                    } else {
                        ffi_matured.as_ptr()
                    };
                    let account_balances_ptr = if ffi_account_balances.is_empty() {
                        ptr::null()
                    } else {
                        ffi_account_balances.as_ptr()
                    };
                    let addresses_derived_ptr = if ffi_addresses_derived.is_empty() {
                        ptr::null()
                    } else {
                        ffi_addresses_derived.as_ptr()
                    };
                    // Null pointers (and `cl_height=0`) when the block isn't
                    // chainlocked; non-null hash + signature pointers borrow
                    // from `chain_lock` for the duration of the callback.
                    let (cl_height_arg, cl_hash_arg, cl_signature_arg) = match chain_lock {
                        Some(cl) => (
                            cl.block_height,
                            cl.block_hash.as_byte_array() as *const [u8; 32],
                            cl.signature.as_bytes() as *const [u8; 96],
                        ),
                        None => (0, ptr::null(), ptr::null()),
                    };

                    cb(
                        c_wallet_id.as_ptr(),
                        *height,
                        inserted_ptr,
                        ffi_inserted.len() as u32,
                        updated_ptr,
                        ffi_updated.len() as u32,
                        matured_ptr,
                        ffi_matured.len() as u32,
                        &ffi_balance as *const FFIBalance,
                        account_balances_ptr,
                        ffi_account_balances.len() as u32,
                        addresses_derived_ptr,
                        ffi_addresses_derived.len() as u32,
                        cl_height_arg,
                        cl_hash_arg,
                        cl_signature_arg,
                        self.user_data,
                    );

                    drop(ffi_inserted);
                    drop(ffi_updated);
                    drop(ffi_matured);
                    drop(ffi_account_balances);
                    drop(ffi_addresses_derived);
                }
            }
            WalletEvent::SyncHeightAdvanced {
                wallet_id,
                height,
            } => {
                if let Some(cb) = self.on_sync_height_advanced {
                    let wallet_id_hex = wallet_id.to_lower_hex_string();
                    let c_wallet_id = CString::new(wallet_id_hex).unwrap_or_default();
                    cb(c_wallet_id.as_ptr(), *height, self.user_data);
                }
            }
            WalletEvent::ChainLockProcessed {
                wallet_id,
                chain_lock,
                locked_transactions,
            } => {
                if let Some(cb) = self.on_chain_lock_processed {
                    let wallet_id_hex = wallet_id.to_lower_hex_string();
                    let c_wallet_id = CString::new(wallet_id_hex).unwrap_or_default();
                    let ffi_finalized = FFIChainlockedTxid::from_map(locked_transactions);
                    let finalized_ptr = if ffi_finalized.is_empty() {
                        ptr::null()
                    } else {
                        ffi_finalized.as_ptr()
                    };

                    cb(
                        c_wallet_id.as_ptr(),
                        chain_lock.block_height,
                        chain_lock.block_hash.as_byte_array() as *const [u8; 32],
                        chain_lock.signature.as_bytes() as *const [u8; 96],
                        finalized_ptr,
                        ffi_finalized.len() as u32,
                        self.user_data,
                    );

                    drop(ffi_finalized);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dashcore::{Address, BlockHash, ChainLock, Network, ScriptBuf, Txid};
    use key_wallet::account::AccountType;
    use key_wallet::WalletCoreBalance;
    use key_wallet_manager::{FilterMatchKey, WalletId};
    use std::collections::{BTreeMap, BTreeSet};
    use std::os::raw::{c_char, c_void};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A sweep with nothing released must hand the callback `null` and `0`,
    /// not a dangling pointer into an empty `Vec`.
    ///
    /// This is the layer where getting it wrong does not fail an assertion:
    /// a consumer reading `count` first sees zero and stops, but one that
    /// dereferences the pointer defensively would read whatever the empty
    /// allocation points at. The ordinary resend — the winner takes every
    /// input the loser named — releases nothing, so this is the common case,
    /// not the edge one.
    #[test]
    fn test_transactions_swept_dispatch_passes_null_when_nothing_released() {
        static RELEASED_PTR_WAS_NULL: AtomicU32 = AtomicU32::new(u32::MAX);
        static RELEASED_COUNT: AtomicU32 = AtomicU32::new(u32::MAX);

        extern "C" fn cb(
            _wallet_id: *const c_char,
            _txids: *const [u8; 32],
            _txids_count: usize,
            _superseded_by: *const [u8; 32],
            released: *const FFIOutPoint,
            released_count: usize,
            _balance: *const FFIBalance,
            _account_balances: *const FFIAccountBalance,
            _account_balances_count: u32,
            _user: *mut c_void,
        ) {
            RELEASED_PTR_WAS_NULL.store(u32::from(released.is_null()), Ordering::SeqCst);
            RELEASED_COUNT.store(released_count as u32, Ordering::SeqCst);
        }

        let callbacks = FFIWalletEventCallbacks {
            on_transactions_swept: Some(cb),
            ..FFIWalletEventCallbacks::default()
        };

        callbacks.dispatch(&WalletEvent::TransactionsSwept {
            wallet_id: [7u8; 32],
            txids: vec![Txid::from_byte_array([1u8; 32])],
            superseded_by: Txid::from_byte_array([2u8; 32]),
            winner_mined_height: Some(1_000),
            released_outpoints: Vec::new(),
            balance: WalletCoreBalance::default(),
            account_balances: BTreeMap::new(),
        });

        assert_eq!(RELEASED_PTR_WAS_NULL.load(Ordering::SeqCst), 1, "expected a null pointer");
        assert_eq!(RELEASED_COUNT.load(Ordering::SeqCst), 0);
    }

    /// The released outpoints must arrive intact and in order, and the
    /// parameters after them must not be shifted by their insertion — the
    /// balance a consumer reads has to be the balance, not an outpoint.
    #[test]
    fn test_transactions_swept_dispatch_marshals_released_outpoints() {
        static FIRST_TXID: std::sync::Mutex<[u8; 32]> = std::sync::Mutex::new([0u8; 32]);
        static VOUTS: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());
        static CONFIRMED: AtomicU32 = AtomicU32::new(u32::MAX);

        extern "C" fn cb(
            _wallet_id: *const c_char,
            _txids: *const [u8; 32],
            _txids_count: usize,
            _superseded_by: *const [u8; 32],
            released: *const FFIOutPoint,
            released_count: usize,
            balance: *const FFIBalance,
            _account_balances: *const FFIAccountBalance,
            _account_balances_count: u32,
            _user: *mut c_void,
        ) {
            assert!(!released.is_null());
            let entries = unsafe { std::slice::from_raw_parts(released, released_count) };
            *FIRST_TXID.lock().expect("txid") = entries[0].txid;
            *VOUTS.lock().expect("vouts") = entries.iter().map(|e| e.vout).collect();
            CONFIRMED.store(unsafe { (*balance).confirmed } as u32, Ordering::SeqCst);
        }

        let callbacks = FFIWalletEventCallbacks {
            on_transactions_swept: Some(cb),
            ..FFIWalletEventCallbacks::default()
        };

        let parent = Txid::from_byte_array([9u8; 32]);
        callbacks.dispatch(&WalletEvent::TransactionsSwept {
            wallet_id: [7u8; 32],
            txids: vec![Txid::from_byte_array([1u8; 32])],
            superseded_by: Txid::from_byte_array([2u8; 32]),
            winner_mined_height: None,
            released_outpoints: vec![
                dashcore::OutPoint {
                    txid: parent,
                    vout: 3,
                },
                dashcore::OutPoint {
                    txid: parent,
                    vout: 7,
                },
            ],
            balance: WalletCoreBalance::new(123_456, 0, 0, 0),
            account_balances: BTreeMap::new(),
        });

        assert_eq!(*FIRST_TXID.lock().expect("txid"), *parent.as_byte_array());
        assert_eq!(*VOUTS.lock().expect("vouts"), vec![3, 7]);
        assert_eq!(
            CONFIRMED.load(Ordering::SeqCst),
            123_456,
            "the balance parameter must not be shifted by the released-outpoint insertion"
        );
    }

    /// `BlocksNeeded` dispatch must pass exactly one entry per
    /// `FilterMatchKey` to the FFI callback (i.e. iterate keys, not
    /// inflated by the per-block wallet attribution).
    #[test]
    fn test_blocks_needed_dispatch_passes_unique_keys_count() {
        static COUNT: AtomicU32 = AtomicU32::new(u32::MAX);
        extern "C" fn cb(_blocks: *const FFIBlockNeeded, count: u32, _user: *mut c_void) {
            COUNT.store(count, Ordering::SeqCst);
        }

        let callbacks = FFISyncEventCallbacks {
            on_blocks_needed: Some(cb),
            ..FFISyncEventCallbacks::default()
        };

        let mut blocks: BTreeMap<FilterMatchKey, BTreeSet<WalletId>> = BTreeMap::new();
        // Two distinct blocks, each attributed to two wallets. The dispatch
        // must report 2 (unique keys), not 4.
        blocks.insert(
            FilterMatchKey::new(10, BlockHash::from_byte_array([1u8; 32])),
            BTreeSet::from([[1u8; 32], [2u8; 32]]),
        );
        blocks.insert(
            FilterMatchKey::new(20, BlockHash::from_byte_array([2u8; 32])),
            BTreeSet::from([[1u8; 32], [2u8; 32]]),
        );

        callbacks.dispatch(&SyncEvent::BlocksNeeded {
            blocks,
        });
        assert_eq!(COUNT.load(Ordering::SeqCst), 2);
    }

    /// `BlockProcessed` dispatch must report the total address count
    /// summed across all per-wallet entries in the `new_addresses` map.
    #[test]
    fn test_block_processed_dispatch_sums_per_wallet_addresses() {
        static NEW_ADDR_COUNT: AtomicU32 = AtomicU32::new(u32::MAX);
        extern "C" fn cb(
            _height: u32,
            _hash: *const [u8; 32],
            new_address_count: u32,
            _txids: *const [u8; 32],
            _txid_count: u32,
            _user: *mut c_void,
        ) {
            NEW_ADDR_COUNT.store(new_address_count, Ordering::SeqCst);
        }

        let callbacks = FFISyncEventCallbacks {
            on_block_processed: Some(cb),
            ..FFISyncEventCallbacks::default()
        };

        let addr_a = Address::dummy(Network::Regtest, 1);
        let addr_b = Address::dummy(Network::Regtest, 2);
        let addr_c = Address::dummy(Network::Regtest, 3);
        let mut new_scripts: BTreeMap<WalletId, Vec<ScriptBuf>> = BTreeMap::new();
        // Wallet 1 contributes 2 new scripts, wallet 2 contributes 1. Total = 3.
        new_scripts.insert([1u8; 32], vec![addr_a.script_pubkey(), addr_b.script_pubkey()]);
        new_scripts.insert([2u8; 32], vec![addr_c.script_pubkey()]);

        callbacks.dispatch(&SyncEvent::BlockProcessed {
            block_hash: BlockHash::from_byte_array([7u8; 32]),
            height: 100,
            wallets: BTreeSet::new(),
            new_scripts,
            confirmed_txids: vec![Txid::from_byte_array([9u8; 32])],
        });
        assert_eq!(NEW_ADDR_COUNT.load(Ordering::SeqCst), 3);
    }

    /// `ChainLockProcessed` dispatch must hand every wired field
    /// through to the FFI callback unchanged: hex-encoded wallet_id,
    /// height, 32-byte block hash, 96-byte signature, and the count of
    /// per-(account, txid) promotions. A regression that miswires any
    /// of these (e.g. height/hash swap, signature truncation, empty vs.
    /// non-empty promotion handling) shows up as a single assertion
    /// failure here.
    #[test]
    fn test_chain_lock_processed_dispatch_round_trips_every_field() {
        struct Captured {
            wallet_id_hex: String,
            cl_height: u32,
            cl_hash: [u8; 32],
            cl_signature: [u8; 96],
            finalized_count: u32,
        }
        static CAPTURED: std::sync::Mutex<Option<Captured>> = std::sync::Mutex::new(None);

        extern "C" fn cb(
            wallet_id: *const c_char,
            cl_height: u32,
            cl_hash: *const [u8; 32],
            cl_signature: *const [u8; 96],
            _finalized: *const FFIChainlockedTxid,
            finalized_count: u32,
            _user: *mut c_void,
        ) {
            let wid = unsafe { std::ffi::CStr::from_ptr(wallet_id) }
                .to_str()
                .expect("wallet_id must be valid UTF-8 hex")
                .to_string();
            *CAPTURED.lock().unwrap() = Some(Captured {
                wallet_id_hex: wid,
                cl_height,
                cl_hash: unsafe { *cl_hash },
                cl_signature: unsafe { *cl_signature },
                finalized_count,
            });
        }

        let callbacks = FFIWalletEventCallbacks {
            on_chain_lock_processed: Some(cb),
            ..FFIWalletEventCallbacks::default()
        };

        let chain_lock = ChainLock::dummy(777);
        let expected_hash = *chain_lock.block_hash.as_byte_array();
        let expected_sig = *chain_lock.signature.as_bytes();
        let wallet_id: WalletId = [3u8; 32];

        // Two promotions to verify `finalized_count` reflects total
        // (account, txid) pairs, not the number of accounts.
        let account_a = AccountType::Standard {
            index: 0,
            standard_account_type: key_wallet::account::StandardAccountType::BIP44Account,
        };
        let account_b = AccountType::Standard {
            index: 1,
            standard_account_type: key_wallet::account::StandardAccountType::BIP44Account,
        };
        let mut locked: BTreeMap<AccountType, Vec<Txid>> = BTreeMap::new();
        locked.insert(account_a, vec![Txid::from_byte_array([0xaa; 32])]);
        locked.insert(account_b, vec![Txid::from_byte_array([0xbb; 32])]);

        callbacks.dispatch(&WalletEvent::ChainLockProcessed {
            wallet_id,
            chain_lock,
            locked_transactions: locked,
        });

        let captured = CAPTURED.lock().unwrap().take().expect("callback fired");
        assert_eq!(
            captured.wallet_id_hex,
            wallet_id.to_lower_hex_string(),
            "wallet_id hex-encoding"
        );
        assert_eq!(captured.cl_height, 777, "cl_height");
        assert_eq!(captured.cl_hash, expected_hash, "cl_hash round-trip");
        assert_eq!(captured.cl_signature, expected_sig, "cl_signature round-trip");
        assert_eq!(captured.finalized_count, 2, "finalized_count counts (account, txid) pairs");
    }

    /// `ChainLockProcessed` with empty `locked_transactions` must still
    /// fire the callback (durable consumers persist the chainlock proof
    /// even when no record was promoted) with `finalized_count == 0`.
    #[test]
    fn test_chain_lock_processed_dispatch_fires_with_empty_promotions() {
        static FIRED: AtomicU32 = AtomicU32::new(u32::MAX);
        extern "C" fn cb(
            _wallet_id: *const c_char,
            _cl_height: u32,
            _cl_hash: *const [u8; 32],
            _cl_signature: *const [u8; 96],
            _finalized: *const FFIChainlockedTxid,
            finalized_count: u32,
            _user: *mut c_void,
        ) {
            FIRED.store(finalized_count, Ordering::SeqCst);
        }

        let callbacks = FFIWalletEventCallbacks {
            on_chain_lock_processed: Some(cb),
            ..FFIWalletEventCallbacks::default()
        };

        callbacks.dispatch(&WalletEvent::ChainLockProcessed {
            wallet_id: [4u8; 32],
            chain_lock: ChainLock::dummy(900),
            locked_transactions: BTreeMap::new(),
        });
        assert_eq!(FIRED.load(Ordering::SeqCst), 0);
    }

    /// `TransactionBroadcastResult` dispatch must marshal the outcome fields
    /// (status, relayed_by) for each variant.
    #[test]
    fn test_transaction_broadcast_result_dispatch() {
        use dash_spv::BroadcastResult;

        static STATUS: AtomicU32 = AtomicU32::new(u32::MAX);
        static RELAYED: AtomicU32 = AtomicU32::new(u32::MAX);

        extern "C" fn cb(
            txid: *const [u8; 32],
            status: FFIBroadcastStatus,
            relayed_by: u32,
            _user: *mut c_void,
        ) {
            assert!(!txid.is_null());
            STATUS.store(status as u32, Ordering::SeqCst);
            RELAYED.store(relayed_by, Ordering::SeqCst);
        }

        let callbacks = FFISyncEventCallbacks {
            on_transaction_broadcast_result: Some(cb),
            ..FFISyncEventCallbacks::default()
        };
        let txid = Txid::from_byte_array([7u8; 32]);

        callbacks.dispatch(&SyncEvent::TransactionBroadcastResult {
            txid,
            result: BroadcastResult::Accepted {
                relayed_by: 3,
            },
        });
        assert_eq!(STATUS.load(Ordering::SeqCst), FFIBroadcastStatus::Accepted as u32);
        assert_eq!(RELAYED.load(Ordering::SeqCst), 3);

        callbacks.dispatch(&SyncEvent::TransactionBroadcastResult {
            txid,
            result: BroadcastResult::Uncertain,
        });
        assert_eq!(STATUS.load(Ordering::SeqCst), FFIBroadcastStatus::Uncertain as u32);
        assert_eq!(RELAYED.load(Ordering::SeqCst), 0);
    }
}
