//! The logging options (`init/common.cpp`): `SetLoggingOptions`, `SetLoggingLevel`,
//! `SetLoggingCategories`, `StartLogging` and `LogPackageVersion`.

use std::path::Path;
use std::sync::Arc;

use dash_args::ArgsManager;
use dash_args::args::abs_path_for_config_val;

use super::{Logger, time};
use crate::error::InitFailure;
use crate::ui::Ui;

/// Core's `DEFAULT_DEBUGLOGFILE`.
pub const DEFAULT_DEBUGLOGFILE: &str = "debug.log";

/// Sends the lines the arguments log (`LogArgs`, the config-file warnings) to `logger`, after
/// those buffered so far.
pub fn route_args_log(args: &ArgsManager, logger: &Arc<Logger>) {
    let logger = Arc::clone(logger);
    args.set_log_sink(Box::new(move |line| logger.log_printf(line)));
}

/// Core's `SetLoggingOptions`.
// Core: init/common.cpp:45-57
pub fn set_logging_options(logger: &Logger, args: &ArgsManager) -> Result<(), InitFailure> {
    let mut opts = logger.options();
    opts.print_to_file = !args.is_arg_negated("-debuglogfile");
    let file = args.get_path_arg("-debuglogfile", Path::new(DEFAULT_DEBUGLOGFILE))?;
    opts.file_path = abs_path_for_config_val(args, &file, true)?;
    opts.print_to_console =
        args.get_bool_arg("-printtoconsole", !args.get_bool_arg("-daemon", false)?)?;
    opts.log_timestamps = args.get_bool_arg("-logtimestamps", true)?;
    opts.log_time_micros = args.get_bool_arg("-logtimemicros", false)?;
    opts.log_threadnames = args.get_bool_arg("-logthreadnames", false)?;
    opts.log_sourcelocations = args.get_bool_arg("-logsourcelocations", false)?;
    opts.always_print_category_level = args.get_bool_arg("-loglevelalways", false)?;
    logger.set_options(opts);
    logger.set_log_ips(args.get_bool_arg("-logips", false)?);
    Ok(())
}

/// Core's `SetLoggingLevel`: `-loglevel=<level>` and `-loglevel=<category>:<level>` (P2P-24).
// Core: init/common.cpp:59-78
pub fn set_logging_level(logger: &Logger, args: &ArgsManager) -> Result<(), InitFailure> {
    if !args.is_arg_set("-loglevel") {
        return Ok(());
    }
    for level in args.get_args("-loglevel")? {
        // `find_first_of(':', 3)`: a colon in the first three bytes does not make a category.
        let has_category = level.as_bytes().iter().skip(3).any(|&b| b == b':');
        if !has_category {
            if !logger.set_log_level_str(&level) {
                return Err(InitFailure::Error(format!(
                    "Unsupported global logging level -loglevel={level}. Valid values: {}.",
                    Logger::log_levels_string()
                )));
            }
        } else {
            let toks: Vec<&str> = level.split(':').collect();
            if !(toks.len() == 2 && logger.set_category_log_level_str(toks[0], toks[1])) {
                return Err(InitFailure::Error(format!(
                    "Unsupported category-specific logging level -loglevel={level}. Expected \
                     -loglevel=<category>:<loglevel>. Valid categories: {}. Valid loglevels: {}.",
                    logger.log_categories_string(false),
                    Logger::log_levels_string()
                )));
            }
        }
    }
    Ok(())
}

/// Core's `SetLoggingCategories`: `-debug` (unless one value is `0` or `none`), then
/// `-debugexclude`.
// Core: init/common.cpp:80-103
pub fn set_logging_categories(logger: &Logger, args: &ArgsManager) -> Result<(), InitFailure> {
    if args.is_arg_set("-debug") {
        let categories = args.get_args("-debug")?;
        if !categories.iter().any(|c| c == "0" || c == "none") {
            for cat in &categories {
                if !logger.enable_category_str(cat) {
                    return Err(InitFailure::Error(format!(
                        "Unsupported logging category -debug={cat}."
                    )));
                }
            }
        }
    }
    for cat in args.get_args("-debugexclude")? {
        if !logger.disable_category_str(&cat) {
            return Err(InitFailure::Error(format!(
                "Unsupported logging category -debugexclude={cat}."
            )));
        }
    }
    Ok(())
}

/// Core's `StartLogging` (`init/common.cpp`): shrinks `debug.log` unless a category is on (or
/// `-shrinkdebugfile` says otherwise), opens it, and logs the data directories, the config file
/// and the arguments. `default_data_dir` is the product's default (`GetDefaultDataDir`).
// Core: init/common.cpp:105-140
pub fn start_logging(
    ui: &Ui,
    args: &ArgsManager,
    default_data_dir: &Path,
) -> Result<(), InitFailure> {
    let logger = ui.logger();
    let opts = logger.options();
    if opts.print_to_file
        && args.get_bool_arg("-shrinkdebugfile", logger.default_shrink_debug_file())?
    {
        // First: it reads much of debug.log into memory, and must precede any other write.
        logger.shrink_debug_file();
    }
    if !logger.start_logging() {
        return Err(InitFailure::Error(format!(
            "Could not open debug log file {}",
            opts.file_path.to_string_lossy()
        )));
    }
    if !opts.log_timestamps {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        logger.log_printf(&format!("Startup time: {}\n", time::format_iso8601_date_time(now)));
    }
    logger.log_printf(&format!("Default data directory {}\n", default_data_dir.to_string_lossy()));
    logger.log_printf(&format!(
        "Using data directory {}\n",
        args.get_data_dir_net()?.to_string_lossy()
    ));
    let config = args.get_config_file_path();
    if config.exists() {
        logger.log_printf(&format!("Config file: {}\n", config.to_string_lossy()));
    } else if args.is_arg_set("-conf") {
        ui.init_warning(&format!(
            "The specified config file {} does not exist",
            config.to_string_lossy()
        ));
    } else {
        logger.log_printf(&format!(
            "Config file: {} (not found, skipping)\n",
            config.to_string_lossy()
        ));
    }
    args.log_args();
    Ok(())
}

/// Core's `LogPackageVersion`: `<PACKAGE_NAME> version <version> (release build)`.
// Core: init/common.cpp:142-151
pub fn log_package_version(logger: &Logger) {
    logger.log_printf(&format!(
        "{} version {} (release build)\n",
        crate::PACKAGE_NAME,
        crate::FULL_VERSION
    ));
}
