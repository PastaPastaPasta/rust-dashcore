//! The reader for the corpora under `corpus/`, which are JSON behind `//` header lines.

use serde_json::Value;

use std::fs;

/// Parses `corpus/<name>.json5` of this crate.
pub fn corpus(name: &str) -> Value {
    let path = format!("{}/corpus/{name}.json5", env!("CARGO_MANIFEST_DIR"));
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"));
    let body: Vec<&str> = text.lines().filter(|l| !l.trim_start().starts_with("//")).collect();
    serde_json::from_str(&body.join("\n")).unwrap_or_else(|e| panic!("{name}: parse: {e}"))
}
