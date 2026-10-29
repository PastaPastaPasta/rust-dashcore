//! The node's warnings (`warnings.cpp`), which `dashd-node` provides: the `warnings` field of
//! `getblockchaininfo`, `getnetworkinfo` and `getmininginfo` (RP1a's commands, BL-175).

/// Core's `GetWarnings`, which the commands read.
pub trait NodeWarnings: Send + Sync {
    /// `GetWarnings(verbose).original`: the last warning that applies, or every one joined by
    /// `<hr />` when `verbose`.
    fn get_warnings(&self, verbose: bool) -> String;
}
