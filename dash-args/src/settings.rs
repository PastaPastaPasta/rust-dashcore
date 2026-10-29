//! Settings sources and their merge rules, and `settings.json` I/O.
//!
//! Core: src/util/settings.{h,cpp} at v24.0.0-rc.3. The merge functions keep Core's
//! backwards-compatibility quirks (first config-file value wins, negations in the default
//! section reach network-only options, "zombie" config values after a re-enabled command-line
//! option); the tests port `settings_tests.cpp`.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use crate::paths::{self, path_to_string};
use crate::univalue::{RuntimeError, SettingsValue};

/// Core: `PACKAGE_NAME`, written into `settings.json`'s `_warning_` entry.
pub const PACKAGE_NAME: &str = "Dash Core";

/// Every settings source (Core: `util::Settings`). The maps are ordered by key, as Core's
/// `std::map`s are.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    /// Setting name to forced value (`ForceSetArg`, `SoftSetArg`).
    pub forced_settings: BTreeMap<String, SettingsValue>,
    /// Setting name to its command-line values, in order.
    pub command_line_options: BTreeMap<String, Vec<SettingsValue>>,
    /// Setting name to its `settings.json` value.
    pub rw_settings: BTreeMap<String, SettingsValue>,
    /// Config section (`""` for the default section) to setting name to values, in file order.
    pub ro_config: BTreeMap<String, BTreeMap<String, Vec<SettingsValue>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Forced,
    CommandLine,
    RwSettings,
    ConfigFileNetworkSection,
    ConfigFileDefaultSection,
}

/// Core: `SettingsSpan`: a run of values for one setting from one source, where a `false`
/// value is a negation that cancels the values before it.
#[derive(Clone, Copy, Debug)]
pub struct SettingsSpan<'a> {
    pub data: &'a [SettingsValue],
}

impl<'a> SettingsSpan<'a> {
    pub fn new(data: &'a [SettingsValue]) -> Self {
        SettingsSpan {
            data,
        }
    }

    /// The values after the last negation (Core: `[begin(), end())`).
    pub fn values(&self) -> &'a [SettingsValue] {
        &self.data[self.negated()..]
    }

    /// True if there are no non-negated values.
    pub fn empty(&self) -> bool {
        self.data.is_empty() || self.last_negated()
    }

    /// True if the last value is a negation.
    pub fn last_negated(&self) -> bool {
        self.data.last().is_some_and(SettingsValue::is_false)
    }

    /// The number of values up to and including the last negation.
    pub fn negated(&self) -> usize {
        self.data.iter().rposition(SettingsValue::is_false).map_or(0, |i| i + 1)
    }
}

/// Core: `MergeSettings`: visits the sources in precedence order, forced settings first and
/// the default config section last.
fn merge_settings<'a>(
    settings: &'a Settings,
    section: &str,
    name: &str,
    mut f: impl FnMut(SettingsSpan<'a>, Source),
) {
    if let Some(value) = settings.forced_settings.get(name) {
        f(SettingsSpan::new(std::slice::from_ref(value)), Source::Forced);
    }
    if let Some(values) = settings.command_line_options.get(name) {
        f(SettingsSpan::new(values), Source::CommandLine);
    }
    if let Some(value) = settings.rw_settings.get(name) {
        f(SettingsSpan::new(std::slice::from_ref(value)), Source::RwSettings);
    }
    if !section.is_empty()
        && let Some(values) = settings.ro_config.get(section).and_then(|map| map.get(name))
    {
        f(SettingsSpan::new(values), Source::ConfigFileNetworkSection);
    }
    if let Some(values) = settings.ro_config.get("").and_then(|map| map.get(name)) {
        f(SettingsSpan::new(values), Source::ConfigFileDefaultSection);
    }
}

/// Core: `util::GetSetting`: the effective value of a setting.
pub fn get_setting(
    settings: &Settings,
    section: &str,
    name: &str,
    ignore_default_section_config: bool,
    ignore_nonpersistent: bool,
    get_chain_name: bool,
) -> SettingsValue {
    let mut result = SettingsValue::Null;
    let mut done = false;
    merge_settings(settings, section, name, |span, source| {
        // Weird behaviour preserved for backwards compatibility: apply a negated setting even
        // if a non-negated one would be ignored. A negated value in the default section is
        // applied to network-specific options.
        let never_ignore_negated_setting = span.last_negated();

        // Weird behaviour preserved for backwards compatibility: in the config file the
        // first assigned value wins, except for chain name settings.
        let is_config =
            matches!(source, Source::ConfigFileNetworkSection | Source::ConfigFileDefaultSection);
        let reverse_precedence = is_config && !get_chain_name;

        // Weird behaviour preserved for backwards compatibility: negated -regtest and -testnet
        // command-line arguments are accepted but silently ignored.
        let skip_negated_command_line = get_chain_name;

        if done {
            return;
        }
        if ignore_default_section_config
            && source == Source::ConfigFileDefaultSection
            && !never_ignore_negated_setting
        {
            return;
        }
        if ignore_nonpersistent && matches!(source, Source::CommandLine | Source::Forced) {
            return;
        }
        if skip_negated_command_line && span.last_negated() {
            return;
        }
        if !span.empty() {
            let values = span.values();
            result = if reverse_precedence {
                values[0].clone()
            } else {
                values[values.len() - 1].clone()
            };
            done = true;
        } else if span.last_negated() {
            result = SettingsValue::Bool(false);
            done = true;
        }
    });
    result
}

/// Core: `util::GetSettingsList`: every effective value of a multi-value setting.
pub fn get_settings_list(
    settings: &Settings,
    section: &str,
    name: &str,
    ignore_default_section_config: bool,
) -> Vec<SettingsValue> {
    let mut result = Vec::new();
    let mut done = false;
    let mut prev_negated_empty = false;
    merge_settings(settings, section, name, |span, source| {
        // Weird behaviour preserved for backwards compatibility: config file settings come
        // back from the dead when a command-line negation is followed by a non-negated value
        // (earlier command-line values stay ignored).
        let is_config =
            matches!(source, Source::ConfigFileNetworkSection | Source::ConfigFileDefaultSection);
        let add_zombie_config_values = is_config && !prev_negated_empty;

        if ignore_default_section_config && source == Source::ConfigFileDefaultSection {
            return;
        }
        if !done || add_zombie_config_values {
            for value in span.values() {
                match value {
                    SettingsValue::Arr(items) => result.extend(items.iter().cloned()),
                    other => result.push(other.clone()),
                }
            }
        }
        // A negation, or a forced setting, ignores every lower-priority source.
        done |= span.negated() > 0 || source == Source::Forced;
        prev_negated_empty |= span.last_negated() && result.is_empty();
    });
    result
}

/// Core: `util::OnlyHasDefaultSectionSetting`: true if the setting is only set in the default
/// config section (and so is ignored for a network-only option outside mainnet).
pub fn only_has_default_section_setting(settings: &Settings, section: &str, name: &str) -> bool {
    let mut has_default_section_setting = false;
    let mut has_other_setting = false;
    merge_settings(settings, section, name, |span, source| {
        if span.empty() {
            return;
        }
        if source == Source::ConfigFileDefaultSection {
            has_default_section_setting = true;
        } else {
            has_other_setting = true;
        }
    });
    has_default_section_setting && !has_other_setting
}

/// Core: `util::ReadSettings`. On failure `values` is cleared or partial, as in Core, and the
/// errors are Core's messages. `Err` is Core's `filesystem_error` from `exists` or `remove`.
pub fn read_settings(
    path: &Path,
    values: &mut BTreeMap<String, SettingsValue>,
    errors: &mut Vec<String>,
) -> Result<bool, RuntimeError> {
    values.clear();
    errors.clear();

    // Ok for the file to not exist.
    if !paths::exists(path)? {
        return Ok(true);
    }
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(_) => {
            errors.push(format!("{}. Please check permissions.", path_to_string(path)));
            return Ok(false);
        }
    };
    let mut contents = Vec::new();
    // Core reads with `std::istreambuf_iterator`, which stops at the first read error.
    let read_ok = file.read_to_end(&mut contents).is_ok();
    drop(file);
    // Core's `std::ifstream` reads in text mode.
    #[cfg(windows)]
    let contents = paths::text_mode_read(&contents);

    // An empty settings file is deleted; it is created with defaults later. Core's `peek()`
    // also reports end-of-file when the first read fails, so an empty directory is removed
    // and a non-empty one makes `remove` throw.
    if contents.is_empty() {
        if !paths::remove(path)? {
            errors.push(format!("Unable to delete empty settings file {}", path_to_string(path)));
            return Ok(false);
        }
        return Ok(true);
    }

    let Some(parsed) = SettingsValue::read(&contents) else {
        errors.push(format!(
            "Settings file {} does not contain valid JSON. This is probably caused by disk corruption or a crash, \
             and can be fixed by removing the file, which will reset settings to default values.",
            path_to_string(path)
        ));
        return Ok(false);
    };
    if !read_ok {
        errors.push(format!("Failed reading settings file {}", path_to_string(path)));
        return Ok(false);
    }
    let SettingsValue::Obj(entries) = parsed else {
        errors.push(format!(
            "Found non-object value {} in settings file {}",
            parsed.write(),
            path_to_string(path)
        ));
        return Ok(false);
    };
    for (key, value) in entries {
        if values.contains_key(&key) {
            errors.push(format!(
                "Found duplicate key {} in settings file {}",
                key,
                path_to_string(path)
            ));
            values.clear();
            break;
        }
        values.insert(key, value);
    }
    Ok(errors.is_empty())
}

/// Core: `util::WriteSettings`: the values as a pretty-printed object (4-space indent) with
/// Core's `_warning_` entry first unless the values carry one.
pub fn write_settings(
    path: &Path,
    values: &BTreeMap<String, SettingsValue>,
    errors: &mut Vec<String>,
) -> bool {
    let mut out = SettingsValue::new_object();
    if !values.contains_key("_warning_") {
        out.push_kv_end(
            "_warning_",
            SettingsValue::Str(format!(
                "This file is automatically generated and updated by {PACKAGE_NAME}. Please do not edit this file while the node \
                 is running, as any changes might be ignored or overwritten."
            )),
        );
    }
    for (key, value) in values {
        out.push_kv_end(key.clone(), value.clone());
    }
    let mut file = match fs::File::create(path) {
        Ok(file) => file,
        Err(_) => {
            errors.push(format!(
                "Error: Unable to open settings file {} for writing",
                path_to_string(path)
            ));
            return false;
        }
    };
    // Core ignores stream errors after a successful open (`file << ... << std::endl`). Its
    // `std::ofstream` writes in text mode.
    let text = format!("{}\n", out.write_pretty(4, 1));
    #[cfg(windows)]
    let text = paths::text_mode_write(&text);
    let _ = file.write_all(text.as_bytes());
    true
}

/// Core: `RenameOver`: `rename()` on POSIX, `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` on Windows,
/// which is what `std::fs::rename` does on each.
pub(crate) fn rename_over(src: &Path, dest: &Path) -> bool {
    fs::rename(src, dest).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_counts_negations() {
        let values =
            [SettingsValue::from("a"), SettingsValue::Bool(false), SettingsValue::from("b")];
        let span = SettingsSpan::new(&values);
        assert_eq!(span.negated(), 2);
        assert!(!span.empty());
        assert!(!span.last_negated());
        assert_eq!(span.values(), &values[2..]);
    }
}
