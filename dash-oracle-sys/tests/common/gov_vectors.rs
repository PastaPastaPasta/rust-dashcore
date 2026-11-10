//! GV1a's governance vectors (`tests/data/gov/*.txt`): one line per oracle answer, every one
//! Core's. The format is in `tests/data/gov/README.md`.

use dash_oracle_sys::Oracle;

/// Compare `lines` with the committed `file`, or write it with `DASH_ORACLE_REGENERATE=1`.
pub fn check_or_write(oracle: &Oracle, file: &str, source: &str, lines: &[String]) {
    let path = format!("{}/tests/data/gov/{file}", env!("CARGO_MANIFEST_DIR"));
    let text = format!(
        "# Written by dash-oracle-sys {source} (DASH_ORACLE_REGENERATE=1): every answer is Core's\n\
         # at the {} pin. Format: README.md.\n{}\n",
        oracle.pin(),
        lines.join("\n")
    );
    if std::env::var("DASH_ORACLE_REGENERATE").is_ok_and(|v| v == "1") {
        std::fs::write(&path, text).expect(&path);
        return;
    }
    let committed = std::fs::read_to_string(&path).expect(&path);
    assert_eq!(committed, text, "the oracle no longer computes {path}");
}
