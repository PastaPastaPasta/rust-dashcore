//
// Copyright (c) 2026-present, The Dash Core developers
// SPDX-License-Identifier: MIT
// See the accompanying file LICENSE or https://opensource.org/license/MIT
//

//! Chain parameters as a node runs them: a network's at a release, with the overrides applied.

mod tables;

use crate::prelude::*;
use crate::{
    AddrParams, Bip9Deployment, Bip9Deployments, ChainParams, ChainPolicy, Checkpoint,
    ConsensusParams, DeploymentPos, DevnetGenesisParams, LlmqParams, LlmqType, Network,
    ParamsError, Release, SporkSigners, devnet,
};

use bitcoin_units::BlockHeight;
use dash_num::Hash256;
use dash_types::Numeric;

use core::ops::{Deref, DerefMut};

/// Chain parameters as a node runs them, built by [`ChainParamsBuilder`](crate::ChainParamsBuilder):
/// Core's `CChainParams` plus its `CBaseChainParams` at a [`Release`], with the overrides applied.
///
/// Every [`ChainParams`] field is reached through `Deref`. The fields
/// [`consensus`](Self::consensus), [`addr_params`](Self::addr_params) and
/// [`checkpoints`](Self::checkpoints) shadow the [`ChainParams`] ones, which cannot hold an LLMQ
/// override, the release's deployments, the devnet-only consensus values or a named devnet's
/// devnet genesis checkpoint.
#[derive(Clone, Debug, PartialEq)]
pub struct ChainSetup {
    /// The network.
    pub network: Network,
    /// Core release whose parameters these are.
    pub release: Release,
    /// The parameters with every override that [`ChainParams`] can hold.
    chain: ChainParams,
    /// Consensus rules for this chain.
    pub consensus: ConsensusSetup,
    /// Address encoding parameters.
    pub addr_params: AddrSetup,
    /// Hard-coded checkpoints for fast validation.
    pub checkpoints: Vec<Checkpoint>,
    /// DIP-18 Platform address bech32m HRP (v24 release only).
    pub platform_hrp: Option<&'static str>,
    /// Core's `GetDevNetName()` on a devnet (`devnet` or `devnet-<name>`), `None` elsewhere.
    pub devnet_name: Option<String>,
    /// The devnet genesis block (height 1), on a devnet.
    pub devnet_genesis: Option<DevnetGenesisParams>,
    /// Spork signers after the spork options.
    pub spork_signers: SporkSigners,
    /// Whether non-standard transactions are rejected by default (v23 release only).
    pub require_standard: Option<bool>,
    /// Trusted assumeutxo snapshots.
    pub assumeutxo: &'static [AssumeutxoData],
    /// BIP155-serialized fixed seed addresses (`chainparamsseeds.h`).
    pub fixed_seeds: &'static [u8],
    /// Whether a masternode's external IP must be routable.
    pub require_routable_external_ip: bool,
    /// Whether several masternode addresses may share a network group.
    pub allow_multiple_addresses_from_group: bool,
    /// Seconds before an LLMQ connection is retried.
    pub llmq_connection_retry_timeout: i32,
    /// Seconds until a fulfilled request expires.
    pub fulfilled_request_expire_time: i32,
    /// Values Core hard-codes outside its chain parameters.
    pub pins: HardCodedPins,
    /// Core's runtime policy, `-fastprune` applied.
    pub policy: ChainPolicy,
}

impl ChainSetup {
    /// The parameters of `network` at `release`, with no overrides.
    pub(crate) fn new(network: Network, release: Release) -> Self {
        let mut chain = *network.chain();
        let deployments = ReleaseDeployments::new(release, &chain.consensus.deployments);
        deployments.write_to(&mut chain.consensus.deployments);
        let extras = tables::extras(network);
        Self {
            network,
            release,
            consensus: ConsensusSetup::new(chain.consensus, deployments, network),
            addr_params: AddrSetup::from(chain.addr_params),
            checkpoints: chain.checkpoints.to_vec(),
            platform_hrp: Some(chain.addr_params.bech32_hrp),
            devnet_name: (network == Network::Devnet).then(|| String::from("devnet")),
            devnet_genesis: (network == Network::Devnet)
                .then(|| {
                    DevnetGenesisParams::find(
                        "devnet",
                        &chain.genesis,
                        chain.consensus.hash_genesis_block,
                    )
                })
                .flatten(),
            spork_signers: SporkSigners::default(),
            require_standard: None,
            assumeutxo: tables::assumeutxo(network),
            fixed_seeds: tables::fixed_seeds(network),
            require_routable_external_ip: extras.require_routable_external_ip,
            allow_multiple_addresses_from_group: extras.allow_multiple_addresses_from_group,
            llmq_connection_retry_timeout: extras.llmq_connection_retry_timeout,
            fulfilled_request_expire_time: extras.fulfilled_request_expire_time,
            pins: extras.pins,
            policy: ChainPolicy::new(network, false),
            chain,
        }
    }

    /// Core's `CBaseChainParams::DataDir()`: the data directory suffix.
    pub fn data_dir(&self) -> &str {
        match self.network {
            Network::Main => "",
            Network::Testnet3 => "testnet3",
            Network::Devnet => self.devnet_name.as_deref().unwrap_or("devnet"),
            Network::Regtest => "regtest",
        }
    }

    /// The version-bits deployment at `pos`, if this release has it.
    pub fn deployment(&self, pos: DeploymentPos) -> Option<&Bip9Deployment> {
        self.consensus.deployments.get(pos)
    }

    /// The release's version-bits deployments, in position order.
    pub fn deployments(&self) -> impl Iterator<Item = (DeploymentPos, &Bip9Deployment)> {
        self.consensus.deployments.iter()
    }

    /// The address parameters, for base-sdk calls that take [`AddrParams`].
    ///
    /// Its `bech32_hrp` is the [`platform_hrp`](Self::platform_hrp), empty at releases without
    /// one.
    pub fn base_sdk_addr_params(&self) -> AddrParams {
        let AddrSetup {
            pubkey_addr,
            script_addr,
            secret_key,
            ext_pubkey,
            ext_secret,
            bip44_idx,
        } = self.addr_params;
        AddrParams {
            pubkey_addr,
            script_addr,
            secret_key,
            ext_pubkey,
            ext_secret,
            bip44_idx,
            bech32_hrp: self.platform_hrp.unwrap_or(""),
        }
    }

    /// Makes this devnet the one Core calls `name` (`devnet-<name>`): its devnet genesis block
    /// and the checkpoint on it.
    ///
    /// The checkpoint list of the [`ChainParams`] copy is leaked, to give it a `'static` slice
    /// (DEC-911).
    pub(crate) fn name_devnet(&mut self, name: String) -> Result<(), ParamsError> {
        let genesis_hash = self.consensus.hash_genesis_block;
        let Some(devnet_genesis) =
            DevnetGenesisParams::find(&name, &self.chain.genesis, genesis_hash)
        else {
            return Err(ParamsError::DevnetGenesis(name));
        };
        self.consensus.hash_devnet_genesis_block = devnet_genesis.hash;
        self.checkpoints = vec![
            (BlockHeight::from_u32(0), genesis_hash),
            (BlockHeight::from_u32(1), devnet_genesis.hash),
        ];
        self.chain.checkpoints = Box::leak(self.checkpoints.clone().into_boxed_slice());
        self.chain.chain_tx_data.timestamp = i64::from(devnet_genesis.time);
        self.devnet_name = Some(name);
        self.devnet_genesis = Some(devnet_genesis);
        Ok(())
    }

    /// Writes the consensus values [`ChainParams`] can hold into its copy.
    pub(crate) fn sync_chain(&mut self) {
        self.consensus.deployments.write_to(&mut self.consensus.base.deployments);
        self.chain.consensus = self.consensus.base;
    }
}

impl Deref for ChainSetup {
    type Target = ChainParams;

    fn deref(&self) -> &ChainParams {
        &self.chain
    }
}

impl DerefMut for ChainSetup {
    fn deref_mut(&mut self) -> &mut ChainParams {
        &mut self.chain
    }
}

/// Consensus parameters as a node runs them: a [`ConsensusParams`] with the LLMQ overrides, the
/// release's deployments and the devnet-only values.
///
/// Every [`ConsensusParams`] field is reached through `Deref`, except
/// [`deployments`](Self::deployments), which this type shadows. [`llmq`](Self::llmq) is Core's
/// `GetLLMQ` and sees the overrides, where [`ConsensusParams::llmq`] reads the static table.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ConsensusSetup {
    /// The parameters with every override [`ConsensusParams`] can hold.
    base: ConsensusParams,
    /// Parameters of each configured LLMQ type, in Core's order, overrides applied.
    llmq_params: Vec<(LlmqType, LlmqParams)>,
    /// Version-bits deployments of the release.
    pub deployments: ReleaseDeployments,
    /// Hash of the devnet genesis block (height 1), zero off devnets.
    pub hash_devnet_genesis_block: Hash256,
    /// Blocks mined at minimum difficulty at the start of a devnet. A signed count the reference
    /// implementation leaves unchecked.
    pub minimum_difficulty_blocks: i32,
    /// Blocks with a raised subsidy at the start of a devnet. A signed count the reference
    /// implementation leaves unchecked.
    pub high_subsidy_blocks: i32,
    /// Subsidy multiplier for those blocks, which may be zero or negative.
    pub high_subsidy_factor: i32,
}

impl ConsensusSetup {
    /// `base` with its LLMQ types' table parameters and `deployments`, and the devnet-only
    /// values of `network`: Core's defaults (v24.0.0 consensus/params.h:199-201), and the devnet genesis
    /// hash of the empty-named devnet on a devnet.
    fn new(base: ConsensusParams, deployments: ReleaseDeployments, network: Network) -> Self {
        let llmq_params = base
            .llmqs
            .iter()
            .filter_map(|&llmq_type| Some((llmq_type, llmq_type.params()?)))
            .collect();
        Self {
            base,
            llmq_params,
            deployments,
            hash_devnet_genesis_block: if network == Network::Devnet {
                devnet::DEVNET_GENESIS_HASH
            } else {
                Hash256::ZERO
            },
            minimum_difficulty_blocks: 0,
            high_subsidy_blocks: 0,
            high_subsidy_factor: 1,
        }
    }

    /// The configured parameters of `llmq_type` (Core's `GetLLMQ`), or `None` if the chain
    /// does not run it.
    pub fn llmq(&self, llmq_type: LlmqType) -> Option<&LlmqParams> {
        self.llmq_params.iter().find_map(|(t, params)| (*t == llmq_type).then_some(params))
    }

    /// The configured parameters of `llmq_type`, to change.
    pub fn llmq_mut(&mut self, llmq_type: LlmqType) -> Option<&mut LlmqParams> {
        self.llmq_params.iter_mut().find_map(|(t, params)| (*t == llmq_type).then_some(params))
    }

    /// Each configured LLMQ type with its parameters, in Core's order.
    pub fn llmq_params(&self) -> impl Iterator<Item = (LlmqType, &LlmqParams)> {
        self.llmq_params.iter().map(|(t, params)| (*t, params))
    }

    /// Core's `DifficultyAdjustmentInterval`, or `None` where it is 0.
    ///
    /// A devnet spacing above the timespan makes it 0, and Core's
    /// `% DifficultyAdjustmentInterval()` then divides by zero.
    pub const fn difficulty_adjustment_interval(&self) -> Option<i64> {
        match self.base.pow_target_timespan.checked_div(self.base.pow_target_spacing) {
            Some(0) | None => None,
            interval => interval,
        }
    }
}

impl Deref for ConsensusSetup {
    type Target = ConsensusParams;

    fn deref(&self) -> &ConsensusParams {
        &self.base
    }
}

impl DerefMut for ConsensusSetup {
    fn deref_mut(&mut self) -> &mut ConsensusParams {
        &mut self.base
    }
}

/// The version-bits deployments of a [`Release`], Core's `vDeployments`.
///
/// `evo_shares` is read by position, as a release without it would lack it.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ReleaseDeployments {
    /// Reserved deployment used only for testing.
    pub test_dummy: Bip9Deployment,
    /// Deployment parameters for the v24 upgrade (bit 13).
    pub v24: Bip9Deployment,
    /// Deployment enabling EvoNode shared collateral and multiple owner payouts, from v24.
    shares: Option<Bip9Deployment>,
}

impl ReleaseDeployments {
    /// The deployments of `release`, from its network table's.
    fn new(release: Release, table: &Bip9Deployments) -> Self {
        Self {
            test_dummy: table.test_dummy,
            v24: table.v24,
            shares: release
                .deployments()
                .contains(&DeploymentPos::EvoShares)
                .then_some(table.evo_shares),
        }
    }

    /// The deployment at `pos`, if the release has it.
    pub fn get(&self, pos: DeploymentPos) -> Option<&Bip9Deployment> {
        match pos {
            DeploymentPos::TestDummy => Some(&self.test_dummy),
            DeploymentPos::V24 => Some(&self.v24),
            DeploymentPos::EvoShares => self.shares.as_ref(),
        }
    }

    /// The deployment at `pos`, to change, if the release has it.
    ///
    /// This changes the release's view, which [`get`](Self::get), [`iter`](Self::iter) and
    /// [`ChainSetup::deployment`] read; the [`ChainParams`] copy keeps the value it was built
    /// with.
    pub fn deployment_mut(&mut self, pos: DeploymentPos) -> Option<&mut Bip9Deployment> {
        match pos {
            DeploymentPos::TestDummy => Some(&mut self.test_dummy),
            DeploymentPos::V24 => Some(&mut self.v24),
            DeploymentPos::EvoShares => self.shares.as_mut(),
        }
    }

    /// The release's deployments with their positions, in position order.
    pub fn iter(&self) -> impl Iterator<Item = (DeploymentPos, &Bip9Deployment)> {
        [
            (DeploymentPos::TestDummy, Some(&self.test_dummy)),
            (DeploymentPos::V24, Some(&self.v24)),
            (DeploymentPos::EvoShares, self.shares.as_ref()),
        ]
        .into_iter()
        .filter_map(|(pos, deployment)| Some((pos, deployment?)))
    }

    /// Writes the deployments into `out`. A deployment the release lacks keeps its value there,
    /// which never activates.
    fn write_to(&self, out: &mut Bip9Deployments) {
        out.test_dummy = self.test_dummy;
        out.v24 = self.v24;
        if let Some(shares) = self.shares {
            out.evo_shares = shares;
        }
    }
}

/// Address encoding parameters, without the Platform HRP, which [`ChainSetup::platform_hrp`]
/// holds by release.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AddrSetup {
    /// P2PKH address version byte.
    pub pubkey_addr: u8,
    /// P2SH address version byte.
    pub script_addr: u8,
    /// WIF private key version byte.
    pub secret_key: u8,
    /// BIP32 extended public key prefix.
    pub ext_pubkey: [u8; 4],
    /// BIP32 extended secret key prefix.
    pub ext_secret: [u8; 4],
    /// BIP44 coin type index.
    pub bip44_idx: u32,
}

impl From<AddrParams> for AddrSetup {
    fn from(addr: AddrParams) -> Self {
        Self {
            pubkey_addr: addr.pubkey_addr,
            script_addr: addr.script_addr,
            secret_key: addr.secret_key,
            ext_pubkey: addr.ext_pubkey,
            ext_secret: addr.ext_secret,
            bip44_idx: addr.bip44_idx,
        }
    }
}

/// An assumeutxo snapshot Core trusts (`AssumeutxoData`).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AssumeutxoData {
    /// Height of the snapshot base block.
    pub height: BlockHeight,
    /// Expected hash of the serialized UTXO set.
    pub hash_serialized: Hash256,
    /// Expected hash of the evo section (v24 release only).
    pub evo_hash: Option<Hash256>,
    /// Transactions up to and including the base block.
    pub chain_tx: u32,
}

/// Network values Core hard-codes outside `CChainParams` (quirk 34).
///
/// The `bad-sb-start` and `bad-dip3-enf-block` pins are chain parameters already,
/// [`ConsensusParams::superblock_start`] and [`ConsensusParams::dip0003_enforcement`].
///
/// Deliberately absent: Core literals that decide a rule's path by network rather than pin a
/// height of one, which stay at their rule sites. These are the mainnet `nPrevHeight <= 4500`
/// subsidy branch, the devnet subsidy path and the `LLMQ_50_60` testnet and devnet branch of
/// `IsQuorumTypeEnabled`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct HardCodedPins {
    /// Last height whose header nBits only needs to be within 50% of DGW's (`bad-diffbits`);
    /// Core applies it on mainnet only.
    ///
    /// Core: validation.cpp ContextualCheckBlockHeader, `nHeight <= 68589` (v23.1.8 :4110,
    /// v24.0.0-rc.3 :4476).
    pub pre_dgw_tolerance_until: Option<BlockHeight>,
    /// `LLMQ_25_67` is enabled once the previous block is at this height. Core applies it on
    /// any chain that configures the type; only testnet does.
    ///
    /// Core: validation.cpp IsQuorumTypeEnabled, `TESTNET_LLMQ_25_67_ACTIVATION_HEIGHT`
    /// (v23.1.8 :6078, v24.0.0-rc.3 :6670).
    pub llmq_25_67_activation_height: BlockHeight,
}
