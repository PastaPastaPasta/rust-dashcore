//! Port of Dash Core v24.0.0-rc.3 `src/test/getarg_tests.cpp` (suite `getarg_tests`).
//!
//! Every `BOOST_AUTO_TEST_CASE` is one `#[test]` with the same cases and expectations.
//! `BOOST_CHECK_THROW(..., std::runtime_error)` becomes "returns `Err`".

use std::path::{Path, PathBuf};

use dash_args::{ArgsManager, SettingsValue};

mod common;
use common::{get_arg, get_bool_arg, get_int_arg, setup_args};

/// Core: `ResetArgs`: split on single spaces, insert the dummy program name, parse.
fn reset_args(local_args: &ArgsManager, str_arg: &str) {
    let mut vec_arg: Vec<&str> = vec!["testdash"];
    if !str_arg.is_empty() {
        vec_arg.extend(str_arg.split(' '));
    }
    assert!(
        local_args.parse_parameters(&vec_arg).is_ok(),
        "ParseParameters failed for {str_arg:?}"
    );
}

/// `fs::path` equality as Core's `BOOST_CHECK_EQUAL` sees it: element-wise, so a trailing
/// separator or a `.` element matters (unlike `PathBuf`'s `==`). On POSIX that is string
/// equality; on Windows both separators are equivalent.
fn assert_path_eq(actual: PathBuf, expected: &str) {
    let actual = actual.to_str().expect("utf-8 path").to_string();
    #[cfg(windows)]
    let (actual, expected) = (actual.replace('/', "\\"), expected.replace('/', "\\"));
    assert_eq!(actual, expected);
}

fn get_path_arg(args: &ArgsManager, name: &str) -> PathBuf {
    args.get_path_arg(name, Path::new("")).unwrap()
}

// Test behavior of GetArg functions when string, integer, and boolean types
// are specified in the settings.json file. GetArg functions are convenience
// functions. The GetSetting method can always be used instead of GetArg
// methods to retrieve original values, and there's not always an objective
// answer to what GetArg behavior is best in every case. This test makes sure
// there's test coverage for whatever the current behavior is, so it's not
// broken or changed unintentionally.
#[test]
fn setting_args() {
    let args = ArgsManager::new();
    setup_args(&args, &[("-foo", ArgsManager::ALLOW_ANY)]);

    let set_foo = |value: SettingsValue| {
        args.lock_settings(|settings| {
            settings.rw_settings.insert("foo".to_string(), value);
        });
    };

    set_foo("str".into());
    assert_eq!(args.get_setting("foo").write(), "\"str\"");
    assert_eq!(get_arg(&args, "foo", "default"), "str");
    assert_eq!(get_int_arg(&args, "foo", 100), 0);
    assert!(!get_bool_arg(&args, "foo", true));
    assert!(!get_bool_arg(&args, "foo", false));

    set_foo("99".into());
    assert_eq!(args.get_setting("foo").write(), "\"99\"");
    assert_eq!(get_arg(&args, "foo", "default"), "99");
    assert_eq!(get_int_arg(&args, "foo", 100), 99);
    assert!(get_bool_arg(&args, "foo", true));
    assert!(get_bool_arg(&args, "foo", false));

    set_foo("3.25".into());
    assert_eq!(args.get_setting("foo").write(), "\"3.25\"");
    assert_eq!(get_arg(&args, "foo", "default"), "3.25");
    assert_eq!(get_int_arg(&args, "foo", 100), 3);
    assert!(get_bool_arg(&args, "foo", true));
    assert!(get_bool_arg(&args, "foo", false));

    set_foo("0".into());
    assert_eq!(args.get_setting("foo").write(), "\"0\"");
    assert_eq!(get_arg(&args, "foo", "default"), "0");
    assert_eq!(get_int_arg(&args, "foo", 100), 0);
    assert!(!get_bool_arg(&args, "foo", true));
    assert!(!get_bool_arg(&args, "foo", false));

    set_foo("".into());
    assert_eq!(args.get_setting("foo").write(), "\"\"");
    assert_eq!(get_arg(&args, "foo", "default"), "");
    assert_eq!(get_int_arg(&args, "foo", 100), 0);
    assert!(get_bool_arg(&args, "foo", true));
    assert!(get_bool_arg(&args, "foo", false));

    set_foo(99i64.into());
    assert_eq!(args.get_setting("foo").write(), "99");
    assert_eq!(get_arg(&args, "foo", "default"), "99");
    assert_eq!(get_int_arg(&args, "foo", 100), 99);
    assert!(args.get_bool_arg("foo", true).is_err());
    assert!(args.get_bool_arg("foo", false).is_err());

    set_foo(SettingsValue::from_f64(3.25).unwrap());
    assert_eq!(args.get_setting("foo").write(), "3.25");
    assert_eq!(get_arg(&args, "foo", "default"), "3.25");
    assert!(args.get_int_arg("foo", 100).is_err());
    assert!(args.get_bool_arg("foo", true).is_err());
    assert!(args.get_bool_arg("foo", false).is_err());

    set_foo(0i64.into());
    assert_eq!(args.get_setting("foo").write(), "0");
    assert_eq!(get_arg(&args, "foo", "default"), "0");
    assert_eq!(get_int_arg(&args, "foo", 100), 0);
    assert!(args.get_bool_arg("foo", true).is_err());
    assert!(args.get_bool_arg("foo", false).is_err());

    set_foo(true.into());
    assert_eq!(args.get_setting("foo").write(), "true");
    assert_eq!(get_arg(&args, "foo", "default"), "1");
    assert_eq!(get_int_arg(&args, "foo", 100), 1);
    assert!(get_bool_arg(&args, "foo", true));
    assert!(get_bool_arg(&args, "foo", false));

    set_foo(false.into());
    assert_eq!(args.get_setting("foo").write(), "false");
    assert_eq!(get_arg(&args, "foo", "default"), "0");
    assert_eq!(get_int_arg(&args, "foo", 100), 0);
    assert!(!get_bool_arg(&args, "foo", true));
    assert!(!get_bool_arg(&args, "foo", false));

    set_foo(SettingsValue::new_object());
    assert_eq!(args.get_setting("foo").write(), "{}");
    assert!(args.get_arg("foo", "default").is_err());
    assert!(args.get_int_arg("foo", 100).is_err());
    assert!(args.get_bool_arg("foo", true).is_err());
    assert!(args.get_bool_arg("foo", false).is_err());

    set_foo(SettingsValue::new_array());
    assert_eq!(args.get_setting("foo").write(), "[]");
    assert!(args.get_arg("foo", "default").is_err());
    assert!(args.get_int_arg("foo", 100).is_err());
    assert!(args.get_bool_arg("foo", true).is_err());
    assert!(args.get_bool_arg("foo", false).is_err());

    set_foo(SettingsValue::Null);
    assert_eq!(args.get_setting("foo").write(), "null");
    assert_eq!(get_arg(&args, "foo", "default"), "default");
    assert_eq!(get_int_arg(&args, "foo", 100), 100);
    assert!(get_bool_arg(&args, "foo", true));
    assert!(!get_bool_arg(&args, "foo", false));
}

#[test]
fn boolarg() {
    let local_args = ArgsManager::new();

    let foo = ("-foo", ArgsManager::ALLOW_ANY);
    setup_args(&local_args, &[foo]);
    reset_args(&local_args, "-foo");
    assert!(get_bool_arg(&local_args, "-foo", false));
    assert!(get_bool_arg(&local_args, "-foo", true));

    assert!(!get_bool_arg(&local_args, "-fo", false));
    assert!(get_bool_arg(&local_args, "-fo", true));

    assert!(!get_bool_arg(&local_args, "-fooo", false));
    assert!(get_bool_arg(&local_args, "-fooo", true));

    reset_args(&local_args, "-foo=0");
    assert!(!get_bool_arg(&local_args, "-foo", false));
    assert!(!get_bool_arg(&local_args, "-foo", true));

    reset_args(&local_args, "-foo=1");
    assert!(get_bool_arg(&local_args, "-foo", false));
    assert!(get_bool_arg(&local_args, "-foo", true));

    // New 0.6 feature: auto-map -nosomething to !-something:
    reset_args(&local_args, "-nofoo");
    assert!(!get_bool_arg(&local_args, "-foo", false));
    assert!(!get_bool_arg(&local_args, "-foo", true));

    reset_args(&local_args, "-nofoo=1");
    assert!(!get_bool_arg(&local_args, "-foo", false));
    assert!(!get_bool_arg(&local_args, "-foo", true));

    reset_args(&local_args, "-foo -nofoo"); // -nofoo should win
    assert!(!get_bool_arg(&local_args, "-foo", false));
    assert!(!get_bool_arg(&local_args, "-foo", true));

    reset_args(&local_args, "-foo=1 -nofoo=1"); // -nofoo should win
    assert!(!get_bool_arg(&local_args, "-foo", false));
    assert!(!get_bool_arg(&local_args, "-foo", true));

    reset_args(&local_args, "-foo=0 -nofoo=0"); // -nofoo=0 should win
    assert!(get_bool_arg(&local_args, "-foo", false));
    assert!(get_bool_arg(&local_args, "-foo", true));

    // New 0.6 feature: treat -- same as -:
    reset_args(&local_args, "--foo=1");
    assert!(get_bool_arg(&local_args, "-foo", false));
    assert!(get_bool_arg(&local_args, "-foo", true));

    reset_args(&local_args, "--nofoo=1");
    assert!(!get_bool_arg(&local_args, "-foo", false));
    assert!(!get_bool_arg(&local_args, "-foo", true));
}

#[test]
fn stringarg() {
    let local_args = ArgsManager::new();

    let foo = ("-foo", ArgsManager::ALLOW_ANY);
    let bar = ("-bar", ArgsManager::ALLOW_ANY);
    setup_args(&local_args, &[foo, bar]);
    reset_args(&local_args, "");
    assert_eq!(get_arg(&local_args, "-foo", ""), "");
    assert_eq!(get_arg(&local_args, "-foo", "eleven"), "eleven");

    reset_args(&local_args, "-foo -bar");
    assert_eq!(get_arg(&local_args, "-foo", ""), "");
    assert_eq!(get_arg(&local_args, "-foo", "eleven"), "");

    reset_args(&local_args, "-foo=");
    assert_eq!(get_arg(&local_args, "-foo", ""), "");
    assert_eq!(get_arg(&local_args, "-foo", "eleven"), "");

    reset_args(&local_args, "-foo=11");
    assert_eq!(get_arg(&local_args, "-foo", ""), "11");
    assert_eq!(get_arg(&local_args, "-foo", "eleven"), "11");

    reset_args(&local_args, "-foo=eleven");
    assert_eq!(get_arg(&local_args, "-foo", ""), "eleven");
    assert_eq!(get_arg(&local_args, "-foo", "eleven"), "eleven");
}

#[test]
fn intarg() {
    let local_args = ArgsManager::new();

    let foo = ("-foo", ArgsManager::ALLOW_ANY);
    let bar = ("-bar", ArgsManager::ALLOW_ANY);
    setup_args(&local_args, &[foo, bar]);
    reset_args(&local_args, "");
    assert_eq!(get_int_arg(&local_args, "-foo", 11), 11);
    assert_eq!(get_int_arg(&local_args, "-foo", 0), 0);

    reset_args(&local_args, "-foo -bar");
    assert_eq!(get_int_arg(&local_args, "-foo", 11), 0);
    assert_eq!(get_int_arg(&local_args, "-bar", 11), 0);

    // Check under-/overflow behavior.
    reset_args(&local_args, "-foo=-9223372036854775809 -bar=9223372036854775808");
    assert_eq!(get_int_arg(&local_args, "-foo", 0), i64::MIN);
    assert_eq!(get_int_arg(&local_args, "-bar", 0), i64::MAX);

    reset_args(&local_args, "-foo=11 -bar=12");
    assert_eq!(get_int_arg(&local_args, "-foo", 0), 11);
    assert_eq!(get_int_arg(&local_args, "-bar", 11), 12);

    reset_args(&local_args, "-foo=NaN -bar=NotANumber");
    assert_eq!(get_int_arg(&local_args, "-foo", 1), 0);
    assert_eq!(get_int_arg(&local_args, "-bar", 11), 0);
}

#[test]
fn patharg() {
    let local_args = ArgsManager::new();

    let dir = ("-dir", ArgsManager::ALLOW_ANY);
    setup_args(&local_args, &[dir]);
    reset_args(&local_args, "");
    assert_path_eq(get_path_arg(&local_args, "-dir"), "");

    let root_path = "/";
    reset_args(&local_args, "-dir=/");
    assert_path_eq(get_path_arg(&local_args, "-dir"), root_path);

    reset_args(&local_args, "-dir=/.");
    assert_path_eq(get_path_arg(&local_args, "-dir"), root_path);

    reset_args(&local_args, "-dir=/./");
    assert_path_eq(get_path_arg(&local_args, "-dir"), root_path);

    reset_args(&local_args, "-dir=/.//");
    assert_path_eq(get_path_arg(&local_args, "-dir"), root_path);

    #[cfg(windows)]
    {
        let win_root_path = "C:\\";
        reset_args(&local_args, "-dir=C:\\");
        assert_path_eq(get_path_arg(&local_args, "-dir"), win_root_path);

        reset_args(&local_args, "-dir=C:/");
        assert_path_eq(get_path_arg(&local_args, "-dir"), win_root_path);

        reset_args(&local_args, "-dir=C:\\\\");
        assert_path_eq(get_path_arg(&local_args, "-dir"), win_root_path);

        reset_args(&local_args, "-dir=C:\\.");
        assert_path_eq(get_path_arg(&local_args, "-dir"), win_root_path);

        reset_args(&local_args, "-dir=C:\\.\\");
        assert_path_eq(get_path_arg(&local_args, "-dir"), win_root_path);

        reset_args(&local_args, "-dir=C:\\.\\\\");
        assert_path_eq(get_path_arg(&local_args, "-dir"), win_root_path);
    }

    let absolute_path = "/home/user/.bitcoin";
    reset_args(&local_args, "-dir=/home/user/.bitcoin");
    assert_path_eq(get_path_arg(&local_args, "-dir"), absolute_path);

    reset_args(&local_args, "-dir=/root/../home/user/.bitcoin");
    assert_path_eq(get_path_arg(&local_args, "-dir"), absolute_path);

    reset_args(&local_args, "-dir=/home/./user/.bitcoin");
    assert_path_eq(get_path_arg(&local_args, "-dir"), absolute_path);

    reset_args(&local_args, "-dir=/home/user/.bitcoin/");
    assert_path_eq(get_path_arg(&local_args, "-dir"), absolute_path);

    reset_args(&local_args, "-dir=/home/user/.bitcoin//");
    assert_path_eq(get_path_arg(&local_args, "-dir"), absolute_path);

    reset_args(&local_args, "-dir=/home/user/.bitcoin/.");
    assert_path_eq(get_path_arg(&local_args, "-dir"), absolute_path);

    reset_args(&local_args, "-dir=/home/user/.bitcoin/./");
    assert_path_eq(get_path_arg(&local_args, "-dir"), absolute_path);

    reset_args(&local_args, "-dir=/home/user/.bitcoin/.//");
    assert_path_eq(get_path_arg(&local_args, "-dir"), absolute_path);

    let relative_path = "user/.bitcoin";
    reset_args(&local_args, "-dir=user/.bitcoin");
    assert_path_eq(get_path_arg(&local_args, "-dir"), relative_path);

    reset_args(&local_args, "-dir=somewhere/../user/.bitcoin");
    assert_path_eq(get_path_arg(&local_args, "-dir"), relative_path);

    reset_args(&local_args, "-dir=user/./.bitcoin");
    assert_path_eq(get_path_arg(&local_args, "-dir"), relative_path);

    reset_args(&local_args, "-dir=user/.bitcoin/");
    assert_path_eq(get_path_arg(&local_args, "-dir"), relative_path);

    reset_args(&local_args, "-dir=user/.bitcoin//");
    assert_path_eq(get_path_arg(&local_args, "-dir"), relative_path);

    reset_args(&local_args, "-dir=user/.bitcoin/.");
    assert_path_eq(get_path_arg(&local_args, "-dir"), relative_path);

    reset_args(&local_args, "-dir=user/.bitcoin/./");
    assert_path_eq(get_path_arg(&local_args, "-dir"), relative_path);

    reset_args(&local_args, "-dir=user/.bitcoin/.//");
    assert_path_eq(get_path_arg(&local_args, "-dir"), relative_path);

    // Check negated and default argument handling. Specifying an empty argument
    // is the same as not specifying the argument. This is convenient for
    // scripting so later command line arguments can override earlier command
    // line arguments or bitcoin.conf values. Currently the -dir= case cannot be
    // distinguished from -dir case with no assignment, but #16545 would add the
    // ability to distinguish these in the future (and treat the no-assign case
    // like an imperative command or an error).
    let default = Path::new("default");
    reset_args(&local_args, "");
    assert_path_eq(local_args.get_path_arg("-dir", default).unwrap(), "default");
    reset_args(&local_args, "-dir=override");
    assert_path_eq(local_args.get_path_arg("-dir", default).unwrap(), "override");
    reset_args(&local_args, "-dir=");
    assert_path_eq(local_args.get_path_arg("-dir", default).unwrap(), "default");
    reset_args(&local_args, "-dir");
    assert_path_eq(local_args.get_path_arg("-dir", default).unwrap(), "default");
    reset_args(&local_args, "-nodir");
    assert_path_eq(local_args.get_path_arg("-dir", default).unwrap(), "");
}

#[test]
fn doubledash() {
    let local_args = ArgsManager::new();

    let foo = ("-foo", ArgsManager::ALLOW_ANY);
    let bar = ("-bar", ArgsManager::ALLOW_ANY);
    setup_args(&local_args, &[foo, bar]);
    reset_args(&local_args, "--foo");
    assert!(get_bool_arg(&local_args, "-foo", false));

    reset_args(&local_args, "--foo=verbose --bar=1");
    assert_eq!(get_arg(&local_args, "-foo", ""), "verbose");
    assert_eq!(get_int_arg(&local_args, "-bar", 0), 1);
}

#[test]
fn boolargno() {
    let local_args = ArgsManager::new();

    let foo = ("-foo", ArgsManager::ALLOW_ANY);
    let bar = ("-bar", ArgsManager::ALLOW_ANY);
    setup_args(&local_args, &[foo, bar]);
    reset_args(&local_args, "-nofoo");
    assert!(!get_bool_arg(&local_args, "-foo", true));
    assert!(!get_bool_arg(&local_args, "-foo", false));

    reset_args(&local_args, "-nofoo=1");
    assert!(!get_bool_arg(&local_args, "-foo", true));
    assert!(!get_bool_arg(&local_args, "-foo", false));

    reset_args(&local_args, "-nofoo=0");
    assert!(get_bool_arg(&local_args, "-foo", true));
    assert!(get_bool_arg(&local_args, "-foo", false));

    reset_args(&local_args, "-foo --nofoo"); // --nofoo should win
    assert!(!get_bool_arg(&local_args, "-foo", true));
    assert!(!get_bool_arg(&local_args, "-foo", false));

    reset_args(&local_args, "-nofoo -foo"); // foo always wins:
    assert!(get_bool_arg(&local_args, "-foo", true));
    assert!(get_bool_arg(&local_args, "-foo", false));
}

#[test]
fn logargs() {
    let local_args = ArgsManager::new();

    let okaylog_bool = ("-okaylog-bool", ArgsManager::ALLOW_ANY);
    let okaylog_negbool = ("-okaylog-negbool", ArgsManager::ALLOW_ANY);
    let okaylog = ("-okaylog", ArgsManager::ALLOW_ANY);
    let dontlog = ("-dontlog", ArgsManager::ALLOW_ANY | ArgsManager::SENSITIVE);
    setup_args(&local_args, &[okaylog_bool, okaylog_negbool, okaylog, dontlog]);
    reset_args(&local_args, "-okaylog-bool -nookaylog-negbool -okaylog=public -dontlog=private42");

    // Everything logged to debug.log will also append to str (here: the log lines buffered
    // while no sink is installed).
    local_args.take_buffered_log();

    // Log the arguments
    local_args.log_args();

    let str: String = local_args.take_buffered_log().concat();
    // Check that what should appear does, and what shouldn't doesn't.
    assert!(str.contains("Command-line arg: okaylog-bool=\"\""), "{str}");
    assert!(str.contains("Command-line arg: okaylog-negbool=false"), "{str}");
    assert!(str.contains("Command-line arg: okaylog=\"public\""), "{str}");
    assert!(str.contains("dontlog=****"), "{str}");
    assert!(!str.contains("private42"), "{str}");
}
