//! The generated option tables against Dash Core v24.0.0-rc.5's release binaries: every
//! program's `-help` / `-help-debug` output is rebuilt byte for byte (from the x86_64 Linux
//! release on Unix, from the win64 release on Windows), and every option name is one the
//! program's parser accepts (tests/data/core-v24/recognized.txt, which lists the names of the
//! tables Core's sources generate, so it cannot show a name is missing; the help comparison
//! does that for visible options).

use std::collections::BTreeSet;

use dash_args::{ArgsManager, OptionsCategory, Program};

mod common;
use common::core_data;

fn program_args(program: Program, argv: &[&str]) -> ArgsManager {
    let args = ArgsManager::new();
    program.setup_args(&args);
    args.parse_parameters(argv).unwrap();
    args
}

fn help(program: Program, debug: bool) -> String {
    let flag = if debug {
        "-help-debug"
    } else {
        "-help"
    };
    program_args(program, &["prog", flag]).get_help_message().unwrap()
}

/// The captures of a program's help on this platform: (file, -help-debug). The Windows dash-qt
/// shows its help in a dialog, and macOS's differs from Linux's in two GUI defaults (see
/// `platform_dependent_defaults`), so neither has one.
#[cfg(target_pointer_width = "64")]
fn captures(program: Program) -> &'static [(&'static str, bool)] {
    match program {
        Program::Dashd => &[("dashd-help.txt", false), ("dashd-help-debug.txt", true)],
        Program::DashQt if cfg!(any(windows, target_os = "macos")) => &[],
        Program::DashQt => &[("dash-qt-help.txt", false), ("dash-qt-help-debug.txt", true)],
        Program::DashCli => &[("dash-cli-help.txt", false)],
        Program::DashTx => &[("dash-tx-help.txt", false)],
        Program::DashWallet => &[("dash-wallet-help.txt", false)],
        Program::DashUtil => &[("dash-util-help.txt", false)],
    }
}

// Both releases are 64-bit; 32-bit builds print a lower -dbcache maximum.
#[cfg(target_pointer_width = "64")]
#[test]
fn help_matches_core_byte_for_byte() {
    let dir = if cfg!(windows) {
        "help-win64"
    } else {
        "help"
    };
    let mut compared = 0;
    for program in Program::ALL {
        for &(file, debug) in captures(program) {
            let core = core_data(&format!("{dir}/{file}"));
            // The banner before the Options: group is the program's, not GetHelpMessage's.
            let start = core.find("\nOptions:\n\n").expect("an Options: group") + 1;
            assert!(
                help(program, debug) == core[start..],
                "{program:?} {dir}/{file} differs from Core"
            );
            compared += 1;
        }
    }
    assert!(compared >= 6);
}

/// The help defaults Core computes from the platform it was built for, as its sources set them:
/// `GUIUtil::defaultUIPlatform` (src/qt/guiutil.cpp:171), `TARGET_WEIGHT_NORMAL` and
/// `TARGET_WEIGHT_BOLD` mapped by `mapWeightArgs` (src/qt/guiutil_font.cpp:44, 187) and
/// `nMaxDbCache` (src/txdb.h:35).
#[test]
fn platform_dependent_defaults() {
    let paragraph = |text: &str, option: &str| -> String {
        let start = text.find(&format!("\n  {option}\n")).unwrap_or_else(|| panic!("no {option}"));
        let rest = &text[start + 1..];
        rest[..rest.find("\n\n").unwrap()].split_whitespace().collect::<Vec<_>>().join(" ")
    };
    let qt = help(Program::DashQt, true);
    let ui_platform = if cfg!(target_os = "macos") {
        "macosx"
    } else if cfg!(windows) {
        "windows"
    } else {
        "other"
    };
    assert!(
        paragraph(&qt, "-uiplatform").ends_with(&format!("default: {ui_platform})")),
        "{}",
        paragraph(&qt, "-uiplatform")
    );
    let normal = if cfg!(target_os = "macos") {
        1
    } else {
        2
    };
    assert!(paragraph(&qt, "-font-weight-normal").ends_with(&format!("(default: {normal})")));
    assert!(paragraph(&qt, "-font-weight-bold").ends_with("(default: 4)"));
    let max = if cfg!(target_pointer_width = "64") {
        16384
    } else {
        1024
    };
    let dashd = help(Program::Dashd, false);
    assert!(paragraph(&dashd, "-dbcache=<n>").contains(&format!("(4 to {max}, default: 300)")));
    // Windows releases build without the external signer.
    assert_eq!(qt.contains("\n  -signer=<cmd>\n"), cfg!(not(windows)));
}

#[test]
fn help_requested_flags() {
    for flag in ["-?", "-h", "-help", "-help-debug"] {
        let args = program_args(Program::Dashd, &["dashd", flag]);
        assert!(dash_args::help_requested(&args), "{flag}");
    }
    assert!(!dash_args::help_requested(&program_args(Program::Dashd, &["dashd"])));
}

#[test]
fn every_core_option_name_is_recognized() {
    let recognized = core_data("recognized.txt");
    for (core_name, program) in [
        ("dashd", Program::Dashd),
        ("dash-cli", Program::DashCli),
        ("dash-tx", Program::DashTx),
        ("dash-wallet", Program::DashWallet),
        ("dash-util", Program::DashUtil),
    ] {
        let core: BTreeSet<&str> = recognized
            .lines()
            .filter_map(|line| {
                let mut fields = line.split(' ');
                let (prog, name, verdict) = (fields.next()?, fields.next()?, fields.next()?);
                (prog == core_name).then(|| {
                    assert_eq!(verdict, "accepted", "Core rejects {name}");
                    name
                })
            })
            .collect();
        assert!(!core.is_empty(), "{core_name}: no names captured");
        // The capture is the Linux release's; the win64 release has no external signer
        // (help-win64/dashd-help.txt).
        #[cfg(windows)]
        let core: BTreeSet<&str> = core.into_iter().filter(|&name| name != "-signer").collect();

        let args = ArgsManager::new();
        program.setup_args(&args);
        let ours: BTreeSet<String> = args
            .registered_args()
            .into_iter()
            // dash-tx's command words are positional, so Core's parser never sees them as
            // options; the capture skips them.
            .filter(|a| a.name.starts_with('-') || a.category == OptionsCategory::Commands)
            .filter(|a| program != Program::DashTx || a.category != OptionsCategory::Commands)
            .map(|a| a.name)
            .collect();
        let ours: BTreeSet<&str> = ours.iter().map(String::as_str).collect();
        assert_eq!(ours, core, "{core_name}: registered names differ from the names captured");

        // Each name is also one our parser accepts.
        for name in &core {
            let argv: Vec<String> = if name.starts_with('-') {
                vec!["prog".into(), format!("{name}=1")]
            } else {
                vec!["prog".into(), (*name).to_string()]
            };
            let args = ArgsManager::new();
            program.setup_args(&args);
            let result = args.parse_parameters(&argv);
            // -includeconf is a known option that Core still refuses on the command line.
            if *name == "-includeconf" {
                assert_eq!(
                    result.unwrap_err(),
                    "-includeconf cannot be used from commandline; -includeconf=\"1\""
                );
            } else {
                assert_eq!(result, Ok(()), "{core_name} {name}");
            }
        }
    }
}

#[test]
fn unknown_option_is_rejected() {
    for program in Program::ALL {
        let args = ArgsManager::new();
        program.setup_args(&args);
        assert_eq!(
            args.parse_parameters(&["prog", "-nosuchoption"]),
            Err("Invalid parameter -nosuchoption".to_string())
        );
    }
}

#[test]
fn dash_qt_overrides_hidden_gui_args() {
    // SetupServerArgs hides the GUI options; SetupUIArgs registers them again under GUI, and
    // GetArgFlags finds the GUI entry first.
    let qt = ArgsManager::new();
    Program::DashQt.setup_args(&qt);
    let entries: Vec<_> = qt.registered_args().into_iter().filter(|a| a.name == "-min").collect();
    assert_eq!(
        entries.iter().map(|a| a.category).collect::<Vec<_>>(),
        [OptionsCategory::Gui, OptionsCategory::Hidden]
    );
    assert_eq!(
        qt.get_arg_flags("-uiplatform"),
        Some(ArgsManager::ALLOW_ANY | ArgsManager::DEBUG_ONLY)
    );
    let dashd = ArgsManager::new();
    Program::Dashd.setup_args(&dashd);
    assert_eq!(dashd.get_arg_flags("-uiplatform"), Some(ArgsManager::ALLOW_ANY));
}

#[test]
fn flags_of_sample_options() {
    let args = ArgsManager::new();
    Program::Dashd.setup_args(&args);
    let flags = |name| args.get_arg_flags(name).unwrap();
    assert_eq!(flags("-addnode"), ArgsManager::ALLOW_ANY | ArgsManager::NETWORK_ONLY);
    assert_eq!(flags("-rpcpassword"), ArgsManager::ALLOW_ANY | ArgsManager::SENSITIVE);
    assert_eq!(flags("-proxy"), ArgsManager::ALLOW_ANY | ArgsManager::DISALLOW_ELISION);
    assert_eq!(flags("-powtargetspacing"), ArgsManager::ALLOW_ANY | ArgsManager::DISALLOW_NEGATION);
    assert_eq!(flags("-regtest"), ArgsManager::ALLOW_ANY | ArgsManager::DEBUG_ONLY);
    assert_eq!(flags("-?"), 0);
    assert_eq!(flags("-help"), ArgsManager::ALLOW_ANY);

    let wallet = ArgsManager::new();
    Program::DashWallet.setup_args(&wallet);
    assert_eq!(wallet.get_arg_flags("info"), Some(ArgsManager::COMMAND));
    assert_eq!(
        wallet.parse_parameters(&["dash-wallet", "nosuch"]),
        Err("Invalid command 'nosuch'".into())
    );
    wallet.parse_parameters(&["dash-wallet", "-regtest", "info", "x", "-y"]).unwrap();
    let command = wallet.get_command().unwrap();
    assert_eq!(
        (command.command.as_str(), command.args),
        ("info", vec!["x".to_string(), "-y".to_string()])
    );
}
