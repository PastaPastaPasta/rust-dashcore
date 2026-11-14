//! One mock clock for every node of a test.

use dashcore::Network;
use dashcore::blockdata::constants::genesis_block;

use crate::rpc::{NodeRpc, RpcError};

/// A mock time in seconds since the epoch, shared by the nodes of a test.
///
/// Nodes start with [`MockTime::arg`]; [`MockTime::advance`] moves the clock and
/// [`MockTime::apply`] sends it to running nodes with `setmocktime`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MockTime(i64);

impl MockTime {
    pub fn at(time: i64) -> Self {
        Self(time)
    }

    /// The regtest genesis block's time, where Core's functional tests start their clock.
    pub fn regtest_genesis() -> Self {
        Self(i64::from(genesis_block(Network::Regtest).header.time))
    }

    pub fn time(self) -> i64 {
        self.0
    }

    /// The start argument, `-mocktime=<time>`.
    pub fn arg(self) -> String {
        format!("-mocktime={}", self.0)
    }

    /// Moves the clock by `secs`.
    pub fn advance(&mut self, secs: i64) {
        self.0 += secs;
    }

    /// Sets every node in `nodes` to this time. Every node is tried; the error names, by
    /// position, the nodes left on their old time.
    pub fn apply<'a, N: NodeRpc + ?Sized + 'a>(
        self,
        nodes: impl IntoIterator<Item = &'a N>,
    ) -> Result<(), RpcError> {
        let failed: Vec<String> = nodes
            .into_iter()
            .enumerate()
            .filter_map(|(i, node)| node.set_mocktime(self.0).err().map(|e| format!("{i}: {e}")))
            .collect();
        if failed.is_empty() {
            Ok(())
        } else {
            Err(format!("setmocktime {} failed on nodes {}", self.0, failed.join("; ")).into())
        }
    }
}
