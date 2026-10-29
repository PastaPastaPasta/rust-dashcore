//! Port of Dash Core v24.0.0-rc.3 `src/test/settings_tests.cpp` (suite `settings_tests`).
//!
//! Every `BOOST_AUTO_TEST_CASE` is one `#[test]` with the same cases and expectations. The
//! `Merge` results text is compared byte for byte with Core's captured output
//! (`tests/data/core-v24/settings_merge.txt`) and its `CHash256` (sha256d) hex with Core's
//! constant.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use dash_args::settings::{
    get_setting, get_settings_list, only_has_default_section_setting, read_settings,
};
use dash_args::{Settings, SettingsValue};

mod common;
use common::{assert_matches_core, for_each_no_dup, path_string, sha256d_hex};

/// Core: `WriteText`.
fn write_text(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
}

/// Core compares `SettingsValue`s (and prints them) through `write()`.
fn written(values: &BTreeMap<String, SettingsValue>) -> Vec<(String, String)> {
    values.iter().map(|(k, v)| (k.clone(), v.write())).collect()
}

#[test]
fn read_write() {
    // Core: `m_args.GetDataDirBase() / "settings.json"` (BasicTestingSetup's datadir).
    let datadir = tempfile::tempdir().unwrap();
    let path = datadir.path().join("settings.json");

    write_text(
        &path,
        r#"{
        "string": "string",
        "num": 5,
        "bool": true,
        "null": null
    }"#,
    );

    let expected: BTreeMap<String, SettingsValue> = [
        ("string".to_string(), SettingsValue::from("string")),
        ("num".to_string(), SettingsValue::from(5i64)),
        ("bool".to_string(), SettingsValue::from(true)),
        ("null".to_string(), SettingsValue::Null),
    ]
    .into_iter()
    .collect();

    // Check file read.
    let mut values = BTreeMap::new();
    let mut errors = Vec::new();
    assert!(read_settings(&path, &mut values, &mut errors).unwrap());
    assert_eq!(written(&values), written(&expected));
    assert!(errors.is_empty());

    // Check no errors if file doesn't exist.
    fs::remove_file(&path).unwrap();
    assert!(read_settings(&path, &mut values, &mut errors).unwrap());
    assert!(values.is_empty());
    assert!(errors.is_empty());

    // Check duplicate keys not allowed and that values returns empty if a duplicate is found.
    write_text(
        &path,
        r#"{
        "dupe": "string",
        "dupe": "dupe"
    }"#,
    );
    assert!(!read_settings(&path, &mut values, &mut errors).unwrap());
    let dup_keys =
        vec![format!("Found duplicate key dupe in settings file {}", path_string(&path))];
    assert_eq!(errors, dup_keys);
    assert!(values.is_empty());

    // Check non-kv json files not allowed
    write_text(&path, r#""non-kv""#);
    assert!(!read_settings(&path, &mut values, &mut errors).unwrap());
    let non_kv =
        vec![format!("Found non-object value \"non-kv\" in settings file {}", path_string(&path))];
    assert_eq!(errors, non_kv);

    // Check invalid json not allowed
    write_text(&path, r#"invalid json"#);
    assert!(!read_settings(&path, &mut values, &mut errors).unwrap());
    let fail_parse = vec![format!(
        "Settings file {} does not contain valid JSON. This is probably caused by disk corruption or a crash, \
         and can be fixed by removing the file, which will reset settings to default values.",
        path_string(&path)
    )];
    assert_eq!(errors, fail_parse);
}

/// Check settings struct contents against expected json strings.
fn check_values(settings: &Settings, single_val: &str, list_val: &str) {
    let single_value = get_setting(settings, "section", "name", false, false, false);
    let mut list_value = SettingsValue::new_array();
    for item in get_settings_list(settings, "section", "name", false) {
        list_value.push_back(item);
    }
    assert_eq!(single_value.write(), single_val);
    assert_eq!(list_value.write(), list_val);
}

// Simple settings merge test case.
#[test]
fn simple() {
    let mut settings = Settings::default();
    settings.command_line_options.entry("name".into()).or_default().push("val1".into());
    settings.command_line_options.entry("name".into()).or_default().push("val2".into());
    settings
        .ro_config
        .entry("section".into())
        .or_default()
        .entry("name".into())
        .or_default()
        .push(2i64.into());

    // The last given arg takes precedence when specified via commandline.
    check_values(&settings, r#""val2""#, r#"["val1","val2",2]"#);

    let mut settings2 = Settings::default();
    let section = settings2.ro_config.entry("section".into()).or_default();
    section.entry("name".into()).or_default().push("val2".into());
    section.entry("name".into()).or_default().push("val3".into());

    // The first given arg takes precedence when specified via config file.
    check_values(&settings2, r#""val2""#, r#"["val2","val3"]"#);
}

// Confirm that a high priority setting overrides a lower priority setting even
// if the high priority setting is null. This behavior is useful for a high
// priority setting source to be able to effectively reset any setting back to
// its default value.
#[test]
fn null_override() {
    let mut settings = Settings::default();
    settings.command_line_options.entry("name".into()).or_default().push("value".into());
    assert_eq!(
        r#""value""#,
        get_setting(&settings, "section", "name", false, false, false).write()
    );
    settings.forced_settings.insert("name".into(), SettingsValue::Null);
    assert_eq!(r#"null"#, get_setting(&settings, "section", "name", false, false, false).write());
}

// Test different ways settings can be merged, and verify results. This test can
// be used to confirm that updates to settings code don't change behavior
// unintentionally.
mod merge_setup {
    /// Max number of actions to sequence together. Can decrease this when
    /// debugging to make test results easier to understand.
    pub const MAX_ACTIONS: usize = 3;

    // enum Action { END, SET, NEGATE, SECTION_SET, SECTION_NEGATE };
    pub const END: i32 = 0;
    pub const SET: i32 = 1;
    pub const NEGATE: i32 = 2;
    pub const SECTION_SET: i32 = 3;
    pub const SECTION_NEGATE: i32 = 4;
}

/// Core: `MergeTestingSetup::ForEachMergeSetup`: enumerate all possible test configurations.
fn for_each_merge_setup(mut f: impl FnMut(&[i32], &[i32], bool, bool)) {
    use merge_setup::*;
    let mut arg_actions = [END; MAX_ACTIONS];
    // command_line_options do not have sections. Only iterate over SET and NEGATE
    for_each_no_dup(&mut arg_actions, SET, NEGATE, |arg_actions| {
        let mut conf_actions = [END; MAX_ACTIONS];
        for_each_no_dup(&mut conf_actions, SET, SECTION_NEGATE, |conf_actions| {
            for force_set in [false, true] {
                for ignore_default_section_config in [false, true] {
                    f(arg_actions, conf_actions, force_set, ignore_default_section_config);
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
fn merge() {
    use merge_setup::*;
    let mut out = String::new();

    let network = "main"; // CBaseChainParams::MAIN
    for_each_merge_setup(|arg_actions, conf_actions, force_set, ignore_default_section_config| {
        let mut desc = String::new();
        let mut value_suffix = 0;
        let mut settings = Settings::default();

        let name = if ignore_default_section_config {
            "wallet"
        } else {
            "server"
        };
        let push_values = |desc: &mut String,
                           value_suffix: &mut i32,
                           action: i32,
                           value_prefix: &str,
                           name_prefix: &str,
                           dest: &mut Vec<SettingsValue>| {
            if action == SET || action == SECTION_SET {
                for _ in 0..2 {
                    *value_suffix += 1;
                    dest.push(format!("{value_prefix}{value_suffix}").into());
                    *desc += &format!(
                        " {name_prefix}{name}={}",
                        dest.last().unwrap().get_str().unwrap()
                    );
                }
            } else if action == NEGATE || action == SECTION_NEGATE {
                dest.push(false.into());
                *desc += &format!(" {name_prefix}no{name}");
            }
        };

        if force_set {
            settings.forced_settings.insert(name.into(), "forced".into());
            desc += &format!(" {name}=forced");
        }
        for &arg_action in arg_actions {
            let dest = settings.command_line_options.entry(name.into()).or_default();
            push_values(&mut desc, &mut value_suffix, arg_action, "a", "-", dest);
        }
        for &conf_action in conf_actions {
            let use_section = conf_action == SECTION_SET || conf_action == SECTION_NEGATE;
            let name_prefix = if use_section {
                format!("{network}.")
            } else {
                String::new()
            };
            let dest = settings
                .ro_config
                .entry(
                    if use_section {
                        network
                    } else {
                        ""
                    }
                    .into(),
                )
                .or_default()
                .entry(name.into())
                .or_default();
            push_values(&mut desc, &mut value_suffix, conf_action, "c", &name_prefix, dest);
        }

        desc += " || ";
        desc += &get_setting(
            &settings,
            network,
            name,
            ignore_default_section_config,
            /*ignore_nonpersistent=*/ false,
            /*get_chain_name=*/ false,
        )
        .write();
        desc += " |";
        for s in get_settings_list(&settings, network, name, ignore_default_section_config) {
            desc += " ";
            desc += &s.write();
        }
        desc += " |";
        if only_has_default_section_setting(&settings, network, name) {
            desc += " ignored";
        }
        desc += "\n";

        out += &desc;
    });

    // Core dumps these results with:
    //
    //   SETTINGS_MERGE_TEST_OUT=results.txt ./test_bitcoin --run_test=settings_tests/Merge
    //
    // Results file is formatted like:
    //
    //   <input> || GetSetting() | GetSettingsList() | OnlyHasDefaultSectionSetting()
    assert_matches_core(&out, "settings_merge.txt");
    assert_eq!(
        sha256d_hex(out.as_bytes()),
        "79db02d74e3e193196541b67c068b40ebd0c124a24b3ecbe9cbf7e85b1c4ba7a"
    );
}
