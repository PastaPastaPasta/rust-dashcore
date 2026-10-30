//! Wallet layout goldens (`key-wallet/tests/data/wallet_layout_v0/`), generated and
//! checked here under `bincode,bls,eddsa` so that every fixture carries BLS and
//! EdDSA accounts, plus the serde snapshots (see `support/wallet_layout.rs` for how
//! to regenerate). Also drives each golden through the FFI import path.

#![cfg(all(feature = "bincode", feature = "bls", feature = "eddsa"))]

#[path = "../../key-wallet/tests/support/wallet_layout.rs"]
mod wallet_layout;

use dash_network::ffi::FFINetwork;
use key_wallet::account::ECDSAAddressDerivation;
use key_wallet::Wallet;
use key_wallet_ffi::error::{FFIError, FFIErrorCode};
use key_wallet_ffi::wallet::wallet_free_const;
use key_wallet_ffi::wallet_manager::{
    wallet_manager_create, wallet_manager_free, wallet_manager_get_wallet,
    wallet_manager_import_wallet_from_bytes,
};

fn serde_encode(wallet: &Wallet) -> Vec<u8> {
    bincode::serde::encode_to_vec(wallet, bincode::config::standard()).expect("serde encode")
}

fn serde_decode(bytes: &[u8]) -> Wallet {
    let (wallet, read): (Wallet, usize) =
        bincode::serde::decode_from_slice(bytes, bincode::config::standard())
            .expect("serde decode");
    assert_eq!(read, bytes.len(), "trailing bytes after the wallet");
    wallet
}

#[test]
fn goldens_match_layout_v0() {
    wallet_layout::check_all_goldens(Some(wallet_layout::Snapshot {
        suffix: "serde",
        encode: serde_encode,
        decode: serde_decode,
    }));
}

#[test]
fn envelope_goldens_match() {
    wallet_layout::check_envelope_goldens();
}

fn first_receive_addresses(wallet: &Wallet) -> Vec<String> {
    let account = wallet.accounts.standard_bip44_accounts.get(&0).expect("BIP44 account 0");
    account
        .derive_receive_addresses(0, 20)
        .expect("addresses")
        .iter()
        .map(|a| a.to_string())
        .collect()
}

/// `wallet_manager_import_wallet_from_bytes` on the exact golden bytes (legacy and
/// envelope) yields the fixture's wallet id, accounts and first 20 receive addresses.
#[test]
fn ffi_import_of_each_golden() {
    if wallet_layout::regen() {
        return;
    }
    let legacy = wallet_layout::VARIANTS
        .map(|v| (v, wallet_layout::fixture(v), wallet_layout::read_golden(v)));
    let envelope = wallet_layout::ENVELOPE_VARIANTS
        .map(|v| (v, wallet_layout::envelope_fixture(v), wallet_layout::read_envelope_golden(v)));
    for (variant, expected, golden) in legacy.into_iter().chain(envelope) {
        unsafe {
            let mut error = FFIError::default();
            let manager = wallet_manager_create(FFINetwork::Testnet, &mut error);
            assert_eq!(error.code, FFIErrorCode::Success);

            let mut wallet_id = [0u8; 32];
            let ok = wallet_manager_import_wallet_from_bytes(
                manager,
                golden.as_ptr(),
                golden.len(),
                0,
                wallet_id.as_mut_ptr(),
                &mut error,
            );
            assert!(ok, "{variant}: import failed: {:?}", error.code);
            assert_eq!(wallet_id, expected.wallet_id, "{variant}: wallet id");

            let handle = wallet_manager_get_wallet(manager, wallet_id.as_ptr(), &mut error);
            assert!(!handle.is_null(), "{variant}: get_wallet");
            let imported = (*handle).inner();
            wallet_layout::assert_wallet_eq(imported, &expected);
            assert_eq!(imported.accounts.count(), expected.accounts.count(), "{variant}: count");
            assert_eq!(
                first_receive_addresses(imported),
                first_receive_addresses(&expected),
                "{variant}: receive addresses"
            );

            wallet_free_const(handle);
            wallet_manager_free(manager);
        }
    }
}

/// Compact JSON with every object's keys sorted. `AddressPool::address_index` is a
/// `HashMap`, which serde_json writes in hash order (random per process), and
/// `preserve_order` (enabled in the workspace by rpc-json) would keep that order in
/// a `Value`; sorting makes the snapshot deterministic in every build.
fn canonical_json(value: serde_json::Value) -> Vec<u8> {
    fn sort(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let mut entries: Vec<_> = map.into_iter().collect();
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                serde_json::Value::Object(entries.into_iter().map(|(k, v)| (k, sort(v))).collect())
            }
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.into_iter().map(sort).collect())
            }
            other => other,
        }
    }
    serde_json::to_vec(&sort(value)).expect("write JSON")
}

/// The serde_json snapshots of D89 (`account_free.json`, `managed_account_free.json`).
#[test]
fn json_snapshots_match() {
    wallet_layout::check_json_snapshots(
        wallet_layout::JsonSnapshot {
            name: "account_free.json",
            encode: |wallet| canonical_json(serde_json::to_value(wallet).expect("encode wallet")),
            decode: |bytes| serde_json::from_slice(bytes).expect("decode account_free.json"),
            expected: None,
        },
        wallet_layout::JsonSnapshot {
            name: "managed_account_free.json",
            encode: |info| canonical_json(serde_json::to_value(info).expect("encode info")),
            decode: |bytes| {
                serde_json::from_slice(bytes).expect("decode managed_account_free.json")
            },
            expected: if cfg!(feature = "keep-finalized-transactions") {
                Some(without_finalized_txids)
            } else {
                None
            },
        },
    );
}

/// `managed_account_free.json` as a build with `keep-finalized-transactions` writes it:
/// that feature (which dash-spv-ffi turns on through this crate, so a workspace build has
/// it) compiles out the keys accounts' `finalized_txids`, empty in the fixture.
fn without_finalized_txids(golden: &[u8]) -> Vec<u8> {
    fn strip(value: &mut serde_json::Value, removed: &mut usize) {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(txids) = map.remove("finalized_txids") {
                    assert_eq!(txids, serde_json::json!([]), "finalized_txids");
                    *removed += 1;
                }
                map.values_mut().for_each(|v| strip(v, removed));
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(|v| strip(v, removed)),
            _ => {}
        }
    }
    let mut value = serde_json::from_slice(golden).expect("decode managed_account_free.json");
    let mut removed = 0;
    strip(&mut value, &mut removed);
    assert!(removed > 0, "managed_account_free.json has no finalized_txids");
    canonical_json(value)
}
