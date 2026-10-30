//! Port of Dash Core v24.0.0-rc.3 `src/test/argsman_tests.cpp` (suite `argsman_tests`).
//!
//! Every `BOOST_AUTO_TEST_CASE` / `BOOST_FIXTURE_TEST_CASE` is one `#[test]` with the same
//! cases and expectations. `BOOST_CHECK_THROW(..., std::runtime_error)` becomes "returns
//! `Err`". The `util_ArgsMerge` and `util_ChainMerge` results texts are compared byte for byte
//! with Core's captured output (`tests/data/core-v24/{args,chain}_merge.txt`) and their
//! `CHash256` (sha256d) hex with Core's constants.
//!
//! Core's `TestArgsManager` maps onto the public API: its constructor's
//! `m_network_only_args.clear()` is `ArgsManager::new()` (no network-only args),
//! `ReadConfigString` is `clear_config` + `read_config_stream(.., "", false)`, direct
//! `m_settings` access is `lock_settings`, and `m_network = x` is `select_config_network(x)`.

use std::fs;
use std::path::PathBuf;

use dash_args::{ArgsManager, OptionsCategory, SettingsValue};

mod common;
use common::{
    assert_matches_core, for_each_no_dup, get_arg, get_bool_arg, get_int_arg, path_string,
    setup_args, sha256d_hex,
};

/// Core: `TestArgsManager::ReadConfigString` (`BOOST_REQUIRE`s success).
fn read_config_string(args: &ArgsManager, str_config: &str) {
    args.clear_config();
    if let Err(error) = args.read_config_stream(str_config, "", false) {
        panic!("ReadConfigStream failed: {error}");
    }
}

fn get_args(args: &ArgsManager, name: &str) -> Vec<String> {
    args.get_args(name).unwrap()
}

/// Core's `ParseParameters(argc, argv, error)`: `(success, error)`; `error` is `""` on success.
fn parse(args: &ArgsManager, argv: &[&str]) -> (bool, String) {
    match args.parse_parameters(argv) {
        Ok(()) => (true, String::new()),
        Err(e) => (false, e),
    }
}

#[test]
fn util_datadir() {
    // Core: BasicTestingSetup's m_path_root.
    let root = tempfile::tempdir().unwrap();
    let m_path_root = root.path();

    // Use local args variable instead of m_args to avoid making assumptions about test setup
    let args = ArgsManager::new();
    args.force_set_arg("-datadir", &path_string(m_path_root));

    let dd_norm = path_string(&args.get_data_dir_base().unwrap());
    assert!(!dd_norm.is_empty());

    for suffix in ["/", "/.", "/./", "/.//"] {
        args.force_set_arg("-datadir", &format!("{dd_norm}{suffix}"));
        args.clear_path_cache();
        assert_eq!(dd_norm, path_string(&args.get_data_dir_base().unwrap()), "suffix {suffix:?}");
    }
}

/// Core: `CheckValueTest::Expect`.
struct Expect {
    setting: SettingsValue,
    default_string: bool,
    default_int: bool,
    default_bool: bool,
    string_value: Option<&'static str>,
    int_value: Option<i64>,
    bool_value: Option<bool>,
    list_value: Option<Vec<&'static str>>,
    error: Option<&'static str>,
}

impl Expect {
    fn new(s: impl Into<SettingsValue>) -> Self {
        Expect {
            setting: s.into(),
            default_string: false,
            default_int: false,
            default_bool: false,
            string_value: None,
            int_value: None,
            bool_value: None,
            list_value: None,
            error: None,
        }
    }
    /// Core: `Expect{{}}` (a null setting).
    fn null() -> Self {
        Expect::new(SettingsValue::Null)
    }
    fn default_string(mut self) -> Self {
        self.default_string = true;
        self
    }
    fn default_int(mut self) -> Self {
        self.default_int = true;
        self
    }
    fn default_bool(mut self) -> Self {
        self.default_bool = true;
        self
    }
    fn string(mut self, s: &'static str) -> Self {
        self.string_value = Some(s);
        self
    }
    fn int(mut self, i: i64) -> Self {
        self.int_value = Some(i);
        self
    }
    fn bool(mut self, b: bool) -> Self {
        self.bool_value = Some(b);
        self
    }
    fn list(mut self, m: &[&'static str]) -> Self {
        self.list_value = Some(m.to_vec());
        self
    }
    #[allow(dead_code)] // Core's Expect::Error; no case in this file uses it.
    fn error(mut self, e: &'static str) -> Self {
        self.error = Some(e);
        self
    }
}

/// Core: `CheckValueTest::CheckValue`: test GetSetting and GetArg type coercion, negation, and
/// default value handling.
fn check_value(flags: u32, arg: Option<&str>, expect: Expect) {
    let ctx = format!("arg {arg:?}");
    let test = ArgsManager::new();
    setup_args(&test, &[("-value", flags)]);
    let argv: Vec<&str> = match arg {
        Some(a) => vec!["ignored", a],
        None => vec!["ignored"],
    };
    let (success, error) = parse(&test, &argv);

    assert_eq!(test.get_setting("-value").write(), expect.setting.write(), "{ctx}");
    let settings_list = test.get_settings_list("-value");
    if expect.setting.is_null() || expect.setting.is_false() {
        assert_eq!(settings_list.len(), 0, "{ctx}");
    } else {
        assert_eq!(settings_list.len(), 1, "{ctx}");
        assert_eq!(settings_list[0].write(), expect.setting.write(), "{ctx}");
    }

    if let Some(e) = expect.error {
        assert!(!success, "{ctx}");
        assert!(error.contains(e), "{ctx}: {error}");
    } else {
        assert!(success, "{ctx}");
        assert_eq!(error, "", "{ctx}");
    }

    if expect.default_string {
        assert_eq!(get_arg(&test, "-value", "zzzzz"), "zzzzz", "{ctx}");
    } else if let Some(s) = expect.string_value {
        assert_eq!(get_arg(&test, "-value", "zzzzz"), s, "{ctx}");
    } else {
        assert!(!success, "{ctx}");
    }

    if expect.default_int {
        assert_eq!(get_int_arg(&test, "-value", 99999), 99999, "{ctx}");
    } else if let Some(i) = expect.int_value {
        assert_eq!(get_int_arg(&test, "-value", 99999), i, "{ctx}");
    } else {
        assert!(!success, "{ctx}");
    }

    if expect.default_bool {
        assert!(!get_bool_arg(&test, "-value", false), "{ctx}");
        assert!(get_bool_arg(&test, "-value", true), "{ctx}");
    } else if let Some(b) = expect.bool_value {
        assert_eq!(get_bool_arg(&test, "-value", false), b, "{ctx}");
        assert_eq!(get_bool_arg(&test, "-value", true), b, "{ctx}");
    } else {
        assert!(!success, "{ctx}");
    }

    if let Some(list) = expect.list_value {
        assert_eq!(get_args(&test, "-value"), list, "{ctx}");
    } else {
        assert!(!success, "{ctx}");
    }
}

#[test]
fn util_check_value() {
    const ALLOW_ANY: u32 = ArgsManager::ALLOW_ANY;

    check_value(
        ALLOW_ANY,
        None,
        Expect::null().default_string().default_int().default_bool().list(&[]),
    );
    check_value(
        ALLOW_ANY,
        Some("-novalue"),
        Expect::new(false).string("0").int(0).bool(false).list(&[]),
    );
    check_value(
        ALLOW_ANY,
        Some("-novalue="),
        Expect::new(false).string("0").int(0).bool(false).list(&[]),
    );
    check_value(
        ALLOW_ANY,
        Some("-novalue=0"),
        Expect::new(true).string("1").int(1).bool(true).list(&["1"]),
    );
    check_value(
        ALLOW_ANY,
        Some("-novalue=1"),
        Expect::new(false).string("0").int(0).bool(false).list(&[]),
    );
    check_value(
        ALLOW_ANY,
        Some("-novalue=2"),
        Expect::new(false).string("0").int(0).bool(false).list(&[]),
    );
    check_value(
        ALLOW_ANY,
        Some("-novalue=abc"),
        Expect::new(true).string("1").int(1).bool(true).list(&["1"]),
    );
    check_value(
        ALLOW_ANY,
        Some("-value"),
        Expect::new("").string("").int(0).bool(true).list(&[""]),
    );
    check_value(
        ALLOW_ANY,
        Some("-value="),
        Expect::new("").string("").int(0).bool(true).list(&[""]),
    );
    check_value(
        ALLOW_ANY,
        Some("-value=0"),
        Expect::new("0").string("0").int(0).bool(false).list(&["0"]),
    );
    check_value(
        ALLOW_ANY,
        Some("-value=1"),
        Expect::new("1").string("1").int(1).bool(true).list(&["1"]),
    );
    check_value(
        ALLOW_ANY,
        Some("-value=2"),
        Expect::new("2").string("2").int(2).bool(true).list(&["2"]),
    );
    check_value(
        ALLOW_ANY,
        Some("-value=abc"),
        Expect::new("abc").string("abc").int(0).bool(false).list(&["abc"]),
    );
}

/// Core: `NoIncludeConfTest::Parse`.
fn parse_no_include_conf(arg: &str) -> String {
    let test = ArgsManager::new();
    setup_args(&test, &[("-includeconf", ArgsManager::ALLOW_ANY)]);
    parse(&test, &["ignored", arg]).1
}

#[test]
fn util_no_include_conf() {
    assert_eq!(parse_no_include_conf("-noincludeconf"), "");
    assert_eq!(
        parse_no_include_conf("-includeconf"),
        "-includeconf cannot be used from commandline; -includeconf=\"\""
    );
    assert_eq!(
        parse_no_include_conf("-includeconf=file"),
        "-includeconf cannot be used from commandline; -includeconf=\"file\""
    );
}

#[test]
fn util_parse_parameters() {
    let test_args = ArgsManager::new();
    let a = ("-a", ArgsManager::ALLOW_ANY);
    let b = ("-b", ArgsManager::ALLOW_ANY);
    let ccc = ("-ccc", ArgsManager::ALLOW_ANY);
    let d = ("-d", ArgsManager::ALLOW_ANY);

    let argv_test = ["-ignored", "-a", "-b", "-ccc=argument", "-ccc=multiple", "f", "-d=e"];

    setup_args(&test_args, &[a, b, ccc, d]);
    let empty = |args: &ArgsManager| {
        args.lock_settings(|s| s.command_line_options.is_empty() && s.ro_config.is_empty())
    };
    assert!(parse(&test_args, &argv_test[..0]).0);
    assert!(empty(&test_args));

    assert!(parse(&test_args, &argv_test[..1]).0);
    assert!(empty(&test_args));

    assert!(parse(&test_args, &argv_test[..7]).0);
    // expectation: -ignored is ignored (program name argument),
    // -a, -b and -ccc end up in map, -d ignored because it is after
    // a non-option argument (non-GNU option parsing)
    let (clo, ro_empty) =
        test_args.lock_settings(|s| (s.command_line_options.clone(), s.ro_config.is_empty()));
    assert!(clo.len() == 3 && ro_empty);
    assert!(
        test_args.is_arg_set("-a")
            && test_args.is_arg_set("-b")
            && test_args.is_arg_set("-ccc")
            && !test_args.is_arg_set("f")
            && !test_args.is_arg_set("-d")
    );
    assert!(
        clo.contains_key("a")
            && clo.contains_key("b")
            && clo.contains_key("ccc")
            && !clo.contains_key("f")
            && !clo.contains_key("d")
    );

    assert!(clo["a"].len() == 1);
    assert!(clo["a"].first().unwrap().get_str().unwrap().is_empty());
    assert!(clo["ccc"].len() == 2);
    assert!(clo["ccc"].first().unwrap().get_str().unwrap() == "argument");
    assert!(clo["ccc"].last().unwrap().get_str().unwrap() == "multiple");
    assert!(get_args(&test_args, "-ccc").len() == 2);
}

#[test]
fn util_parse_invalid_parameters() {
    let test = ArgsManager::new();
    setup_args(&test, &[("-registered", ArgsManager::ALLOW_ANY)]);

    let mut argv = ["ignored", "-registered"];
    let (ok, error) = parse(&test, &argv);
    assert!(ok);
    assert_eq!(error, "");

    argv[1] = "-unregistered";
    let (ok, error) = parse(&test, &argv);
    assert!(!ok);
    assert_eq!(error, "Invalid parameter -unregistered");

    // Make sure registered parameters prefixed with a chain name trigger errors.
    // (Previously, they were accepted and ignored.)
    argv[1] = "-test.registered";
    let (ok, error) = parse(&test, &argv);
    assert!(!ok);
    assert_eq!(error, "Invalid parameter -test.registered");
}

/// Core: `TestParse`.
fn test_parse(str: &str, expected_bool: bool, expected_int: i64) {
    let test = ArgsManager::new();
    setup_args(&test, &[("-value", ArgsManager::ALLOW_ANY)]);
    let arg = format!("-value={str}");
    assert!(parse(&test, &["ignored", &arg]).0);
    assert_eq!(get_bool_arg(&test, "-value", false), expected_bool, "{str:?}");
    assert_eq!(get_bool_arg(&test, "-value", true), expected_bool, "{str:?}");
    assert_eq!(get_int_arg(&test, "-value", 99998), expected_int, "{str:?}");
    assert_eq!(get_int_arg(&test, "-value", 99999), expected_int, "{str:?}");
}

// Test bool and int parsing.
#[test]
fn util_arg_parsing() {
    // Some of these cases could be ambiguous or surprising to users, and might
    // be worth triggering errors or warnings in the future. But for now basic
    // test coverage is useful to avoid breaking backwards compatibility
    // unintentionally.
    test_parse("", true, 0);
    test_parse(" ", false, 0);
    test_parse("0", false, 0);
    test_parse("0 ", false, 0);
    test_parse(" 0", false, 0);
    test_parse("+0", false, 0);
    test_parse("-0", false, 0);
    test_parse("5", true, 5);
    test_parse("5 ", true, 5);
    test_parse(" 5", true, 5);
    test_parse("+5", true, 5);
    test_parse("-5", true, -5);
    test_parse("0 5", false, 0);
    test_parse("5 0", true, 5);
    test_parse("050", true, 50);
    test_parse("0.", false, 0);
    test_parse("5.", true, 5);
    test_parse("0.0", false, 0);
    test_parse("0.5", false, 0);
    test_parse("5.0", true, 5);
    test_parse("5.5", true, 5);
    test_parse("x", false, 0);
    test_parse("x0", false, 0);
    test_parse("x5", false, 0);
    test_parse("0x", false, 0);
    test_parse("5x", true, 5);
    test_parse("0x5", false, 0);
    test_parse("false", false, 0);
    test_parse("true", false, 0);
    test_parse("yes", false, 0);
    test_parse("no", false, 0);
}

#[test]
fn util_get_bool_arg() {
    let test_args = ArgsManager::new();
    let names = ["-a", "-b", "-c", "-d", "-e", "-f"];
    let list: Vec<(&str, u32)> = names.iter().map(|n| (*n, ArgsManager::ALLOW_ANY)).collect();

    let argv_test = ["ignored", "-a", "-nob", "-c=0", "-d=1", "-e=false", "-f=true"];
    setup_args(&test_args, &list);
    assert!(parse(&test_args, &argv_test).0);

    // Each letter should be set. (Core also iterates the literal's trailing NUL, which its
    // `|| !opt` accepts.)
    for opt in "abcdef".chars() {
        assert!(test_args.is_arg_set(&format!("-{opt}")), "-{opt}");
    }

    // Nothing else should be in the map
    assert!(
        test_args.lock_settings(|s| s.command_line_options.len() == 6 && s.ro_config.is_empty())
    );

    // The -no prefix should get stripped on the way in.
    assert!(!test_args.is_arg_set("-nob"));

    // The -b option is flagged as negated, and nothing else is
    assert!(test_args.is_arg_negated("-b"));
    assert!(!test_args.is_arg_negated("-a"));

    // Check expected values.
    assert!(get_bool_arg(&test_args, "-a", false));
    assert!(!get_bool_arg(&test_args, "-b", true));
    assert!(!get_bool_arg(&test_args, "-c", true));
    assert!(get_bool_arg(&test_args, "-d", false));
    assert!(!get_bool_arg(&test_args, "-e", true));
    assert!(!get_bool_arg(&test_args, "-f", true));
}

#[test]
fn util_get_bool_arg_edge_cases() {
    // Test some awful edge cases that hopefully no user will ever exercise.
    let test_args = ArgsManager::new();

    // Params test
    let foo = ("-foo", ArgsManager::ALLOW_ANY);
    let bar = ("-bar", ArgsManager::ALLOW_ANY);
    let argv_test = ["ignored", "-nofoo", "-foo", "-nobar=0"];
    setup_args(&test_args, &[foo, bar]);
    assert!(parse(&test_args, &argv_test[..4]).0);

    // This was passed twice, second one overrides the negative setting.
    assert!(!test_args.is_arg_negated("-foo"));
    assert!(get_arg(&test_args, "-foo", "xxx").is_empty());

    // A double negative is a positive, and not marked as negated.
    assert!(!test_args.is_arg_negated("-bar"));
    assert!(get_arg(&test_args, "-bar", "xxx") == "1");

    // Config test
    let conf_test = "nofoo=1\nfoo=1\nnobar=0\n";
    assert!(parse(&test_args, &argv_test[..1]).0);
    read_config_string(&test_args, conf_test);

    // This was passed twice, second one overrides the negative setting,
    // and the value.
    assert!(!test_args.is_arg_negated("-foo"));
    assert!(get_arg(&test_args, "-foo", "xxx") == "1");

    // A double negative is a positive, and does not count as negated.
    assert!(!test_args.is_arg_negated("-bar"));
    assert!(get_arg(&test_args, "-bar", "xxx") == "1");

    // Combined test
    let combo_test_args = ["ignored", "-nofoo", "-bar"];
    let combo_test_conf = "foo=1\nnobar=1\n";
    assert!(parse(&test_args, &combo_test_args[..3]).0);
    read_config_string(&test_args, combo_test_conf);

    // Command line overrides, but doesn't erase old setting
    assert!(test_args.is_arg_negated("-foo"));
    assert!(get_arg(&test_args, "-foo", "xxx") == "0");
    assert!(get_args(&test_args, "-foo").is_empty());

    // Command line overrides, but doesn't erase old setting
    assert!(!test_args.is_arg_negated("-bar"));
    assert!(get_arg(&test_args, "-bar", "xxx").is_empty());
    assert!(get_args(&test_args, "-bar").len() == 1 && get_args(&test_args, "-bar")[0].is_empty());
}

#[test]
fn util_read_config_stream() {
    let str_config = "a=\n\
                      b=1\n\
                      ccc=argument\n\
                      ccc=multiple\n\
                      d=e\n\
                      nofff=1\n\
                      noggg=0\n\
                      h=1\n\
                      noh=1\n\
                      noi=1\n\
                      i=1\n\
                      sec1.ccc=extend1\n\
                      \n\
                      [sec1]\n\
                      ccc=extend2\n\
                      d=eee\n\
                      h=1\n\
                      [sec2]\n\
                      ccc=extend3\n\
                      iii=2\n";

    let test_args = ArgsManager::new();
    let names = ["-a", "-b", "-ccc", "-d", "-e", "-fff", "-ggg", "-h", "-i", "-iii"];
    let list: Vec<(&str, u32)> = names.iter().map(|n| (*n, ArgsManager::ALLOW_ANY)).collect();
    setup_args(&test_args, &list);

    read_config_string(&test_args, str_config);
    // expectation: a, b, ccc, d, fff, ggg, h, i end up in map
    // so do sec1.ccc, sec1.d, sec1.h, sec2.ccc, sec2.iii

    let (clo_empty, ro) =
        test_args.lock_settings(|s| (s.command_line_options.is_empty(), s.ro_config.clone()));
    assert!(clo_empty);
    assert!(ro.len() == 3);
    assert!(ro[""].len() == 8);
    assert!(ro["sec1"].len() == 3);
    assert!(ro["sec2"].len() == 2);

    for key in ["a", "b", "ccc", "d", "fff", "ggg", "h", "i"] {
        assert!(ro[""].contains_key(key), "{key}");
    }
    assert!(ro["sec1"].contains_key("ccc"));
    assert!(ro["sec1"].contains_key("h"));
    assert!(ro["sec2"].contains_key("ccc"));
    assert!(ro["sec2"].contains_key("iii"));

    assert!(test_args.is_arg_set("-a"));
    assert!(test_args.is_arg_set("-b"));
    assert!(test_args.is_arg_set("-ccc"));
    assert!(test_args.is_arg_set("-d"));
    assert!(test_args.is_arg_set("-fff"));
    assert!(test_args.is_arg_set("-ggg"));
    assert!(test_args.is_arg_set("-h"));
    assert!(test_args.is_arg_set("-i"));
    assert!(!test_args.is_arg_set("-zzz"));
    assert!(!test_args.is_arg_set("-iii"));

    assert_eq!(get_arg(&test_args, "-a", "xxx"), "");
    assert_eq!(get_arg(&test_args, "-b", "xxx"), "1");
    assert_eq!(get_arg(&test_args, "-ccc", "xxx"), "argument");
    assert_eq!(get_arg(&test_args, "-d", "xxx"), "e");
    assert_eq!(get_arg(&test_args, "-fff", "xxx"), "0");
    assert_eq!(get_arg(&test_args, "-ggg", "xxx"), "1");
    assert_eq!(get_arg(&test_args, "-h", "xxx"), "0");
    assert_eq!(get_arg(&test_args, "-i", "xxx"), "1");
    assert_eq!(get_arg(&test_args, "-zzz", "xxx"), "xxx");
    assert_eq!(get_arg(&test_args, "-iii", "xxx"), "xxx");

    for def in [false, true] {
        assert!(get_bool_arg(&test_args, "-a", def));
        assert!(get_bool_arg(&test_args, "-b", def));
        assert!(!get_bool_arg(&test_args, "-ccc", def));
        assert!(!get_bool_arg(&test_args, "-d", def));
        assert!(!get_bool_arg(&test_args, "-fff", def));
        assert!(get_bool_arg(&test_args, "-ggg", def));
        assert!(!get_bool_arg(&test_args, "-h", def));
        assert!(get_bool_arg(&test_args, "-i", def));
        assert!(get_bool_arg(&test_args, "-zzz", def) == def);
        assert!(get_bool_arg(&test_args, "-iii", def) == def);
    }

    assert_eq!(get_args(&test_args, "-a"), [""]);
    assert_eq!(get_args(&test_args, "-b"), ["1"]);
    assert_eq!(get_args(&test_args, "-ccc"), ["argument", "multiple"]);
    assert!(get_args(&test_args, "-fff").is_empty());
    assert!(get_args(&test_args, "-nofff").is_empty());
    assert_eq!(get_args(&test_args, "-ggg"), ["1"]);
    assert!(get_args(&test_args, "-noggg").is_empty());
    assert!(get_args(&test_args, "-h").is_empty());
    assert!(get_args(&test_args, "-noh").is_empty());
    assert_eq!(get_args(&test_args, "-i"), ["1"]);
    assert!(get_args(&test_args, "-noi").is_empty());
    assert!(get_args(&test_args, "-zzz").is_empty());

    assert!(!test_args.is_arg_negated("-a"));
    assert!(!test_args.is_arg_negated("-b"));
    assert!(!test_args.is_arg_negated("-ccc"));
    assert!(!test_args.is_arg_negated("-d"));
    assert!(test_args.is_arg_negated("-fff"));
    assert!(!test_args.is_arg_negated("-ggg"));
    assert!(test_args.is_arg_negated("-h")); // last setting takes precedence
    assert!(!test_args.is_arg_negated("-i")); // last setting takes precedence
    assert!(!test_args.is_arg_negated("-zzz"));

    // Test sections work
    test_args.select_config_network("sec1");

    // same as original
    assert_eq!(get_arg(&test_args, "-a", "xxx"), "");
    assert_eq!(get_arg(&test_args, "-b", "xxx"), "1");
    assert_eq!(get_arg(&test_args, "-fff", "xxx"), "0");
    assert_eq!(get_arg(&test_args, "-ggg", "xxx"), "1");
    assert_eq!(get_arg(&test_args, "-zzz", "xxx"), "xxx");
    assert_eq!(get_arg(&test_args, "-iii", "xxx"), "xxx");
    // d is overridden
    assert!(get_arg(&test_args, "-d", "xxx") == "eee");
    // section-specific setting
    assert!(get_arg(&test_args, "-h", "xxx") == "1");
    // section takes priority for multiple values
    assert!(get_arg(&test_args, "-ccc", "xxx") == "extend1");
    // check multiple values works
    let sec1_ccc_expected = ["extend1", "extend2", "argument", "multiple"];
    assert_eq!(get_args(&test_args, "-ccc"), sec1_ccc_expected);

    test_args.select_config_network("sec2");

    // same as original
    assert!(get_arg(&test_args, "-a", "xxx").is_empty());
    assert!(get_arg(&test_args, "-b", "xxx") == "1");
    assert!(get_arg(&test_args, "-d", "xxx") == "e");
    assert!(get_arg(&test_args, "-fff", "xxx") == "0");
    assert!(get_arg(&test_args, "-ggg", "xxx") == "1");
    assert!(get_arg(&test_args, "-zzz", "xxx") == "xxx");
    assert!(get_arg(&test_args, "-h", "xxx") == "0");
    // section-specific setting
    assert!(get_arg(&test_args, "-iii", "xxx") == "2");
    // section takes priority for multiple values
    assert!(get_arg(&test_args, "-ccc", "xxx") == "extend3");
    // check multiple values works
    let sec2_ccc_expected = ["extend3", "argument", "multiple"];
    assert_eq!(get_args(&test_args, "-ccc"), sec2_ccc_expected);

    // Test section only options

    test_args.set_network_only_arg("-d");
    test_args.set_network_only_arg("-ccc");
    test_args.set_network_only_arg("-h");

    test_args.select_config_network("main"); // CBaseChainParams::MAIN
    assert!(get_arg(&test_args, "-d", "xxx") == "e");
    assert!(get_args(&test_args, "-ccc").len() == 2);
    assert!(get_arg(&test_args, "-h", "xxx") == "0");

    test_args.select_config_network("sec1");
    assert!(get_arg(&test_args, "-d", "xxx") == "eee");
    assert!(get_args(&test_args, "-d").len() == 1);
    assert!(get_args(&test_args, "-ccc").len() == 2);
    assert!(get_arg(&test_args, "-h", "xxx") == "1");

    test_args.select_config_network("sec2");
    assert!(get_arg(&test_args, "-d", "xxx") == "xxx");
    assert!(get_args(&test_args, "-d").is_empty());
    assert!(get_args(&test_args, "-ccc").len() == 1);
    assert!(get_arg(&test_args, "-h", "xxx") == "0");
}

#[test]
fn util_get_arg() {
    let test_args = ArgsManager::new();
    fn strs(v: &[&str]) -> Vec<SettingsValue> {
        v.iter().map(|s| SettingsValue::from(*s)).collect()
    }
    test_args.lock_settings(|s| {
        let clo = &mut s.command_line_options;
        clo.clear();
        clo.insert("strtest1".into(), strs(&["string..."]));
        // strtest2 undefined on purpose
        clo.insert("inttest1".into(), strs(&["12345"]));
        clo.insert("inttest2".into(), strs(&["81985529216486895"]));
        // inttest3 undefined on purpose
        clo.insert("booltest1".into(), strs(&[""]));
        // booltest2 undefined on purpose
        clo.insert("booltest3".into(), strs(&["0"]));
        clo.insert("booltest4".into(), strs(&["1"]));

        // priorities
        let ro = s.ro_config.entry(String::new()).or_default();
        clo.insert("pritest1".into(), strs(&["a", "b"]));
        ro.insert("pritest2".into(), strs(&["a", "b"]));
        clo.insert("pritest3".into(), strs(&["a"]));
        ro.insert("pritest3".into(), strs(&["b"]));
        clo.insert("pritest4".into(), strs(&["a", "b"]));
        ro.insert("pritest4".into(), strs(&["c", "d"]));
    });

    assert_eq!(get_arg(&test_args, "strtest1", "default"), "string...");
    assert_eq!(get_arg(&test_args, "strtest2", "default"), "default");
    assert_eq!(get_int_arg(&test_args, "inttest1", -1), 12345);
    assert_eq!(get_int_arg(&test_args, "inttest2", -1), 81985529216486895);
    assert_eq!(get_int_arg(&test_args, "inttest3", -1), -1);
    assert!(get_bool_arg(&test_args, "booltest1", false));
    assert!(!get_bool_arg(&test_args, "booltest2", false));
    assert!(!get_bool_arg(&test_args, "booltest3", false));
    assert!(get_bool_arg(&test_args, "booltest4", false));

    assert_eq!(get_arg(&test_args, "pritest1", "default"), "b");
    assert_eq!(get_arg(&test_args, "pritest2", "default"), "a");
    assert_eq!(get_arg(&test_args, "pritest3", "default"), "a");
    assert_eq!(get_arg(&test_args, "pritest4", "default"), "b");
}

#[test]
fn util_get_chain_name() {
    let test_args = ArgsManager::new();
    let testnet = ("-testnet", ArgsManager::ALLOW_ANY);
    let regtest = ("-regtest", ArgsManager::ALLOW_ANY);
    setup_args(&test_args, &[testnet, regtest]);

    let argv_testnet = ["cmd", "-testnet"];
    let argv_regtest = ["cmd", "-regtest"];
    let argv_test_no_reg = ["cmd", "-testnet", "-noregtest"];
    let argv_both = ["cmd", "-testnet", "-regtest"];

    // equivalent to "-testnet"
    // regtest in testnet section is ignored
    let testnetconf = "testnet=1\nregtest=0\n[test]\nregtest=1";
    let chain = |args: &ArgsManager| args.get_chain_name().unwrap();

    assert!(parse(&test_args, &argv_testnet[..0]).0);
    assert_eq!(chain(&test_args), "main");

    assert!(parse(&test_args, &argv_testnet[..2]).0);
    assert_eq!(chain(&test_args), "test");

    assert!(parse(&test_args, &argv_regtest[..2]).0);
    assert_eq!(chain(&test_args), "regtest");

    assert!(parse(&test_args, &argv_test_no_reg[..3]).0);
    assert_eq!(chain(&test_args), "test");

    assert!(parse(&test_args, &argv_both[..3]).0);
    assert!(test_args.get_chain_name().is_err());

    assert!(parse(&test_args, &argv_testnet[..0]).0);
    read_config_string(&test_args, testnetconf);
    assert_eq!(chain(&test_args), "test");

    assert!(parse(&test_args, &argv_testnet[..2]).0);
    read_config_string(&test_args, testnetconf);
    assert_eq!(chain(&test_args), "test");

    assert!(parse(&test_args, &argv_regtest[..2]).0);
    read_config_string(&test_args, testnetconf);
    assert!(test_args.get_chain_name().is_err());

    assert!(parse(&test_args, &argv_test_no_reg[..3]).0);
    read_config_string(&test_args, testnetconf);
    assert_eq!(chain(&test_args), "test");

    assert!(parse(&test_args, &argv_both[..3]).0);
    read_config_string(&test_args, testnetconf);
    assert!(test_args.get_chain_name().is_err());

    // check setting the network to test (and thus making
    // [test] regtest=1 potentially relevant) doesn't break things
    test_args.select_config_network("test");

    assert!(parse(&test_args, &argv_testnet[..0]).0);
    read_config_string(&test_args, testnetconf);
    assert_eq!(chain(&test_args), "test");

    assert!(parse(&test_args, &argv_testnet[..2]).0);
    read_config_string(&test_args, testnetconf);
    assert_eq!(chain(&test_args), "test");

    assert!(parse(&test_args, &argv_regtest[..2]).0);
    read_config_string(&test_args, testnetconf);
    assert!(test_args.get_chain_name().is_err());

    assert!(parse(&test_args, &argv_test_no_reg[..2]).0);
    read_config_string(&test_args, testnetconf);
    assert_eq!(chain(&test_args), "test");

    assert!(parse(&test_args, &argv_both[..3]).0);
    read_config_string(&test_args, testnetconf);
    assert!(test_args.get_chain_name().is_err());
}

// Test different ways settings can be merged, and verify results. This test can
// be used to confirm that updates to settings code don't change behavior
// unintentionally.
//
// The test covers:
//
// - Combining different setting actions. Possible actions are: configuring a
//   setting, negating a setting (adding "-no" prefix), and configuring/negating
//   settings in a network section (adding "main." or "test." prefixes).
//
// - Combining settings from command line arguments and a config file.
//
// - Combining SoftSet and ForceSet calls.
//
// - Testing "main" and "test" network values to make sure settings from network
//   sections are applied and to check for mainnet-specific behaviors like
//   inheriting settings from the default section.
//
// - Testing network-specific settings like "-wallet", that may be ignored
//   outside a network section, and non-network specific settings like "-server"
//   that aren't sensitive to the network.
mod args_merge {
    /// Max number of actions to sequence together. Can decrease this when
    /// debugging to make test results easier to understand.
    pub const MAX_ACTIONS: usize = 3;

    // enum Action { NONE, SET, NEGATE, SECTION_SET, SECTION_NEGATE };
    pub const NONE: i32 = 0;
    pub const SET: i32 = 1;
    pub const NEGATE: i32 = 2;
    pub const SECTION_SET: i32 = 3;
    pub const SECTION_NEGATE: i32 = 4;

    /// Core: `ArgsMergeTestingSetup::GetValues`: translate actions into a list of
    /// `<key>=<value>` setting strings.
    pub fn get_values(
        actions: &[i32],
        section: &str,
        name: &str,
        value_prefix: &str,
    ) -> Vec<String> {
        let mut values = Vec::new();
        let mut suffix = 0;
        for &action in actions {
            if action == NONE {
                break;
            }
            let mut prefix = String::new();
            if action == SECTION_SET || action == SECTION_NEGATE {
                prefix = format!("{section}.");
            }
            if action == SET || action == SECTION_SET {
                for _ in 0..2 {
                    suffix += 1;
                    values.push(format!("{prefix}{name}={value_prefix}{suffix}"));
                }
            }
            if action == NEGATE || action == SECTION_NEGATE {
                values.push(format!("{prefix}no{name}=1"));
            }
        }
        values
    }
}

/// Core: `ArgsMergeTestingSetup::ForEachMergeSetup`: enumerate all possible test
/// configurations.
#[allow(clippy::type_complexity)]
fn for_each_args_merge_setup(mut f: impl FnMut(&[i32], &[i32], bool, bool, &str, &str, bool)) {
    use args_merge::*;
    // CBaseChainParams::MAIN, TESTNET, DEVNET
    const CHAINS: [&str; 3] = ["main", "test", "devnet"];
    let mut arg_actions = [NONE; MAX_ACTIONS];
    // command_line_options do not have sections. Only iterate over SET and NEGATE
    for_each_no_dup(&mut arg_actions, SET, NEGATE, |arg_actions| {
        let mut conf_actions = [NONE; MAX_ACTIONS];
        for_each_no_dup(&mut conf_actions, SET, SECTION_NEGATE, |conf_actions| {
            for soft_set in [false, true] {
                for force_set in [false, true] {
                    for section in CHAINS {
                        for network in CHAINS {
                            for net_specific in [false, true] {
                                f(
                                    arg_actions,
                                    conf_actions,
                                    soft_set,
                                    force_set,
                                    section,
                                    network,
                                    net_specific,
                                );
                            }
                        }
                    }
                }
            }
        });
    });
}

// Regression test covering different ways config settings can be merged. The
// test parses and merges settings, representing the results as strings that get
// compared against an expected hash. To debug, the result strings can be dumped
// to a file (see comments below).
#[test]
fn util_args_merge() {
    let mut out = String::new();

    for_each_args_merge_setup(
        |arg_actions, conf_actions, soft_set, force_set, section, network, net_specific| {
            let parser = ArgsManager::new();

            let mut desc = format!("net={network}");
            parser.select_config_network(network);

            let name = if net_specific {
                "wallet"
            } else {
                "server"
            };
            let key = format!("-{name}");
            parser.add_arg(&key, name, ArgsManager::ALLOW_ANY, OptionsCategory::Options);
            if net_specific {
                parser.set_network_only_arg(&key);
            }

            let args = args_merge::get_values(arg_actions, section, name, "a");
            let mut argv: Vec<String> = vec!["ignored".into()];
            for arg in args {
                let arg = format!("-{arg}");
                desc += " ";
                desc += &arg;
                argv.push(arg);
            }
            let (ok, error) = match parser.parse_parameters(&argv) {
                Ok(()) => (true, String::new()),
                Err(e) => (false, e),
            };
            assert!(ok, "{desc}");
            assert_eq!(error, "");

            let mut conf = String::new();
            for conf_val in args_merge::get_values(conf_actions, section, name, "c") {
                desc += " ";
                desc += &conf_val;
                conf += &conf_val;
                conf += "\n";
            }
            assert_eq!(parser.read_config_stream(&conf, "filepath", false), Ok(()), "{desc}");

            if soft_set {
                desc += " soft";
                parser.soft_set_arg(&key, "soft1");
                parser.soft_set_arg(&key, "soft2");
            }

            if force_set {
                desc += " force";
                parser.force_set_arg(&key, "force1");
                parser.force_set_arg(&key, "force2");
            }

            desc += " || ";

            if !parser.is_arg_set(&key) {
                desc += "unset";
                assert!(!parser.is_arg_negated(&key));
                assert_eq!(get_arg(&parser, &key, "default"), "default");
                assert!(get_args(&parser, &key).is_empty());
            } else if parser.is_arg_negated(&key) {
                desc += "negated";
                assert_eq!(get_arg(&parser, &key, "default"), "0");
                assert!(get_args(&parser, &key).is_empty());
            } else {
                desc += &get_arg(&parser, &key, "default");
                desc += " |";
                for arg in get_args(&parser, &key) {
                    desc += " ";
                    desc += &arg;
                }
            }

            let ignored = parser.get_unsuitable_section_only_args();
            if !ignored.is_empty() {
                desc += " | ignored";
                for arg in &ignored {
                    desc += " ";
                    desc += arg;
                }
            }

            desc += "\n";

            out += &desc;
        },
    );

    // Core dumps these results with:
    //
    //   ARGS_MERGE_TEST_OUT=results.txt ./test_dash --run_test=argsman_tests/util_ArgsMerge
    //
    // Results file is formatted like:
    //
    //   <input> || <IsArgSet/IsArgNegated/GetArg output> | <GetArgs output> | <GetUnsuitable output>
    assert_matches_core(&out, "args_merge.txt");
    assert_eq!(
        sha256d_hex(out.as_bytes()),
        "c78db4195fcbe4cf3f535f9a72b963bba77fd36099da29dfb8845291a2cf760a"
    );
}

// Similar test as above, but for ArgsManager::GetChainName function.
mod chain_merge {
    pub const MAX_ACTIONS: usize = 2;

    // enum Action { NONE, ENABLE_TEST, DISABLE_TEST, NEGATE_TEST, ENABLE_REG, DISABLE_REG, NEGATE_REG };
    pub const NONE: i32 = 0;
    pub const ENABLE_TEST: i32 = 1;
    pub const DISABLE_TEST: i32 = 2;
    pub const NEGATE_TEST: i32 = 3;
    pub const ENABLE_REG: i32 = 4;
    pub const DISABLE_REG: i32 = 5;
    pub const NEGATE_REG: i32 = 6;

    pub fn arg(action: i32) -> Option<&'static str> {
        match action {
            ENABLE_TEST => Some("-testnet=1"),
            DISABLE_TEST => Some("-testnet=0"),
            NEGATE_TEST => Some("-notestnet=1"),
            ENABLE_REG => Some("-regtest=1"),
            DISABLE_REG => Some("-regtest=0"),
            NEGATE_REG => Some("-noregtest=1"),
            _ => None,
        }
    }
}

#[test]
fn util_chain_merge() {
    use chain_merge::*;
    let mut out = String::new();

    // Core: ChainMergeTestingSetup::ForEachMergeSetup.
    let mut each = |arg_actions: &[i32], conf_actions: &[i32]| {
        let parser = ArgsManager::new();
        parser.add_arg("-regtest", "regtest", ArgsManager::ALLOW_ANY, OptionsCategory::Options);
        parser.add_arg("-testnet", "testnet", ArgsManager::ALLOW_ANY, OptionsCategory::Options);

        let mut desc = String::new();
        let mut argv = vec!["ignored"];
        for &action in arg_actions {
            let Some(argstr) = arg(action) else {
                break;
            };
            argv.push(argstr);
            desc += " ";
            desc += argstr;
        }
        assert_eq!(parse(&parser, &argv), (true, String::new()), "{desc}");

        let mut conf = String::new();
        for &action in conf_actions {
            let Some(argstr) = arg(action) else {
                break;
            };
            desc += " ";
            desc += &argstr[1..];
            conf += &argstr[1..];
            conf += "\n";
        }
        assert_eq!(parser.read_config_stream(&conf, "filepath", false), Ok(()), "{desc}");

        desc += " || ";
        match parser.get_chain_name() {
            Ok(name) => desc += &name,
            Err(e) => {
                desc += "error: ";
                desc += &e.0;
            }
        }
        desc += "\n";

        out += &desc;
    };
    let mut arg_actions = [NONE; MAX_ACTIONS];
    for_each_no_dup(&mut arg_actions, ENABLE_TEST, NEGATE_REG, |arg_actions| {
        let mut conf_actions = [NONE; MAX_ACTIONS];
        for_each_no_dup(&mut conf_actions, ENABLE_TEST, NEGATE_REG, |conf_actions| {
            each(arg_actions, conf_actions)
        });
    });

    // Core dumps these results with:
    //
    //   CHAIN_MERGE_TEST_OUT=results.txt ./test_dash --run_test=argsman_tests/util_ChainMerge
    //
    // Results file is formatted like:
    //
    //   <input> || <output>
    assert_matches_core(&out, "chain_merge.txt");
    assert_eq!(
        sha256d_hex(out.as_bytes()),
        "1cd2697d1665d086f15ed18b3333bc356c3d39ba490589dc58afd8fc2f457731"
    );
}

#[test]
fn util_read_write_settings() {
    // Core: BasicTestingSetup's m_path_root, and its SelectParams(CBaseChainParams::MAIN),
    // which sets the global BaseParams() that GetDataDirNet reads. The port keeps the base
    // params per ArgsManager, so each manager selects main itself.
    let root = tempfile::tempdir().unwrap();
    let m_path_root = path_string(root.path());

    // Test writing setting.
    let args1 = ArgsManager::new();
    args1.select_base_params("main").unwrap();
    args1.force_set_arg("-datadir", &m_path_root);
    args1.lock_settings(|settings| {
        settings.rw_settings.insert("name".into(), "value".into());
    });
    args1.write_settings_file(None, false).unwrap();

    // Test reading setting.
    let args2 = ArgsManager::new();
    args2.select_base_params("main").unwrap();
    args2.force_set_arg("-datadir", &m_path_root);
    args2.read_settings_file(None).unwrap();
    args2.lock_settings(|settings| {
        assert_eq!(settings.rw_settings["name"].get_str().unwrap(), "value");
    });

    // Test error logging, and remove previously written setting.
    {
        // ASSERT_DEBUG_LOG("Failed renaming settings file");
        args2.take_buffered_log();
        let settings_json: PathBuf = args1.get_data_dir_base().unwrap().join("settings.json");
        fs::remove_file(&settings_json).unwrap();
        fs::create_dir(&settings_json).unwrap();
        args2.write_settings_file(None, false).unwrap();
        fs::remove_dir(&settings_json).unwrap();
        let log = args2.take_buffered_log().concat();
        assert!(log.contains("Failed renaming settings file"), "debug log: {log:?}");
    }
}
