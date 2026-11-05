//! rdashd: Dash Core's `dashd` main (`bitcoind.cpp`) over `dashd-node`.

#[cfg(unix)]
mod daemon;
mod signals;
#[cfg(feature = "test-hooks")]
mod test_hooks;

use std::io::Write as _;
use std::sync::Arc;

use dash_args::paths::Product;
use dash_args::strencodings::format_paragraph;
use dash_args::{ArgsManager, args::help_requested};
use dashd_node::init::support::{Support, not_supported, support};
use dashd_node::init::{self, setup_server_args};
use dashd_node::logging::{CoreSubscriber, Logger, route_args_log, set_thread_name};
use dashd_node::shutdown::EXIT_FAILURE;
use dashd_node::ui::Ui;
use dashd_node::{FULL_VERSION, InitFailure, Node, PACKAGE_NAME};

/// Core's `COPYRIGHT_YEAR` for this release.
const COPYRIGHT_YEAR: u32 = 2026;
/// Where rdashd's source lives (Core's `PACKAGE_URL` and `URL_SOURCE_CODE`).
const URL_SOURCE_CODE: &str = "<https://github.com/dashpay/rust-dashcore>";

fn main() {
    std::process::exit(run());
}

// Core: bitcoind.cpp:248-288 (main)
fn run() -> i32 {
    setup_environment();
    set_thread_name("init");
    let logger = Arc::new(Logger::new());
    // Every crate logs through `tracing` (DEC-200). Nothing else sets the subscriber.
    let _ = tracing::subscriber::set_global_default(CoreSubscriber::new(Arc::clone(&logger)));
    let ui = Arc::new(Ui::new(Arc::clone(&logger)));
    let args = Arc::new(ArgsManager::new());
    args.set_product(Product::RDashCore);
    route_args_log(&args, &logger);

    let argv = match utf8_args(std::env::args_os()) {
        Ok(argv) => argv,
        Err(msg) => {
            show(&ui, InitFailure::Error(msg));
            return EXIT_FAILURE;
        }
    };
    let params = match parse_args(&args, &argv) {
        Ok(params) => params,
        Err(failure) => {
            show(&ui, failure);
            return EXIT_FAILURE;
        }
    };
    match process_init_commands(&args) {
        Ok(true) => return 0,
        Ok(false) => {}
        Err(failure) => {
            show(&ui, failure);
            return EXIT_FAILURE;
        }
    }

    let mut node = Node::new(args, ui, params);
    if app_init(&mut node) {
        #[cfg(feature = "test-hooks")]
        test_hooks::after_init(&node);
        node.shutdown.wait_for_shutdown();
    } else {
        node.shutdown.set_exit_status(EXIT_FAILURE);
    }
    node.interrupt();
    node.shutdown();
    node.exit_status()
}

/// Core's `SetupEnvironment`: files the node creates are private to its user. (Core's locale
/// fallback has no Rust counterpart.)
// Core: util/system.cpp:1170-1197
fn setup_environment() {
    #[cfg(unix)]
    // SAFETY: `umask` only sets the process's file mode creation mask.
    unsafe {
        libc::umask(0o077);
    }
    #[cfg(windows)]
    // SAFETY: plain console calls; a failure leaves the code pages as they were.
    unsafe {
        use windows_sys::Win32::System::Console::{SetConsoleCP, SetConsoleOutputCP};
        const CP_UTF8: u32 = 65001;
        SetConsoleCP(CP_UTF8);
        SetConsoleOutputCP(CP_UTF8);
    }
}

/// The arguments as UTF-8. Core passes the bytes on; a lossy conversion would silently name
/// another file, so an argument that is not UTF-8 is refused. The refusal names the argument's
/// position and option, never its value, which may be a secret (`-rpcpassword=`).
fn utf8_args(argv: impl Iterator<Item = std::ffi::OsString>) -> Result<Vec<String>, String> {
    argv.enumerate()
        .map(|(i, arg)| {
            arg.into_string().map_err(|arg| {
                let bytes = arg.as_encoded_bytes();
                let name = bytes.iter().position(|&b| b == b'=').map(|eq| &bytes[..eq]);
                let name =
                    name.and_then(|n| std::str::from_utf8(n).ok()).filter(|n| n.starts_with('-'));
                match name {
                    Some(name) => format!(
                        "Error parsing command line arguments: argument {i} ({name}) is not valid \
                         UTF-8"
                    ),
                    None => format!(
                        "Error parsing command line arguments: argument {i} is not valid UTF-8"
                    ),
                }
            })
        })
        .collect()
}

/// How `main` shows a failure before `AppInit`.
fn show(ui: &Ui, failure: InitFailure) {
    match failure {
        InitFailure::Error(msg) => ui.init_error(&msg),
        InitFailure::Exception(what) => dashd_node::host::print_exception_continue("main", &what),
        InitFailure::Reported => {}
    }
}

/// Core's `IsSwitchChar`.
fn is_switch_char(c: char) -> bool {
    c == '-' || (cfg!(windows) && c == '/')
}

/// Core's `ParseArgs`: the command line, the config file and the chain. `-printcrashinfo`, which
/// Core answers without reading the config file, is refused at once (DEC-201 Q16).
// Core: bitcoind.cpp:110-133
fn parse_args(args: &ArgsManager, argv: &[String]) -> Result<init::SelectedParams, InitFailure> {
    setup_server_args(args);
    args.parse_parameters(argv)
        .map_err(|e| InitFailure::Error(dash_args::init::parse_parameters_error_text(&e)))?;
    if args.is_arg_set("-printcrashinfo") {
        let Some(Support::Unsupported(owner)) = support(args, "-printcrashinfo") else {
            unreachable!("-printcrashinfo is unsupported");
        };
        return Err(InitFailure::Error(not_supported("-printcrashinfo", owner)));
    }
    let params = init::init_config(args)?;
    // Loose non-argument tokens. Core names its binary, `dashd`; rdashd names itself.
    if let Some(token) = argv.iter().skip(1).find(|a| !a.starts_with(is_switch_char)) {
        return Err(InitFailure::Error(format!(
            "Command line contains unexpected token '{token}', see {PACKAGE_NAME} -h for a list \
             of options."
        )));
    }
    Ok(params)
}

/// Core's `LicenseInfo` for rdashd: the copyright of the Core code it ports, and its own source.
// Core: clientversion.cpp:85-116
fn license_info() -> String {
    format!(
        "Copyright (C) 2014-{COPYRIGHT_YEAR} The Dash Core developers\n\
         Copyright (C) 2009-{COPYRIGHT_YEAR} The Bitcoin Core developers\n\
         \n\
         Please contribute if you find {PACKAGE_NAME} useful. Visit {URL_SOURCE_CODE} for further \
         information about the software.\n\
         The source code is available from {URL_SOURCE_CODE}.\n\
         \n\
         This is experimental software.\n\
         Distributed under the MIT software license, see the accompanying file COPYING or \
         <https://opensource.org/licenses/MIT>\n"
    )
}

/// Core's `ProcessInitCommands`: `-help` and `-version` print and exit (`true`).
// Core: bitcoind.cpp:135-157
fn process_init_commands(args: &ArgsManager) -> Result<bool, InitFailure> {
    if !help_requested(args) && !args.is_arg_set("-version") {
        return Ok(false);
    }
    let mut usage = format!("{PACKAGE_NAME} version {FULL_VERSION}\n");
    if args.is_arg_set("-version") {
        usage += &format_paragraph(&license_info(), 79, 0);
    } else {
        usage += &format!(
            "\nUsage:  {PACKAGE_NAME} [options]                     Start {PACKAGE_NAME}\n\n"
        );
        usage += &args.get_help_message()?;
    }
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(usage.as_bytes());
    let _ = out.flush();
    Ok(true)
}

/// Core's `AppInit`: init before the daemon fork, the fork, the rest of init. With `-daemonwait`
/// the child tells the parent how init ended.
// Core: bitcoind.cpp:159-246
fn app_init(node: &mut Node) -> bool {
    // AppInitBasicSetup's signal handlers, ahead of InitLogging (Core installs them just after).
    let signals = match signals::install() {
        Ok(signals) => signals,
        Err(e) => return node.fail(InitFailure::Exception(e.to_string())),
    };
    if !node.app_init_early() {
        return false;
    }
    #[cfg(unix)]
    let mut daemon_ep = None;
    let args = Arc::clone(&node.args);
    let daemon = args.get_bool_arg("-daemon", false);
    let daemonwait = args.get_bool_arg("-daemonwait", false);
    let (daemon, daemonwait) = match (daemon, daemonwait) {
        (Ok(d), Ok(w)) => (d, w),
        (Err(e), _) | (_, Err(e)) => return node.fail(e.into()),
    };
    if daemon || daemonwait {
        #[cfg(unix)]
        {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{PACKAGE_NAME} starting");
            let _ = out.flush();
            drop(out);
            // Don't chdir, do close the standard descriptors.
            match daemon::fork_daemon(true, false) {
                Ok(daemon::Forked::Child(ep)) => {
                    if daemonwait {
                        daemon_ep = Some(ep);
                    } else {
                        ep.token_write(1);
                    }
                }
                Ok(daemon::Forked::Parent(ep)) => {
                    signals::forget();
                    // Core tests the token for nonzero, so a child that dies before writing
                    // one (TS_EOS) counts as a success.
                    if ep.token_read() != 0 {
                        std::process::exit(0);
                    }
                    let _ = writeln!(
                        std::io::stderr(),
                        "Error during initialization - check debug.log for details"
                    );
                    std::process::exit(EXIT_FAILURE);
                }
                Err(e) => {
                    return node.fail(InitFailure::Error(format!(
                        "fork_daemon() failed: {}",
                        dashd_node::datadir::sys_error_string(&e)
                    )));
                }
            }
        }
        #[cfg(not(unix))]
        return node.fail(InitFailure::Error(
            "-daemon is not supported on this operating system".to_owned(),
        ));
    }
    // Only now may threads exist.
    if let Err(e) = signals.start(Arc::clone(&node.shutdown), Arc::clone(node.ui.logger())) {
        return node.fail(InitFailure::Exception(e.to_string()));
    }
    #[cfg(feature = "test-hooks")]
    test_hooks::before_app_init_main(node);
    let ok = node.app_init_main();
    #[cfg(unix)]
    if let Some(ep) = daemon_ep {
        ep.token_write(u8::from(ok));
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_chars() {
        assert!(is_switch_char('-'));
        assert!(!is_switch_char('x'));
        assert_eq!(is_switch_char('/'), cfg!(windows));
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_argument_is_named_without_its_value() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;
        let args = |last: &[u8]| {
            ["rdashd", "-regtest"]
                .map(OsString::from)
                .into_iter()
                .chain([OsString::from_vec(last.to_vec())])
        };
        assert_eq!(utf8_args(args(b"-printtoconsole=1")).unwrap()[2], "-printtoconsole=1");
        assert_eq!(
            utf8_args(args(b"-rpcpassword=canary\xff")).unwrap_err(),
            "Error parsing command line arguments: argument 2 (-rpcpassword) is not valid UTF-8"
        );
        for value in [&b"canary\xff"[..], b"-canary\xff", b"-x\xffcanary=1"] {
            let msg = utf8_args(args(value)).unwrap_err();
            assert_eq!(msg, "Error parsing command line arguments: argument 2 is not valid UTF-8");
        }
    }

    #[test]
    fn license_info_names_rdashd() {
        let text = format_paragraph(&license_info(), 79, 0);
        assert!(text.starts_with("Copyright (C) 2014-2026 The Dash Core developers\n"));
        assert!(text.contains("if you find rdashd useful"));
        assert!(text.ends_with("<https://opensource.org/licenses/MIT>\n"));
    }
}
