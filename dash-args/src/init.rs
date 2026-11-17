//! Config initialization and the init-time messages built from the parsed arguments.
//!
//! Core: src/common/init.cpp (`InitConfig`), the config checks at the top of
//! `AppInitParameterInteraction` (src/init.cpp), `InitError`/`InitWarning`
//! (src/node/interface_ui.cpp) and their no-GUI rendering (src/noui.cpp), at v24.0.0-rc.3.

use std::path::Path;

use crate::args::{ArgsManager, ReadConfigError, abs_path_for_config_val, check_data_dir_option};
use crate::paths::{self, DASH_CONF_FILENAME, path_to_string};
use crate::strencodings::{make_unordered_list, quoted};
use crate::univalue::RuntimeError;

/// Core: `common::ConfigStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigStatus {
    /// Failed generically.
    Failed,
    /// Failed to write the settings file.
    FailedWrite,
    /// Aborted by the user.
    Aborted,
}

/// Core: `common::ConfigError`. `message` is the untranslated (`original`) text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError {
    pub status: ConfigStatus,
    pub message: String,
    pub details: Vec<String>,
}

impl ConfigError {
    fn new(status: ConfigStatus, message: &str, details: Vec<String>) -> Self {
        ConfigError {
            status,
            message: message.to_string(),
            details,
        }
    }

    fn failed(message: String) -> Self {
        ConfigError {
            status: ConfigStatus::Failed,
            message,
            details: Vec::new(),
        }
    }

    /// The text `InitError(message, details)` reports.
    pub fn init_error_text(&self) -> String {
        init_error_text(&self.message, &self.details)
    }
}

/// Core: `SettingsAbortFn`: asked whether to abort when the settings file cannot be read
/// (the GUI asks the user); `true` aborts.
pub type SettingsAbortFn<'a> = &'a dyn Fn(&str, &[String]) -> bool;

/// Core: `common::InitConfig`.
///
/// `select_params` is Core's `SelectParams(chain)`; it must call
/// [`ArgsManager::select_base_params`] (as Core's `SelectParams` calls `SelectBaseParams`) and
/// may set up the full chain parameters. Its error is the `what()` of the exception Core's
/// `SelectParams` would throw.
pub fn init_config(
    args: &ArgsManager,
    select_params: &dyn Fn(&ArgsManager, &str) -> Result<(), RuntimeError>,
    settings_abort_fn: Option<SettingsAbortFn<'_>>,
) -> Result<(), ConfigError> {
    // Core catches every exception and reports its what().
    init_config_inner(args, select_params, settings_abort_fn)
        .unwrap_or_else(|e| Err(ConfigError::failed(e.0)))
}

fn init_config_inner(
    args: &ArgsManager,
    select_params: &dyn Fn(&ArgsManager, &str) -> Result<(), RuntimeError>,
    settings_abort_fn: Option<SettingsAbortFn<'_>>,
) -> Result<Result<(), ConfigError>, RuntimeError> {
    if !check_data_dir_option(args)? {
        return Ok(Err(ConfigError::failed(format!(
            "Specified data directory \"{}\" does not exist.",
            args.get_arg("-datadir", "")?
        ))));
    }

    // Record the original datadir and config paths before parsing the config file: it may
    // contain a datadir= line that changes the datadir path after it is parsed. (It cannot
    // cause another configuration to be used: conf= in the config file is a parse error, and a
    // datadir= location containing another dash.conf just ignores the other file.)
    let orig_datadir_path = args.get_data_dir_base()?;
    let conf = args.get_path_arg("-conf", Path::new(DASH_CONF_FILENAME))?;
    let orig_config_path = abs_path_for_config_val(args, &conf, false)?;

    match args.read_config_files(true) {
        Ok(()) => {}
        Err(ReadConfigError::Failed(error)) => {
            return Ok(Err(ConfigError::failed(format!(
                "Error reading configuration file: {error}"
            ))));
        }
        Err(ReadConfigError::Exception(e)) => return Err(e),
    }

    // Check for chain settings (Params() calls are only valid after this clause).
    select_params(args, &args.get_chain_name()?)?;

    // Create the datadir if it does not exist. A new datadir also gets a "wallets"
    // subdirectory, whether or not the wallet is enabled now, so a wallet enabled later uses
    // it rather than the top-level directory.
    let base_path = args.get_data_dir_base()?;
    if !paths::exists(&base_path)? {
        paths::create_directories(&base_path.join("wallets"))?;
    }
    let net_path = args.get_data_dir_net()?;
    if !paths::exists(&net_path)? {
        paths::create_directories(&net_path.join("wallets"))?;
    }

    // Show an error or warning if there is a dash.conf file in the datadir that is being
    // ignored.
    let base_config_path = base_path.join(DASH_CONF_FILENAME);
    if paths::exists(&base_config_path)? && !paths::equivalent(&orig_config_path, &base_config_path)
    {
        let cli_config_path = args.get_arg("-conf", "")?;
        let config_source = if cli_config_path.is_empty() {
            format!("data directory {}", quoted(&path_to_string(&orig_datadir_path)))
        } else {
            format!("command line argument {}", quoted(&format!("-conf={cli_config_path}")))
        };
        let base = quoted(&path_to_string(&base_path));
        let file = quoted(DASH_CONF_FILENAME);
        let error = format!(
            "Data directory {base} contains a {file} file which is ignored, because a different configuration file \
             {} from {config_source} is being used instead. Possible ways to address this would be to:\n\
             - Delete or rename the {file} file in data directory {base}.\n\
             - Change datadir= or conf= options to specify one configuration file, not two, and use \
             includeconf= to include any other configuration files.\n\
             - Set allowignoredconf=1 option to treat this condition as a warning, not an error.",
            quoted(&path_to_string(&orig_config_path)),
        );
        if args.get_bool_arg("-allowignoredconf", false)? {
            args.log_printf(&format!("Warning: {error}\n"));
        } else {
            return Ok(Err(ConfigError::failed(error)));
        }
    }

    // Create settings.json if -nosettings was not specified.
    if args.settings_enabled()? {
        let mut details = Vec::new();
        if !args.read_settings_file(Some(&mut details))? {
            let message = "Settings file could not be read";
            match settings_abort_fn {
                None => return Ok(Err(ConfigError::new(ConfigStatus::Failed, message, details))),
                Some(abort) if abort(message, &details) => {
                    return Ok(Err(ConfigError::new(ConfigStatus::Aborted, message, details)));
                }
                // The user chose to ignore the error and proceed.
                Some(_) => details.clear(),
            }
        }
        if !args.write_settings_file(Some(&mut details), false)? {
            let message = "Settings file could not be written";
            return Ok(Err(ConfigError::new(ConfigStatus::FailedWrite, message, details)));
        }
    }
    Ok(Ok(()))
}

/// Core: `InitError(str, details)`: the details flattened into the message.
pub fn init_error_text(message: &str, details: &[String]) -> String {
    if details.is_empty() {
        message.to_string()
    } else {
        format!("{message}:\n{}", make_unordered_list(details))
    }
}

/// What the no-GUI message box (Core: `noui_ThreadSafeMessageBox`) writes for an
/// `InitError`, to both the log and stderr.
pub fn noui_error_line(message: &str) -> String {
    format!("Error: {message}\n")
}

/// What the no-GUI message box writes for an `InitWarning`.
pub fn noui_warning_line(message: &str) -> String {
    format!("Warning: {message}\n")
}

/// Core: the `InitError` of `ParseArgs` (src/bitcoind.cpp) when `ParseParameters` fails.
pub fn parse_parameters_error_text(error: &str) -> String {
    format!("Error parsing command line arguments: {error}")
}

/// Core: the network-only check of `AppInitParameterInteraction`: the `InitError` text for
/// network-only options set only in the default section, or `None`.
pub fn unsuitable_section_only_args_error(
    args: &ArgsManager,
) -> Result<Option<String>, RuntimeError> {
    let network = args.get_chain_name()?;
    let errors: String = args
        .get_unsuitable_section_only_args()
        .iter()
        .map(|arg| {
            format!("Config setting for {arg} only applied on {network} network when in [{network}] section.\n")
        })
        .collect();
    Ok((!errors.is_empty()).then_some(errors))
}

/// Core: the section check of `AppInitParameterInteraction`: the `InitWarning` text for
/// unrecognized config sections, or `None`.
pub fn unrecognized_sections_warning(args: &ArgsManager) -> Option<String> {
    let warnings: String = args
        .get_unrecognized_sections()
        .iter()
        .map(|s| format!("{}:{} Section [{}] is not recognized.\n", s.file, s.line, s.name))
        .collect();
    (!warnings.is_empty()).then_some(warnings)
}

/// The default `select_params` for programs that only need the base parameters.
pub fn select_base_params(args: &ArgsManager, chain: &str) -> Result<(), RuntimeError> {
    args.select_base_params(chain)
}
