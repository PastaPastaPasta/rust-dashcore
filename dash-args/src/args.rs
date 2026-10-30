//! `ArgsManager`: option registration, command-line and config-file parsing, and the getters.
//!
//! Core: src/util/system.{h,cpp} at v24.0.0-rc.3. Function names, error strings, log lines
//! and evaluation order follow Core. Where Core throws (a `UniValue` type mismatch in a getter,
//! a conflicting chain selection, a `std::filesystem` failure), the Rust method returns
//! `Err(RuntimeError)` carrying Core's `what()` text; where Core asserts, the Rust method
//! panics.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use crate::chain::{self, BaseChainParams};
use crate::paths::{self, DASH_CONF_FILENAME, DASH_SETTINGS_FILENAME, path_to_string};
use crate::settings::{self, Settings, SettingsSpan};
use crate::strencodings::{format_paragraph, interpret_bool, locale_independent_atoi, trim_string};
use crate::univalue::{RuntimeError, SettingsValue};

/// Option categories, in the order `GetHelpMessage` prints them (Core: `OptionsCategory`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OptionsCategory {
    Options,
    Connection,
    Indexing,
    Masternode,
    Statsd,
    Wallet,
    WalletFee,
    WalletHd,
    WalletCoinjoin,
    WalletDebugTest,
    Zmq,
    DebugTest,
    Chainparams,
    NodeRelay,
    BlockCreation,
    Rpc,
    Gui,
    Commands,
    RegisterCommands,
    /// Always last, so the help stops before these.
    Hidden,
}

impl OptionsCategory {
    /// The help group header (Core: the `switch` in `GetHelpMessage`).
    fn header(self, show_debug: bool) -> Option<&'static str> {
        use OptionsCategory::*;
        Some(match self {
            Options => "Options:",
            Connection => "Connection options:",
            Indexing => "Indexing options:",
            Masternode => "Masternode options:",
            Statsd => "Statsd options:",
            Zmq => "ZeroMQ notification options:",
            DebugTest => "Debugging/Testing options:",
            NodeRelay => "Node relay options:",
            BlockCreation => "Block creation options:",
            Rpc => "RPC server options:",
            Wallet => "Wallet options:",
            WalletFee => "Wallet fee options:",
            WalletHd => "HD wallet options:",
            WalletCoinjoin => "CoinJoin options:",
            WalletDebugTest if show_debug => "Wallet debugging/testing options:",
            Chainparams => "Chain selection options:",
            Gui => "UI Options:",
            Commands => "Commands:",
            RegisterCommands => "Register Commands:",
            WalletDebugTest | Hidden => return None,
        })
    }
}

/// Where a section header (or a `section.name=` key) appeared in a config file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionInfo {
    pub name: String,
    pub file: String,
    pub line: i32,
}

/// Core: `ArgsManager::Command`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Command {
    /// The command (if one has been registered with `add_command`), or empty.
    pub command: String,
    /// If `command` is non-empty, the args that followed it; otherwise the unregistered command
    /// and the args that followed it.
    pub args: Vec<String>,
}

/// A registered option or command (Core: an `m_available_args` entry).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisteredArg {
    pub category: OptionsCategory,
    /// The name, with its leading `-` for options (`-datadir`, or `info` for a command).
    pub name: String,
    /// The help parameter, starting with `=` (`=<dir>`), or empty.
    pub help_param: String,
    pub help_text: String,
    pub flags: u32,
}

#[derive(Clone, Debug)]
struct Arg {
    help_param: String,
    help_text: String,
    flags: u32,
}

/// Core: `KeyInfo`, a parsed `name`, `section.name`, `noname` or `section.noname` key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyInfo {
    pub name: String,
    pub section: String,
    pub negated: bool,
}

/// Core: `InterpretKey`.
pub fn interpret_key(key: &str) -> KeyInfo {
    let mut result = KeyInfo::default();
    let mut key = key;
    // Split section name from key name for keys like "testnet.foo" or "regtest.bar".
    if let Some(option_index) = key.find('.') {
        result.section = key[..option_index].to_string();
        key = &key[option_index + 1..];
    }
    if let Some(rest) = key.strip_prefix("no") {
        key = rest;
        result.negated = true;
    }
    result.name = key.to_string();
    result
}

/// Core: `SettingName`: strips one leading `-`.
fn setting_name(arg: &str) -> &str {
    arg.strip_prefix('-').unwrap_or(arg)
}

/// Core: `SettingToString`.
pub fn setting_to_string(value: &SettingsValue) -> Result<Option<String>, RuntimeError> {
    Ok(Some(match value {
        SettingsValue::Null => return Ok(None),
        SettingsValue::Bool(false) => "0".to_string(),
        SettingsValue::Bool(true) => "1".to_string(),
        SettingsValue::Num(_) => value.get_val_str().to_string(),
        _ => value.get_str()?.to_string(),
    }))
}

/// Core: `SettingToInt`.
pub fn setting_to_int(value: &SettingsValue) -> Result<Option<i64>, RuntimeError> {
    Ok(Some(match value {
        SettingsValue::Null => return Ok(None),
        SettingsValue::Bool(b) => i64::from(*b),
        SettingsValue::Num(_) => value.get_int64()?,
        _ => locale_independent_atoi(value.get_str()?),
    }))
}

/// Core: `SettingToBool`.
pub fn setting_to_bool(value: &SettingsValue) -> Result<Option<bool>, RuntimeError> {
    Ok(Some(match value {
        SettingsValue::Null => return Ok(None),
        SettingsValue::Bool(b) => *b,
        _ => interpret_bool(value.get_str()?),
    }))
}

/// Why `read_config_files` failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadConfigError {
    /// Core returned false with this error string (possibly empty, as when Core cannot create
    /// an empty `dash.conf`).
    Failed(String),
    /// Core threw; the string is `what()`.
    Exception(RuntimeError),
}

impl From<RuntimeError> for ReadConfigError {
    fn from(e: RuntimeError) -> Self {
        ReadConfigError::Exception(e)
    }
}

/// A sink for log lines. Each line keeps Core's trailing `"\n"`.
pub type LineSink = Box<dyn Fn(&str) + Send + Sync>;

enum LogSink {
    /// Lines logged before a sink was installed (Core buffers them until logging starts).
    Buffer(Vec<String>),
    Installed(LineSink),
}

/// Core: `ArgsManager`. Every method takes `&self`; the state sits behind a mutex, as Core's
/// sits behind `cs_args`. The log and stderr sinks run with that mutex held, so they must not
/// call back into the same `ArgsManager`.
pub struct ArgsManager {
    state: Mutex<State>,
}

struct State {
    settings: Settings,
    command: Vec<String>,
    network: String,
    network_only_args: BTreeSet<String>,
    available_args: BTreeMap<OptionsCategory, BTreeMap<String, Arg>>,
    accept_any_command: bool,
    config_sections: Vec<SectionInfo>,
    config_path: Option<PathBuf>,
    cached_blocks_path: PathBuf,
    cached_datadir_path: PathBuf,
    cached_network_datadir_path: PathBuf,
    /// Core: the global `BaseParams()`, set by `SelectBaseParams`.
    base_params: Option<BaseChainParams>,
    /// Whose default data directory an unset `-datadir` means.
    product: paths::Product,
    log: LogSink,
    stderr: LineSink,
}

impl Default for ArgsManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ArgsManager {
    /// Core: `ArgsManager::ALLOW_ANY`: disable validation.
    pub const ALLOW_ANY: u32 = 0x01;
    /// Core: `ArgsManager::DISALLOW_NEGATION`: disallow `-nofoo`.
    pub const DISALLOW_NEGATION: u32 = 0x20;
    /// Core: `ArgsManager::DISALLOW_ELISION`: disallow `-foo` without a value.
    pub const DISALLOW_ELISION: u32 = 0x40;
    /// Core: `ArgsManager::DEBUG_ONLY`: shown only by `-help-debug`.
    pub const DEBUG_ONLY: u32 = 0x100;
    /// Core: `ArgsManager::NETWORK_ONLY`: a default-section value applies to mainnet only.
    pub const NETWORK_ONLY: u32 = 0x200;
    /// Core: `ArgsManager::SENSITIVE`: logged as `****`.
    pub const SENSITIVE: u32 = 0x400;
    /// Core: `ArgsManager::COMMAND`.
    pub const COMMAND: u32 = 0x800;

    pub fn new() -> Self {
        ArgsManager {
            state: Mutex::new(State {
                settings: Settings::default(),
                command: Vec::new(),
                network: String::new(),
                network_only_args: BTreeSet::new(),
                available_args: BTreeMap::new(),
                accept_any_command: true,
                config_sections: Vec::new(),
                config_path: None,
                cached_blocks_path: PathBuf::new(),
                cached_datadir_path: PathBuf::new(),
                cached_network_datadir_path: PathBuf::new(),
                base_params: None,
                product: paths::Product::DashCore,
                log: LogSink::Buffer(Vec::new()),
                stderr: Box::new(|s| eprint!("{s}")),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Install the sink for Core's `LogPrintf` lines, first replaying any buffered lines.
    pub fn set_log_sink(&self, sink: LineSink) {
        let mut state = self.lock();
        if let LogSink::Buffer(lines) = &state.log {
            for line in lines {
                sink(line);
            }
        }
        state.log = LogSink::Installed(sink);
    }

    /// Take the lines logged while no sink was installed.
    pub fn take_buffered_log(&self) -> Vec<String> {
        match &mut self.lock().log {
            LogSink::Buffer(lines) => std::mem::take(lines),
            LogSink::Installed(_) => Vec::new(),
        }
    }

    /// Replace the sink for what Core writes to `std::cerr` (default: the process's stderr).
    pub fn set_stderr_sink(&self, sink: LineSink) {
        self.lock().stderr = sink;
    }

    /// Write a line to the log sink (Core: `LogPrintf`).
    pub fn log_printf(&self, line: &str) {
        self.lock().log(line.to_string());
    }

    /// Write to the stderr sink (Core: `tfm::format(std::cerr, ...)`).
    pub fn print_stderr(&self, text: &str) {
        (self.lock().stderr)(text);
    }

    /// Core: `GetSetting`.
    pub fn get_setting(&self, arg: &str) -> SettingsValue {
        self.lock().get_setting(arg)
    }

    /// Core: `GetSettingsList`.
    pub fn get_settings_list(&self, arg: &str) -> Vec<SettingsValue> {
        self.lock().get_settings_list(arg)
    }

    /// Core: `SelectConfigNetwork`.
    pub fn select_config_network(&self, network: &str) {
        self.lock().network = network.to_string();
    }

    /// Core: `SelectBaseParams`: select the base chain parameters (the datadir subdirectory
    /// and the default ports) and the config network.
    pub fn select_base_params(&self, chain: &str) -> Result<(), RuntimeError> {
        let mut state = self.lock();
        // Core's devnet callback (`GetDevNetName`) can throw, so it runs first.
        let devnet_name = if chain == chain::DEVNET {
            state.get_dev_net_name()?
        } else {
            String::new()
        };
        let params =
            chain::create_base_chain_params(chain, || devnet_name).map_err(RuntimeError)?;
        state.base_params = Some(params);
        state.network = chain.to_string();
        Ok(())
    }

    /// Core: `BaseParams()`. Panics, as Core asserts, if no chain was selected.
    pub fn base_params(&self) -> BaseChainParams {
        self.lock().base_params().clone()
    }

    /// Core: `ParseParameters(argc, argv, error)`. `argv[0]` is the program name and is skipped.
    pub fn parse_parameters<S: AsRef<str>>(&self, argv: &[S]) -> Result<(), String> {
        let argv: Vec<&str> = argv.iter().map(AsRef::as_ref).collect();
        self.lock().parse_parameters(&argv)
    }

    /// Core: `GetArgFlags`.
    pub fn get_arg_flags(&self, name: &str) -> Option<u32> {
        self.lock().get_arg_flags(name)
    }

    /// Core: `GetPathArg`.
    pub fn get_path_arg(&self, arg: &str, default_value: &Path) -> Result<PathBuf, RuntimeError> {
        self.lock().get_path_arg(arg, default_value)
    }

    /// Core: `GetBlocksDirPath`: the network-specific blocks directory, created if needed;
    /// empty if `-blocksdir` is not a directory.
    pub fn get_blocks_dir_path(&self) -> Result<PathBuf, RuntimeError> {
        self.lock().get_blocks_dir_path()
    }

    /// Core: `GetDataDirBase`: the absolute datadir, or an empty path if `-datadir` is not a
    /// directory.
    pub fn get_data_dir_base(&self) -> Result<PathBuf, RuntimeError> {
        self.lock().get_data_dir(false)
    }

    /// Core: `GetDataDirNet`: `get_data_dir_base` plus the network subdirectory. Panics if no
    /// chain was selected.
    pub fn get_data_dir_net(&self) -> Result<PathBuf, RuntimeError> {
        self.lock().get_data_dir(true)
    }

    /// Core: `GetBackupsDirPath`.
    pub fn get_backups_dir_path(&self) -> Result<PathBuf, RuntimeError> {
        let mut state = self.lock();
        if !state.is_arg_set("-walletbackupsdir") {
            return Ok(state.get_data_dir(true)?.join("backups"));
        }
        paths::absolute(&state.get_path_arg("-walletbackupsdir", Path::new(""))?)
    }

    /// Selects whose default data directory an unset `-datadir` means (Dash Core's unless set),
    /// and clears the path cache.
    pub fn set_product(&self, product: paths::Product) {
        let mut state = self.lock();
        state.product = product;
        state.clear_path_cache();
    }

    /// Core: `GetDefaultDataDir()`, for this manager's product.
    pub fn get_default_data_dir(&self) -> PathBuf {
        let mut state = self.lock();
        let product = state.product;
        paths::default_data_dir(product, &mut |line| state.log(line))
    }

    /// Core: `ClearPathCache`.
    pub fn clear_path_cache(&self) {
        self.lock().clear_path_cache();
    }

    /// Core: `GetCommand`.
    pub fn get_command(&self) -> Option<Command> {
        let state = self.lock();
        let mut it = state.command.iter();
        let mut ret = Command::default();
        if state.command.is_empty() {
            // No command was passed.
            return None;
        }
        if !state.accept_any_command {
            // The registered command.
            ret.command = it.next().expect("non-empty").clone();
        }
        // The unregistered command and args (if any).
        ret.args.extend(it.cloned());
        Some(ret)
    }

    /// Core: `GetCommandLineArgs`.
    pub fn get_command_line_args(&self) -> BTreeMap<String, Vec<SettingsValue>> {
        self.lock().settings.command_line_options.clone()
    }

    /// Core: `GetArgs`.
    pub fn get_args(&self, arg: &str) -> Result<Vec<String>, RuntimeError> {
        self.lock().get_args(arg)
    }

    /// Core: `IsArgSet`.
    pub fn is_arg_set(&self, arg: &str) -> bool {
        self.lock().is_arg_set(arg)
    }

    /// Core: `IsArgNegated`.
    pub fn is_arg_negated(&self, arg: &str) -> bool {
        self.get_setting(arg).is_false()
    }

    /// Core: `GetArg(strArg)`.
    pub fn get_arg_opt(&self, arg: &str) -> Result<Option<String>, RuntimeError> {
        setting_to_string(&self.get_setting(arg))
    }

    /// Core: `GetArg(strArg, strDefault)`.
    pub fn get_arg(&self, arg: &str, default: &str) -> Result<String, RuntimeError> {
        Ok(self.get_arg_opt(arg)?.unwrap_or_else(|| default.to_string()))
    }

    /// Core: `GetIntArg(strArg)`.
    pub fn get_int_arg_opt(&self, arg: &str) -> Result<Option<i64>, RuntimeError> {
        setting_to_int(&self.get_setting(arg))
    }

    /// Core: `GetIntArg(strArg, nDefault)`.
    pub fn get_int_arg(&self, arg: &str, default: i64) -> Result<i64, RuntimeError> {
        Ok(self.get_int_arg_opt(arg)?.unwrap_or(default))
    }

    /// Core: `GetBoolArg(strArg)`.
    pub fn get_bool_arg_opt(&self, arg: &str) -> Result<Option<bool>, RuntimeError> {
        setting_to_bool(&self.get_setting(arg))
    }

    /// Core: `GetBoolArg(strArg, fDefault)`.
    pub fn get_bool_arg(&self, arg: &str, default: bool) -> Result<bool, RuntimeError> {
        Ok(self.get_bool_arg_opt(arg)?.unwrap_or(default))
    }

    /// Core: `SoftSetArg`.
    pub fn soft_set_arg(&self, arg: &str, value: &str) -> bool {
        let mut state = self.lock();
        if state.is_arg_set(arg) {
            return false;
        }
        state.force_set_arg(arg, value);
        true
    }

    /// Core: `SoftSetBoolArg`.
    pub fn soft_set_bool_arg(&self, arg: &str, value: bool) -> bool {
        self.soft_set_arg(
            arg,
            if value {
                "1"
            } else {
                "0"
            },
        )
    }

    /// Core: `ForceSetArg`.
    pub fn force_set_arg(&self, arg: &str, value: &str) {
        self.lock().force_set_arg(arg, value);
    }

    /// Core: `ForceRemoveArg`. As in Core, `arg` is the bare setting name (`"devnet"`), and the
    /// default config section is left alone.
    pub fn force_remove_arg(&self, arg: &str) {
        let mut state = self.lock();
        state.settings.forced_settings.remove(arg);
        for network in [chain::MAIN, chain::TESTNET, chain::REGTEST, chain::DEVNET] {
            if let Some(section) = state.settings.ro_config.get_mut(network) {
                section.remove(arg);
            }
        }
    }

    /// Core: `GetChainName`. Errs if more than one chain is selected.
    pub fn get_chain_name(&self) -> Result<String, RuntimeError> {
        self.lock().get_chain_name()
    }

    /// Core: `GetDevNetName`: `devnet` or `devnet-<name>`.
    pub fn get_dev_net_name(&self) -> Result<String, RuntimeError> {
        self.lock().get_dev_net_name()
    }

    /// Core: `AddArg`. `name` may carry its help parameter (`-foo=<n>`). Panics, as Core asserts,
    /// on a duplicate in the same category or on the `COMMAND` flag.
    pub fn add_arg(&self, name: &str, help: &str, flags: u32, cat: OptionsCategory) {
        assert!(flags & Self::COMMAND == 0, "use add_command");
        // Split arg name from its help param.
        let eq_index = name.find('=').unwrap_or(name.len());
        let arg_name = &name[..eq_index];

        let mut state = self.lock();
        let arg_map = state.available_args.entry(cat).or_default();
        assert!(!arg_map.contains_key(arg_name), "duplicate argument {arg_name}");
        arg_map.insert(
            arg_name.to_string(),
            Arg {
                help_param: name[eq_index..].to_string(),
                help_text: help.to_string(),
                flags,
            },
        );
        if flags & Self::NETWORK_ONLY != 0 {
            state.network_only_args.insert(arg_name.to_string());
        }
    }

    /// Core: `AddCommand`. Panics, as Core asserts, on a duplicate or malformed command.
    pub fn add_command(&self, cmd: &str, help: &str) {
        assert!(!cmd.contains('='));
        assert!(!cmd.starts_with('-'));
        let mut state = self.lock();
        state.accept_any_command = false; // latch to false
        let arg_map = state.available_args.entry(OptionsCategory::Commands).or_default();
        assert!(!arg_map.contains_key(cmd), "duplicate command {cmd}");
        arg_map.insert(
            cmd.to_string(),
            Arg {
                help_param: String::new(),
                help_text: help.to_string(),
                flags: Self::COMMAND,
            },
        );
    }

    /// Core: `AddHiddenArgs`.
    pub fn add_hidden_args(&self, names: &[&str]) {
        for name in names {
            self.add_arg(name, "", Self::ALLOW_ANY, OptionsCategory::Hidden);
        }
    }

    /// Every registered option and command, in the order `GetHelpMessage` walks them (by
    /// category, then by name). Core has no such accessor; it exposes `m_available_args`.
    pub fn registered_args(&self) -> Vec<RegisteredArg> {
        let state = self.lock();
        let mut out = Vec::new();
        for (&category, arg_map) in &state.available_args {
            for (name, arg) in arg_map {
                out.push(RegisteredArg {
                    category,
                    name: name.clone(),
                    help_param: arg.help_param.clone(),
                    help_text: arg.help_text.clone(),
                    flags: arg.flags,
                });
            }
        }
        out
    }

    /// Core: `ClearArgs`.
    pub fn clear_args(&self) {
        let mut state = self.lock();
        state.available_args.clear();
        state.network_only_args.clear();
    }

    /// Core: `GetHelpMessage`.
    pub fn get_help_message(&self) -> Result<String, RuntimeError> {
        let show_debug = self.get_bool_arg("-help-debug", false)?;
        let state = self.lock();
        let mut usage = String::new();
        for (&cat, arg_map) in &state.available_args {
            if let Some(header) = cat.header(show_debug) {
                usage += &help_message_group(header);
            }
            // When we get to the hidden options, stop.
            if cat == OptionsCategory::Hidden {
                break;
            }
            for (name, arg) in arg_map {
                if show_debug || arg.flags & Self::DEBUG_ONLY == 0 {
                    usage +=
                        &help_message_opt(&format!("{name}{}", arg.help_param), &arg.help_text);
                }
            }
        }
        Ok(usage)
    }

    /// Core: `GetSettingsPath(filepath, temp, backup)`: `None` if `-nosettings`; otherwise the
    /// absolute path (computed only when `want_path`, as Core only computes it for a non-null
    /// `filepath`).
    fn settings_path(
        state: &mut State,
        want_path: bool,
        temp: bool,
        backup: bool,
    ) -> Result<Option<PathBuf>, RuntimeError> {
        let settings = state.get_path_arg("-settings", Path::new(DASH_SETTINGS_FILENAME))?;
        if settings.as_os_str().is_empty() {
            return Ok(None);
        }
        let mut settings = settings.into_os_string();
        if backup {
            settings.push(".bak");
        }
        if temp {
            settings.push(".tmp");
        }
        if !want_path {
            return Ok(Some(PathBuf::new()));
        }
        Ok(Some(paths::abs_path_join(&state.get_data_dir(true)?, Path::new(&settings))))
    }

    /// Core: `GetSettingsPath(filepath, temp, backup)`.
    pub fn get_settings_path(
        &self,
        temp: bool,
        backup: bool,
    ) -> Result<Option<PathBuf>, RuntimeError> {
        Self::settings_path(&mut self.lock(), true, temp, backup)
    }

    /// Core: `GetSettingsPath()` with no output path: false if `-nosettings`.
    pub fn settings_enabled(&self) -> Result<bool, RuntimeError> {
        Ok(Self::settings_path(&mut self.lock(), false, false, false)?.is_some())
    }

    /// Core: `ReadSettingsFile(errors)`. With `errors` `None`, errors are logged.
    pub fn read_settings_file(
        &self,
        errors: Option<&mut Vec<String>>,
    ) -> Result<bool, RuntimeError> {
        let mut state = self.lock();
        let Some(path) = Self::settings_path(&mut state, true, false, false)? else {
            return Ok(true); // Do nothing if settings file disabled.
        };
        state.settings.rw_settings.clear();
        let mut read_errors = Vec::new();
        if !settings::read_settings(&path, &mut state.settings.rw_settings, &mut read_errors)? {
            state.save_errors(read_errors, errors);
            return Ok(false);
        }
        let unknown: Vec<String> = state
            .settings
            .rw_settings
            .keys()
            // Split setting key into section and argname.
            .filter(|key| state.get_arg_flags(&format!("-{}", interpret_key(key).name)).is_none())
            .map(|key| format!("Ignoring unknown rw_settings value {key}\n"))
            .collect();
        for line in unknown {
            state.log(line);
        }
        Ok(true)
    }

    /// Core: `WriteSettingsFile(errors, backup)`. Panics with Core's `logic_error` text if
    /// settings are disabled. The written file is synced before the rename and its directory
    /// after it, which Core does not do (BL-512, [`settings::SettingsSync`]).
    pub fn write_settings_file(
        &self,
        errors: Option<&mut Vec<String>>,
        backup: bool,
    ) -> Result<bool, RuntimeError> {
        self.write_settings_file_synced(errors, backup, &settings::OsSync)
    }

    fn write_settings_file_synced(
        &self,
        errors: Option<&mut Vec<String>>,
        backup: bool,
        sync: &dyn settings::SettingsSync,
    ) -> Result<bool, RuntimeError> {
        let mut state = self.lock();
        let (Some(path), Some(path_tmp)) = (
            Self::settings_path(&mut state, true, false, backup)?,
            Self::settings_path(&mut state, true, true, backup)?,
        ) else {
            panic!("Attempt to write settings file when dynamic settings are disabled.");
        };
        let mut write_errors = Vec::new();
        let values = &state.settings.rw_settings;
        match settings::write_settings_synced(&path_tmp, values, &mut write_errors, sync) {
            settings::Written::NotOpened => {
                state.save_errors(write_errors, errors);
                return Ok(false);
            }
            // Not renamed, so the old file stays whole; Core's result (BL-512 r1 m-2).
            settings::Written::Unsynced(e) => {
                state.log(format!("fsync failed: {e}\n"));
                return Ok(true);
            }
            settings::Written::Synced => {}
        }
        if !settings::rename_over(&path_tmp, &path) {
            let error = format!(
                "Failed renaming settings file {} to {}\n",
                path_to_string(&path_tmp),
                path_to_string(&path)
            );
            state.save_errors(vec![error], errors);
            return Ok(false);
        }
        if let Some(dir) = path.parent() {
            sync.dir(dir);
        }
        Ok(true)
    }

    /// Core: `GetPersistentSetting`: ignores command-line and forced values.
    pub fn get_persistent_setting(&self, name: &str) -> SettingsValue {
        let state = self.lock();
        settings::get_setting(
            &state.settings,
            &state.network,
            name,
            !state.use_default_section(&format!("-{name}")),
            true,
            false,
        )
    }

    /// Core: `LockSettings`: run `f` on the settings with the lock held. Unlike Core's recursive
    /// `cs_args`, the lock is not re-entrant: `f` must not call this `ArgsManager`.
    pub fn lock_settings<R>(&self, f: impl FnOnce(&mut Settings) -> R) -> R {
        f(&mut self.lock().settings)
    }

    /// Core: `GetConfigFilePath`. Panics, as Core asserts, before `read_config_files`.
    pub fn get_config_file_path(&self) -> PathBuf {
        self.lock().config_path.clone().expect("GetConfigFilePath: no config file read yet")
    }

    /// Core: `ReadConfigStream` on `contents`, read from `filepath`.
    pub fn read_config_stream(
        &self,
        contents: &str,
        filepath: &str,
        ignore_invalid_keys: bool,
    ) -> Result<(), String> {
        self.lock().read_config_stream(contents, filepath, ignore_invalid_keys)
    }

    /// Clear the config-file settings and sections (what Core's unit tests do before calling
    /// `ReadConfigStream`).
    pub fn clear_config(&self) {
        let mut state = self.lock();
        state.settings.ro_config.clear();
        state.config_sections.clear();
    }

    /// Mark an argument network-only (Core's test-only `SetNetworkOnlyArg`).
    pub fn set_network_only_arg(&self, arg: &str) {
        self.lock().network_only_args.insert(arg.to_string());
    }

    /// Core: `ReadConfigFiles(error, ignore_invalid_keys)`.
    pub fn read_config_files(&self, ignore_invalid_keys: bool) -> Result<(), ReadConfigError> {
        self.lock().read_config_files(ignore_invalid_keys)
    }

    /// Core: `GetUnsuitableSectionOnlyArgs`: network-only arguments set only in the default
    /// section while running a non-mainnet chain.
    pub fn get_unsuitable_section_only_args(&self) -> BTreeSet<String> {
        let state = self.lock();
        // If there's no section selected, don't worry; if it's okay to use the default section
        // for this network, don't worry.
        if state.network.is_empty() || state.network == chain::MAIN {
            return BTreeSet::new();
        }
        state
            .network_only_args
            .iter()
            .filter(|arg| {
                settings::only_has_default_section_setting(
                    &state.settings,
                    &state.network,
                    setting_name(arg),
                )
            })
            .cloned()
            .collect()
    }

    /// Core: `GetUnrecognizedSections`.
    pub fn get_unrecognized_sections(&self) -> Vec<SectionInfo> {
        // Section names to be recognized in the config file.
        const AVAILABLE_SECTIONS: [&str; 4] =
            [chain::REGTEST, chain::DEVNET, chain::TESTNET, chain::MAIN];
        let state = self.lock();
        state
            .config_sections
            .iter()
            .filter(|s| !AVAILABLE_SECTIONS.contains(&s.name.as_str()))
            .cloned()
            .collect()
    }

    /// Core: `LogArgs`: the config file, settings file and command-line arguments, with
    /// `SENSITIVE` values masked.
    pub fn log_args(&self) {
        let mut state = self.lock();
        let mut lines = Vec::new();
        for (section, args) in &state.settings.ro_config {
            state.log_args_prefix(&mut lines, "Config file arg:", section, args);
        }
        for (name, value) in &state.settings.rw_settings {
            lines.push(format!("Setting file arg: {name} = {}\n", value.write()));
        }
        state.log_args_prefix(
            &mut lines,
            "Command-line arg:",
            "",
            &state.settings.command_line_options,
        );
        for line in lines {
            state.log(line);
        }
    }
}

/// Core: `HelpRequested`.
pub fn help_requested(args: &ArgsManager) -> bool {
    ["-?", "-h", "-help", "-help-debug"].iter().any(|arg| args.is_arg_set(arg))
}

/// Core: `SetupHelpOptions`.
pub fn setup_help_options(args: &ArgsManager) {
    args.add_arg("-?", "Print this help message and exit", 0, OptionsCategory::Options);
    args.add_hidden_args(&["-h", "-help"]);
}

const SCREEN_WIDTH: usize = 79;
const OPT_INDENT: usize = 2;
const MSG_INDENT: usize = 7;

/// Core: `HelpMessageGroup`.
pub fn help_message_group(message: &str) -> String {
    format!("{message}\n\n")
}

/// Core: `HelpMessageOpt`.
pub fn help_message_opt(option: &str, message: &str) -> String {
    format!(
        "{}{option}\n{}{}\n\n",
        " ".repeat(OPT_INDENT),
        " ".repeat(MSG_INDENT),
        format_paragraph(message, SCREEN_WIDTH - MSG_INDENT, MSG_INDENT)
    )
}

/// Core: `CheckDataDirOption`: true if `-datadir` is unset or names a directory.
pub fn check_data_dir_option(args: &ArgsManager) -> Result<bool, RuntimeError> {
    check_data_dir(&mut args.lock())
}

fn check_data_dir(state: &mut State) -> Result<bool, RuntimeError> {
    let datadir = state.get_path_arg("-datadir", Path::new(""))?;
    Ok(datadir.as_os_str().is_empty() || paths::is_directory(&paths::absolute(&datadir)?)?)
}

/// Core: `AbsPathForConfigVal`: a relative path joined to the (network) datadir.
pub fn abs_path_for_config_val(
    args: &ArgsManager,
    path: &Path,
    net_specific: bool,
) -> Result<PathBuf, RuntimeError> {
    abs_path_for_config_val_locked(&mut args.lock(), path, net_specific)
}

fn abs_path_for_config_val_locked(
    state: &mut State,
    path: &Path,
    net_specific: bool,
) -> Result<PathBuf, RuntimeError> {
    if paths::is_absolute(path) {
        return Ok(path.to_path_buf());
    }
    Ok(paths::abs_path_join(&state.get_data_dir(net_specific)?, path))
}

/// `std::getline` over a whole buffer: `'\n'`-separated lines, without a trailing empty line.
fn getlines(contents: &str) -> impl Iterator<Item = &str> {
    let body = contents.strip_suffix('\n').unwrap_or(contents);
    (!contents.is_empty()).then(|| body.split('\n')).into_iter().flatten()
}

/// Core: `GetConfigOptions`: the `(name, value)` pairs of one config file, recording the
/// sections it names.
fn get_config_options(
    contents: &str,
    filepath: &str,
    options: &mut Vec<(String, String)>,
    sections: &mut Vec<SectionInfo>,
) -> Result<(), String> {
    const PATTERN: &str = " \t\r\n";
    let mut prefix = String::new();
    for (linenr, line) in (1_i32..).zip(getlines(contents)) {
        let mut used_hash = false;
        let mut s = line;
        if let Some(pos) = s.find('#') {
            s = &s[..pos];
            used_hash = true;
        }
        let s = trim_string(s, PATTERN);
        if !s.is_empty() {
            if s.starts_with('[') && s.ends_with(']') {
                let section = if s.len() >= 2 {
                    &s[1..s.len() - 1]
                } else {
                    ""
                };
                sections.push(SectionInfo {
                    name: section.to_string(),
                    file: filepath.to_string(),
                    line: linenr,
                });
                prefix = format!("{section}.");
            } else if s.starts_with('-') {
                return Err(format!(
                    "parse error on line {linenr}: {s}, options in configuration file must be specified without leading -"
                ));
            } else if let Some(pos) = s.find('=') {
                let name = format!("{prefix}{}", trim_string(&s[..pos], PATTERN));
                let value = trim_string(&s[pos + 1..], PATTERN);
                if used_hash && name.contains("rpcpassword") {
                    return Err(format!(
                        "parse error on line {linenr}, using # in rpcpassword can be ambiguous and should be avoided"
                    ));
                }
                if let Some(pos) = name.rfind('.')
                    && prefix.len() <= pos
                {
                    sections.push(SectionInfo {
                        name: name[..pos].to_string(),
                        file: filepath.to_string(),
                        line: linenr,
                    });
                }
                options.push((name, value.to_string()));
            } else {
                let mut error = format!("parse error on line {linenr}: {s}");
                if s.starts_with("no") {
                    error += &format!(
                        ", if you intended to specify a negated option, use {s}=1 instead"
                    );
                }
                return Err(error);
            }
        }
    }
    Ok(())
}

/// Core: `InterpretValue`: the value per the argument's flags (Core's `nullopt` is the `Err`).
fn interpret_value(
    key: &KeyInfo,
    value: Option<&str>,
    flags: u32,
    log: &mut impl FnMut(String),
) -> Result<SettingsValue, String> {
    // Return negated settings as false values.
    if key.negated {
        if flags & ArgsManager::DISALLOW_NEGATION != 0 {
            return Err(format!(
                "Negating of -{} is meaningless and therefore forbidden",
                key.name
            ));
        }
        // Double negatives like -nofoo=0 are supported (but discouraged).
        if let Some(value) = value
            && !interpret_bool(value)
        {
            log(format!(
                "Warning: parsed potentially confusing double-negative -{}={value}\n",
                key.name
            ));
            return Ok(SettingsValue::Bool(true));
        }
        return Ok(SettingsValue::Bool(false));
    }
    if value.is_none() && flags & ArgsManager::DISALLOW_ELISION != 0 {
        return Err(format!(
            "Can not set -{0} with no value. Please specify value with -{0}=value.",
            key.name
        ));
    }
    Ok(SettingsValue::Str(value.unwrap_or("").to_string()))
}

/// The contents of a file Core opens with `std::ifstream`: `None` if it cannot be opened;
/// otherwise what could be read (a directory opens on POSIX and reads as empty), in text mode on
/// Windows.
fn read_stream(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    // `std::getline` stops at the first read error; keep what was read before it.
    let _ = file.read_to_end(&mut bytes);
    #[cfg(windows)]
    let bytes = paths::text_mode_read(&bytes);
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

impl State {
    fn log(&mut self, line: String) {
        match &mut self.log {
            LogSink::Buffer(lines) => lines.push(line),
            LogSink::Installed(sink) => sink(&line),
        }
    }

    fn base_params(&self) -> &BaseChainParams {
        self.base_params.as_ref().expect("BaseParams: no chain selected (call select_base_params)")
    }

    fn use_default_section(&self, arg: &str) -> bool {
        self.network == chain::MAIN || !self.network_only_args.contains(arg)
    }

    fn get_setting(&self, arg: &str) -> SettingsValue {
        settings::get_setting(
            &self.settings,
            &self.network,
            setting_name(arg),
            !self.use_default_section(arg),
            false,
            false,
        )
    }

    fn get_settings_list(&self, arg: &str) -> Vec<SettingsValue> {
        settings::get_settings_list(
            &self.settings,
            &self.network,
            setting_name(arg),
            !self.use_default_section(arg),
        )
    }

    fn is_arg_set(&self, arg: &str) -> bool {
        !self.get_setting(arg).is_null()
    }

    fn get_arg(&self, arg: &str, default: &str) -> Result<String, RuntimeError> {
        Ok(setting_to_string(&self.get_setting(arg))?.unwrap_or_else(|| default.to_string()))
    }

    fn get_args(&self, arg: &str) -> Result<Vec<String>, RuntimeError> {
        self.get_settings_list(arg)
            .iter()
            .map(|value| match value {
                SettingsValue::Bool(false) => Ok("0".to_string()),
                SettingsValue::Bool(true) => Ok("1".to_string()),
                _ => value.get_str().map(str::to_string),
            })
            .collect()
    }

    fn force_set_arg(&mut self, arg: &str, value: &str) {
        self.settings
            .forced_settings
            .insert(setting_name(arg).to_string(), SettingsValue::Str(value.to_string()));
    }

    fn get_arg_flags(&self, name: &str) -> Option<u32> {
        self.available_args.values().find_map(|arg_map| arg_map.get(name).map(|arg| arg.flags))
    }

    fn get_path_arg(&self, arg: &str, default_value: &Path) -> Result<PathBuf, RuntimeError> {
        if self.get_setting(arg).is_false() {
            return Ok(PathBuf::new());
        }
        let path_str = self.get_arg(arg, "")?;
        if path_str.is_empty() {
            return Ok(default_value.to_path_buf());
        }
        // Normalize, then remove the trailing slash, if present.
        Ok(paths::normalize_path_arg(&path_str))
    }

    fn clear_path_cache(&mut self) {
        self.cached_datadir_path = PathBuf::new();
        self.cached_network_datadir_path = PathBuf::new();
        self.cached_blocks_path = PathBuf::new();
    }

    fn get_data_dir(&mut self, net_specific: bool) -> Result<PathBuf, RuntimeError> {
        let cached = if net_specific {
            &self.cached_network_datadir_path
        } else {
            &self.cached_datadir_path
        };
        // Use the cached path if available.
        if !cached.as_os_str().is_empty() {
            return Ok(cached.clone());
        }
        let datadir = self.get_path_arg("-datadir", Path::new(""))?;
        let mut path = if !datadir.as_os_str().is_empty() {
            let path = paths::absolute(&datadir)?;
            if !paths::is_directory(&path)? {
                // Core caches the empty path, which is the same as not caching.
                return Ok(PathBuf::new());
            }
            path
        } else {
            paths::default_data_dir(self.product, &mut |line| self.log(line))
        };
        if net_specific && !self.base_params().data_dir().is_empty() {
            path.push(self.base_params().data_dir());
        }
        if net_specific {
            self.cached_network_datadir_path = path.clone();
        } else {
            self.cached_datadir_path = path.clone();
        }
        Ok(path)
    }

    fn get_blocks_dir_path(&mut self) -> Result<PathBuf, RuntimeError> {
        // Cache the path to avoid calling create_directories on every call.
        if !self.cached_blocks_path.as_os_str().is_empty() {
            return Ok(self.cached_blocks_path.clone());
        }
        let mut path = if self.is_arg_set("-blocksdir") {
            let path = paths::absolute(&self.get_path_arg("-blocksdir", Path::new(""))?)?;
            if !paths::is_directory(&path)? {
                return Ok(PathBuf::new());
            }
            path
        } else {
            self.get_data_dir(false)?
        };
        path.push(self.base_params().data_dir());
        path.push("blocks");
        // Core caches before creating the directories, so a failure still leaves the cache set.
        self.cached_blocks_path = path.clone();
        paths::create_directories(&path)?;
        Ok(path)
    }

    fn get_chain_name(&self) -> Result<String, RuntimeError> {
        let get_net = |arg: &str, interpret: bool| -> Result<bool, RuntimeError> {
            let value =
                settings::get_setting(&self.settings, "", setting_name(arg), false, false, true);
            Ok(match value {
                SettingsValue::Null => false,
                SettingsValue::Bool(b) => b,
                _ => !interpret || interpret_bool(value.get_str()?),
            })
        };
        let reg_test = get_net("-regtest", true)?;
        let dev_net = get_net("-devnet", false)?;
        let test_net = get_net("-testnet", true)?;
        let is_chain_arg_set = self.is_arg_set("-chain");

        let count = [reg_test, dev_net, test_net, is_chain_arg_set].iter().filter(|&&b| b).count();
        if count > 1 {
            return Err(RuntimeError(
                "Only one of -regtest, -testnet, -devnet or -chain can be used.".to_string(),
            ));
        }
        if reg_test {
            return Ok(chain::REGTEST.to_string());
        }
        if dev_net {
            return Ok(chain::DEVNET.to_string());
        }
        if test_net {
            return Ok(chain::TESTNET.to_string());
        }
        self.get_arg("-chain", chain::MAIN)
    }

    fn get_dev_net_name(&self) -> Result<String, RuntimeError> {
        let name = self.get_arg("-devnet", "")?;
        Ok(if name.is_empty() {
            "devnet".to_string()
        } else {
            format!("devnet-{name}")
        })
    }

    fn save_errors(&mut self, errors: Vec<String>, error_out: Option<&mut Vec<String>>) {
        match error_out {
            Some(out) => out.extend(errors),
            None => {
                for error in errors {
                    self.log(format!("{error}\n"));
                }
            }
        }
    }

    fn parse_parameters(&mut self, argv: &[&str]) -> Result<(), String> {
        self.settings.command_line_options.clear();

        let mut i = 1;
        while i < argv.len() {
            let arg = argv[i];
            let mut key = arg.to_string();
            if key == "-" {
                break; // dash-tx using stdin
            }

            // At the first time when a user gets the "App downloaded from the internet"
            // warning, and clicks the Open button, macOS passes a unique process serial number
            // (PSN) as -psn_... command-line argument, which we filter out.
            if cfg!(target_os = "macos") && key.starts_with("-psn_") {
                i += 1;
                continue;
            }

            let mut val: Option<String> = None;
            if let Some(is_index) = key.find('=') {
                val = Some(key[is_index + 1..].to_string());
                key.truncate(is_index);
            }
            if cfg!(windows) {
                key = key.to_ascii_lowercase();
                if key.starts_with('/') {
                    key.replace_range(0..1, "-");
                }
            }

            if !key.starts_with('-') {
                if !self.accept_any_command && self.command.is_empty() {
                    // The first non-dash arg is a registered command.
                    let flags = self.get_arg_flags(&key);
                    if flags.is_none_or(|f| f & ArgsManager::COMMAND == 0) {
                        return Err(format!("Invalid command '{arg}'"));
                    }
                }
                self.command.push(key);
                // The remaining args are command args.
                self.command.extend(argv[i + 1..].iter().map(|s| s.to_string()));
                break;
            }

            // Transform --foo to -foo.
            if key.len() > 1 && key.as_bytes()[1] == b'-' {
                key.remove(0);
            }
            // Transform -foo to foo.
            key.remove(0);
            let keyinfo = interpret_key(&key);
            let flags = self.get_arg_flags(&format!("-{}", keyinfo.name));

            // Unknown command line options and command line options with dot characters
            // (which are returned from InterpretKey with nonempty section strings) are not
            // valid.
            let Some(flags) = flags.filter(|_| keyinfo.section.is_empty()) else {
                return Err(format!("Invalid parameter {arg}"));
            };

            let value = interpret_value(&keyinfo, val.as_deref(), flags, &mut |l| self.log(l))?;
            self.settings.command_line_options.entry(keyinfo.name).or_default().push(value);
            i += 1;
        }

        // We do not allow -includeconf from command line, only -noincludeconf.
        if let Some(includes) = self.settings.command_line_options.get("includeconf") {
            let values = SettingsSpan::new(includes);
            // Range may be empty if -noincludeconf was passed.
            if !values.empty() {
                // Pick the first value as example.
                return Err(format!(
                    "-includeconf cannot be used from commandline; -includeconf={}",
                    values.values()[0].write()
                ));
            }
        }
        Ok(())
    }

    fn read_config_stream(
        &mut self,
        contents: &str,
        filepath: &str,
        ignore_invalid_keys: bool,
    ) -> Result<(), String> {
        let mut options = Vec::new();
        get_config_options(contents, filepath, &mut options, &mut self.config_sections)?;
        for (name, value) in options {
            let key = interpret_key(&name);
            let flags = self.get_arg_flags(&format!("-{}", key.name));
            // Core: IsConfSupported.
            if key.name == "conf" {
                return Err("conf cannot be set in the configuration file; use includeconf= if you want to include additional config files".to_string());
            }
            if key.name == "reindex" {
                // reindex can be set in a config file but it is strongly discouraged as this
                // will cause the node to reindex on every restart. Allow the config but warn.
                self.log("Warning: reindex=1 is set in the configuration file, which will significantly slow down startup. Consider removing or commenting out this option for better performance, unless there is currently a condition which makes rebuilding the indexes necessary\n".to_string());
            }
            match flags {
                Some(flags) => {
                    let value = interpret_value(&key, Some(&value), flags, &mut |l| self.log(l))?;
                    self.settings
                        .ro_config
                        .entry(key.section)
                        .or_default()
                        .entry(key.name)
                        .or_default()
                        .push(value);
                }
                None if ignore_invalid_keys => {
                    self.log(format!("Ignoring unknown configuration value {name}\n"));
                }
                None => return Err(format!("Invalid configuration value {name}")),
            }
        }
        Ok(())
    }

    /// The `includeconf` values of one config section from index `skip` (or the last
    /// negation) on, appended to `names`; returns how many values the section has.
    fn add_includes(
        &self,
        network: &str,
        skip: usize,
        names: &mut Vec<String>,
    ) -> Result<usize, RuntimeError> {
        let Some(values) = self.settings.ro_config.get(network).and_then(|s| s.get("includeconf"))
        else {
            return Ok(0);
        };
        let start = skip.max(SettingsSpan::new(values).negated());
        for value in values.iter().skip(start) {
            names.push(value.get_str()?.to_string());
        }
        Ok(values.len())
    }

    fn read_config_files(&mut self, ignore_invalid_keys: bool) -> Result<(), ReadConfigError> {
        self.settings.ro_config.clear();
        self.config_sections.clear();
        let conf = self.get_path_arg("-conf", Path::new(DASH_CONF_FILENAME))?;
        let conf_path = abs_path_for_config_val_locked(self, &conf, false)?;
        self.config_path = Some(conf_path.clone());

        let stream = read_stream(&conf_path);

        // Not ok to have a config file specified that cannot be opened.
        if self.is_arg_set("-conf") && stream.is_none() {
            return Err(ReadConfigError::Failed(format!(
                "specified config file \"{}\" could not be opened.",
                path_to_string(&conf_path)
            )));
        }
        // Ok to not have a config file.
        let Some(contents) = stream else {
            // Create an empty dash.conf if it does not exist.
            if fs::OpenOptions::new().append(true).create(true).open(&conf_path).is_err() {
                return Err(ReadConfigError::Failed(String::new()));
            }
            return Ok(()); // Nothing to read, so just return.
        };
        self.read_config_stream(&contents, &path_to_string(&conf_path), ignore_invalid_keys)
            .map_err(ReadConfigError::Failed)?;

        // -includeconf cannot be used on the command line except as -noincludeconf (which
        // indicates that no included conf file should be used).
        let use_conf_file = match self.settings.command_line_options.get("includeconf") {
            Some(includes) => {
                // parse_parameters fails if a non-negated -includeconf is passed.
                assert!(SettingsSpan::new(includes).last_negated());
                false
            }
            None => true,
        };
        if use_conf_file {
            let chain_id = self.get_chain_name()?;
            let mut conf_file_names = Vec::new();

            // The network is not selected yet (that happens in SelectParams()), so manually
            // check for network.includeconf args.
            let chain_includes = self.add_includes(&chain_id, 0, &mut conf_file_names)?;
            let default_includes = self.add_includes("", 0, &mut conf_file_names)?;

            for conf_file_name in &conf_file_names {
                let path = abs_path_for_config_val_locked(self, Path::new(conf_file_name), false)?;
                let Some(contents) = read_stream(&path) else {
                    return Err(ReadConfigError::Failed(format!(
                        "Failed to include configuration file {conf_file_name}"
                    )));
                };
                self.read_config_stream(&contents, conf_file_name, ignore_invalid_keys)
                    .map_err(ReadConfigError::Failed)?;
                self.log(format!("Included configuration file {conf_file_name}\n"));
            }

            // Warn about recursive -includeconf.
            conf_file_names.clear();
            self.add_includes(&chain_id, chain_includes, &mut conf_file_names)?;
            self.add_includes("", default_includes, &mut conf_file_names)?;
            let chain_id_final = self.get_chain_name()?;
            if chain_id_final != chain_id {
                // Also warn about recursive includeconf for the chain that was specified in one
                // of the includeconfs.
                self.add_includes(&chain_id_final, 0, &mut conf_file_names)?;
            }
            for conf_file_name in conf_file_names {
                (self.stderr)(&format!(
                    "warning: -includeconf cannot be used from included files; ignoring -includeconf={conf_file_name}\n"
                ));
            }
        }

        // If datadir is changed in .conf file:
        self.clear_path_cache();
        if !check_data_dir(self)? {
            return Err(ReadConfigError::Failed(format!(
                "specified data directory \"{}\" does not exist.",
                self.get_arg("-datadir", "")?
            )));
        }
        Ok(())
    }

    fn log_args_prefix(
        &self,
        lines: &mut Vec<String>,
        prefix: &str,
        section: &str,
        args: &BTreeMap<String, Vec<SettingsValue>>,
    ) {
        let section_str = if section.is_empty() {
            String::new()
        } else {
            format!("[{section}] ")
        };
        for (name, values) in args {
            for value in values {
                if let Some(flags) = self.get_arg_flags(&format!("-{name}")) {
                    let value_str = if flags & ArgsManager::SENSITIVE != 0 {
                        "****".to_string()
                    } else {
                        value.write()
                    };
                    lines.push(format!("{prefix} {section_str}{name}={value_str}\n"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpret_key_splits_section_and_negation() {
        assert_eq!(
            interpret_key("test.nofoo"),
            KeyInfo {
                name: "foo".into(),
                section: "test".into(),
                negated: true
            }
        );
        assert_eq!(
            interpret_key("a.b.c"),
            KeyInfo {
                name: "b.c".into(),
                section: "a".into(),
                negated: false
            }
        );
        assert_eq!(
            interpret_key("no"),
            KeyInfo {
                name: "".into(),
                section: "".into(),
                negated: true
            }
        );
    }

    /// Records each sync of a write of the settings file `name` with what the filesystem holds
    /// when it is made; the file sync fails with `fail`'s error, if any.
    struct SeenSyncs {
        name: &'static str,
        fail: Option<fn() -> std::io::Error>,
        seen: Mutex<Vec<String>>,
    }

    impl SeenSyncs {
        fn new(name: &'static str, fail: Option<fn() -> std::io::Error>) -> Self {
            SeenSyncs {
                name,
                fail,
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    impl settings::SettingsSync for SeenSyncs {
        fn file(&self, file: &fs::File, path: &Path) -> std::io::Result<()> {
            let len = file.metadata().unwrap().len();
            let on_disk = fs::read_to_string(path).unwrap();
            let seen = format!("file {} {len} B, all written: {}", path.display(), on_disk.len());
            self.seen.lock().unwrap().push(seen);
            self.fail.map_or(Ok(()), |e| Err(e()))
        }

        fn dir(&self, dir: &Path) {
            let renamed = fs::read_to_string(dir.join(self.name)).unwrap_or_default();
            let tmp = dir.join(format!("{}.tmp", self.name)).exists();
            let seen = format!("dir {} after the rename: {}", dir.display(), renamed.len());
            self.seen.lock().unwrap().push(format!("{seen}, tmp left: {tmp}"));
        }
    }

    /// A regtest manager on a new datadir, with `-settings=settings` if given and one setting to
    /// write; the datadir's network directory and `settings`' own directory exist.
    fn settings_writer(root: &Path, settings: Option<&str>) -> ArgsManager {
        let args = ArgsManager::new();
        args.select_base_params("regtest").unwrap();
        args.force_set_arg("-datadir", root.to_str().unwrap());
        if let Some(settings) = settings {
            args.force_set_arg("-settings", settings);
        }
        let path = args.get_settings_path(false, false).unwrap().unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        args.lock_settings(|s| {
            s.rw_settings.insert("name".into(), "value".into());
        });
        args
    }

    /// BL-512: the temporary file is synced once written and before the rename, and the
    /// directory that holds the settings file after it, which is not the datadir's when
    /// `-settings` names another one (r1 m-1).
    #[test]
    fn a_settings_write_syncs_the_file_then_the_directory() {
        for (settings, name, sub) in
            [(None, "settings.json", ""), (Some("sub/alt.json"), "alt.json", "sub")]
        {
            let root = tempfile::tempdir().unwrap();
            let args = settings_writer(root.path(), settings);
            let syncs = SeenSyncs::new(name, None);
            assert!(args.write_settings_file_synced(None, false, &syncs).unwrap());
            let net = root.path().join("regtest");
            let dir = if sub.is_empty() {
                net
            } else {
                net.join(sub)
            };
            let len = fs::read_to_string(dir.join(name)).unwrap().len();
            let tmp = dir.join(format!("{name}.tmp"));
            assert_eq!(
                syncs.seen.into_inner().unwrap(),
                [
                    format!("file {} {len} B, all written: {len}", tmp.display()),
                    format!("dir {} after the rename: {len}, tmp left: false", dir.display()),
                ],
                "-settings={settings:?}"
            );
        }
    }

    /// BL-512 r1 m-2: a file sync that fails is logged and the file is not renamed over the old
    /// one, which stays whole; the result is still Core's. EINVAL (a filesystem that cannot sync)
    /// counts as synced, as in Core's `FileCommit`.
    #[test]
    fn a_failed_file_sync_keeps_the_old_settings_file() {
        let eio: fn() -> std::io::Error = || std::io::Error::from_raw_os_error(5);
        let einval: fn() -> std::io::Error = || std::io::ErrorKind::InvalidInput.into();
        for (fail, renamed) in [(eio, false), (einval, true)] {
            let root = tempfile::tempdir().unwrap();
            let args = settings_writer(root.path(), None);
            let path = args.get_settings_path(false, false).unwrap().unwrap();
            fs::write(&path, "{\n    \"old\": 1\n}\n").unwrap();
            args.take_buffered_log();
            let syncs = SeenSyncs::new("settings.json", Some(fail));
            assert!(args.write_settings_file_synced(None, false, &syncs).unwrap());
            let now = fs::read_to_string(&path).unwrap();
            let log = args.take_buffered_log().concat();
            assert_eq!(now.contains("\"name\": \"value\""), renamed, "{fail:?}: {now}");
            assert_eq!(!now.contains("\"old\""), renamed, "{fail:?}: {now}");
            assert_eq!(
                syncs.seen.into_inner().unwrap().len(),
                if renamed {
                    2
                } else {
                    1
                }
            );
            assert_eq!(log.contains("fsync failed: "), !renamed, "{fail:?}: {log:?}");
        }
    }

    #[test]
    fn getlines_matches_std_getline() {
        assert_eq!(getlines("").count(), 0);
        assert_eq!(getlines("\n").collect::<Vec<_>>(), vec![""]);
        assert_eq!(getlines("a\nb").collect::<Vec<_>>(), vec!["a", "b"]);
        assert_eq!(getlines("a\nb\n").collect::<Vec<_>>(), vec!["a", "b"]);
        assert_eq!(getlines("a\n\n").collect::<Vec<_>>(), vec!["a", ""]);
    }
}
