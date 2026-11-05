//! The deployment report of `getdeploymentinfo` and of `getblockchaininfo`'s deprecated
//! `softforks` (`DeploymentInfo` in `rpc/blockchain.cpp`), as typed values for the RPC layer.

use dash_params::{BuriedDeployment, DeploymentPos};

use super::{Bip9Stats, Deployments, EhfSignals, ThresholdState, bip9_enabled, buried_enabled};
use crate::ctx::BlockId;

/// The buried deployments in the report's order ("sorted by activation block").
// Core: rpc/blockchain.cpp:1667-1683 DeploymentInfo
pub const BURIED_ORDER: [BuriedDeployment; 15] = [
    BuriedDeployment::HeightInCoinbase,
    BuriedDeployment::DerSig,
    BuriedDeployment::Cltv,
    BuriedDeployment::Bip147,
    BuriedDeployment::Csv,
    BuriedDeployment::Dip0001,
    BuriedDeployment::Dip0003,
    BuriedDeployment::Dip0008,
    BuriedDeployment::Dip0020,
    BuriedDeployment::Dip0024,
    BuriedDeployment::Brr,
    BuriedDeployment::V19,
    BuriedDeployment::V20,
    BuriedDeployment::MnRr,
    BuriedDeployment::Withdrawals,
];

/// The versionbits deployments in the report's order.
// Core: rpc/blockchain.cpp:1686-1689 DeploymentInfo
pub const BIP9_ORDER: [DeploymentPos; 3] =
    [DeploymentPos::V24, DeploymentPos::EvoShares, DeploymentPos::TestDummy];

/// One entry of the report, keyed in the RPC by `DeploymentName`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeploymentInfo {
    /// `"type": "buried"`.
    Buried(BuriedInfo),
    /// `"type": "bip9"`.
    Bip9(Bip9Info),
}

impl DeploymentInfo {
    /// `DeploymentName(dep)`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Buried(b) => b.deployment.name(),
            Self::Bip9(b) => b.pos.name(),
        }
    }
}

/// A buried deployment's entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuriedInfo {
    pub deployment: BuriedDeployment,
    /// `active`: active for the block after the queried one.
    pub active: bool,
    /// `height`: `DeploymentHeight(dep)`.
    pub height: i32,
}

/// A versionbits deployment's entry: the fields of its `bip9` object, then `height` and
/// `active`. Optional fields are absent from the RPC object when `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bip9Info {
    pub pos: DeploymentPos,
    /// `bip9.bit`, while the queried block's state is started or locked in.
    pub bit: Option<i32>,
    pub start_time: i64,
    pub timeout: i64,
    pub ehf: bool,
    /// `bip9.ehf_height`: the deployment's bit in the stage signals of the queried block.
    pub ehf_height: Option<i32>,
    /// `bip9.activation_height`, while locked in: `since + nWindowSize` (the parameter as set,
    /// not the period).
    pub activation_height: Option<i64>,
    pub min_activation_height: i32,
    /// `bip9.status`: the queried block's own state.
    pub status: ThresholdState,
    /// `bip9.since`: `StateSinceHeight` of the queried block's parent.
    pub since: i64,
    /// `bip9.status_next`: the state of the block after the queried one.
    pub status_next: ThresholdState,
    /// `bip9.statistics`, while started or locked in.
    pub statistics: Option<Bip9StatsInfo>,
    /// `bip9.signalling`, while started or locked in: one `#` (signalled) or `-` per block of the
    /// period so far.
    pub signalling: Option<String>,
    /// `height`: `StateSinceHeight` of the queried block, when `status_next` is active.
    pub height: Option<i32>,
    /// `active`: `status_next` is active.
    pub active: bool,
}

/// `bip9.statistics`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bip9StatsInfo {
    pub period: i32,
    pub elapsed: i32,
    pub count: i32,
    /// Absent while locked in.
    pub threshold: Option<i32>,
    /// Absent while locked in.
    pub possible: Option<bool>,
}

impl Deployments<'_> {
    /// `DeploymentInfo(blockindex, GetSignalsStage(blockindex), chainman)`: the entries
    /// `getdeploymentinfo <block>` reports, in its order, for enabled deployments only.
    // Core: rpc/blockchain.cpp:1664-1693 DeploymentInfo; 1733 the stage signals
    pub fn deployment_info(&self, block: BlockId) -> Vec<DeploymentInfo> {
        let ehf_signals = self.ehf_signals_stage(Some(block));
        let buried = BURIED_ORDER
            .into_iter()
            .filter_map(|dep| self.buried_info(block, dep))
            .map(DeploymentInfo::Buried);
        let bip9 = BIP9_ORDER
            .into_iter()
            .filter_map(|pos| self.bip9_info(block, &ehf_signals, pos))
            .map(DeploymentInfo::Bip9);
        buried.chain(bip9).collect()
    }

    // Core: rpc/blockchain.cpp:1445-1458 SoftForkDescPushBack (BuriedDeployment)
    fn buried_info(&self, block: BlockId, dep: BuriedDeployment) -> Option<BuriedInfo> {
        if !buried_enabled(self.params, dep) {
            return None;
        }
        Some(BuriedInfo {
            deployment: dep,
            active: self.buried_active_after(Some(block), dep),
            height: self.buried_height(dep),
        })
    }

    // Core: rpc/blockchain.cpp:1460-1538 SoftForkDescPushBack (DeploymentPos)
    fn bip9_info(
        &self,
        block: BlockId,
        signals: &EhfSignals,
        pos: DeploymentPos,
    ) -> Option<Bip9Info> {
        if !bip9_enabled(self.params, pos) {
            return None;
        }
        let d = self.bip9(pos);
        let parent = self.chain.prev(block);

        let next_state = self.state(Some(block), pos);
        let current_state = self.state(parent, pos);

        let has_signal =
            matches!(current_state, ThresholdState::Started | ThresholdState::LockedIn);

        let since = i64::from(self.state_since_height(parent, pos));
        let activation_height = (current_state == ThresholdState::LockedIn)
            .then(|| since + i64::from(d.window_size as i32));

        let (statistics, signalling) = if has_signal {
            let mut blocks = Vec::new();
            let stats: Bip9Stats = self.statistics(Some(block), pos, Some(&mut blocks));
            let locked_in = current_state == ThresholdState::LockedIn;
            let info = Bip9StatsInfo {
                period: stats.period,
                elapsed: stats.elapsed,
                count: stats.count,
                threshold: (!locked_in).then_some(stats.threshold),
                possible: (!locked_in).then_some(stats.possible),
            };
            let string = blocks
                .iter()
                .map(|&s| {
                    if s {
                        '#'
                    } else {
                        '-'
                    }
                })
                .collect();
            (Some(info), Some(string))
        } else {
            (None, None)
        };

        let active = next_state == ThresholdState::Active;
        Some(Bip9Info {
            pos,
            bit: has_signal.then_some(d.bit),
            start_time: d.start_time,
            timeout: d.timeout,
            ehf: d.use_ehf,
            ehf_height: signals.get(d.bit as u8),
            activation_height,
            min_activation_height: i32::try_from(d.min_activation_height.to_u32())
                .expect("min_activation_height is Core's int"),
            status: current_state,
            since,
            status_next: next_state,
            statistics,
            signalling,
            height: active.then(|| self.state_since_height(Some(block), pos)),
            active,
        })
    }
}
