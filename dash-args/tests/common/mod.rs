//! Helpers shared by the test binaries (each uses a subset).
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use bitcoin_hashes::{Hash, sha256d};
use dash_args::{ArgsManager, OptionsCategory, SettingsValue};

/// A file captured from Dash Core v24.0.0-rc.4 (gen/PROVENANCE.md).
pub fn core_data_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/core-v24").join(name)
}

pub fn core_data(name: &str) -> String {
    let path = core_data_path(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Compare a merge test's results text with Core's captured file, reporting the first
/// differing line.
pub fn assert_matches_core(out: &str, file: &str) {
    let core = core_data(file);
    if out != core {
        let (i, (a, b)) = out
            .lines()
            .zip(core.lines())
            .enumerate()
            .find(|(_, (a, b))| a != b)
            .unwrap_or((out.lines().count().min(core.lines().count()), ("<end>", "<end>")));
        panic!(
            "results differ from Core's {file} at line {} ({} vs {} bytes):\n  rust: {a}\n  core: {b}",
            i + 1,
            out.len(),
            core.len()
        );
    }
}

pub fn sha256d_hex(data: &[u8]) -> String {
    sha256d::Hash::hash(data).to_byte_array().iter().map(|b| format!("{b:02x}")).collect()
}

pub fn path_string(path: &Path) -> String {
    path.to_str().expect("utf-8 path").to_string()
}

/// Core: `SetupArgs` / `TestArgsManager::SetupArgs`.
pub fn setup_args(args: &ArgsManager, list: &[(&str, u32)]) {
    for (name, flags) in list {
        args.add_arg(name, "", *flags, OptionsCategory::Options);
    }
}

pub fn get_arg(args: &ArgsManager, name: &str, default: &str) -> String {
    args.get_arg(name, default).unwrap()
}

pub fn get_int_arg(args: &ArgsManager, name: &str, default: i64) -> i64 {
    args.get_int_arg(name, default).unwrap()
}

pub fn get_bool_arg(args: &ArgsManager, name: &str, default: bool) -> bool {
    args.get_bool_arg(name, default).unwrap()
}

/// Core: `NextString` (test/util/str.h). Increment a string. Useful to enumerate all fixed
/// length strings with characters in [min_char, max_char].
pub fn next_string(string: &mut [i32], min_char: i32, max_char: i32) -> bool {
    for elem in string.iter_mut() {
        let has_next = *elem != max_char;
        *elem = if *elem < min_char || *elem >= max_char {
            min_char
        } else {
            *elem + 1
        };
        if has_next {
            return true;
        }
    }
    false
}

/// Core: `ForEachNoDup` (test/util/str.h). Iterate over string values and call function for
/// each string without successive duplicate characters.
pub fn for_each_no_dup(
    string: &mut [i32],
    min_char: i32,
    max_char: i32,
    mut f: impl FnMut(&[i32]),
) {
    let mut has_next = true;
    while has_next {
        let mut prev = -1;
        let mut skip_string = false;
        for &c in string.iter() {
            if c == prev {
                skip_string = true;
            }
            if skip_string || c < min_char || c > max_char {
                break;
            }
            prev = c;
        }
        if !skip_string {
            f(string);
        }
        has_next = next_string(string, min_char, max_char);
    }
}

/// The captured init cases of a fixture file.
pub fn cases_from(file: &str) -> Vec<SettingsValue> {
    let raw = std::fs::read(core_data_path(file)).unwrap();
    let fixture = SettingsValue::read(&raw).expect("valid JSON");
    field(&fixture, "cases").get_values().unwrap().into_iter().cloned().collect()
}

/// The captured init case `name`, from the Windows capture on Windows.
pub fn init_case(name: &str) -> SettingsValue {
    cases_from(if cfg!(windows) {
        "init_cases_win64.json"
    } else {
        "init_cases.json"
    })
    .into_iter()
    .find(|case| field(case, "name").get_str().unwrap() == name)
    .unwrap_or_else(|| panic!("no init case {name}"))
}

pub fn field<'a>(value: &'a SettingsValue, key: &str) -> &'a SettingsValue {
    &entries(value).iter().find(|(k, _)| k == key).unwrap_or_else(|| panic!("no {key}")).1
}

pub fn entries(value: &SettingsValue) -> &[(String, SettingsValue)] {
    match value {
        SettingsValue::Obj(entries) => entries,
        _ => panic!("not an object"),
    }
}

pub fn strings(value: &SettingsValue) -> Vec<String> {
    value.get_values().unwrap().iter().map(|v| v.get_str().unwrap().to_string()).collect()
}
