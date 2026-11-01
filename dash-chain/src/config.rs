//! Configuration and injected services (SB4 §4).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use dash_clock::Clock;
use dash_params::ChainSetup;

use crate::Work;
use crate::error::FailStop;
use crate::feed::FeedConfig;

/// Which product runs the chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChainMode {
    /// The full node: from genesis, unwindowed, `BlockId` = arena index (SB0 §4.1.1).
    Node,
    /// SPV: anchored or from genesis, windowed (SB0 §4.4).
    Spv,
}

/// Core's `DEFAULT_MAX_TIP_AGE` (`kernel/chainstatemanager_opts.h:18`): 6 hours.
pub const DEFAULT_MAX_TIP_AGE: Duration = Duration::from_secs(6 * 60 * 60);

/// The chain's configuration (Core's options that the header index reads).
#[derive(Clone, Debug)]
pub struct ChainConfig {
    pub mode: ChainMode,
    /// Node: `None` (enforced). SPV: `Some(w)`, `w >= L_est + H_keep` (the host checks, SB0 §4.4).
    pub window: Option<u32>,
    /// Core `-checkpoints` (default true).
    pub checkpoints_enabled: bool,
    /// Core `-checkblockindex`; default `params.default_consistency_checks`.
    pub consistency_checks: bool,
    /// Core `-minimumchainwork`; `None` = `params.consensus.minimum_chain_work`.
    pub min_chain_work: Option<Work>,
    /// Core `-maxtipage`.
    pub max_tip_age: Duration,
    /// The feeds (SB5-A6, SB5-A14).
    pub feeds: FeedConfig,
    /// SPV: the cold records cache's capacity, which also bounds a step's invalidation list
    /// (SB5-A6, §5.2). Default 1,024; mobile 256. The node reads no cold records.
    pub cold_cache: u32,
}

/// [`ChainConfig::cold_cache`]'s default (SB5 §5.2).
pub const DEFAULT_COLD_CACHE: u32 = 1_024;

impl ChainConfig {
    /// The node's defaults for `params`.
    pub fn node(params: &ChainSetup) -> ChainConfig {
        ChainConfig {
            mode: ChainMode::Node,
            window: None,
            checkpoints_enabled: true,
            consistency_checks: params.default_consistency_checks,
            min_chain_work: None,
            max_tip_age: DEFAULT_MAX_TIP_AGE,
            feeds: FeedConfig::node(),
            cold_cache: DEFAULT_COLD_CACHE,
        }
    }

    /// SPV's defaults for `params` with window `window`.
    pub fn spv(params: &ChainSetup, window: u32) -> ChainConfig {
        ChainConfig {
            mode: ChainMode::Spv,
            window: Some(window),
            feeds: FeedConfig::spv(),
            ..ChainConfig::node(params)
        }
    }
}

/// SPORK_19's live state (SB4 §7.5).
pub trait SporkView: Send + Sync {
    /// Core `Chainlocks::IsEnabled()` = `IsSporkActive(SPORK_19)` (chainlock/chainlock.cpp:33;
    /// spork.cpp:163-196), evaluated live against the source's adjusted time.
    fn chainlocks_enabled(&self) -> bool;
}

/// A fixed SPORK_19 source (SB4 D29): what a Core node that has received no spork message
/// reports, with an optional override; settable for tests.
///
/// On mainnet `GetSporkValue` returns 0 for every spork but SPORK_21 (`spork.cpp:171-181`), so
/// SPORK_19 is always active whatever is set. On test chains the default
/// (`sporkDefs`: 4070908800, `spork.h:66-72`) is inactive.
#[derive(Debug)]
pub struct StaticSporks {
    hardened: bool,
    enabled: AtomicBool,
}

impl StaticSporks {
    /// The network's no-spork-received state; `enabled` overrides it on test chains.
    pub fn for_network(params: &ChainSetup, enabled: Option<bool>) -> StaticSporks {
        StaticSporks {
            hardened: !params.is_test_chain,
            enabled: AtomicBool::new(enabled.unwrap_or(false)),
        }
    }

    /// Sets the value a received spork would set; ignored on mainnet, as in Core.
    pub fn set(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
    }
}

impl SporkView for StaticSporks {
    fn chainlocks_enabled(&self) -> bool {
        self.hardened || self.enabled.load(Ordering::SeqCst)
    }
}

/// The services the chain reads (SB4 §4).
#[derive(Clone)]
pub struct Services {
    pub clock: Arc<dyn Clock>,
    /// SPORK_19: SP0's manager (node), the shared spork core (SPV); `StaticSporks` until then.
    pub sporks: Arc<dyn SporkView>,
    /// The fail-stop latch (D5).
    pub fail: Arc<dyn FailStop>,
}
