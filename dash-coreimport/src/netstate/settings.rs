//! `settings.json`: the read-write settings file (`util/settings.cpp` `ReadSettings`).
//!
//! Values are UniValue values ([`Value`]): numbers stay as written, nested objects keep their
//! duplicate members and the first one wins on lookup. Only the top-level keys must be unique.

use super::error::FormatResult;
use super::json::{JsonStr, MAX_SETTINGS_FILE_SIZE, Value, parse_settings_object};

const WHAT: &str = "settings.json";

/// A parsed `settings.json`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Settings {
    /// The top-level entries in file order, keys unique. Core's `_warning_` comment is among
    /// them.
    pub values: Vec<(JsonStr, Value)>,
    /// The file had no bytes at all. `ReadSettings` deletes such a file and starts with no
    /// settings; this reader leaves the file alone.
    pub empty_file: bool,
}

impl Settings {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.iter().find(|(k, _)| *k == *key).map(|(_, v)| v)
    }
}

/// Parses a `settings.json`: an empty file is no settings; otherwise one JSON object with unique
/// top-level keys.
// Core: util/settings.cpp ReadSettings
pub fn parse(bytes: &[u8]) -> FormatResult<Settings> {
    if bytes.is_empty() {
        return Ok(Settings {
            values: Vec::new(),
            empty_file: true,
        });
    }
    Ok(Settings {
        values: parse_settings_object(bytes, WHAT, MAX_SETTINGS_FILE_SIZE)?.into_entries(),
        empty_file: false,
    })
}
