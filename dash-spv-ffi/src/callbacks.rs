//! FFI callback types for event notifications.
//!
//! This module provides several callback structs, each with one callback per event variant:
//! - `FFIProgressCallback` - Sync progress updates
//! - `FFISyncEventCallbacks` - Sync coordinator events
//! - `FFINetworkEventCallbacks` - Network manager events
//! - `FFIWalletEventCallbacks` - Wallet manager events

use crate::FFISyncProgress;
use key_wallet::account::AccountType;
use key_wallet::WalletCoreBalance;
use key_wallet_ffi::managed_account::{FFIAccountType, FFITransactionRecord};
use key_wallet_ffi::types::FFIBalance;
use std::collections::BTreeMap;
use std::ffi::CString;
use std::os::raw::{c_char, c_void};

// ============================================================================
// Sync Event Types (for FFISyncEventCallbacks)
// ============================================================================

/// Identifies which sync manager generated an event.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FFIManagerId {
    Headers = 0,
    FilterHeaders = 1,
    Filters = 2,
    Blocks = 3,
    Masternodes = 4,
    ChainLocks = 5,
    InstantSend = 6,
    Mempool = 7,
}

impl From<dash_spv::ManagerIdentifier> for FFIManagerId {
    fn from(id: dash_spv::ManagerIdentifier) -> Self {
        match id {
            dash_spv::ManagerIdentifier::BlockHeader => FFIManagerId::Headers,
            dash_spv::ManagerIdentifier::FilterHeader => FFIManagerId::FilterHeaders,
            dash_spv::ManagerIdentifier::Filter => FFIManagerId::Filters,
            dash_spv::ManagerIdentifier::Block => FFIManagerId::Blocks,
            dash_spv::ManagerIdentifier::Masternode => FFIManagerId::Masternodes,
            dash_spv::ManagerIdentifier::ChainLock => FFIManagerId::ChainLocks,
            dash_spv::ManagerIdentifier::InstantSend => FFIManagerId::InstantSend,
            dash_spv::ManagerIdentifier::Mempool => FFIManagerId::Mempool,
        }
    }
}

// ============================================================================
// Progress Callback
// ============================================================================

/// Callback for sync progress updates.
///
/// Called whenever the sync progress changes. The progress pointer is only
/// valid for the duration of the callback. The caller must NOT free the
/// progress pointer - it will be freed automatically after the callback returns.
pub type OnProgressUpdateCallback =
    Option<extern "C" fn(progress: *const FFISyncProgress, user_data: *mut c_void)>;

/// Progress callback configuration.
#[repr(C)]
#[derive(Clone)]
pub struct FFIProgressCallback {
    /// Callback function for progress updates.
    pub on_progress: OnProgressUpdateCallback,
    /// User data passed to the callback.
    pub user_data: *mut c_void,
}

unsafe impl Send for FFIProgressCallback {}
unsafe impl Sync for FFIProgressCallback {}

impl Default for FFIProgressCallback {
    fn default() -> Self {
        Self {
            on_progress: None,
            user_data: std::ptr::null_mut(),
        }
    }
}

// ============================================================================
// FFISyncEventCallbacks - One callback per SyncEvent variant
// ============================================================================

/// Callback for SyncEvent::SyncStart
pub type OnSyncStartCallback =
    Option<extern "C" fn(manager_id: FFIManagerId, user_data: *mut c_void)>;

/// Callback for SyncEvent::BlockHeadersStored
pub type OnBlockHeadersStoredCallback =
    Option<extern "C" fn(tip_height: u32, user_data: *mut c_void)>;

/// Callback for SyncEvent::BlockHeaderSyncComplete
pub type OnBlockHeaderSyncCompleteCallback =
    Option<extern "C" fn(tip_height: u32, user_data: *mut c_void)>;

/// Callback for SyncEvent::FilterHeadersStored
pub type OnFilterHeadersStoredCallback = Option<
    extern "C" fn(start_height: u32, end_height: u32, tip_height: u32, user_data: *mut c_void),
>;

/// Callback for SyncEvent::FilterHeadersSyncComplete
pub type OnFilterHeadersSyncCompleteCallback =
    Option<extern "C" fn(tip_height: u32, user_data: *mut c_void)>;

/// Callback for SyncEvent::FiltersStored
pub type OnFiltersStoredCallback =
    Option<extern "C" fn(start_height: u32, end_height: u32, user_data: *mut c_void)>;

/// Callback for SyncEvent::FiltersSyncComplete
pub type OnFiltersSyncCompleteCallback =
    Option<extern "C" fn(tip_height: u32, user_data: *mut c_void)>;

/// A block that needs to be downloaded (height + hash).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FFIBlockNeeded {
    /// Block height
    pub height: u32,
    /// Block hash (32 bytes)
    pub hash: [u8; 32],
}

/// Callback for SyncEvent::BlocksNeeded
///
/// The `blocks` pointer points to an array of `FFIBlockNeeded` structs.
/// The pointer is borrowed and only valid for the duration of the callback.
/// Callers must memcpy/duplicate any data they need to retain after the
/// callback returns.
pub type OnBlocksNeededCallback =
    Option<extern "C" fn(blocks: *const FFIBlockNeeded, count: u32, user_data: *mut c_void)>;

/// Callback for SyncEvent::BlockProcessed
///
/// The `hash` pointer is borrowed and only valid for the duration of the
/// callback. Callers must memcpy/duplicate it to retain the value after
/// the callback returns.
pub type OnBlockProcessedCallback = Option<
    extern "C" fn(
        height: u32,
        hash: *const [u8; 32],
        new_address_count: u32,
        confirmed_txids: *const [u8; 32],
        confirmed_txid_count: u32,
        user_data: *mut c_void,
    ),
>;

/// Callback for SyncEvent::MasternodeStateUpdated
pub type OnMasternodeStateUpdatedCallback =
    Option<extern "C" fn(height: u32, user_data: *mut c_void)>;

/// Callback for SyncEvent::ChainLockReceived
///
/// The `hash` and `signature` pointers are borrowed and only valid for the
/// duration of the callback. Callers must memcpy/duplicate them to retain
/// the values after the callback returns.
pub type OnChainLockReceivedCallback = Option<
    extern "C" fn(
        height: u32,
        hash: *const [u8; 32],
        signature: *const [u8; 96],
        validated: bool,
        user_data: *mut c_void,
    ),
>;

/// Callback for SyncEvent::InstantLockReceived
///
/// The `txid` pointer is borrowed and only valid for the duration of the callback.
/// The `instantlock_data` pointer points to the consensus-serialized InstantLock
/// bytes and is only valid for the duration of the callback.
/// Callers must memcpy/duplicate any data they need to retain.
pub type OnInstantLockReceivedCallback = Option<
    extern "C" fn(
        txid: *const [u8; 32],
        instantlock_data: *const u8,
        instantlock_len: usize,
        validated: bool,
        user_data: *mut c_void,
    ),
>;

/// Callback for SyncEvent::ManagerError
///
/// The `error` string pointer is borrowed and only valid for the duration
/// of the callback. Callers must copy the string if they need to retain it
/// after the callback returns.
pub type OnManagerErrorCallback =
    Option<extern "C" fn(manager_id: FFIManagerId, error: *const c_char, user_data: *mut c_void)>;

/// Callback for SyncEvent::SyncComplete
pub type OnSyncCompleteCallback =
    Option<extern "C" fn(header_tip: u32, cycle: u32, user_data: *mut c_void)>;

/// Network-level outcome of a transaction broadcast.
///
/// There is no rejected state: modern Dash Core removed the BIP61 `reject`
/// message, so the p2p network provides no negative signal — a transaction
/// the network refuses surfaces as `Uncertain` (no echo within the timeout).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FFIBroadcastStatus {
    /// Non-recipient peers announced the txid back (or it was
    /// InstantSend-locked/confirmed) — the network accepted it.
    Accepted = 0,
    /// No acceptance signal arrived within the acceptance timeout.
    Uncertain = 1,
}

/// Callback for SyncEvent::TransactionBroadcastResult
///
/// The `txid` pointer is borrowed and only valid for the duration of the
/// callback. `relayed_by` is the number of distinct non-recipient peers that
/// announced the txid back (meaningful for `Accepted`).
pub type OnTransactionBroadcastResultCallback = Option<
    extern "C" fn(
        txid: *const [u8; 32],
        status: FFIBroadcastStatus,
        relayed_by: u32,
        user_data: *mut c_void,
    ),
>;

/// Sync event callbacks - one callback per SyncEvent variant.
///
/// Set only the callbacks you're interested in; unset callbacks will be ignored.
///
/// All pointer parameters passed to callbacks (strings, hashes, arrays) are
/// borrowed and only valid for the duration of the callback invocation.
/// Callers must memcpy/duplicate any data they need to retain.
#[repr(C)]
#[derive(Clone)]
pub struct FFISyncEventCallbacks {
    pub on_sync_start: OnSyncStartCallback,
    pub on_block_headers_stored: OnBlockHeadersStoredCallback,
    pub on_block_header_sync_complete: OnBlockHeaderSyncCompleteCallback,
    pub on_filter_headers_stored: OnFilterHeadersStoredCallback,
    pub on_filter_headers_sync_complete: OnFilterHeadersSyncCompleteCallback,
    pub on_filters_stored: OnFiltersStoredCallback,
    pub on_filters_sync_complete: OnFiltersSyncCompleteCallback,
    pub on_blocks_needed: OnBlocksNeededCallback,
    pub on_block_processed: OnBlockProcessedCallback,
    pub on_masternode_state_updated: OnMasternodeStateUpdatedCallback,
    pub on_chainlock_received: OnChainLockReceivedCallback,
    pub on_instantlock_received: OnInstantLockReceivedCallback,
    pub on_manager_error: OnManagerErrorCallback,
    pub on_sync_complete: OnSyncCompleteCallback,
    pub on_transaction_broadcast_result: OnTransactionBroadcastResultCallback,
    pub user_data: *mut c_void,
}

// SAFETY: FFISyncEventCallbacks is safe to send between threads because:
// 1. All callback function pointers are extern "C" functions with no captured state
// 2. The user_data pointer is treated as opaque and managed by the caller
// 3. The caller is responsible for ensuring user_data points to thread-safe memory
unsafe impl Send for FFISyncEventCallbacks {}

// SAFETY: FFISyncEventCallbacks is safe to share between threads because:
// 1. The struct is immutable after construction
// 2. Function pointers are inherently thread-safe
// 3. Thread safety of user_data is the caller's responsibility
unsafe impl Sync for FFISyncEventCallbacks {}

impl Default for FFISyncEventCallbacks {
    fn default() -> Self {
        Self {
            on_sync_start: None,
            on_block_headers_stored: None,
            on_block_header_sync_complete: None,
            on_filter_headers_stored: None,
            on_filter_headers_sync_complete: None,
            on_filters_stored: None,
            on_filters_sync_complete: None,
            on_blocks_needed: None,
            on_block_processed: None,
            on_masternode_state_updated: None,
            on_chainlock_received: None,
            on_instantlock_received: None,
            on_manager_error: None,
            on_sync_complete: None,
            on_transaction_broadcast_result: None,
            user_data: std::ptr::null_mut(),
        }
    }
}

// ============================================================================
// FFINetworkEventCallbacks - One callback per NetworkEvent variant
// ============================================================================

/// Callback for NetworkEvent::PeerConnected
///
/// The `address` string pointer is borrowed and only valid for the duration
/// of the callback. Callers must copy the string if they need to retain it
/// after the callback returns.
pub type OnPeerConnectedCallback =
    Option<extern "C" fn(address: *const c_char, user_data: *mut c_void)>;

/// Callback for NetworkEvent::PeerDisconnected
///
/// The `address` string pointer is borrowed and only valid for the duration
/// of the callback. Callers must copy the string if they need to retain it
/// after the callback returns.
pub type OnPeerDisconnectedCallback =
    Option<extern "C" fn(address: *const c_char, user_data: *mut c_void)>;

/// Callback for NetworkEvent::PeersUpdated
pub type OnPeersUpdatedCallback =
    Option<extern "C" fn(connected_count: u32, best_height: u32, user_data: *mut c_void)>;

/// Network event callbacks - one callback per NetworkEvent variant.
///
/// Set only the callbacks you're interested in; unset callbacks will be ignored.
///
/// All pointer parameters passed to callbacks (strings, addresses) are
/// borrowed and only valid for the duration of the callback invocation.
/// Callers must copy any data they need to retain.
#[repr(C)]
#[derive(Clone)]
pub struct FFINetworkEventCallbacks {
    pub on_peer_connected: OnPeerConnectedCallback,
    pub on_peer_disconnected: OnPeerDisconnectedCallback,
    pub on_peers_updated: OnPeersUpdatedCallback,
    pub user_data: *mut c_void,
}

// SAFETY: Same rationale as FFISyncEventCallbacks
unsafe impl Send for FFINetworkEventCallbacks {}
unsafe impl Sync for FFINetworkEventCallbacks {}

impl Default for FFINetworkEventCallbacks {
    fn default() -> Self {
        Self {
            on_peer_connected: None,
            on_peer_disconnected: None,
            on_peers_updated: None,
            user_data: std::ptr::null_mut(),
        }
    }
}

// ============================================================================
// FFIAccountBalance - Per-account balance entry
// ============================================================================

/// Per-account balance pair carried on wallet events.
///
/// Wallet events deliver an array of these — one entry per account whose
/// balance changed during the event. Accounts whose balance was unchanged
/// are omitted to keep the payload small (most transactions touch only
/// 1–2 accounts).
///
/// `account_type` follows the same memory rules as the equivalent field on
/// [`FFITransactionRecord`]: the embedded `identity_user` / `identity_friend`
/// pointers (non-null only for Dashpay variants) are owned by the
/// `FFIAccountType` and freed when the array is dropped after the callback
/// returns. Consumers that need to retain the data past the callback must
/// copy the contents.
#[repr(C)]
pub struct FFIAccountBalance {
    /// Owning-account descriptor (discriminant + indices + identity ids).
    pub account_type: FFIAccountType,
    /// Balance for the account after the event.
    pub balance: FFIBalance,
}

impl FFIAccountBalance {
    pub(crate) fn from_map(map: &BTreeMap<AccountType, WalletCoreBalance>) -> Vec<Self> {
        map.iter()
            .map(|(account_type, balance)| FFIAccountBalance {
                account_type: FFIAccountType::from(account_type),
                balance: FFIBalance::from(*balance),
            })
            .collect()
    }
}

// ============================================================================
// FFIDerivedAddress - One address derived during gap-limit maintenance
// ============================================================================

/// Pool the derived address belongs to.
///
/// Mirrors `key_wallet::managed_account::address_pool::AddressPoolType`
/// 1:1 — kept distinct from the existing `FFIAddressPoolType` (which
/// collapses Absent / AbsentHardened into a single `Single` variant) so
/// event consumers can distinguish hardened single-pool variants
/// (Provider operator keys, etc.) from non-hardened ones.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FFIDerivedAddressPoolType {
    External = 0,
    Internal = 1,
    Absent = 2,
    AbsentHardened = 3,
}

impl From<key_wallet::managed_account::address_pool::AddressPoolType>
    for FFIDerivedAddressPoolType
{
    fn from(t: key_wallet::managed_account::address_pool::AddressPoolType) -> Self {
        use key_wallet::managed_account::address_pool::AddressPoolType as P;
        match t {
            P::External => FFIDerivedAddressPoolType::External,
            P::Internal => FFIDerivedAddressPoolType::Internal,
            P::Absent => FFIDerivedAddressPoolType::Absent,
            P::AbsentHardened => FFIDerivedAddressPoolType::AbsentHardened,
        }
    }
}

/// One address derived as a side effect of gap-limit maintenance during
/// transaction or block processing.
///
/// Wallet events deliver an array of these so persisters can mirror the
/// on-disk address pool transactionally with the tx/block records that
/// triggered the derivation. Without this, UTXOs landing on freshly
/// derived addresses orphan their parent address row at the persister.
///
/// `account_type` follows the same memory rules as on
/// [`FFIAccountBalance`]: the embedded `identity_user` / `identity_friend`
/// pointers are owned by the `FFIAccountType` and freed when the array is
/// dropped after the callback returns. `address` is a heap-allocated
/// null-terminated UTF-8 string, owned by this struct and freed on drop.
/// Consumers that need to retain the data past the callback must copy
/// every owning field — not just retain pointers.
#[repr(C)]
pub struct FFIDerivedAddress {
    /// Owning-account descriptor (discriminant + indices + identity ids).
    pub account_type: FFIAccountType,
    /// Pool within the account that derived this address.
    pub pool_type: FFIDerivedAddressPoolType,
    /// Derivation index within the pool. Combined with `account_type`
    /// and `pool_type`, this fully determines the derivation path —
    /// consumers that need a rendered path can recompute it
    /// deterministically.
    pub derivation_index: u32,
    /// Heap-allocated null-terminated UTF-8 string. Owned by this
    /// struct; freed when the struct is dropped.
    pub address: *mut c_char,
    /// 33-byte compressed ECDSA public key (inline, no allocation).
    pub public_key: [u8; 33],
}

impl FFIDerivedAddress {
    pub(crate) fn from_slice(addresses: &[key_wallet_manager::DerivedAddress]) -> Vec<Self> {
        addresses
            .iter()
            .map(|d| {
                let address_str = d.address.to_string();
                let c_address = CString::new(address_str).unwrap_or_else(|_| CString::default());
                FFIDerivedAddress {
                    account_type: FFIAccountType::from(&d.account_type),
                    pool_type: FFIDerivedAddressPoolType::from(d.pool_type),
                    derivation_index: d.derivation_index,
                    address: c_address.into_raw(),
                    public_key: d.public_key.inner.to_compressed(),
                }
            })
            .collect()
    }
}

impl Drop for FFIDerivedAddress {
    fn drop(&mut self) {
        if !self.address.is_null() {
            // SAFETY: `address` was constructed via `CString::into_raw` in
            // `FFIDerivedAddress::from_slice`, so reclaiming via
            // `CString::from_raw` is the matching free.
            let _ = unsafe { CString::from_raw(self.address) };
            self.address = std::ptr::null_mut();
        }
        // `account_type` has its own Drop impl that frees the
        // identity_user / identity_friend allocations when applicable.
    }
}

// ============================================================================
// FFIWalletEventCallbacks - One callback per WalletEvent variant
// ============================================================================

/// Callback for `WalletEvent::TransactionDetected`.
///
/// Fires when a wallet-relevant transaction is first seen off-chain — either
/// in the mempool, or directly via an InstantSend lock (in that case the
/// record's `context` is `InstantSend(..)`). Also fires when an off-chain
/// funding arrival corrects an earlier transaction's accounting. Consumers must
/// upsert by wallet, account and txid; a correction retains the spender's context.
/// A lock on a known mempool transaction emits only the lock callback when no
/// account backfill or accounting correction is needed.
///
/// All pointer parameters are borrowed and only valid for the duration of the
/// callback. `balance` is the wallet's balance *after* the transaction was
/// recorded. `account_balances` is an array of size `account_balances_count`
/// containing one entry per account whose balance changed (typically 1–2
/// entries for a normal transaction); accounts whose balance is unchanged
/// are omitted. The array is null with a zero count when no per-account
/// balance changed.
///
/// `addresses_derived` is an array of size `addresses_derived_count` of
/// addresses derived as a side effect of gap-limit maintenance while
/// processing this transaction, attributed to the same account as
/// `record`. Empty in the common case (null pointer with zero count).
/// Persisters should write these rows transactionally with `record` so
/// UTXOs landing on freshly-derived addresses retain a parent row.
pub type OnTransactionDetectedCallback = Option<
    extern "C" fn(
        wallet_id: *const c_char,
        record: *const FFITransactionRecord,
        balance: *const FFIBalance,
        account_balances: *const FFIAccountBalance,
        account_balances_count: u32,
        addresses_derived: *const FFIDerivedAddress,
        addresses_derived_count: u32,
        user_data: *mut c_void,
    ),
>;

/// C representation of a Core [`OutPoint`](dashcore::OutPoint): the parent
/// txid and output index of a coin.
#[repr(C)]
pub struct FFIOutPoint {
    /// Parent transaction id.
    pub txid: [u8; 32],
    /// Output index within the parent transaction.
    pub vout: u32,
}

impl FFIOutPoint {
    pub(crate) fn from_slice(outpoints: &[dashcore::OutPoint]) -> Vec<Self> {
        outpoints
            .iter()
            .map(|outpoint| FFIOutPoint {
                txid: *outpoint.txid.as_byte_array(),
                vout: outpoint.vout,
            })
            .collect()
    }
}

/// Callback for `WalletEvent::TransactionsSwept`.
///
/// Fires when the wallet removes transactions that a later, final transaction
/// provably beat to one of their inputs: they can never confirm, so their
/// outputs are gone from the UTXO set and their records deleted.
///
/// **The only removal-shaped wallet callback.** Every other one is additive,
/// so a consumer mirroring wallet state to disk must act on this — delete the
/// named transactions and any UTXO they created. Ignoring it leaves the dead
/// rows in the mirror, which replays them on the next load and re-creates a
/// balance the wallet has already corrected.
///
/// `txids` points to `txids_count` consecutive 32-byte txids.
/// `superseded_by` is the transaction whose arrival settled the inputs.
/// `released_outpoints` points to `released_outpoints_count` outpoints freed
/// by the removal, as `WalletEvent::TransactionsSwept::released_outpoints`
/// documents: inputs of the removed transactions that no surviving record
/// spends too, and outpoints whose spent-output claim named a removed
/// transaction, each listed only once nothing in the wallet guards it any
/// more. Mark these coins spendable again. This is
/// not the same set as `txids`' inputs — a loser spending A+B against a
/// winner spending only A leaves A marked and frees only B — and it cannot
/// be recomputed from `txids` on the consumer side: `superseded_by` need not
/// be wallet-relevant at all (it can spend our coin while paying only
/// external addresses), so it may never appear in any other callback. Null
/// with a zero count when the removal released nothing.
///
/// The Rust `WalletEvent::TransactionsSwept` additionally carries
/// `winner_mined_height` (the winner's block height, `None` for an
/// InstantSend-locked winner not yet mined). It is deliberately not
/// forwarded here: this signature has no safely-degrading insertion point
/// for hand-declared C consumers (see the released-outpoints addition),
/// so growing it is a flagged breaking change deferred until a C consumer
/// needs the field.
/// All pointer parameters are borrowed and only valid for the duration of the
/// callback. `balance` is the wallet's balance *after* the removal;
/// `account_balances` follows the same contract as on
/// [`OnTransactionDetectedCallback`].
pub type OnTransactionsSweptCallback = Option<
    extern "C" fn(
        wallet_id: *const c_char,
        txids: *const [u8; 32],
        txids_count: usize,
        superseded_by: *const [u8; 32],
        released_outpoints: *const FFIOutPoint,
        released_outpoints_count: usize,
        balance: *const FFIBalance,
        account_balances: *const FFIAccountBalance,
        account_balances_count: u32,
        user_data: *mut c_void,
    ),
>;

/// Callback for `WalletEvent::ChainTruncated`.
///
/// Fires once per wallet that processed a block above `height` when a fork
/// replaced the chain above it, and the wallet dropped what it recorded from
/// those blocks. A consumer mirroring wallet state to disk must delete the
/// `txids_count` transactions at `txids` with any UTXO they created, move the
/// `unconfirmed_txids_count` InstantSend-locked transactions at
/// `unconfirmed_txids` back to the mempool with their UTXOs, mark the
/// `restored_outpoints_count` coins at `restored_outpoints` unspent, and lower
/// its synced height to `height` when it is above it. `txids`,
/// `unconfirmed_txids` and `restored_outpoints` are null when their count is 0.
///
/// All pointer parameters are borrowed and only valid for the duration of the
/// callback. `balance` is the wallet's balance *after* the truncation;
/// `account_balances` follows the same contract as on
/// [`OnTransactionDetectedCallback`].
pub type OnChainTruncatedCallback = Option<
    extern "C" fn(
        wallet_id: *const c_char,
        height: u32,
        txids: *const [u8; 32],
        txids_count: usize,
        unconfirmed_txids: *const [u8; 32],
        unconfirmed_txids_count: usize,
        restored_outpoints: *const FFIOutPoint,
        restored_outpoints_count: usize,
        balance: *const FFIBalance,
        account_balances: *const FFIAccountBalance,
        account_balances_count: u32,
        user_data: *mut c_void,
    ),
>;

/// Callback for `WalletEvent::TransactionInstantLocked`.
///
/// Fires when an InstantSend lock is applied to a previously-seen off-chain
/// wallet-relevant transaction. Consumers already hold the full record from
/// the prior `TransactionDetected`; only the txid, the consensus-serialized
/// `InstantLock` bytes, and the post-change balance are delivered.
///
/// All pointer parameters are borrowed and only valid for the duration of
/// the callback. `balance` is the wallet's balance *after* the change.
/// `account_balances` follows the same contract as on
/// [`OnTransactionDetectedCallback`].
pub type OnTransactionInstantLockedCallback = Option<
    extern "C" fn(
        wallet_id: *const c_char,
        txid: *const [u8; 32],
        islock_data: *const u8,
        islock_len: usize,
        balance: *const FFIBalance,
        account_balances: *const FFIAccountBalance,
        account_balances_count: u32,
        user_data: *mut c_void,
    ),
>;

/// Callback for `WalletEvent::BlockProcessed`.
///
/// Fires once per wallet affected by a processed block. The three record
/// arrays bucket what happened in this block: `inserted` is records first
/// stored, `updated` is previously-known records confirmed or corrected,
/// `matured` is older coinbase records whose maturity threshold was just
/// crossed. Empty arrays are passed as null with a zero count. `balance` is
/// the wallet's balance *after* the block was processed. `account_balances`
/// follows the same contract as on [`OnTransactionDetectedCallback`].
///
/// `addresses_derived` is an array of size `addresses_derived_count` of
/// addresses derived as a side effect of gap-limit maintenance across
/// every record in the block, deduplicated by
/// `(account_type, pool_type, derivation_index)`. Empty in the common
/// case (null pointer with zero count). Persisters should write these
/// rows transactionally with the inserted/updated records.
///
/// `cl_hash` and `cl_signature` are non-null iff the processed block is
/// covered by the wallet's chainlock at processing time. When non-null,
/// every record in this event has an `InChainLockedBlock` context and
/// the carried chainlock is the proof that established it (`cl_height
/// >= height` by construction). When null, the block is above the
/// wallet's finality boundary and records are `InBlock`.
///
/// All array pointers and their contents are borrowed and only valid for the
/// duration of the callback.
pub type OnWalletBlockProcessedCallback = Option<
    extern "C" fn(
        wallet_id: *const c_char,
        height: u32,
        inserted: *const FFITransactionRecord,
        inserted_count: u32,
        updated: *const FFITransactionRecord,
        updated_count: u32,
        matured: *const FFITransactionRecord,
        matured_count: u32,
        balance: *const FFIBalance,
        account_balances: *const FFIAccountBalance,
        account_balances_count: u32,
        addresses_derived: *const FFIDerivedAddress,
        addresses_derived_count: u32,
        cl_height: u32,
        cl_hash: *const [u8; 32],
        cl_signature: *const [u8; 96],
        user_data: *mut c_void,
    ),
>;

/// Callback for `WalletEvent::SyncHeightAdvanced`.
///
/// Fires once per wallet when the filter pipeline commits a batch — the
/// wallet has been scanned up to `height`. Consumers can persist this as a
/// checkpoint atomically with any records/balance already persisted from
/// prior `BlockProcessed` events inside the batch.
pub type OnSyncHeightAdvancedCallback =
    Option<extern "C" fn(wallet_id: *const c_char, height: u32, user_data: *mut c_void)>;

/// One net-new chainlock-finalized txid, scoped to the account it was
/// promoted on. `WalletEvent::ChainLockProcessed` delivers an
/// array of these — one entry per (account, txid) pair promoted by
/// the chainlock.
///
/// `account_type` follows the same memory rules as on
/// [`FFIAccountBalance`]: the embedded `identity_user` /
/// `identity_friend` pointers (non-null only for Dashpay variants)
/// are owned by the `FFIAccountType` and freed when the array is
/// dropped after the callback returns.
#[repr(C)]
pub struct FFIChainlockedTxid {
    /// Owning-account descriptor.
    pub account_type: FFIAccountType,
    /// Promoted transaction id.
    pub txid: [u8; 32],
}

impl FFIChainlockedTxid {
    pub(crate) fn from_map(map: &BTreeMap<AccountType, Vec<dashcore::Txid>>) -> Vec<Self> {
        let mut out = Vec::new();
        for (account_type, txids) in map {
            for txid in txids {
                out.push(FFIChainlockedTxid {
                    account_type: FFIAccountType::from(account_type),
                    txid: *txid.as_byte_array(),
                });
            }
        }
        out
    }
}

/// Callback for `WalletEvent::ChainLockProcessed`.
///
/// Fires once per wallet whenever the wallet's
/// `last_applied_chain_lock` advances forward by height (or moves from
/// `None` to `Some`). Carries the full signing proof so durable
/// consumers can persist the chainlock alongside the height — important
/// for SDKs that need to reconstruct chainlock-derived state across
/// restarts (e.g. building a `ChainAssetLockProof` for an `InBlock`
/// asset-lock TX from the persisted chainlock).
///
/// `finalized` carries the per-(account, txid) promotions when the
/// same chainlock also flipped one or more `InBlock` records to
/// `InChainLockedBlock`. `finalized_count == 0` (and `finalized ==
/// NULL`) when the chainlock advanced the wallet's metadata without
/// promoting any record — consumers that persist the chainlock proof
/// must still observe these empty-promotion events.
///
/// All pointers are borrowed and only valid for the duration of the
/// callback.
pub type OnWalletChainLockProcessedCallback = Option<
    extern "C" fn(
        wallet_id: *const c_char,
        cl_height: u32,
        cl_hash: *const [u8; 32],
        cl_signature: *const [u8; 96],
        finalized: *const FFIChainlockedTxid,
        finalized_count: u32,
        user_data: *mut c_void,
    ),
>;

/// Wallet event callbacks - one callback per WalletEvent variant.
///
/// Set only the callbacks you're interested in; unset callbacks will be ignored.
///
/// All pointer parameters passed to callbacks (wallet IDs, txids, records,
/// balances) are borrowed and only valid for the duration of the callback
/// invocation. Callers must copy any data they need to retain.
#[repr(C)]
#[derive(Clone)]
pub struct FFIWalletEventCallbacks {
    pub on_transaction_detected: OnTransactionDetectedCallback,
    pub on_transaction_instant_locked: OnTransactionInstantLockedCallback,
    pub on_transactions_swept: OnTransactionsSweptCallback,
    pub on_block_processed: OnWalletBlockProcessedCallback,
    pub on_sync_height_advanced: OnSyncHeightAdvancedCallback,
    pub on_chain_lock_processed: OnWalletChainLockProcessedCallback,
    pub on_chain_truncated: OnChainTruncatedCallback,
    pub user_data: *mut c_void,
}

// SAFETY: Same rationale as FFISyncEventCallbacks
unsafe impl Send for FFIWalletEventCallbacks {}
unsafe impl Sync for FFIWalletEventCallbacks {}

impl Default for FFIWalletEventCallbacks {
    fn default() -> Self {
        Self {
            on_transaction_detected: None,
            on_transaction_instant_locked: None,
            on_transactions_swept: None,
            on_block_processed: None,
            on_sync_height_advanced: None,
            on_chain_lock_processed: None,
            on_chain_truncated: None,
            user_data: std::ptr::null_mut(),
        }
    }
}

// ============================================================================
// FFIClientErrorCallback - Fatal client-level errors
// ============================================================================

/// Callback for fatal client errors (e.g. start failure, monitor thread crash).
///
/// The `error` string pointer is borrowed and only valid for the duration
/// of the callback. Callers must copy the string if they need to retain it
/// after the callback returns.
pub type OnClientErrorCallback =
    Option<extern "C" fn(error: *const c_char, user_data: *mut c_void)>;

/// Client error callback configuration.
#[repr(C)]
#[derive(Clone)]
pub struct FFIClientErrorCallback {
    pub on_error: OnClientErrorCallback,
    pub user_data: *mut c_void,
}

unsafe impl Send for FFIClientErrorCallback {}
unsafe impl Sync for FFIClientErrorCallback {}

impl Default for FFIClientErrorCallback {
    fn default() -> Self {
        Self {
            on_error: None,
            user_data: std::ptr::null_mut(),
        }
    }
}

// ============================================================================
// FFIEventCallbacks - All callbacks in a single C-compatible struct
// ============================================================================

/// All event callbacks grouped into a single struct.
///
/// Pass this to `dash_spv_ffi_client_new`. Any callback group left at its
/// default (all function pointers null) will simply not receive events.
#[repr(C)]
#[derive(Clone, Default)]
pub struct FFIEventCallbacks {
    pub sync: FFISyncEventCallbacks,
    pub network: FFINetworkEventCallbacks,
    pub progress: FFIProgressCallback,
    pub wallet: FFIWalletEventCallbacks,
    pub error: FFIClientErrorCallback,
}

unsafe impl Send for FFIEventCallbacks {}
unsafe impl Sync for FFIEventCallbacks {}

// ============================================================================
// FFIExtEventCallbacks - Extension events, set after the client is created
// ============================================================================

/// The `version` this library writes into `FFIExtEventCallbacks`.
pub const FFI_EXT_EVENT_CALLBACKS_VERSION: u32 = 1;

/// The event bus an `on_events_resynced` call reports on.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FFIEventBus {
    Sync = 0,
    Network = 1,
    Wallet = 2,
}

/// The kind of failure that stopped the client.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FFIFailStopSource {
    Storage = 0,
    Chain = 1,
    Other = 2,
}

/// Callback for a bus whose subscriber fell behind and missed `missed` events.
pub type OnEventsResyncedCallback =
    Option<extern "C" fn(bus: FFIEventBus, missed: u64, user_data: *mut c_void)>;

/// Callback for a chain reorganisation: `disconnected` blocks above `fork_height` were replaced
/// by `connected` blocks.
pub type OnChainReorganizedCallback = Option<
    extern "C" fn(fork_height: u32, disconnected: u32, connected: u32, user_data: *mut c_void),
>;

/// Callback for the failure that stopped the client.
///
/// The `message` string is borrowed and only valid for the duration of the callback.
pub type OnFailStopCallback = Option<
    extern "C" fn(source: FFIFailStopSource, message: *const c_char, user_data: *mut c_void),
>;

/// Callback for a subscriber that stopped draining the message kinds in `commands`.
///
/// The `commands` array and its strings are borrowed and only valid for the duration of the
/// callback.
pub type OnSubscriberStalledCallback =
    Option<extern "C" fn(commands: *const *const c_char, count: u32, user_data: *mut c_void)>;

/// Callback for a configured peer that keeps failing: `failures` in a row, the last one `last`.
///
/// The `addr` and `last` strings are borrowed and only valid for the duration of the callback.
pub type OnPinnedPeerFailingCallback = Option<
    extern "C" fn(addr: *const c_char, failures: u32, last: *const c_char, user_data: *mut c_void),
>;

/// Extension event callbacks, set with `dash_spv_ffi_client_set_ext_event_callbacks`.
///
/// Fields only ever append. A caller sets `size` to `sizeof(FFIExtEventCallbacks)` as it
/// compiled it and `version` to `FFI_EXT_EVENT_CALLBACKS_VERSION`, so a library of another
/// version reads exactly the fields the caller has.
#[repr(C)]
#[derive(Clone)]
pub struct FFIExtEventCallbacks {
    pub size: usize,
    pub version: u32,
    pub user_data: *mut c_void,
    pub on_events_resynced: OnEventsResyncedCallback,
    pub on_chain_reorganized: OnChainReorganizedCallback,
    pub on_fail_stop: OnFailStopCallback,
    pub on_subscriber_stalled: OnSubscriberStalledCallback,
    pub on_pinned_peer_failing: OnPinnedPeerFailingCallback,
}

// SAFETY: Same rationale as FFISyncEventCallbacks
unsafe impl Send for FFIExtEventCallbacks {}
unsafe impl Sync for FFIExtEventCallbacks {}

impl Default for FFIExtEventCallbacks {
    fn default() -> Self {
        Self {
            size: size_of::<Self>(),
            version: FFI_EXT_EVENT_CALLBACKS_VERSION,
            user_data: std::ptr::null_mut(),
            on_events_resynced: None,
            on_chain_reorganized: None,
            on_fail_stop: None,
            on_subscriber_stalled: None,
            on_pinned_peer_failing: None,
        }
    }
}

// ============================================================================
// FFIMasternodeEventCallbacks - Masternode diagnostics, set after the client is created
// ============================================================================

/// The `version` this library writes into `FFIMasternodeEventCallbacks`.
pub const FFI_MASTERNODE_EVENT_CALLBACKS_VERSION: u32 = 1;

/// The masternode initial-sync driver's phase.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FFIInitialSyncPhase {
    Unopened = 0,
    Candidate = 1,
    Coverage = 2,
    Establishing = 3,
    Established = 4,
    Failed = 5,
}

impl From<dash_spv::InitialSyncPhase> for FFIInitialSyncPhase {
    fn from(phase: dash_spv::InitialSyncPhase) -> Self {
        use dash_spv::InitialSyncPhase;
        match phase {
            InitialSyncPhase::Unopened => Self::Unopened,
            InitialSyncPhase::Candidate => Self::Candidate,
            InitialSyncPhase::Coverage => Self::Coverage,
            InitialSyncPhase::Establishing => Self::Establishing,
            InitialSyncPhase::Established => Self::Established,
            InitialSyncPhase::Failed => Self::Failed,
        }
    }
}

/// Callback for a coverage miss: a used commitment of `llmq_type` and `quorum_hash` needs
/// history below the chain's `anchor` height. `missing` is the missing height when
/// `has_missing` is true; otherwise the base itself is unknown and `missing` is 0.
///
/// `quorum_hash` holds the hash's internal little-endian bytes, the reverse of the hex that logs
/// and RPC print. The pointer is borrowed and only valid for the duration of the callback.
pub type OnCoverageMissCallback = Option<
    extern "C" fn(
        llmq_type: u8,
        quorum_hash: *const [u8; 32],
        anchor: u32,
        has_missing: bool,
        missing: u32,
        user_data: *mut c_void,
    ),
>;

/// Callback for the masternode initial-sync driver entering `phase`.
pub type OnInitialSyncPhaseCallback =
    Option<extern "C" fn(phase: FFIInitialSyncPhase, user_data: *mut c_void)>;

/// Masternode event callbacks, set with `dash_spv_ffi_client_set_masternode_event_callbacks`.
///
/// Fields only ever append. A caller sets `size` to `sizeof(FFIMasternodeEventCallbacks)` as it
/// compiled it and `version` to `FFI_MASTERNODE_EVENT_CALLBACKS_VERSION`, so a library of
/// another version reads exactly the fields the caller has.
#[repr(C)]
#[derive(Clone)]
pub struct FFIMasternodeEventCallbacks {
    pub size: usize,
    pub version: u32,
    pub user_data: *mut c_void,
    pub on_coverage_miss: OnCoverageMissCallback,
    pub on_initial_sync_phase: OnInitialSyncPhaseCallback,
}

// SAFETY: Same rationale as FFISyncEventCallbacks
unsafe impl Send for FFIMasternodeEventCallbacks {}
unsafe impl Sync for FFIMasternodeEventCallbacks {}

impl Default for FFIMasternodeEventCallbacks {
    fn default() -> Self {
        Self {
            size: size_of::<Self>(),
            version: FFI_MASTERNODE_EVENT_CALLBACKS_VERSION,
            user_data: std::ptr::null_mut(),
            on_coverage_miss: None,
            on_initial_sync_phase: None,
        }
    }
}
