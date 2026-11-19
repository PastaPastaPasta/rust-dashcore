//! Core's per-chain runtime policy (DEC-1041).
//!
//! Core keeps these `CChainParams` members next to the chain's definition, but they steer the
//! node rather than define the chain: when pruning may start, how many blocks pruning keeps,
//! the disk-size hints, the `-checkblockindex` default and whether mock time is allowed.
//! base-sdk's `dash-params` carries only stateless chain parameters, so the node keeps them here.
//!
//! The node targets Core v24 alone (DEC-82), so the table has no release axis. Cites are at
//! Dash Core v24.0.0-rc.5, the release line's target. The `dash-params` pin before the S5 pin
//! move still holds rc.3's values, which differ in three places: testnet's and devnet's
//! `prune_after_height` (rc.3 1000, rc.5 3000), mainnet's `assumed_blockchain_size_gb`
//! (rc.3 57, rc.5 62), and `min_blocks_to_keep`, which rc.3 does not have (it keeps
//! `MIN_BLOCKS_TO_KEEP`, 288, everywhere).

use crate::{ChainSetup, Network};

/// Core's runtime policy for one chain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChainPolicy {
    /// `nPruneAfterHeight`: pruning starts only above this height.
    pub prune_after_height: u64,
    /// `MinBlocksToKeep()` (`chainparams.h:118`): pruning never deletes a block this close to
    /// the tip. Not `BLOCKS_NEAR_TIP` (`validation.h:88`, 288 on every chain), which bounds the
    /// unrequested blocks a node accepts and what a pruned node announces; rc.5 split the two.
    pub min_blocks_to_keep: u32,
    /// `m_assumed_blockchain_size`: the block files' expected size, in GB.
    pub assumed_blockchain_size_gb: u64,
    /// `m_assumed_chain_state_size`: the chainstate's expected size, in GB.
    pub assumed_chain_state_size_gb: u64,
    /// `fDefaultConsistencyChecks`: the default of `-checkblockindex`.
    pub default_consistency_checks: bool,
    /// `m_is_mockable_chain`: whether `setmocktime` and the other mock RPCs are allowed.
    pub is_mockable_chain: bool,
}

impl ChainPolicy {
    /// Core's policy for `network`.
    ///
    /// `fast_prune` is Core's `-fastprune`, which only `CRegTestParams` reads: on regtest it
    /// lowers [`prune_after_height`](Self::prune_after_height) to 100. It changes nothing else,
    /// so a caller that reads no other field may pass `false`.
    ///
    /// v24.0.0-rc.5 `chainparams.cpp`: main :245-248, :292, :297; testnet :451-454, :497,
    /// :502; devnet :635-638, :691, :696; regtest :880-883, :898, :903.
    pub const fn new(network: Network, fast_prune: bool) -> Self {
        match network {
            Network::Main => Self {
                prune_after_height: 100_000,
                min_blocks_to_keep: 2_304,
                assumed_blockchain_size_gb: 62,
                assumed_chain_state_size_gb: 1,
                default_consistency_checks: false,
                is_mockable_chain: false,
            },
            Network::Testnet3 => Self {
                prune_after_height: 3_000,
                min_blocks_to_keep: 2_304,
                assumed_blockchain_size_gb: 10,
                assumed_chain_state_size_gb: 1,
                default_consistency_checks: false,
                is_mockable_chain: false,
            },
            Network::Devnet => Self {
                prune_after_height: 3_000,
                min_blocks_to_keep: 2_304,
                assumed_blockchain_size_gb: 0,
                assumed_chain_state_size_gb: 0,
                default_consistency_checks: false,
                is_mockable_chain: false,
            },
            Network::Regtest => Self {
                prune_after_height: if fast_prune {
                    100
                } else {
                    1_000
                },
                min_blocks_to_keep: 288,
                assumed_blockchain_size_gb: 0,
                assumed_chain_state_size_gb: 0,
                default_consistency_checks: true,
                is_mockable_chain: true,
            },
        }
    }

    /// Core's policy for `setup`'s network; `fast_prune` as in [`new`](Self::new).
    pub const fn for_setup(setup: &ChainSetup, fast_prune: bool) -> Self {
        Self::new(setup.network, fast_prune)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (network, prune start, blocks kept, chain GB, state GB, consistency checks, mockable),
    /// copied from Core's v24.0.0-rc.5 sources.
    type Row = (Network, u64, u32, u64, u64, bool, bool);
    const ROWS: [Row; 4] = [
        (Network::Main, 100_000, 2_304, 62, 1, false, false),
        (Network::Testnet3, 3_000, 2_304, 10, 1, false, false),
        (Network::Devnet, 3_000, 2_304, 0, 0, false, false),
        (Network::Regtest, 1_000, 288, 0, 0, true, true),
    ];

    /// Every network has a row, and the table matches it.
    #[test]
    fn matches_core() {
        for network in Network::ALL {
            let (_, prune, keep, chain_gb, state_gb, checks, mockable) = *ROWS
                .iter()
                .find(|row| row.0 == network)
                .unwrap_or_else(|| panic!("no Core row for {network:?}"));
            let want = ChainPolicy {
                prune_after_height: prune,
                min_blocks_to_keep: keep,
                assumed_blockchain_size_gb: chain_gb,
                assumed_chain_state_size_gb: state_gb,
                default_consistency_checks: checks,
                is_mockable_chain: mockable,
            };
            assert_eq!(ChainPolicy::new(network, false), want, "{network:?}");
        }
    }

    /// `-fastprune` lowers regtest's prune start to 100 and changes nothing else anywhere.
    #[test]
    fn fast_prune_is_regtest_only() {
        for network in Network::ALL {
            let slow = ChainPolicy::new(network, false);
            let want = if network == Network::Regtest {
                ChainPolicy {
                    prune_after_height: 100,
                    ..slow
                }
            } else {
                slow
            };
            assert_eq!(ChainPolicy::new(network, true), want, "{network:?}");
        }
    }

    /// `for_setup` reads the setup's network.
    #[test]
    fn for_setup_uses_network() {
        for network in Network::ALL {
            let setup = crate::ChainParamsBuilder::new(network, crate::Release::V24_0)
                .build()
                .expect("Core's parameters");
            assert_eq!(ChainPolicy::for_setup(&setup, false), ChainPolicy::new(network, false));
        }
    }
}
