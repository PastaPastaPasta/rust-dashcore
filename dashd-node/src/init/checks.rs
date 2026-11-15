//! Core's `AppInitParameterInteraction` step 2: options that conflict, and the connection limit
//! the file descriptors allow.

use dash_args::ArgsManager;

use super::consts::*;
use crate::InitFailure;
use crate::ui::Ui;

fn error(msg: impl Into<String>) -> InitFailure {
    InitFailure::Error(msg.into())
}

/// What step 2 decides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetSetup {
    /// `g_enabled_filter_types` holds BASIC (the only filter type).
    pub basic_filter_index: bool,
    /// `NODE_P2P_V2` is signalled.
    pub v2_transport: bool,
    /// `NODE_COMPACT_FILTERS` is signalled.
    pub compact_filters: bool,
    /// `fAllowPrivateNet`.
    pub allow_private_net: bool,
    /// `nUserMaxConnections` (the option, as an `int`).
    pub user_max_connections: i32,
    /// `nMaxConnections` after the descriptor limit.
    pub max_connections: i32,
    /// `nFD`: the descriptor limit.
    pub fd: i32,
}

/// Step 2 of Core's `AppInitParameterInteraction`.
// Core: init.cpp:1188-1305
pub fn parameter_interaction(ui: &Ui, args: &ArgsManager) -> Result<NetSetup, InitFailure> {
    if let Some(errors) = dash_args::init::unsuitable_section_only_args_error(args)? {
        return Err(error(errors));
    }
    if let Some(warnings) = dash_args::init::unrecognized_sections_warning(args) {
        ui.init_warning(&warnings);
    }

    if !args.get_blocks_dir_path()?.is_dir() {
        return Err(error(format!(
            "Specified blocks directory \"{}\" does not exist.",
            args.get_arg("-blocksdir", "")?
        )));
    }

    // BASIC is the only filter type.
    let filter_index = args.get_arg("-blockfilterindex", DEFAULT_BLOCKFILTERINDEX)?;
    let basic_filter_index = match filter_index.as_str() {
        "" | "1" => true,
        "0" => false,
        _ => {
            for name in args.get_args("-blockfilterindex")? {
                if name != "basic" {
                    return Err(error(format!("Unknown -blockfilterindex value {name}.")));
                }
            }
            true
        }
    };

    let v2_transport = args.get_bool_arg("-v2transport", DEFAULT_V2_TRANSPORT)?;

    let compact_filters = args.get_bool_arg("-peerblockfilters", DEFAULT_PEERBLOCKFILTERS)?;
    if compact_filters && !basic_filter_index {
        return Err(error("Cannot set -peerblockfilters without -blockfilterindex."));
    }

    if args.get_int_arg("-prune", 0)? != 0 {
        if args.get_bool_arg("-txindex", DEFAULT_TXINDEX)? {
            return Err(error("Prune mode is incompatible with -txindex."));
        }
        if args.get_bool_arg("-addressindex", DEFAULT_ADDRESSINDEX)? {
            return Err(error("Prune mode is incompatible with -addressindex."));
        }
        if args.get_bool_arg("-spentindex", DEFAULT_SPENTINDEX)? {
            return Err(error("Prune mode is incompatible with -spentindex."));
        }
        if args.get_bool_arg("-reindex-chainstate", false)? {
            return Err(error(
                "Prune mode is incompatible with -reindex-chainstate. Use full -reindex instead.",
            ));
        }
        if !args.get_bool_arg("-disablegovernance", !DEFAULT_GOVERNANCE_ENABLE)? {
            return Err(error("Prune mode is incompatible with -disablegovernance=false."));
        }
    }

    if args.is_arg_set("-devnet") {
        // A devnet has no default ports.
        if args.get_bool_arg("-listen", DEFAULT_LISTEN)? && !args.is_arg_set("-port") {
            return Err(error("-port must be specified when -devnet and -listen are specified"));
        }
        if args.get_bool_arg("-server", false)? && !args.is_arg_set("-rpcport") {
            return Err(error("-rpcport must be specified when -devnet and -server are specified"));
        }
        if args.get_args("-devnet")?.len() > 1 {
            return Err(error("-devnet can only be specified once"));
        }
    }

    let allow_private_net = args.get_bool_arg("-allowprivatenet", DEFAULT_ALLOWPRIVATENET)?;

    if args.get_bool_arg("-forcednsseed", DEFAULT_FORCEDNSSEED)?
        && !args.get_bool_arg("-dnsseed", DEFAULT_DNSSEED)?
    {
        return Err(error("Cannot set -forcednsseed to true when setting -dnsseed to false."));
    }

    let listen = args.get_bool_arg("-listen", DEFAULT_LISTEN)?;
    let user_bind = args.get_args("-bind")?.len() + args.get_args("-whitebind")?.len();
    if user_bind != 0 && !listen {
        return Err(error("Cannot set -bind or -whitebind together with -listen=0"));
    }
    if !listen && args.get_bool_arg("-listenonion", DEFAULT_LISTEN_ONION)? {
        return Err(error("Cannot set -listen=0 together with -listenonion=1"));
    }

    // Core's `int` arithmetic, truncations included.
    let bind = i32::try_from(user_bind.max(1)).unwrap_or(i32::MAX);
    let user_max_connections =
        args.get_int_arg("-maxconnections", DEFAULT_MAX_PEER_CONNECTIONS)? as i32;
    let mut max_connections = user_max_connections.max(0);
    let reserved = MIN_CORE_FILEDESCRIPTORS + MAX_ADDNODE_CONNECTIONS + NUM_FDS_MESSAGE_CAPTURE;
    let fd = raise_file_descriptor_limit(max_connections.wrapping_add(reserved).wrapping_add(bind));
    let fd_max = if cfg!(any(target_os = "linux", target_os = "freebsd")) {
        fd // USE_POLL
    } else {
        1024 // FD_SETSIZE (compat.h sets Windows' to 1024 too)
    };
    max_connections = max_connections.min(fd_max.wrapping_sub(bind).wrapping_sub(reserved)).max(0);
    if fd < MIN_CORE_FILEDESCRIPTORS {
        return Err(error("Not enough file descriptors available."));
    }
    max_connections = max_connections.min(fd - reserved);
    if max_connections < user_max_connections {
        ui.init_warning(&format!(
            "Reducing -maxconnections from {user_max_connections} to {max_connections}, because of system limitations."
        ));
    }

    Ok(NetSetup {
        basic_filter_index,
        v2_transport,
        compact_filters,
        allow_private_net,
        user_max_connections,
        max_connections,
        fd,
    })
}

/// Core's `RaiseFileDescriptorLimit`: raises the soft `RLIMIT_NOFILE` towards `min_fd` (capped
/// at the hard limit) and returns the soft limit as an `int`, as Core does.
// Core: util/fs_helpers.cpp:214-232
pub fn raise_file_descriptor_limit(min_fd: i32) -> i32 {
    #[cfg(unix)]
    {
        // SAFETY: getrlimit and setrlimit write only the struct we pass.
        unsafe {
            let mut limit: libc::rlimit = std::mem::zeroed();
            if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) == -1 {
                return min_fd;
            }
            if limit.rlim_cur < min_fd as libc::rlim_t {
                limit.rlim_cur = (min_fd as libc::rlim_t).min(limit.rlim_max);
                libc::setrlimit(libc::RLIMIT_NOFILE, &limit);
                libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit);
            }
            limit.rlim_cur as i32
        }
    }
    #[cfg(not(unix))]
    {
        let _ = min_fd;
        2048
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init::{test_args, test_ui};

    fn check(argv: &[&str]) -> Result<NetSetup, InitFailure> {
        let (ui, _) = test_ui();
        parameter_interaction(&ui, &test_args(argv))
    }

    fn err(argv: &[&str]) -> String {
        match check(argv) {
            Err(InitFailure::Error(e)) => e,
            other => panic!("{argv:?}: {other:?}"),
        }
    }

    #[test]
    fn conflicts_give_core_s_texts() {
        assert_eq!(err(&["-blockfilterindex=foo"]), "Unknown -blockfilterindex value foo.");
        assert_eq!(
            err(&["-peerblockfilters"]),
            "Cannot set -peerblockfilters without -blockfilterindex."
        );
        assert_eq!(err(&["-prune=550"]), "Prune mode is incompatible with -txindex.");
        assert_eq!(
            err(&["-prune=550", "-txindex=0", "-addressindex"]),
            "Prune mode is incompatible with -addressindex."
        );
        assert_eq!(
            err(&["-prune=550", "-txindex=0", "-spentindex"]),
            "Prune mode is incompatible with -spentindex."
        );
        assert_eq!(
            err(&["-prune=550", "-txindex=0", "-reindex-chainstate"]),
            "Prune mode is incompatible with -reindex-chainstate. Use full -reindex instead."
        );
        assert_eq!(
            err(&["-prune=550", "-txindex=0"]),
            "Prune mode is incompatible with -disablegovernance=false."
        );
        assert_eq!(
            err(&["-forcednsseed", "-dnsseed=0"]),
            "Cannot set -forcednsseed to true when setting -dnsseed to false."
        );
        assert_eq!(
            err(&["-bind=127.0.0.1", "-listen=0"]),
            "Cannot set -bind or -whitebind together with -listen=0"
        );
        assert_eq!(
            err(&["-listen=0", "-listenonion=1"]),
            "Cannot set -listen=0 together with -listenonion=1"
        );
    }

    #[test]
    fn filter_index_values() {
        assert!(check(&["-blockfilterindex"]).unwrap().basic_filter_index);
        assert!(check(&["-blockfilterindex=basic", "-peerblockfilters"]).unwrap().compact_filters);
        assert!(!check(&[]).unwrap().basic_filter_index);
    }

    #[test]
    fn connections_fit_the_descriptor_limit() {
        let setup = check(&["-listen=0", "-listenonion=0"]).unwrap();
        assert_eq!(setup.user_max_connections, 125);
        assert!(setup.fd >= MIN_CORE_FILEDESCRIPTORS);
        assert!(setup.max_connections <= 125);
        let setup = check(&["-maxconnections=-5"]).unwrap();
        assert_eq!(setup.max_connections, 0);
    }
}
