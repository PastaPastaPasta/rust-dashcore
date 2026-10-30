//! The wallet backup layout (`Wallet::backup` / `Wallet::restore`) and the envelope
//! v1 wallet file (`Wallet::to_bytes` / `Wallet::from_bytes`) against the
//! `wallet_layout_v0` goldens. See `support/wallet_layout.rs`; the serde snapshots
//! are checked in key-wallet-ffi.

#[path = "support/wallet_layout.rs"]
mod wallet_layout;

#[test]
fn goldens_match_layout_v0() {
    wallet_layout::check_all_goldens(None);
}

#[test]
fn envelope_goldens_match() {
    wallet_layout::check_envelope_goldens();
}
