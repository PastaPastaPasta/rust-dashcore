//! SP0 in the node: Core's `sporkman`, made with the spork address and `-sporkkey` in
//! `AppInitMain`, loaded at step 7a, the chain's [`SporkView`](dash_chain::SporkView), and stored
//! at shutdown after a load that succeeded (its destructor).
//!
//! Until P2a-node runs the network core, the adjusted time is the node clock: no peer has a time
//! offset yet (SP0 D8).

use std::sync::Arc;

use dash_args::ArgsManager;
use dash_clock::{AdjustedTime, Clock, NodeSeconds};
use dash_params::ChainParams;
use dashd_masternode::sporks::SporkManager;

use crate::InitFailure;

/// Core's `GetAdjustedTime()` with no peer time offset.
struct NodeAdjustedTime(Arc<dyn Clock>);

impl AdjustedTime for NodeAdjustedTime {
    fn now(&self) -> NodeSeconds {
        self.0.node_now()
    }
}

/// Core's `sporkman` after `SetSporkAddress` (the address `ChainParams` already checked) and
/// `-sporkkey`.
// Core: init.cpp:1742-1757
pub fn spork_manager(
    args: &ArgsManager,
    params: &ChainParams,
    clock: Arc<dyn Clock>,
) -> Result<Arc<SporkManager>, InitFailure> {
    let sporks = Arc::new(SporkManager::new(params, Arc::new(NodeAdjustedTime(clock))));
    if args.is_arg_set("-sporkkey") && sporks.set_priv_key(&args.get_arg("-sporkkey", "")?).is_err()
    {
        return Err(InitFailure::Error("Unable to sign spork message, wrong key?".to_owned()));
    }
    Ok(sporks)
}
