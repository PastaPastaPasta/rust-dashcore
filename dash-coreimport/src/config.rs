//! `dash.conf` import (IM1b; FR-IMP-04, US-IMP-07): a Core configuration file read into the
//! effective settings, without writing anything.
//!
//! The parsing is `dash-args`' port of Core's `ArgsManager` (`ReadConfigStream`,
//! `GetConfigOptions`, the option tables of `dashd`): comments, `[section]` headers, `key=value`
//! lines, `-no` negation, the network sections, `NETWORK_ONLY` options, and Core's errors for
//! unknown options and malformed lines. What this module adds is the file-level policy of an
//! importer:
//!
//! - the file is opened first and refused unless it is a regular file, and at most
//!   [`MAX_DASH_CONF_SIZE`] bytes are read ([`read_dash_conf`]);
//! - the parser is `ReadConfigStream`, not `ReadConfigFiles`: that one **creates** an empty
//!   `dash.conf` when none exists, which an importer must never do to a Core datadir;
//! - `includeconf` is **not followed**. The values are kept in the settings like any other
//!   (`includeconf` is a normal repeatable option), and [`ImportedConfig::includes`] lists the
//!   files Core would include, in Core's order, so that the caller can decide whether to read them
//!   (each one is an arbitrary path chosen by the file being imported);
//! - the file must be UTF-8. Core reads bytes, and a value with other bytes is representable
//!   there but not in `dash-args`' strings; such a file is refused rather than altered.
//!
//! Unknown options are an error unless `ignore_invalid_keys` is set. `dashd` reads its own file
//! with it **on** (`common::InitConfig` calls `ReadConfigFiles(error, true)`: an unknown key is
//! logged and ignored), so an importer that wants to accept what `dashd` starts with passes
//! `true`; `false` is the strict reading that refuses a file with an unknown key. A parse error
//! quotes the offending
//! line, as Core's does, so an importer should not show it where a line may hold a password. The
//! diagnostics Core would log are returned in [`ImportedConfig::log`].

use std::path::Path;

pub use dash_args::SettingsValue;
use dash_args::{ArgsManager, Program};
use dash_network::Network;

use crate::netstate::{FormatError, LoadError, read_file_capped};

const WHAT: &str = "dash.conf";

/// A `dash.conf` is a few dozen lines; dashmate's template is under 4 KiB.
pub const MAX_DASH_CONF_SIZE: usize = 1 << 20;

/// A configuration file read into `dash-args`.
pub struct ImportedConfig {
    args: ArgsManager,
    /// The files Core would include, in the order it would read them: the values of
    /// `includeconf` in the network's section after the last `noincludeconf` of that section,
    /// then those of the top section after the last `noincludeconf` of it (`ReadConfigFiles`'s
    /// `add_includes`). Not followed; see the module documentation.
    pub includes: Vec<String>,
    /// The lines `dash-args` logged while reading (Core's `LogPrintf` and warnings).
    pub log: Vec<String>,
}

impl ImportedConfig {
    /// The settings as `ArgsManager` serves them (`GetArg`, `GetBoolArg`, `GetArgs`, ...), for the
    /// network section the file was read for. No command-line arguments are set.
    pub fn args(&self) -> &ArgsManager {
        &self.args
    }

    /// The raw settings: section (`""` for the top of the file) to option name to its values in
    /// file order. A negation (`nofoo=1`) is a `false` value of `foo`.
    pub fn sections(
        &self,
    ) -> std::collections::BTreeMap<String, std::collections::BTreeMap<String, Vec<SettingsValue>>>
    {
        self.args.lock_settings(|s| s.ro_config.clone())
    }
}

impl std::fmt::Debug for ImportedConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportedConfig").field("includes", &self.includes).finish_non_exhaustive()
    }
}

/// The `-chain` name `dash-args` uses for a network section: `GetChainName()`, which is
/// `"devnet"` for every devnet (the devnet's own name only picks its datadir).
fn section_name(network: Network) -> &'static str {
    match network {
        Network::Mainnet => dash_args::chain::MAIN,
        Network::Testnet => dash_args::chain::TESTNET,
        Network::Regtest => dash_args::chain::REGTEST,
        Network::Devnet => dash_args::chain::DEVNET,
    }
}

/// `ReadConfigFiles`' `add_includes` for one section: the values after the last negation.
// Core: util/system.cpp ArgsManager::ReadConfigFiles, util/settings.cpp SettingsSpan::negated
fn section_includes(
    sections: &std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, Vec<SettingsValue>>,
    >,
    section: &str,
    out: &mut Vec<String>,
) {
    let Some(values) = sections.get(section).and_then(|options| options.get("includeconf")) else {
        return;
    };
    let negated =
        values.iter().rposition(|v| matches!(v, SettingsValue::Bool(false))).map_or(0, |i| i + 1);
    for value in &values[negated..] {
        if let SettingsValue::Str(path) = value {
            out.push(path.clone());
        }
    }
}

/// Reads the `dash.conf` text `bytes` for `network`.
// Core: util/system.cpp ArgsManager::ReadConfigStream, GetConfigOptions
pub fn parse_dash_conf(
    bytes: &[u8],
    network: Network,
    ignore_invalid_keys: bool,
) -> Result<ImportedConfig, FormatError> {
    if bytes.len() > MAX_DASH_CONF_SIZE {
        return Err(FormatError::TooLarge {
            what: WHAT,
            size: bytes.len() as u64,
            limit: MAX_DASH_CONF_SIZE as u64,
        });
    }
    let text = std::str::from_utf8(bytes).map_err(|_| FormatError::Corrupt {
        what: WHAT,
        reason: "not valid UTF-8",
    })?;
    let args = ArgsManager::new();
    Program::Dashd.setup_args(&args);
    let section = section_name(network);
    args.select_config_network(section);
    args.read_config_stream(text, WHAT, ignore_invalid_keys).map_err(|message| {
        FormatError::Nested {
            what: WHAT,
            message,
        }
    })?;
    let mut includes = Vec::new();
    let sections = args.lock_settings(|s| s.ro_config.clone());
    section_includes(&sections, section, &mut includes);
    section_includes(&sections, "", &mut includes);
    let log = args.take_buffered_log();
    Ok(ImportedConfig {
        args,
        includes,
        log,
    })
}

/// Reads the file at `path` (see [`parse_dash_conf`]).
pub fn read_dash_conf(
    path: &Path,
    network: Network,
    ignore_invalid_keys: bool,
) -> Result<ImportedConfig, LoadError> {
    let bytes = read_file_capped(path, MAX_DASH_CONF_SIZE, WHAT)?;
    Ok(parse_dash_conf(&bytes, network, ignore_invalid_keys)?)
}
