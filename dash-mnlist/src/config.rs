//! `LlmqConfig`: one client's LLMQ parameters and the constants derived from them (SB0 §4.3.1,
//! §5.2.1, §5.3; DEC-109 (5)).

use std::fmt;
use std::sync::Arc;

use dash_consensus::BlockId;
use dash_consensus::ctx::{Deployment, DeploymentView, HeaderCtx};
use dash_evo::BlsCtx;
use dash_evo::llmq::{LlmqChain, LlmqCtx, ParamsOutOfDomain, SIGN_HEIGHT_OFFSET, WORK_DIFF_DEPTH};
use dash_params::{BuriedDeployment, ChainParams, LlmqParams};
use dash_primitives::LlmqType;
use dash_types::Numeric;

/// The roles SPV uses a type for (SB0 §5.2 step 5). A type in none of them is `Skipped`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Roles {
    /// `llmqTypeChainLocks`.
    pub chain_locks: bool,
    /// `llmqTypeDIP0024InstantSend`.
    pub instant_send: bool,
    /// `llmqTypePlatform`.
    pub platform: bool,
}

/// A configuration the engine cannot run with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// A role names a type the chain parameters do not configure.
    #[error("the {role} type {llmq_type} is not configured")]
    UnknownType {
        /// The role.
        role: &'static str,
        /// The type's number.
        llmq_type: u8,
    },
    /// A configured type is outside LQ1's parameter domain (`LlmqCtx::new`).
    #[error("LLMQ type {0} has parameters outside the supported domain")]
    OutOfDomain(u8),
    /// The chain parameters' target spacing is not positive.
    #[error("the target spacing {0} is not positive")]
    Spacing(i64),
    /// The rotated type's cycle is no longer than `WORK_DIFF_DEPTH`: a cycle's work block would
    /// not lie inside the cycle before, as `qrinfo`'s construction assumes.
    #[error("the rotated type {0}'s dkgInterval is not above the work-block depth")]
    RotationInterval(u8),
    /// The served (Platform) type's commitments can be mined and replaced within W_P: the
    /// lookup's two targets h − W_P and h would miss one (SB0 §5.3 the lifetime invariant,
    /// DEC-115, DEC-116).
    #[error("the Platform type {llmq_type}'s minimum lifetime {lifetime} is not above W_P {w_p}")]
    Lifetime {
        /// The served type's number.
        llmq_type: u8,
        /// Its minimum lifetime in blocks, [`min_lifetime`].
        lifetime: i64,
        /// W_P.
        w_p: u32,
    },
}

/// One client's LLMQ parameters: the chain parameters, the used types and the derived constants
/// (SB0 §5.3). Every LQ1 call builds its own `LlmqCtx` from it.
#[derive(Clone)]
pub struct LlmqConfig {
    params: Arc<ChainParams>,
    l_est: u32,
    h_keep: u32,
    w_p: u32,
    a_q: u32,
    dip3: u32,
}

impl fmt::Debug for LlmqConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LlmqConfig")
            .field("l_est", &self.l_est)
            .field("h_keep", &self.h_keep)
            .field("w_p", &self.w_p)
            .field("a_q", &self.a_q)
            .finish_non_exhaustive()
    }
}

/// One hour of relay delay, the margin M of H_keep (SB0 §5.2.1).
const RELAY_MARGIN_SECS: i64 = 3_600;

/// T_fresh + T_empty, the age a Platform proof's ChainLock height can reach (SB0 §5.3, W_P).
const PLATFORM_LAG_SECS: i64 = 34 * 60;

impl LlmqConfig {
    /// The configuration for `params`: the CL, IS and Platform types it names, each configured,
    /// every configured type inside LQ1's domain.
    pub fn new(params: Arc<ChainParams>) -> Result<Self, ConfigError> {
        let consensus = &params.consensus;
        let roles = [
            ("ChainLocks", consensus.llmq_type_chain_locks),
            ("InstantSend", consensus.llmq_type_dip0024_instant_send),
            ("Platform", consensus.llmq_type_platform),
        ];
        let mut used = Vec::with_capacity(roles.len());
        for (role, llmq_type) in roles {
            let llmq = consensus.llmq(llmq_type).ok_or(ConfigError::UnknownType {
                role,
                llmq_type: llmq_type.to_base(),
            })?;
            used.push(llmq);
        }
        // `LlmqCtx::new` checks the domain of every configured type; run it once here so a call
        // never meets `ParamsOutOfDomain`.
        LlmqCtx::new(&params, &NoChain, &NoDeployments, BlsCtx::from_legacy(false))
            .map_err(|ParamsOutOfDomain(t)| ConfigError::OutOfDomain(t.to_base()))?;
        if interval(used[1]) <= WORK_DIFF_DEPTH {
            return Err(ConfigError::RotationInterval(used[1].llmq_type.to_base()));
        }
        let spacing = consensus.pow_target_spacing;
        if spacing <= 0 {
            return Err(ConfigError::Spacing(spacing));
        }
        let blocks = |secs: i64| u32::try_from((secs + spacing - 1) / spacing).unwrap_or(u32::MAX);
        let margin = blocks(RELAY_MARGIN_SECS);
        let w_p = blocks(PLATFORM_LAG_SECS).saturating_add(margin);
        let c_max = interval(used[0]).max(interval(used[1]));
        let h_keep = c_max.saturating_add(SIGN_HEIGHT_OFFSET).saturating_add(margin);
        let lifetime = min_lifetime(used[2]);
        if lifetime <= i64::from(w_p) {
            return Err(ConfigError::Lifetime {
                llmq_type: used[2].llmq_type.to_base(),
                lifetime,
                w_p,
            });
        }
        let l_est = used.iter().map(|p| reach(p)).max().unwrap_or(0);
        let dip3 = consensus.deployment_height(BuriedDeployment::Dip0003).to_u32();
        let a_q = dip3.max(consensus.deployment_height(BuriedDeployment::Dip0008).to_u32());
        Ok(LlmqConfig {
            params,
            l_est,
            h_keep,
            w_p,
            a_q,
            dip3,
        })
    }

    /// The chain parameters.
    pub fn params(&self) -> &ChainParams {
        &self.params
    }

    /// The shared chain parameters.
    pub fn params_arc(&self) -> &Arc<ChainParams> {
        &self.params
    }

    /// The roles SPV uses `llmq_type` for; empty for a `Skipped` type (SB0 §5.2 step 5). By
    /// configured use, not by type number: a type that also fills an unused role (testnet's MNHF
    /// type is its CL type) is used.
    pub fn roles(&self, llmq_type: LlmqType) -> Roles {
        let c = &self.params.consensus;
        Roles {
            chain_locks: c.llmq_type_chain_locks == llmq_type,
            instant_send: c.llmq_type_dip0024_instant_send == llmq_type,
            platform: c.llmq_type_platform == llmq_type,
        }
    }

    /// Whether SPV uses `llmq_type` (verifies its commitments).
    pub fn is_used(&self, llmq_type: LlmqType) -> bool {
        self.roles(llmq_type) != Roles::default()
    }

    /// L_est, the regular-DKG sizing estimate (SB0 §4.3.1): the largest reach of a used type,
    /// 5C + W + 16 rotated and A·C + W + 16 otherwise.
    pub fn l_est(&self) -> u32 {
        self.l_est
    }

    /// H_keep = C_max + `SIGN_HEIGHT_OFFSET` + ⌈3600 s / spacing⌉ (SB0 §5.2.1).
    pub fn h_keep(&self) -> u32 {
        self.h_keep
    }

    /// W_P = ⌈34 min / spacing⌉ + M, the Platform-lookup window (SB0 §5.3).
    pub fn w_p(&self) -> u32 {
        self.w_p
    }

    /// R = max(H_keep, 2·W_P): a replaced commitment's row stays while it was replaced at or
    /// above tip − R (SB0 §5.3, D31).
    pub fn r(&self) -> u32 {
        self.h_keep.max(self.w_p.saturating_mul(2))
    }

    /// A_q = max(`DIP0003Height`, `DIP0008Height`), the first height whose CbTx carries
    /// `merkleRootQuorums` (SB0 §5.2.1).
    pub fn a_q(&self) -> u32 {
        self.a_q
    }

    /// `DIP0003Height`: below it no commitment can be mined (SB0 §5.2.1).
    pub fn dip3_height(&self) -> u32 {
        self.dip3
    }

    /// L_est + H_keep: the history dash-chain's anchor candidate rule needs (SB0 §4.3.1 step 1).
    pub fn history_blocks(&self) -> u32 {
        self.l_est.saturating_add(self.h_keep)
    }
}

/// `dkgInterval` as a height distance (positive: `LlmqCtx::new` checked the domain).
pub(crate) fn interval(p: &LlmqParams) -> u32 {
    p.dkg_interval.unsigned_abs().max(1)
}

/// The fewest blocks between a commitment's mining and its replacement (SB0 §5.3, DEC-115):
/// interval × cycles active − the mining window's width. A rotated type keeps the last mined
/// commitment of each index (`llmq/blockprocessor.cpp` rc.3:818-832), so one cycle; a successor
/// can be mined earlier in its window than its predecessor was (rc.3:311-323).
pub fn min_lifetime(p: &LlmqParams) -> i64 {
    let cycles = if p.use_rotation {
        1
    } else {
        i64::from(p.signing_active_quorum_count)
    };
    let width = i64::from(p.dkg_mining_window_end) - i64::from(p.dkg_mining_window_start);
    i64::from(p.dkg_interval) * cycles - width
}

/// A type's regular-DKG reach L below the tip (SB0 §4.3.1): 5C + W + 16 rotated, A·C + W + 16
/// otherwise.
fn reach(p: &LlmqParams) -> u32 {
    let c = interval(p);
    let w = p.dkg_mining_window_end.unsigned_abs();
    let cycles = if p.use_rotation {
        5
    } else {
        p.signing_active_quorum_count.unsigned_abs()
    };
    cycles.saturating_mul(c).saturating_add(w).saturating_add(16)
}

/// The chain the one-off domain check is built with; never read.
struct NoChain;

impl LlmqChain for NoChain {
    type Error = ();
    fn header(&self, _: BlockId) -> Result<HeaderCtx, ()> {
        Err(())
    }
    fn ancestor(&self, _: BlockId, _: u32) -> Result<Option<BlockId>, ()> {
        Err(())
    }
    fn prev(&self, _: BlockId) -> Result<Option<BlockId>, ()> {
        Err(())
    }
}

/// The deployments the one-off domain check is built with; never read.
struct NoDeployments;

impl DeploymentView for NoDeployments {
    fn active_after(&self, _: Option<BlockId>, _: Deployment) -> bool {
        false
    }
    fn active_at(&self, _: BlockId, _: Deployment) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use dash_params::{ChainOption, ChainParamsBuilder, NetworkKind, Release};

    use super::*;

    fn config(network: NetworkKind) -> LlmqConfig {
        let params = ChainParamsBuilder::new(network, Release::V24_0).build().expect("params");
        LlmqConfig::new(Arc::new(params)).expect("a valid configuration")
    }

    /// SB0 §4.3.1's L_est table, §5.2.1's H_keep table and A_q values, and §5.3's W_P and R.
    #[test]
    fn the_derived_constants_match_sb0s_tables() {
        for (network, l_est, h_keep, a_q, r) in [
            (NetworkKind::Mainnet, 1_506, 320, 1_088_640, 320),
            (NetworkKind::Testnet, 1_506, 320, 78_800, 320),
            (NetworkKind::Devnet, 276, 80, 2, 80),
            (NetworkKind::Regtest, 156, 56, 432, 76),
        ] {
            let c = config(network);
            assert_eq!((c.l_est(), c.h_keep(), c.a_q()), (l_est, h_keep, a_q), "{network:?}");
            assert_eq!((c.w_p(), c.r()), (38, r), "{network:?}");
            assert_eq!(c.history_blocks(), l_est + h_keep, "{network:?}");
        }
    }

    /// Mainnet's MNHF type is not in a used role: `Skipped`. Testnet's MNHF type is its CL type:
    /// used.
    #[test]
    fn use_is_by_configured_role() {
        let mainnet = config(NetworkKind::Mainnet);
        let mnhf = mainnet.params().consensus.llmq_type_mnhf;
        assert!(!mainnet.is_used(mnhf));
        assert!(mainnet.is_used(mainnet.params().consensus.llmq_type_chain_locks));
        let testnet = config(NetworkKind::Testnet);
        let mnhf = testnet.params().consensus.llmq_type_mnhf;
        assert!(testnet.roles(mnhf).chain_locks);
        assert!(testnet.is_used(mnhf));
    }

    /// SB0 §5.3's lifetime table: each network's served (Platform) type outlives W_P = 38; a
    /// devnet whose 30 s spacing makes W_P 188 against its type's 88 is refused at load (DEC-116),
    /// and a Platform type that lives one block longer than W_P loads.
    #[test]
    fn the_served_types_outlive_w_p() {
        for (network, lifetime) in [
            (NetworkKind::Mainnet, 568),
            (NetworkKind::Testnet, 568),
            (NetworkKind::Devnet, 88),
            (NetworkKind::Regtest, 40),
        ] {
            let c = config(network);
            let platform = c.params().consensus.llmq(c.params().consensus.llmq_type_platform);
            assert_eq!(min_lifetime(platform.expect("set")), lifetime, "{network:?}");
        }
        let fast = ChainParamsBuilder::new(NetworkKind::Devnet, Release::V24_0)
            .arg(ChainOption::PowTargetSpacing, "30")
            .build()
            .expect("params");
        let platform = fast.consensus.llmq_type_platform.to_base();
        assert_eq!(
            LlmqConfig::new(Arc::new(fast)).map(|_| ()),
            Err(ConfigError::Lifetime {
                llmq_type: platform,
                lifetime: 88,
                w_p: 188,
            })
        );
        let with_window = |start: i32| {
            let mut params = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
                .build()
                .expect("params");
            let served = params.consensus.llmq_type_platform;
            let lp = params.consensus.llmqs.iter_mut().find(|p| p.llmq_type == served);
            lp.expect("set").dkg_mining_window_start = start;
            LlmqConfig::new(Arc::new(params)).map(|_| ())
        };
        // Regtest's window is [10, 18]: 48 − 8 = 40; a start of 8 leaves 38, of 9 leaves 39.
        assert!(matches!(
            with_window(8),
            Err(ConfigError::Lifetime {
                lifetime: 38,
                ..
            })
        ));
        assert_eq!(with_window(9), Ok(()));
    }

    /// A rotated type whose cycle is no longer than the work-block depth is refused at load; one
    /// block longer loads.
    #[test]
    fn a_rotated_cycle_within_the_work_block_depth_is_refused() {
        let with_interval = |dkg_interval: i32| {
            let mut params = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
                .build()
                .expect("params");
            let rotated = params.consensus.llmq_type_dip0024_instant_send;
            let lp =
                params.consensus.llmqs.iter_mut().find(|p| p.llmq_type == rotated).expect("set");
            lp.dkg_interval = dkg_interval;
            LlmqConfig::new(Arc::new(params)).map(|_| ())
        };
        let depth = i32::try_from(WORK_DIFF_DEPTH).expect("small");
        assert_eq!(with_interval(depth), Err(ConfigError::RotationInterval(103)));
        assert_eq!(with_interval(depth + 1), Ok(()));
    }
}
