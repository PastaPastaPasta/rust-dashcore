//! Core's per-chain runtime policy (DEC-1041).
//!
//! Core keeps these `CChainParams` members next to the chain's definition, but they steer the
//! node rather than define the chain: when pruning may start, how many blocks pruning keeps,
//! the disk-size hints, the `-checkblockindex` default and whether mock time is allowed.
//! base-sdk's `dash-params` carries only stateless chain parameters, so the node keeps them here.
//!
//! Cites in this module are at Dash Core v24.0.0-rc.5 (`V24_0`) and v23.1.8 (`V23_1`).
//! `V24_0` holds rc.5's values, the release line's target, although the `dash-params` pin
//! before the S5 pin move still holds rc.3's. They differ in three places: testnet's and
//! devnet's `prune_after_height` (rc.3 1000, rc.5 3000), mainnet's
//! `assumed_blockchain_size_gb` (rc.3 57, rc.5 62), and `min_blocks_to_keep`, which rc.3 does
//! not have (it keeps `MIN_BLOCKS_TO_KEEP`, 288, everywhere).

use dash_params::{ChainSetup, Network, Release};

/// Core's runtime policy for one chain at one release.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChainPolicy {
    /// `nPruneAfterHeight`: pruning starts only above this height.
    pub prune_after_height: u64,
    /// `MinBlocksToKeep()` (`chainparams.h:118`): pruning never deletes a block this close to
    /// the tip. v23.1 has the constant `MIN_BLOCKS_TO_KEEP` (v23.1.8 `validation.h:92`, 288)
    /// instead. Not `BLOCKS_NEAR_TIP` (`validation.h:88`, 288 on every chain), which bounds the
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
    /// Core's policy for `network` at `release`.
    ///
    /// `fast_prune` is Core's `-fastprune`, which only `CRegTestParams` reads: on regtest it
    /// lowers [`prune_after_height`](Self::prune_after_height) to 100. It changes nothing else,
    /// so a caller that reads no other field may pass `false`.
    ///
    /// v24.0.0-rc.5 `chainparams.cpp`: main :245-248, :292, :297; testnet :451-454, :497,
    /// :502; devnet :635-638, :691, :696; regtest :880-883, :898, :903.
    /// v23.1.8 `chainparams.cpp`: main :236-238, :279, :285; testnet :430-432, :472, :478;
    /// devnet :602-604, :654, :660; regtest :836-838, :853, :859; `validation.h:92`.
    pub const fn new(network: Network, release: Release, fast_prune: bool) -> Self {
        // Off regtest: (blocks kept, testnet and devnet prune start, mainnet chain GB).
        let (public_keep, public_prune, main_gb) = match release {
            Release::V23_1 => (288, 1_000, 57),
            Release::V24_0 => (2_304, 3_000, 62),
        };
        match network {
            Network::Main => Self {
                prune_after_height: 100_000,
                min_blocks_to_keep: public_keep,
                assumed_blockchain_size_gb: main_gb,
                assumed_chain_state_size_gb: 1,
                default_consistency_checks: false,
                is_mockable_chain: false,
            },
            Network::Testnet3 => Self {
                prune_after_height: public_prune,
                min_blocks_to_keep: public_keep,
                assumed_blockchain_size_gb: 10,
                assumed_chain_state_size_gb: 1,
                default_consistency_checks: false,
                is_mockable_chain: false,
            },
            Network::Devnet => Self {
                prune_after_height: public_prune,
                min_blocks_to_keep: public_keep,
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

    /// Core's policy for `setup`'s network and release; `fast_prune` as in [`new`](Self::new).
    pub const fn for_setup(setup: &ChainSetup, fast_prune: bool) -> Self {
        Self::new(setup.network, setup.release, fast_prune)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (network, release, prune start, blocks kept, chain GB, state GB, consistency checks,
    /// mockable), copied from Core's sources at each tag.
    type Row = (Network, Release, u64, u32, u64, u64, bool, bool);
    const ROWS: [Row; 8] = [
        (Network::Main, Release::V24_0, 100_000, 2_304, 62, 1, false, false),
        (Network::Testnet3, Release::V24_0, 3_000, 2_304, 10, 1, false, false),
        (Network::Devnet, Release::V24_0, 3_000, 2_304, 0, 0, false, false),
        (Network::Regtest, Release::V24_0, 1_000, 288, 0, 0, true, true),
        (Network::Main, Release::V23_1, 100_000, 288, 57, 1, false, false),
        (Network::Testnet3, Release::V23_1, 1_000, 288, 10, 1, false, false),
        (Network::Devnet, Release::V23_1, 1_000, 288, 0, 0, false, false),
        (Network::Regtest, Release::V23_1, 1_000, 288, 0, 0, true, true),
    ];

    /// Every network at every release has a row, and the table matches it.
    #[test]
    fn matches_core_at_each_release() {
        for release in Release::ALL {
            for network in Network::ALL {
                let (.., prune, keep, chain_gb, state_gb, checks, mockable) = *ROWS
                    .iter()
                    .find(|row| row.0 == network && row.1 == release)
                    .unwrap_or_else(|| panic!("no Core row for {network:?} {release:?}"));
                let want = ChainPolicy {
                    prune_after_height: prune,
                    min_blocks_to_keep: keep,
                    assumed_blockchain_size_gb: chain_gb,
                    assumed_chain_state_size_gb: state_gb,
                    default_consistency_checks: checks,
                    is_mockable_chain: mockable,
                };
                assert_eq!(
                    ChainPolicy::new(network, release, false),
                    want,
                    "{network:?} {release:?}"
                );
            }
        }
    }

    /// `-fastprune` lowers regtest's prune start to 100 and changes nothing else anywhere.
    #[test]
    fn fast_prune_is_regtest_only() {
        for release in Release::ALL {
            for network in Network::ALL {
                let slow = ChainPolicy::new(network, release, false);
                let fast = ChainPolicy::new(network, release, true);
                let want = if network == Network::Regtest {
                    ChainPolicy {
                        prune_after_height: 100,
                        ..slow
                    }
                } else {
                    slow
                };
                assert_eq!(fast, want, "{network:?} {release:?}");
            }
        }
    }

    /// `for_setup` reads the setup's network and release.
    #[test]
    fn for_setup_uses_network_and_release() {
        for release in Release::ALL {
            let setup = dash_params::ChainParamsBuilder::new(Network::Testnet3, release)
                .build()
                .expect("Core's parameters");
            assert_eq!(
                ChainPolicy::for_setup(&setup, false),
                ChainPolicy::new(Network::Testnet3, release, false)
            );
        }
    }
}
