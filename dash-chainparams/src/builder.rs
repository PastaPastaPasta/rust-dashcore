//
// Copyright (c) 2026-present, The Dash Core developers
// SPDX-License-Identifier: MIT
// See the accompanying file LICENSE or https://opensource.org/license/MIT
//

//! Chain parameters with Core's command-line overrides applied.

use crate::error::{ParamsError, VbField};
use crate::parse::{int_arg, interpret_bool, parse_i32, parse_i64};
use crate::prelude::*;
use crate::spork::spork_key_id;
use crate::{
    BuriedDeployment, BuriedDeploymentExt, ChainPolicy, ChainSetup, DeploymentPos, LlmqType,
    MAX_LLMQ_SIZE, Network, Release, SporkSigners,
};

use bitcoin_units::BlockHeight;

/// A Core command-line option that changes chain parameters.
///
/// These are the options `SetupChainParamsOptions` registers, plus three
/// `init.cpp` registers: `-fastprune`, which regtest's prune height reads,
/// and the spork signer options, which startup reads after the chain params.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ChainOption {
    /// `-budgetparams=<masternode>:<budget>:<superblock>` (regtest).
    BudgetParams,
    /// `-devnet=<name>` (devnet).
    Devnet,
    /// `-dip3params=<activation>:<enforcement>` (regtest).
    Dip3Params,
    /// `-fastprune` (regtest).
    FastPrune,
    /// `-highsubsidyblocks=<n>` (devnet).
    HighSubsidyBlocks,
    /// `-highsubsidyfactor=<n>` (devnet).
    HighSubsidyFactor,
    /// `-llmqchainlocks=<quorum name>` (devnet).
    LlmqChainLocks,
    /// `-llmqdevnetparams=<size>:<threshold>` (devnet).
    LlmqDevnetParams,
    /// `-llmqinstantsenddip0024=<quorum name>` (devnet).
    LlmqInstantSendDip0024,
    /// `-llmqmnhf=<quorum name>` (devnet).
    LlmqMnhf,
    /// `-llmqplatform=<quorum name>` (devnet).
    LlmqPlatform,
    /// `-llmqtestinstantsenddip0024=<quorum name>` (regtest).
    LlmqTestInstantSendDip0024,
    /// `-llmqtestinstantsendparams=<size>:<threshold>` (regtest).
    LlmqTestInstantSendParams,
    /// `-llmqtestparams=<size>:<threshold>` (regtest).
    LlmqTestParams,
    /// `-llmqtestplatformparams=<size>:<threshold>` (regtest).
    LlmqTestPlatformParams,
    /// `-minimumdifficultyblocks=<n>` (devnet).
    MinimumDifficultyBlocks,
    /// `-minsporkkeys=<n>` (v23 only; v24 refuses it).
    MinSporkKeys,
    /// `-powtargetspacing=<n>` (devnet).
    PowTargetSpacing,
    /// `-sporkaddr=<address>` (every chain).
    SporkAddr,
    /// `-testactivationheight=name@height` (regtest, repeatable).
    TestActivationHeight,
    /// `-vbparams=<deployment>:<start>:<end>...` (regtest, repeatable).
    VbParams,
}

impl ChainOption {
    /// Every option, in name order.
    pub const ALL: [Self; 21] = [
        Self::BudgetParams,
        Self::Devnet,
        Self::Dip3Params,
        Self::FastPrune,
        Self::HighSubsidyBlocks,
        Self::HighSubsidyFactor,
        Self::LlmqChainLocks,
        Self::LlmqDevnetParams,
        Self::LlmqInstantSendDip0024,
        Self::LlmqMnhf,
        Self::LlmqPlatform,
        Self::LlmqTestInstantSendDip0024,
        Self::LlmqTestInstantSendParams,
        Self::LlmqTestParams,
        Self::LlmqTestPlatformParams,
        Self::MinimumDifficultyBlocks,
        Self::MinSporkKeys,
        Self::PowTargetSpacing,
        Self::SporkAddr,
        Self::TestActivationHeight,
        Self::VbParams,
    ];

    /// The option's name with its leading dash, as Core spells it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::BudgetParams => "-budgetparams",
            Self::Devnet => "-devnet",
            Self::Dip3Params => "-dip3params",
            Self::FastPrune => "-fastprune",
            Self::HighSubsidyBlocks => "-highsubsidyblocks",
            Self::HighSubsidyFactor => "-highsubsidyfactor",
            Self::LlmqChainLocks => "-llmqchainlocks",
            Self::LlmqDevnetParams => "-llmqdevnetparams",
            Self::LlmqInstantSendDip0024 => "-llmqinstantsenddip0024",
            Self::LlmqMnhf => "-llmqmnhf",
            Self::LlmqPlatform => "-llmqplatform",
            Self::LlmqTestInstantSendDip0024 => "-llmqtestinstantsenddip0024",
            Self::LlmqTestInstantSendParams => "-llmqtestinstantsendparams",
            Self::LlmqTestParams => "-llmqtestparams",
            Self::LlmqTestPlatformParams => "-llmqtestplatformparams",
            Self::MinimumDifficultyBlocks => "-minimumdifficultyblocks",
            Self::MinSporkKeys => "-minsporkkeys",
            Self::PowTargetSpacing => "-powtargetspacing",
            Self::SporkAddr => "-sporkaddr",
            Self::TestActivationHeight => "-testactivationheight",
            Self::VbParams => "-vbparams",
        }
    }

    /// The option named `name` (with its leading dash).
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|o| o.name() == name)
    }

    /// Whether Core accepts `-no<option>`.
    ///
    /// Core registers `-powtargetspacing` with `DISALLOW_NEGATION`, so its
    /// argument parser refuses every negation of it on every chain, the
    /// double negative `-nopowtargetspacing=0` included.
    pub const fn negatable(self) -> bool {
        !matches!(self, Self::PowTargetSpacing)
    }
}

/// Which rotation an LLMQ role option requires of the type it names.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Rotation {
    Any,
    Required,
    Forbidden,
}

/// Builds a [`ChainSetup`] the way Core's `CreateChainParams` does from its
/// arguments.
///
/// Build at startup or in tests, never per request: a named devnet's
/// checkpoint list is leaked to give its [`ChainParams`](crate::ChainParams)
/// copy a `'static` slice.
///
/// Overrides are kept as Core's command-line settings. Each [`arg`](Self::arg)
/// or [`negate`](Self::negate) is one occurrence, as `ArgsManager` stores it.
/// A single-valued option reads its last occurrence and a repeatable one the
/// values after its last negation. A bare option is the empty string, and a
/// double negation such as `-nofastprune=0` is the value `1`, but only for a
/// [`negatable`](ChainOption::negatable) option: every `-no` form of any
/// other is a [`negate`](Self::negate), which Core refuses before it reads
/// the value. The builder cannot tell a double negation from a value.
///
/// The occurrences have command-line semantics only. Core merges other
/// sources by other rules (a config file keeps its first value, network
/// sections apply to their chain). A caller with other sources passes the
/// resolved settings: one occurrence per single-valued option and the
/// resolved list for the repeatable ones.
///
/// [`build`](Self::build) applies the options of the chain's kind in Core's
/// order and ignores the others, as Core does, so an error Core reports is
/// reported first.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ChainParamsBuilder {
    network: Network,
    release: Release,
    args: Vec<(ChainOption, Option<String>)>,
}

impl ChainParamsBuilder {
    /// A builder for `network` at `release` with no overrides.
    pub const fn new(network: Network, release: Release) -> Self {
        Self {
            network,
            release,
            args: Vec::new(),
        }
    }

    /// Adds one occurrence of `option` with `value`.
    pub fn arg(mut self, option: ChainOption, value: impl Into<String>) -> Self {
        self.args.push((option, Some(value.into())));
        self
    }

    /// Adds one negated occurrence of `option`, as `-no<option>`.
    pub fn negate(mut self, option: ChainOption) -> Self {
        self.args.push((option, None));
        self
    }

    /// `-devnet=<name>`; the devnet is `devnet-<name>`, or `devnet` if empty.
    pub fn devnet_name(self, name: &str) -> Self {
        self.arg(ChainOption::Devnet, name)
    }

    /// `-fastprune`.
    pub fn fast_prune(self, enabled: bool) -> Self {
        self.arg(
            ChainOption::FastPrune,
            if enabled {
                "1"
            } else {
                "0"
            },
        )
    }

    /// `-testactivationheight=<name>@<height>` for `deployment`.
    ///
    /// A deployment the option cannot move gives Core's invalid-name error.
    pub fn activation_height(self, deployment: BuriedDeployment, height: BlockHeight) -> Self {
        let name = BuriedDeploymentExt::test_activation_name(deployment)
            .unwrap_or(BuriedDeploymentExt::name(deployment));
        self.arg(ChainOption::TestActivationHeight, format!("{name}@{}", height.to_u32()))
    }

    /// `-dip3params=<activation>:<enforcement>`.
    pub fn dip3(self, activation: BlockHeight, enforcement: BlockHeight) -> Self {
        let value = format!("{}:{}", activation.to_u32(), enforcement.to_u32());
        self.arg(ChainOption::Dip3Params, value)
    }

    /// `-budgetparams=<masternode>:<budget>:<superblock>`.
    pub fn budget(
        self,
        masternode: BlockHeight,
        budget: BlockHeight,
        superblock: BlockHeight,
    ) -> Self {
        let value = format!("{}:{}:{}", masternode.to_u32(), budget.to_u32(), superblock.to_u32());
        self.arg(ChainOption::BudgetParams, value)
    }

    /// `-powtargetspacing=<seconds>`.
    pub fn pow_target_spacing(self, seconds: i64) -> Self {
        self.arg(ChainOption::PowTargetSpacing, format!("{seconds}"))
    }

    /// The chain parameters with the overrides applied.
    ///
    /// # Errors
    ///
    /// Core's refusal of an override, or one of the inputs this crate
    /// refuses where Core accepts or asserts (see [`ParamsError`]).
    ///
    /// The phases follow Core's startup: argument parsing (an option the
    /// release lacks or a negation Core forbids, in argument order), the
    /// chain params, then the spork signers. An LLMQ size
    /// override outside `u16` is refused last, once Core's own checks have
    /// passed (quirk 46).
    pub fn build(&self) -> Result<ChainSetup, ParamsError> {
        for (option, value) in &self.args {
            if *option == ChainOption::MinSporkKeys {
                return Err(ParamsError::InvalidParameter(match value.as_deref() {
                    None => String::from("-nominsporkkeys"),
                    Some("") => String::from("-minsporkkeys"),
                    Some(v) => format!("-minsporkkeys={v}"),
                }));
            }
            if value.is_none() && !option.negatable() {
                return Err(ParamsError::NegationForbidden(*option));
            }
        }
        let mut setup = ChainSetup::new(self.network, self.release);
        let mut out_of_range = None;
        match self.network {
            Network::Main | Network::Testnet3 => {}
            Network::Devnet => self.devnet(&mut setup, &mut out_of_range)?,
            Network::Regtest => self.regtest(&mut setup, &mut out_of_range)?,
        }
        setup.sync_chain();
        setup.spork_signers = self.spork_signers(&setup)?;
        match out_of_range {
            Some(err) => Err(err),
            None => Ok(setup),
        }
    }

    /// `-sporkaddr` as Core's `AppInitMain` applies it (v24 init.cpp:1746-1752).
    fn spork_signers(&self, setup: &ChainSetup) -> Result<SporkSigners, ParamsError> {
        let key = |a: &str| {
            spork_key_id(a, setup.addr_params.pubkey_addr, setup.platform_hrp)
                .ok_or(ParamsError::SporkAddress)
        };
        let address = self.last(ChainOption::SporkAddr).unwrap_or(setup.spork_address);
        let warning = (self.all(ChainOption::SporkAddr).count() > 1)
            .then(|| format!("More than one spork address is provided. Using {address}."));
        Ok(SporkSigners {
            key_ids: vec![key(address)?],
            min_spork_keys: None,
            warning,
        })
    }

    /// The last setting of `option` (Core's `GetArg`); a negation reads `0`.
    fn last(&self, option: ChainOption) -> Option<&str> {
        let (_, value) = self.args.iter().rev().find(|(o, _)| *o == option)?;
        Some(value.as_deref().unwrap_or("0"))
    }

    /// The values of `option` after its last negation, in order (Core's
    /// `GetArgs` on command-line settings).
    fn all(&self, option: ChainOption) -> impl Iterator<Item = &str> {
        let settings: Vec<&Option<String>> =
            self.args.iter().filter(|(o, _)| *o == option).map(|(_, v)| v).collect();
        let live = settings.iter().rposition(|v| v.is_none()).map_or(0, |i| i + 1);
        settings.into_iter().skip(live).filter_map(|v| v.as_deref())
    }

    /// Core's `CDevNetParams` constructor.
    fn devnet(
        &self,
        setup: &mut ChainSetup,
        out_of_range: &mut Option<ParamsError>,
    ) -> Result<(), ParamsError> {
        if let Some(name) = self.last(ChainOption::Devnet).filter(|n| !n.is_empty()) {
            setup.name_devnet(format!("devnet-{name}"))?;
        }
        let c = &mut setup.consensus;
        // UpdateDevnetSubsidyAndDiffParametersFromArgs
        if let Some(v) = self.last(ChainOption::MinimumDifficultyBlocks) {
            c.minimum_difficulty_blocks = int_arg(v);
        }
        if let Some(v) = self.last(ChainOption::HighSubsidyBlocks) {
            c.high_subsidy_blocks = int_arg(v);
        }
        if let Some(v) = self.last(ChainOption::HighSubsidyFactor) {
            c.high_subsidy_factor = int_arg(v);
        }
        if let Some(t) = self.llmq_role(setup, ChainOption::LlmqChainLocks, Rotation::Forbidden)? {
            setup.consensus.llmq_type_chain_locks = t;
        }
        if let Some(t) =
            self.llmq_role(setup, ChainOption::LlmqInstantSendDip0024, Rotation::Required)?
        {
            setup.consensus.llmq_type_dip0024_instant_send = t;
        }
        if let Some(t) = self.llmq_role(setup, ChainOption::LlmqPlatform, Rotation::Any)? {
            setup.consensus.llmq_type_platform = t;
        }
        if let Some(t) = self.llmq_role(setup, ChainOption::LlmqMnhf, Rotation::Any)? {
            setup.consensus.llmq_type_mnhf = t;
        }
        self.llmq_size(
            setup,
            out_of_range,
            ChainOption::LlmqDevnetParams,
            "LLMQ_DEVNET",
            LlmqType::LlmqDevnet,
        )?;
        if let Some(v) = self.last(ChainOption::PowTargetSpacing) {
            let spacing = int64(v, ParamsError::PowTargetSpacingParse)?;
            if spacing < 1 {
                return Err(ParamsError::PowTargetSpacingValue(v.into()));
            }
            setup.consensus.pow_target_spacing = spacing;
        }
        Ok(())
    }

    /// Core's `CRegTestParams` constructor.
    ///
    /// Heights Core parses into an `int` stay signed while the options are
    /// replayed, so a later occurrence can replace a negative one. A height
    /// still negative in the final parameters is refused, after every check
    /// Core itself makes, so Core's errors keep precedence (BL-31).
    fn regtest(
        &self,
        setup: &mut ChainSetup,
        out_of_range: &mut Option<ParamsError>,
    ) -> Result<(), ParamsError> {
        if self.last(ChainOption::FastPrune).is_some_and(interpret_bool) {
            setup.policy = ChainPolicy::new(setup.network, true);
        }
        self.activation_heights(setup)?;
        let mut min_heights: Vec<(DeploymentPos, i32)> = setup
            .deployments()
            .map(|(pos, d)| {
                (pos, i32::try_from(d.min_activation_height.to_u32()).unwrap_or(i32::MAX))
            })
            .collect();
        self.version_bits(setup, &mut min_heights)?;
        let mut dip3 = None;
        if let Some(v) = self.last(ChainOption::Dip3Params) {
            let [activation, enforcement] = fields(v).ok_or(ParamsError::Dip3Malformed)?;
            let activation = int32(activation, ParamsError::Dip3Activation)?;
            let enforcement = int32(enforcement, ParamsError::Dip3Enforcement)?;
            dip3 = Some([activation, enforcement]);
        }
        let mut budget_starts = None;
        if let Some(v) = self.last(ChainOption::BudgetParams) {
            let [mn, budget, superblock] = fields(v).ok_or(ParamsError::BudgetMalformed)?;
            let mn = int32(mn, ParamsError::BudgetMasternode)?;
            let budget = int32(budget, ParamsError::BudgetBudget)?;
            let superblock = int32(superblock, ParamsError::BudgetSuperblock)?;
            budget_starts = Some([mn, budget, superblock]);
        }
        self.llmq_size(
            setup,
            out_of_range,
            ChainOption::LlmqTestParams,
            "LLMQ_TEST",
            LlmqType::LlmqTest,
        )?;
        self.llmq_size(
            setup,
            out_of_range,
            ChainOption::LlmqTestInstantSendParams,
            "LLMQ_TEST_INSTANTSEND",
            LlmqType::LlmqTestInstantsend,
        )?;
        self.llmq_size(
            setup,
            out_of_range,
            ChainOption::LlmqTestPlatformParams,
            "LLMQ_TEST_PLATFORM",
            LlmqType::LlmqTestPlatform,
        )?;
        if let Some(t) =
            self.llmq_role(setup, ChainOption::LlmqTestInstantSendDip0024, Rotation::Any)?
        {
            setup.consensus.llmq_type_dip0024_instant_send = t;
        }
        for (pos, h) in min_heights {
            if let Some(d) = setup.consensus.deployments.deployment_mut(pos) {
                d.min_activation_height = height(ChainOption::VbParams, h)?;
            }
        }
        let c = &mut setup.consensus;
        if let Some([activation, enforcement]) = dip3 {
            c.dip0003_height = height(ChainOption::Dip3Params, activation)?;
            c.dip0003_enforcement.0 = height(ChainOption::Dip3Params, enforcement)?;
        }
        if let Some([mn, budget, superblock]) = budget_starts {
            c.masternode_payments_start_block = height(ChainOption::BudgetParams, mn)?;
            c.budget_payments_start_block = height(ChainOption::BudgetParams, budget)?;
            c.superblock_start.0 = height(ChainOption::BudgetParams, superblock)?;
        }
        if c.v20_height < c.dip0003_height {
            return Err(ParamsError::V20BelowDip3);
        }
        if c.mn_rr_height < c.v20_height {
            return Err(ParamsError::MnRrBelowV20);
        }
        Ok(())
    }

    /// Core's `MaybeUpdateHeights` (`-testactivationheight`).
    fn activation_heights(&self, setup: &mut ChainSetup) -> Result<(), ParamsError> {
        for arg in self.all(ChainOption::TestActivationHeight) {
            let (name, value) = arg
                .split_once('@')
                .ok_or_else(|| ParamsError::ActivationHeightFormat(arg.into()))?;
            let h = parse_i32(value)
                .filter(|h| (0..i32::MAX).contains(h))
                .and_then(|h| u32::try_from(h).ok())
                .map(BlockHeight::from_u32)
                .ok_or_else(|| ParamsError::ActivationHeightValue(arg.into()))?;
            let deployment = self
                .release
                .buried_deployments()
                .iter()
                .copied()
                .find(|&d| BuriedDeploymentExt::test_activation_name(d) == Some(name))
                .ok_or_else(|| ParamsError::ActivationHeightName(arg.into()))?;
            let c = &mut setup.consensus;
            match deployment {
                BuriedDeployment::Bip147 => c.bip147_height = h,
                BuriedDeployment::HeightInCoinbase => c.bip34.0 = h,
                BuriedDeployment::DerSig => c.bip66_height = h,
                BuriedDeployment::Cltv => c.bip65_height = h,
                BuriedDeployment::Csv => c.csv_height = h,
                BuriedDeployment::Brr => c.brr_height = h,
                BuriedDeployment::Dip0001 => c.dip0001_height = h,
                BuriedDeployment::Dip0008 => c.dip0008_height = h,
                BuriedDeployment::Dip0024 => {
                    c.dip0024_height = h;
                    c.dip0024_quorums_height = h;
                }
                BuriedDeployment::V19 => c.v19_height = h,
                BuriedDeployment::V20 => c.v20_height = h,
                BuriedDeployment::MnRr => c.mn_rr_height = h,
                BuriedDeployment::Ipv4Optional => c.ipv4_optional_height = h,
                BuriedDeployment::Dip0003
                | BuriedDeployment::Dip0020
                | BuriedDeployment::Withdrawals => {}
            }
        }
        Ok(())
    }

    /// Core's `-vbparams` loop in `UpdateActivationParametersFromArgs`.
    ///
    /// Minimum activation heights go to `min_heights`, one per deployment, as
    /// Core's signed `int`.
    fn version_bits(
        &self,
        setup: &mut ChainSetup,
        min_heights: &mut [(DeploymentPos, i32)],
    ) -> Result<(), ParamsError> {
        for arg in self.all(ChainOption::VbParams) {
            let f: Vec<&str> = arg.split(':').collect();
            if !matches!(f.len(), 3 | 4 | 6 | 9) {
                return Err(ParamsError::VbParamsMalformed);
            }
            let field = |field| move |v| ParamsError::VbParamsField(field, v);
            let start = int64(f[1], field(VbField::StartTime))?;
            let timeout = int64(f[2], field(VbField::Timeout))?;
            let min_height = if f.len() >= 4 {
                int32(f[3], field(VbField::MinActivationHeight))?
            } else {
                0
            };
            let (window, threshold_start) = if f.len() >= 6 {
                (
                    int64(f[4], field(VbField::WindowSize))?,
                    int64(f[5], field(VbField::ThresholdStart))?,
                )
            } else {
                (-1, -1)
            };
            let (threshold_min, falloff, use_ehf) = if f.len() == 9 {
                (
                    int64(f[6], field(VbField::ThresholdMin))?,
                    int64(f[7], field(VbField::FalloffCoeff))?,
                    int64(f[8], field(VbField::UseEhf))?,
                )
            } else {
                (-1, -1, -1)
            };
            let (pos, min) = min_heights
                .iter_mut()
                .find(|(pos, _)| pos.name() == f[0])
                .ok_or_else(|| ParamsError::VbParamsDeployment(f[0].into()))?;
            let Some(d) = setup.consensus.deployments.deployment_mut(*pos) else {
                return Err(ParamsError::VbParamsDeployment(f[0].into()));
            };
            // Core's UpdateVersionBitsParameters; -1 leaves a field unchanged.
            d.start_time = start;
            d.timeout = timeout;
            *min = min_height;
            if window != -1 {
                d.window_size = window;
            }
            if threshold_start != -1 {
                d.threshold_start = threshold_start;
            }
            if threshold_min != -1 {
                d.threshold_min = threshold_min;
            }
            if falloff != -1 {
                d.falloff_coeff = falloff;
            }
            if use_ehf != -1 {
                d.use_ehf = use_ehf > 0;
            }
        }
        Ok(())
    }

    /// An LLMQ `<size>:<threshold>` override of `llmq_type` (Core's
    /// `UpdateLLMQTestParametersFromArgs` and
    /// `UpdateLLMQDevnetParametersFromArgs`).
    ///
    /// Every chain that reads the option configures `llmq_type`. A size or
    /// threshold outside `u16`, which Core's `int` takes, is recorded in
    /// `out_of_range` for [`build`](Self::build) to refuse once Core's own
    /// checks have passed (quirk 46); the first one recorded is kept.
    fn llmq_size(
        &self,
        setup: &mut ChainSetup,
        out_of_range: &mut Option<ParamsError>,
        option: ChainOption,
        core_name: &'static str,
        llmq_type: LlmqType,
    ) -> Result<(), ParamsError> {
        let Some(v) = self.last(option) else {
            return Ok(());
        };
        let [size, threshold] = fields(v).ok_or(ParamsError::LlmqParamsMalformed(core_name))?;
        let size = int32(size, |v| ParamsError::LlmqSize(core_name, v))?;
        let threshold = int32(threshold, |v| ParamsError::LlmqThreshold(core_name, v))?;
        if size > i32::from(MAX_LLMQ_SIZE) {
            return Err(ParamsError::LlmqSizeTooLarge(core_name, size));
        }
        let (Ok(size_u16), Ok(threshold_u16)) = (u16::try_from(size), u16::try_from(threshold))
        else {
            out_of_range.get_or_insert(ParamsError::LlmqOutOfRange(core_name, size, threshold));
            return Ok(());
        };
        if let Some(q) = setup.consensus.llmq_mut(llmq_type) {
            q.size = size_u16;
            q.min_size = threshold_u16;
            q.threshold = threshold_u16;
            q.dkg_bad_votes_threshold = threshold_u16;
        }
        Ok(())
    }

    /// The configured LLMQ type an LLMQ role option names, if it is set.
    ///
    /// The name is looked up over the chain's configured types only, as Core
    /// looks it up in `consensus.llmqs`.
    fn llmq_role(
        &self,
        setup: &ChainSetup,
        option: ChainOption,
        rotation: Rotation,
    ) -> Result<Option<LlmqType>, ParamsError> {
        let Some(name) = self.last(option) else {
            return Ok(None);
        };
        let mut found = None;
        for (llmq_type, q) in setup.consensus.llmq_params() {
            if llmq_type.to_string() != name {
                continue;
            }
            if matches!(
                (rotation, q.use_rotation),
                (Rotation::Required, false) | (Rotation::Forbidden, true)
            ) {
                return Err(ParamsError::LlmqRotation(option));
            }
            found = Some(llmq_type);
        }
        found.map(Some).ok_or(ParamsError::LlmqTypeName(option))
    }
}

/// `value` of `option` as a height; a negative one is refused (BL-31).
fn height(option: ChainOption, value: i32) -> Result<BlockHeight, ParamsError> {
    u32::try_from(value)
        .map(BlockHeight::from_u32)
        .map_err(|_| ParamsError::NegativeHeight(option, i64::from(value)))
}

/// Core's `SplitString(value, ':')` when it has exactly `N` fields.
fn fields<const N: usize>(value: &str) -> Option<[&str; N]> {
    let parts: Vec<&str> = value.split(':').collect();
    parts.try_into().ok()
}

/// Core's `ParseInt32` of `v`, or the error `err` names with `v`.
fn int32(v: &str, err: impl FnOnce(String) -> ParamsError) -> Result<i32, ParamsError> {
    parse_i32(v).ok_or_else(|| err(v.into()))
}

/// Core's `ParseInt64` of `v`, or the error `err` names with `v`.
fn int64(v: &str, err: impl FnOnce(String) -> ParamsError) -> Result<i64, ParamsError> {
    parse_i64(v).ok_or_else(|| err(v.into()))
}
