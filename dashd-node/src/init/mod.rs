//! Core's init steps (`init.cpp`): chain selection, parameter interaction and the init errors.

pub mod caches;
pub mod checks;
pub mod consts;
pub mod flags;
pub mod interaction;
pub mod mempool;
pub mod params;
pub mod parse;
pub mod support;

pub use checks::{NetSetup, parameter_interaction};
pub use flags::{Flags, Prune, internal_flags};
pub use interaction::init_parameter_interaction;
pub use params::{SelectedParams, select_params};
pub use support::{setup_server_args, unsupported_option};

use std::cell::RefCell;

use dash_args::{ArgsManager, RuntimeError};
use dash_clock::MockClock;
use dash_params::ChainParams;

use crate::ui::Ui;
use crate::{InitFailure, datadir};

/// What `AppInitParameterInteraction` decides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamInteraction {
    pub net: NetSetup,
    pub flags: Flags,
}

/// Core's `AppInitParameterInteraction`: step 2, then step 3.
// Core: init.cpp:1185-1453
pub fn app_init_parameter_interaction(
    ui: &Ui,
    args: &ArgsManager,
    chain: &ChainParams,
    clock: &MockClock,
) -> Result<ParamInteraction, InitFailure> {
    let net = parameter_interaction(ui, args)?;
    let flags = internal_flags(ui, args, chain, &net, clock)?;
    Ok(ParamInteraction {
        net,
        flags,
    })
}

/// Core's `InitConfig` for dashd (`bitcoind.cpp` `ParseArgs`), with `SelectParams`: reads the
/// config file, selects the chain and writes the settings file. A Dash Core data directory is
/// refused before anything is written to it (DEC-201 Q6): the base data directory before the
/// config file is read (which creates an empty `dash.conf`), and the network's (which the config
/// file may move) before its directories and settings file are created. A LevelDB-shaped database
/// is confirmed under that directory's lock: one another process holds gets Core's lock error
/// instead (`datadir::refuse_core_dir`).
// Core: bitcoind.cpp:122-124, common/init.cpp:16-90
pub fn init_config(args: &ArgsManager) -> Result<SelectedParams, InitFailure> {
    datadir::refuse_core_base_dir(args)?;
    let selected = RefCell::new(None);
    let select = |args: &ArgsManager, chain: &str| {
        let params = select_params(args, chain)?;
        datadir::refuse_core_dir(&args.get_data_dir_net()?).map_err(RuntimeError)?;
        *selected.borrow_mut() = Some(params);
        Ok(())
    };
    dash_args::init::init_config(args, &select, None).map_err(|e| {
        InitFailure::Error(dash_args::init::init_error_text(&e.message, &e.details))
    })?;
    Ok(selected.into_inner().expect("init_config selects the chain"))
}

#[cfg(test)]
pub(crate) use test_support::*;

#[cfg(test)]
mod test_support {
    use std::ops::Deref;
    use std::sync::{Arc, Mutex};

    use dash_args::ArgsManager;

    use super::setup_server_args;
    use crate::logging::{LogOptions, Logger};
    use crate::ui::Ui;

    /// Arguments parsed as `dashd` parses them, on a private data directory (regtest unless
    /// `argv` names another chain).
    pub(crate) struct TestArgs {
        args: ArgsManager,
        _dir: tempfile::TempDir,
    }

    impl Deref for TestArgs {
        type Target = ArgsManager;
        fn deref(&self) -> &ArgsManager {
            &self.args
        }
    }

    pub(crate) fn test_args(argv: &[&str]) -> TestArgs {
        let dir = tempfile::tempdir().unwrap();
        let args = ArgsManager::new();
        setup_server_args(&args);
        let datadir = format!("-datadir={}", dir.path().display());
        let mut full = vec!["dashd", datadir.as_str()];
        let names_chain = argv.iter().any(|a| {
            ["-regtest", "-testnet", "-devnet", "-chain="].iter().any(|c| a.starts_with(c))
        });
        if !names_chain {
            full.push("-regtest");
        }
        full.extend_from_slice(argv);
        args.parse_parameters(&full).unwrap();
        args.read_config_files(false).unwrap();
        args.select_base_params(&args.get_chain_name().unwrap()).unwrap();
        // `InitConfig` creates it.
        std::fs::create_dir_all(args.get_data_dir_net().unwrap()).unwrap();
        TestArgs {
            args,
            _dir: dir,
        }
    }

    pub(crate) type Lines = Arc<Mutex<Vec<String>>>;

    /// The arguments an in-process test node adds to `argv`: no RPC server, and `-listen=0`
    /// unless `argv` names a listening option or one whose parameter interaction decides
    /// `-listen` (tests run in parallel, and Core's default binds fixed ports).
    pub(crate) fn quiet_node_args(argv: &[&str]) -> Vec<&'static str> {
        let decides_listen = argv.iter().any(|a| {
            let name = a.trim_start_matches('-').split('=').next().unwrap_or_default();
            matches!(name, "listen" | "nolisten" | "bind" | "whitebind" | "connect" | "noconnect")
        });
        let mut extra = vec!["-server=0"];
        if !decides_listen {
            extra.push("-listen=0");
        }
        extra
    }

    /// A started logger without timestamps whose lines collect in `Lines`.
    pub(crate) fn test_logger() -> (Arc<Logger>, Lines) {
        let logger = Logger::new();
        logger.set_options(LogOptions {
            log_timestamps: false,
            ..LogOptions::default()
        });
        let lines = Lines::default();
        let sink = Arc::clone(&lines);
        logger.push_back_callback(move |s| sink.lock().unwrap().push(s.to_owned()));
        assert!(logger.start_logging());
        (Arc::new(logger), lines)
    }

    /// A [`Ui`] on [`test_logger`] whose stderr text collects in `Lines`.
    pub(crate) fn test_ui() -> (Ui, Lines) {
        let (logger, _) = test_logger();
        let stderr = Lines::default();
        let sink = Arc::clone(&stderr);
        let ui =
            Ui::with_stderr(logger, Box::new(move |s| sink.lock().unwrap().push(s.to_owned())));
        (ui, stderr)
    }
}
