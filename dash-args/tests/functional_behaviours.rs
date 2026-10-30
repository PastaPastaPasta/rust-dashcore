//! The behaviours Dash Core's functional tests `feature_config_args.py`,
//! `feature_includeconf.py` and `feature_settings.py` (v24.0.0-rc.3) check, each with a Rust
//! test unless it needs a running node.
//!
//! Many are init-time errors. Those are recorded from the release dashd in
//! tests/data/core-v24/init_cases.json and replayed by tests/init_cases.rs; the table below
//! names the case, and `functional_test_messages_match_captured_cases` checks that the message
//! the functional test expects is the one dashd printed.
//!
//! | Functional test check | Test |
//! |---|---|
//! | config_args: `conf=` in dash.conf is an error | case `conf_sets_conf` |
//! | config_args: `conf=` in an included file is an error | case `included_sets_conf` |
//! | config_args: unknown command-line option is an error (`-dash_cli=1`) | case `invalid_parameter` |
//! | config_args: unknown config key is ignored and logged | case `unknown_conf_key_ignored`, `unknown_config_key_is_logged` |
//! | config_args: `reindex=1` in the config logs a warning | case `reindex_in_conf`, `reindex_in_config_is_logged` |
//! | config_args: an option with a leading `-` in the config is an error | case `leading_dash_in_conf` |
//! | config_args: network-only `wallet=foo` in the default section on regtest is an error | `network_only_option_in_default_section` |
//! | config_args: `acceptnonstdtxn` is refused on main | needs a node (chain parameter interaction, not argument parsing) |
//! | config_args: `nono` suggests `nono=1` | case `bare_negation_in_conf` |
//! | config_args: `#` in `rpcpassword` (plain, dotted, in a section) is an error with the line number | cases `rpcpassword_hash*` |
//! | config_args: unrecognized sections are warned about with file and line | `unrecognized_sections_are_warned_about` |
//! | config_args: the config file path stays the one read even when it changes the datadir | `config_path_survives_datadir_change` |
//! | config_args: `-proxy` without a value is an error | case `proxy_without_value` |
//! | config_args: `-noconnect=0` logs the double-negative warning | case `double_negative`, `double_negative_is_logged` |
//! | config_args: argument logging masks sensitive values | `args_log_masks_sensitive_values` |
//! | config_args: `-networkactive` forms map to true/false | `networkactive_forms` |
//! | config_args: seed peers, DNS seeds, `-connect` with `-seednode` | needs a node (connection manager); the argument forms are covered by `networkactive_forms`'s getters |
//! | config_args: a `dash.conf` in the datadir ignored for `-conf` is an error | case `ignored_conf` |
//! | config_args: a redundant `-conf` naming the datadir's file is fine | case `redundant_conf_same_file` |
//! | config_args: a default-datadir conf pointing elsewhere with its own conf is an error | case `ignored_default_conf` |
//! | config_args: a missing `-datadir` is an error | case `datadir_missing` |
//! | config_args: a missing `datadir=` in the config is an error | case `conf_datadir_missing` |
//! | config_args: an unopenable `-conf` is an error | case `conf_missing_absolute` |
//! | config_args: an existing `datadir=` from the config is used (blocks dir created there) | case `conf_datadir_exists`, `blocks_dir_follows_config_datadir` |
//! | config_args: command-line `-datadir` overrides the config's | case `cmdline_datadir_overrides_conf` |
//! | includeconf: included files are read in order after the main file | `includeconf_order` |
//! | includeconf: `-includeconf` and `-noincludeconf=0` on the command line are errors | cases `includeconf_cmdline`, `includeconf_cmdline_double_negative` |
//! | includeconf: recursive includes are ignored with a stderr warning | case `include_recursive`, `includeconf_order` |
//! | includeconf: a missing include is an error | case `include_missing` |
//! | includeconf: several includes from the main file are all read | `includeconf_order` |
//! | settings: an empty settings file with the `_warning_` entry is created | case `chain_regtest`, `default_settings_file` |
//! | settings: values are read, logged and unknown names noted | `settings_values_are_logged` |
//! | settings: values survive the rewrite at startup (the one at shutdown needs a node) | case `settings_values` |
//! | settings: invalid JSON, a non-object and duplicate keys are errors | cases `settings_invalid_json`, `settings_non_object`, `settings_duplicate_key` |
//! | settings: `-nosettings` ignores a broken file | case `settings_disabled_cmdline`, `nosettings_is_logged` |
//! | settings: `nosettings=1` in the config ignores a broken file | case `settings_disabled_conf`, `nosettings_is_logged` |
//! | settings: `-settings=<path>` reads another file | case `settings_alternate_path`, `settings_values_are_logged` |

use std::path::Path;
use std::sync::{Arc, Mutex};

use dash_args::init::{
    init_config, noui_warning_line, select_base_params, unrecognized_sections_warning,
    unsuitable_section_only_args_error,
};
use dash_args::{ArgsManager, Program};

mod common;
use common::{field, init_case};

/// A dashd `ArgsManager` with the log captured.
fn dashd(argv: &[&str]) -> (ArgsManager, Arc<Mutex<Vec<String>>>) {
    let args = ArgsManager::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&log);
    args.set_log_sink(Box::new(move |line| sink.lock().unwrap().push(line.to_string())));
    args.set_stderr_sink(Box::new(|_| {}));
    Program::Dashd.setup_args(&args);
    let mut full = vec!["dashd"];
    full.extend_from_slice(argv);
    args.parse_parameters(&full).unwrap();
    (args, log)
}

fn init(args: &ArgsManager) {
    init_config(args, &select_base_params, None).unwrap()
}

fn datadir_arg(dir: &Path) -> String {
    format!("-datadir={}", dir.display())
}

fn logged(log: &Arc<Mutex<Vec<String>>>, line: &str) -> bool {
    log.lock().unwrap().iter().any(|l| l == line)
}

#[cfg(unix)]
#[test]
fn functional_test_messages_match_captured_cases() {
    // (case, message the functional test expects, as a prefix of dashd's stderr)
    let expectations = [
        (
            "conf_sets_conf",
            "Error: Error reading configuration file: conf cannot be set in the configuration file; use includeconf= if you want to include additional config files",
        ),
        (
            "included_sets_conf",
            "Error: Error reading configuration file: conf cannot be set in the configuration file; use includeconf= if you want to include additional config files",
        ),
        (
            "invalid_parameter",
            "Error: Error parsing command line arguments: Invalid parameter -dash_cli=1",
        ),
        (
            "leading_dash_in_conf",
            "Error: Error reading configuration file: parse error on line 1: -dash=1, options in configuration file must be specified without leading -",
        ),
        (
            "bare_negation_in_conf",
            "Error: Error reading configuration file: parse error on line 1: nono, if you intended to specify a negated option, use nono=1 instead",
        ),
        (
            "rpcpassword_hash",
            "Error: Error reading configuration file: parse error on line 3, using # in rpcpassword can be ambiguous and should be avoided",
        ),
        (
            "rpcpassword_hash_dotted",
            "Error: Error reading configuration file: parse error on line 3, using # in rpcpassword can be ambiguous and should be avoided",
        ),
        (
            "rpcpassword_hash_section",
            "Error: Error reading configuration file: parse error on line 4, using # in rpcpassword can be ambiguous and should be avoided",
        ),
        (
            "proxy_without_value",
            "Error: Error parsing command line arguments: Can not set -proxy with no value. Please specify value with -proxy=value.",
        ),
        (
            "ignored_conf",
            "Error: Data directory \"{T}/d\" contains a \"dash.conf\" file which is ignored, because a different configuration file \"{T}/other.conf\" from command line argument \"-conf={T}/other.conf\" is being used instead.",
        ),
        (
            "ignored_default_conf",
            "Error: Data directory \"{T}/d\" contains a \"dash.conf\" file which is ignored, because a different configuration file \"{T}/home/.dashcore/dash.conf\" from data directory \"{T}/home/.dashcore\" is being used instead.",
        ),
        ("datadir_missing", "Error: Specified data directory \"{T}/missing\" does not exist."),
        (
            "conf_datadir_missing",
            "Error: Error reading configuration file: specified data directory \"{T}/new\" does not exist.",
        ),
        (
            "conf_missing_absolute",
            "Error: Error reading configuration file: specified config file \"{T}/none.conf\" could not be opened.",
        ),
        (
            "includeconf_cmdline",
            "Error: Error parsing command line arguments: -includeconf cannot be used from commandline; -includeconf=\"relative2.conf\"",
        ),
        (
            "includeconf_cmdline_double_negative",
            "Error: Error parsing command line arguments: -includeconf cannot be used from commandline; -includeconf=true",
        ),
        (
            "include_recursive",
            "warning: -includeconf cannot be used from included files; ignoring -includeconf=relative2.conf",
        ),
        (
            "include_missing",
            "Error: Error reading configuration file: Failed to include configuration file relative.conf",
        ),
        (
            "settings_invalid_json",
            "Error: Settings file could not be read:\n- Settings file {T}/d/regtest/settings.json does not contain valid JSON. This is probably caused by disk corruption or a crash",
        ),
        (
            "settings_non_object",
            "Error: Settings file could not be read:\n- Found non-object value \"string\" in settings file",
        ),
        (
            "settings_duplicate_key",
            "Error: Settings file could not be read:\n- Found duplicate key key in settings file",
        ),
    ];
    for (name, expected) in expectations {
        let stderr = field(&init_case(name), "stderr").get_str().unwrap().to_string();
        assert!(stderr.starts_with(expected), "{name}: {stderr:?}");
    }
}

#[test]
fn unknown_config_key_is_logged() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dash.conf"), "dash_conf=1\n").unwrap();
    let (args, log) = dashd(&[&datadir_arg(dir.path())]);
    init(&args);
    assert!(logged(&log, "Ignoring unknown configuration value dash_conf\n"));
}

#[test]
fn reindex_in_config_is_logged() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dash.conf"), "reindex=1\n").unwrap();
    let (args, log) = dashd(&[&datadir_arg(dir.path())]);
    init(&args);
    assert!(logged(
        &log,
        "Warning: reindex=1 is set in the configuration file, which will significantly slow down startup. Consider removing or commenting out this option for better performance, unless there is currently a condition which makes rebuilding the indexes necessary\n"
    ));
}

#[test]
fn network_only_option_in_default_section() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dash.conf"), "regtest=1\nwallet=foo\n").unwrap();
    let (args, _) = dashd(&[&datadir_arg(dir.path())]);
    init(&args);
    assert_eq!(
        unsuitable_section_only_args_error(&args).unwrap().as_deref(),
        Some(
            "Config setting for -wallet only applied on regtest network when in [regtest] section.\n"
        )
    );
    // The same setting in the [regtest] section, or on the command line, is fine.
    std::fs::write(dir.path().join("dash.conf"), "regtest=1\n[regtest]\nwallet=foo\n").unwrap();
    let (args, _) = dashd(&[&datadir_arg(dir.path())]);
    init(&args);
    assert_eq!(unsuitable_section_only_args_error(&args).unwrap(), None);
    assert_eq!(args.get_args("-wallet").unwrap(), ["foo"]);
}

#[test]
fn unrecognized_sections_are_warned_about() {
    let dir = tempfile::tempdir().unwrap();
    let inc = dir.path().join("include.conf");
    let inc2 = dir.path().join("include2.conf");
    std::fs::write(
        dir.path().join("dash.conf"),
        format!("regtest=1\nincludeconf={}\nincludeconf={}\n", inc.display(), inc2.display()),
    )
    .unwrap();
    std::fs::write(&inc, "testnot.datadir=1\n").unwrap();
    std::fs::write(&inc2, "[testnet]\n").unwrap();
    let (args, _) = dashd(&[&datadir_arg(dir.path())]);
    init(&args);
    let warning = unrecognized_sections_warning(&args).unwrap();
    assert_eq!(
        noui_warning_line(&warning),
        format!(
            "Warning: {}:1 Section [testnot] is not recognized.\n{}:1 Section [testnet] is not recognized.\n\n",
            inc.display(),
            inc2.display()
        )
    );
}

#[test]
fn config_path_survives_datadir_change() {
    let dir = tempfile::tempdir().unwrap();
    let new = dir.path().join("new");
    std::fs::create_dir(&new).unwrap();
    let conf = dir.path().join("first.conf");
    std::fs::write(&conf, format!("datadir={}\nregtest=1\n", new.display())).unwrap();
    let (args, _) = dashd(&[&format!("-conf={}", conf.display())]);
    init(&args);
    assert_eq!(args.get_config_file_path(), conf);
    assert_eq!(args.get_data_dir_base().unwrap(), new);
}

#[test]
fn blocks_dir_follows_config_datadir() {
    let dir = tempfile::tempdir().unwrap();
    let new = dir.path().join("new");
    std::fs::create_dir(&new).unwrap();
    let conf = dir.path().join("dash.conf");
    std::fs::write(&conf, format!("datadir={}\nregtest=1\n", new.display())).unwrap();
    let (args, _) = dashd(&[&format!("-conf={}", conf.display())]);
    init(&args);
    let blocks = args.get_blocks_dir_path().unwrap();
    assert_eq!(blocks, new.join("regtest").join("blocks"));
    assert!(blocks.is_dir());
}

#[test]
fn double_negative_is_logged() {
    let (_, log) = dashd(&["-noconnect=0"]);
    assert!(logged(&log, "Warning: parsed potentially confusing double-negative -connect=0\n"));
}

#[test]
fn args_log_masks_sensitive_values() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dash.conf"), "regtest=1\n[regtest]\nserver=1\n").unwrap();
    let (args, log) = dashd(&[
        &datadir_arg(dir.path()),
        "-addnode=some.node",
        "-rpcauth=alice:f7efda5c189b999524f151318c0c86$d5b51b3beffbc0",
        "-rpcbind=127.1.1.1",
        "-rpcbind=127.0.0.1",
        "-rpcallowip=127.0.0.1",
        "-rpcpassword=",
        "-rpcuser=secret-rpcuser",
        "-torpassword=secret-torpassword",
    ]);
    init(&args);
    log.lock().unwrap().clear();
    args.log_args();
    let log = log.lock().unwrap().concat();
    for expected in [
        "Command-line arg: addnode=\"some.node\"",
        "Command-line arg: rpcauth=****",
        "Command-line arg: rpcpassword=****",
        "Command-line arg: rpcuser=****",
        "Command-line arg: torpassword=****",
        "Config file arg: regtest=\"1\"",
        "Config file arg: [regtest] server=\"1\"",
    ] {
        assert!(log.contains(expected), "missing {expected:?} in\n{log}");
    }
    for unexpected in [
        "alice:f7efda5c189b999524f151318c0c86$d5b51b3beffbc0",
        "secret-rpcuser",
        "secret-torpassword",
        "Command-line arg: rpcbind=****",
        "Command-line arg: rpcallowip=****",
    ] {
        assert!(!log.contains(unexpected), "unexpected {unexpected:?} in\n{log}");
    }
}

#[test]
fn networkactive_forms() {
    // init.cpp: `SetNetworkActive(args.GetBoolArg("-networkactive", true))`.
    for (argv, expected) in [
        (&[][..], true),
        (&["-networkactive"][..], true),
        (&["-networkactive=1"][..], true),
        (&["-networkactive=0"][..], false),
        (&["-nonetworkactive"][..], false),
        (&["-nonetworkactive=1"][..], false),
    ] {
        let (args, _) = dashd(argv);
        assert_eq!(args.get_bool_arg("-networkactive", true).unwrap(), expected, "{argv:?}");
    }
    // `-connect=0` and `-noconnect` both disable connecting (init.cpp checks
    // `GetArgs("-connect")` for a lone "0" and `IsArgNegated`).
    let (args, _) = dashd(&["-connect=0"]);
    assert_eq!(args.get_args("-connect").unwrap(), ["0"]);
    let (args, _) = dashd(&["-noconnect"]);
    assert!(args.is_arg_negated("-connect"));
    assert!(args.get_args("-connect").unwrap().is_empty());
}

#[test]
fn includeconf_order() {
    // The functional test checks the user agent comments ("main; relative)", then
    // "main; relative; relative2)"), which come from GetArgs("-uacomment").
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join("relative.conf"), "uacomment=relative\n").unwrap();
    std::fs::write(d.join("relative2.conf"), "uacomment=relative2\n").unwrap();
    std::fs::write(d.join("dash.conf"), "uacomment=main\nincludeconf=relative.conf\n").unwrap();
    let (args, _) = dashd(&[&datadir_arg(d)]);
    init(&args);
    assert_eq!(args.get_args("-uacomment").unwrap(), ["main", "relative"]);

    // Recursive includes are ignored, with a warning on stderr.
    std::fs::write(d.join("relative.conf"), "uacomment=relative\nincludeconf=relative2.conf\n")
        .unwrap();
    let (args, _) = dashd(&[&datadir_arg(d)]);
    let stderr = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&stderr);
    args.set_stderr_sink(Box::new(move |s| sink.lock().unwrap().push_str(s)));
    init(&args);
    assert_eq!(args.get_args("-uacomment").unwrap(), ["main", "relative"]);
    assert_eq!(
        *stderr.lock().unwrap(),
        "warning: -includeconf cannot be used from included files; ignoring -includeconf=relative2.conf\n"
    );

    // Several includes from the main file are all read.
    std::fs::write(d.join("relative.conf"), "uacomment=relative\n").unwrap();
    std::fs::write(
        d.join("dash.conf"),
        "uacomment=main\nincludeconf=relative.conf\nincludeconf=relative2.conf\n",
    )
    .unwrap();
    let (args, _) = dashd(&[&datadir_arg(d)]);
    init(&args);
    assert_eq!(args.get_args("-uacomment").unwrap(), ["main", "relative", "relative2"]);
}

const DEFAULT_SETTINGS: &str = "{\n    \"_warning_\": \"This file is automatically generated and updated by Dash Core. Please do not edit this file while the node is running, as any changes might be ignored or overwritten.\"\n}\n";

#[test]
fn default_settings_file() {
    let dir = tempfile::tempdir().unwrap();
    let (args, _) = dashd(&[&datadir_arg(dir.path()), "-regtest"]);
    init(&args);
    let settings = dir.path().join("regtest").join("settings.json");
    // Windows writes it in text mode.
    let expected = if cfg!(windows) {
        DEFAULT_SETTINGS.replace('\n', "\r\n")
    } else {
        DEFAULT_SETTINGS.into()
    };
    assert_eq!(std::fs::read_to_string(settings).unwrap(), expected);
    // Core's own file, written by the release dashd for an empty datadir.
    let case = init_case("chain_regtest");
    let core_file = field(field(&case, "created"), "d/regtest/settings.json");
    assert_eq!(core_file.get_str().unwrap(), expected);
}

#[test]
fn settings_values_are_logged() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("regtest")).unwrap();
    std::fs::write(
        dir.path().join("regtest").join("settings.json"),
        r#"{"string": "string", "num": 5, "bool": true, "null": null, "list": [6, 7]}"#,
    )
    .unwrap();
    let (args, log) = dashd(&[&datadir_arg(dir.path()), "-regtest"]);
    init(&args);
    args.log_args();
    for line in [
        "Ignoring unknown rw_settings value bool\n",
        "Ignoring unknown rw_settings value list\n",
        "Ignoring unknown rw_settings value null\n",
        "Ignoring unknown rw_settings value num\n",
        "Ignoring unknown rw_settings value string\n",
        "Setting file arg: string = \"string\"\n",
        "Setting file arg: num = 5\n",
        "Setting file arg: bool = true\n",
        "Setting file arg: null = null\n",
        "Setting file arg: list = [6,7]\n",
    ] {
        assert!(logged(&log, line), "missing {line:?}");
    }

    // An alternate settings path.
    let alt = dir.path().join("altsettings.json");
    std::fs::write(&alt, r#"{"key": "value"}"#).unwrap();
    let (args, log) =
        dashd(&[&datadir_arg(dir.path()), "-regtest", &format!("-settings={}", alt.display())]);
    init(&args);
    args.log_args();
    assert!(logged(&log, "Setting file arg: key = \"value\"\n"));
}

#[test]
fn nosettings_is_logged() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("regtest")).unwrap();
    std::fs::write(dir.path().join("regtest").join("settings.json"), "invalid json").unwrap();
    let (args, log) = dashd(&[&datadir_arg(dir.path()), "-regtest", "-nosettings"]);
    init(&args);
    args.log_args();
    assert!(logged(&log, "Command-line arg: settings=false\n"));

    std::fs::write(dir.path().join("dash.conf"), "regtest=1\n[regtest]\nnosettings=1\n").unwrap();
    let (args, log) = dashd(&[&datadir_arg(dir.path())]);
    init(&args);
    args.log_args();
    assert!(logged(&log, "Config file arg: [regtest] settings=false\n"));
}
