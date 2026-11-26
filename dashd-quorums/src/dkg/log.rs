//! A DKG record's lines and hashes as Core writes them, at `llmq-dkg` debug; a session's
//! events go through [`round::log_events`](super::round::log_events).
//!
//! The lines of one `CDKGLogger` record are one log write, each line indented by four spaces
//! (`CBatchedLogger::Batch`); without `-logsourcelocations` Core prints no header.

use std::fmt::Display;

use tracing::debug;

/// Logs one `CDKGLogger` record of `lines`; nothing when it has none.
pub fn log_record<T: Display>(lines: impl IntoIterator<Item = T>) {
    let text: Vec<String> = lines.into_iter().map(|line| format!("    {line}")).collect();
    if !text.is_empty() {
        debug!(target: "llmq-dkg", "{}", text.join("\n"));
    }
}

/// `uint256::ToString()`: the internal bytes reversed, in hex.
pub fn uint256_hex(bytes: &[u8; 32]) -> String {
    bytes.iter().rev().map(|b| format!("{b:02x}")).collect()
}
