//! The option tables of Dash Core's programs.
//!
//! `generated.rs` holds Core's setup functions (`SetupServerArgs`, `SetupCliArgs`, ...),
//! generated from Core's sources by `gen/gen_options.py`; see `gen/PROVENANCE.md`.

#[rustfmt::skip]
mod generated;

pub use generated::*;

use crate::args::ArgsManager;

/// A Dash Core program whose options can be registered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Program {
    /// `dashd` (Core: `SetupServerArgs`).
    Dashd,
    /// `dash-qt` (Core: `SetupServerArgs` then `SetupUIArgs`).
    DashQt,
    /// `dash-cli` (Core: `SetupCliArgs`).
    DashCli,
    /// `dash-tx` (Core: `SetupBitcoinTxArgs`).
    DashTx,
    /// `dash-wallet` (Core: `SetupWalletToolArgs`).
    DashWallet,
    /// `dash-util` (Core: `SetupBitcoinUtilArgs`).
    DashUtil,
}

impl Program {
    pub const ALL: [Program; 6] = [
        Program::Dashd,
        Program::DashQt,
        Program::DashCli,
        Program::DashTx,
        Program::DashWallet,
        Program::DashUtil,
    ];

    /// Register the program's options, as its `main` does before parsing the command line.
    pub fn setup_args(self, argsman: &ArgsManager) {
        match self {
            Program::Dashd => setup_server_args(argsman),
            Program::DashQt => {
                setup_server_args(argsman);
                setup_ui_args(argsman);
            }
            Program::DashCli => setup_cli_args(argsman),
            Program::DashTx => setup_bitcoin_tx_args(argsman),
            Program::DashWallet => setup_wallet_tool_args(argsman),
            Program::DashUtil => setup_bitcoin_util_args(argsman),
        }
    }
}
