//! The node's warnings (`warnings.cpp`): the text `getnetworkinfo`, `getblockchaininfo` and
//! `getmininginfo` show, which the RPC commands read through
//! [`NodeWarnings`](dashd_rpc::warnings::NodeWarnings).

use std::sync::{Mutex, PoisonError};

/// Core's `CLIENT_VERSION_IS_RELEASE`. rdashd reports what its Dash Core pin, a release
/// candidate built as a release, reports: no pre-release warning.
pub const CLIENT_VERSION_IS_RELEASE: bool = true;

const PRE_RELEASE: &str = "This is a pre-release test build - use at your own risk - do not use for mining or merchant applications";
const LARGE_WORK_INVALID_CHAIN: &str = "Warning: We do not appear to fully agree with our peers! You may need to upgrade, or other nodes may need to upgrade.";

#[derive(Debug, Default)]
struct State {
    misc: String,
    large_work_invalid_chain: bool,
}

/// Core's warning globals (`g_misc_warnings`, `fLargeWorkInvalidChainFound`).
#[derive(Debug, Default)]
pub struct Warnings {
    state: Mutex<State>,
}

impl Warnings {
    pub fn new() -> Warnings {
        Warnings::default()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Core's `SetMiscWarning`.
    pub fn set_misc_warning(&self, warning: &str) {
        self.state().misc = warning.to_owned();
    }

    /// Core's `SetfLargeWorkInvalidChainFound`.
    pub fn set_large_work_invalid_chain_found(&self, found: bool) {
        self.state().large_work_invalid_chain = found;
    }

    /// Core's `GetWarnings`: the last applicable warning, or every one joined by `<hr />` when
    /// `verbose`.
    // Core: warnings.cpp:36-65
    pub fn get_warnings(&self, verbose: bool) -> String {
        let state = self.state();
        let mut all = Vec::new();
        if !CLIENT_VERSION_IS_RELEASE {
            all.push(PRE_RELEASE);
        }
        if !state.misc.is_empty() {
            all.push(state.misc.as_str());
        }
        if state.large_work_invalid_chain {
            all.push(LARGE_WORK_INVALID_CHAIN);
        }
        if verbose {
            all.join("<hr />")
        } else {
            all.last().copied().unwrap_or("").to_owned()
        }
    }
}

impl dashd_rpc::warnings::NodeWarnings for Warnings {
    fn get_warnings(&self, verbose: bool) -> String {
        Warnings::get_warnings(self, verbose)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_concise_text_is_the_last_warning() {
        let w = Warnings::new();
        assert_eq!(w.get_warnings(false), "");
        w.set_misc_warning("Disk space is low!");
        assert_eq!(w.get_warnings(false), "Disk space is low!");
        w.set_large_work_invalid_chain_found(true);
        assert_eq!(w.get_warnings(false), LARGE_WORK_INVALID_CHAIN);
        assert_eq!(
            w.get_warnings(true),
            format!("Disk space is low!<hr />{LARGE_WORK_INVALID_CHAIN}")
        );
    }
}
