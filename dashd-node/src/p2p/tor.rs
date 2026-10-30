//! Step 12's `StartTorControl` (P8b): the controller's options from `-torcontrol`,
//! `-torpassword` and `-onion`, and its files. The onion key is read and encoded by IM1b's codec
//! (`dash_coreimport::keys::OnionV3Key`), with Core's verbatim `ReadBinaryFile` and
//! `WriteBinaryFile` for what the codec does not take (P8b D4).

use std::path::PathBuf;
use std::sync::Arc;

use dash_args::ArgsManager;
use dash_coreimport::keys::OnionV3Key;
use dash_p2p::{NetCore, Service};
use dashd_net::torcontrol::{
    self, CoreTorHost, DEFAULT_TOR_CONTROL, TorControl, TorFiles, TorOptions,
};

use crate::InitFailure;
use crate::init::interaction::{Network as ParsedNetwork, parse_network};
use crate::logging::Logger;

/// The onion key file in the network's data directory.
// Core: torcontrol.cpp:648-651 GetPrivateKeyFile
const ONION_KEY_FILE: &str = "onion_v3_private_key";

/// [`TorFiles`] for the node.
struct NodeTorFiles {
    key_path: PathBuf,
    /// `-onlynet`'s values, `None` when it is not set.
    onlynet: Option<Vec<String>>,
    logger: Arc<Logger>,
}

impl NodeTorFiles {
    fn new(args: &ArgsManager, logger: &Arc<Logger>) -> Result<Self, InitFailure> {
        Ok(Self {
            key_path: args.get_data_dir_net()?.join(ONION_KEY_FILE),
            onlynet: args.is_arg_set("-onlynet").then(|| args.get_args("-onlynet")).transpose()?,
            logger: Arc::clone(logger),
        })
    }
}

impl TorFiles for NodeTorFiles {
    // Core: torcontrol.cpp:397-403
    fn onion_allowed_by_onlynet(&self) -> bool {
        match &self.onlynet {
            None => true,
            Some(nets) => {
                nets.iter().any(|n| parse_network(&self.logger, n) == ParsedNetwork::Onion)
            }
        }
    }

    /// The key as the codec reads it (the same bytes as the file for every key Core writes);
    /// any other readable content verbatim, as Core reads it.
    // Core: torcontrol.cpp:328-333
    fn read_private_key(&self) -> Option<Vec<u8>> {
        match OnionV3Key::read(&self.key_path) {
            Ok(Some(key)) => Some(key.to_file_bytes().as_bytes().to_vec()),
            Ok(None) => Some(Vec::new()),
            Err(_) => std::fs::read(&self.key_path).ok(),
        }
    }

    /// An `ED25519-V3` key as the codec encodes it (the same bytes Tor sent), anything else
    /// verbatim; the file is replaced as Core's `WriteBinaryFile` replaces it, each time Tor
    /// answers `ADD_ONION`. The codec's own writer installs a key only where none exists.
    // Core: torcontrol.cpp:435-439, util/readwritefile.cpp:37-49
    fn write_private_key(&self, key: &[u8]) -> bool {
        let written = match OnionV3Key::parse(key) {
            Ok(Some(parsed)) => write_private(&self.key_path, parsed.to_file_bytes().as_bytes()),
            _ => write_private(&self.key_path, key),
        };
        written.is_ok()
    }

    fn private_key_path(&self) -> String {
        self.key_path.display().to_string()
    }
}

/// `WriteBinaryFile`, readable by the owner only.
fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)?.write_all(data)
}

/// Core's `StartTorControl(onion_service_target)` with step 12's options.
///
/// # Errors
///
/// An option that cannot be read.
// Core: init.cpp:2727, torcontrol.cpp:663-687
pub(super) fn start(
    rt: &tokio::runtime::Handle,
    core: &Arc<NetCore>,
    args: &ArgsManager,
    logger: &Arc<Logger>,
    target: Service,
) -> Result<TorControl, InitFailure> {
    let opts = TorOptions {
        control: args.get_arg("-torcontrol", DEFAULT_TOR_CONTROL)?,
        password: args.get_arg("-torpassword", "")?,
        ask_socks_port: args.get_arg("-onion", "")?.is_empty(),
        default_port: core.config().params.default_port,
        target,
    };
    let files = NodeTorFiles::new(args, logger)?;
    let host = Arc::new(CoreTorHost::new(Arc::clone(core), files));
    let name_lookup = core.config().name_lookup;
    Ok(torcontrol::start(rt, Arc::clone(core), opts, host, name_lookup))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `-noonlynet` sets `-onlynet` with no values, so onion is not allowed; `tor_diff` checks
    /// `-onlynet=ipv4` and `-onlynet=onion` against dashd.
    // Core: torcontrol.cpp:397-403
    #[test]
    fn onion_allowed_by_onlynet() {
        let logger = Arc::new(Logger::new());
        for (argv, allowed) in [
            (&[][..], true),
            (&["-noonlynet"][..], false),
            (&["-onlynet=ipv4"][..], false),
            (&["-onlynet=ipv4", "-onlynet=onion"][..], true),
            // The deprecated alias (netbase.cpp:104-107); tor_diff compares its warning lines.
            (&["-onlynet=tor"][..], true),
        ] {
            let args = crate::init::test_args(argv);
            let files = NodeTorFiles::new(&args, &logger).unwrap();
            assert_eq!(files.onion_allowed_by_onlynet(), allowed, "{argv:?}");
        }
    }
}
