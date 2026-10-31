//! The chain parameters the scan classifiers read, transcribed from Dash Core `v24.0.0-rc.4`
//! (`src/chainparams.cpp`). They are kept here, apart from `dash-params`, so the scan does not
//! lean on a library it also cross-checks (`tests::table_matches_dash_params`).

/// A network the scan covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Net {
    Mainnet,
    Testnet,
}

impl Net {
    pub fn name(self) -> &'static str {
        match self {
            Net::Mainnet => "mainnet",
            Net::Testnet => "testnet",
        }
    }
}

/// `LLMQType` values (`src/llmq/params.h:19-27`).
pub mod llmq {
    pub const LLMQ_50_60: u8 = 1;
    pub const LLMQ_400_60: u8 = 2;
    pub const LLMQ_400_85: u8 = 3;
    pub const LLMQ_100_67: u8 = 4;
    pub const LLMQ_60_75: u8 = 5;
    pub const LLMQ_25_67: u8 = 6;

    pub fn name(ty: u8) -> &'static str {
        match ty {
            LLMQ_50_60 => "LLMQ_50_60",
            LLMQ_400_60 => "LLMQ_400_60",
            LLMQ_400_85 => "LLMQ_400_85",
            LLMQ_100_67 => "LLMQ_100_67",
            LLMQ_60_75 => "LLMQ_60_75",
            LLMQ_25_67 => "LLMQ_25_67",
            _ => "unknown",
        }
    }
}

/// `TESTNET_LLMQ_25_67_ACTIVATION_HEIGHT` (`src/validation.cpp:6666`).
const TESTNET_LLMQ_25_67_ACTIVATION_HEIGHT: u32 = 847_000;

/// `VERSIONBITS_NUM_BITS` (`src/versionbits.h:22`): `MNHFTxPayload::IsTriviallyValid` rejects
/// every signal bit at or above it (`src/evo/mnhftx.cpp:95-103`).
pub const VERSIONBITS_NUM_BITS: u8 = 29;

/// `BIP9Deployment::NEVER_ACTIVE` and `::NO_TIMEOUT` (`src/consensus/params.h:80-91`).
pub const NEVER_ACTIVE: i64 = -2;
pub const NO_TIMEOUT: i64 = i64::MAX;

/// The fields of a `BIP9Deployment` that `CChainParams::IsValidMNActivation` reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Deployment {
    pub name: &'static str,
    pub bit: u8,
    pub start_time: i64,
    pub timeout: i64,
    /// `useEHF`.
    pub ehf: bool,
}

/// `vDeployments` of mainnet (`chainparams.cpp:207-228`): the TESTDUMMY deployment is the only
/// one that is not an EHF deployment.
const MAIN_DEPLOYMENTS: &[Deployment] = &[
    Deployment {
        name: "testdummy",
        bit: 28,
        start_time: NEVER_ACTIVE,
        timeout: NO_TIMEOUT,
        ehf: false,
    },
    Deployment {
        name: "v24",
        bit: 13,
        start_time: NEVER_ACTIVE,
        timeout: NO_TIMEOUT,
        ehf: true,
    },
    Deployment {
        name: "evo_shares",
        bit: 14,
        start_time: NEVER_ACTIVE,
        timeout: NO_TIMEOUT,
        ehf: true,
    },
];
/// `vDeployments` of testnet (`chainparams.cpp:416-437`).
const TEST_DEPLOYMENTS: &[Deployment] = &[
    Deployment {
        name: "testdummy",
        bit: 28,
        start_time: NEVER_ACTIVE,
        timeout: NO_TIMEOUT,
        ehf: false,
    },
    Deployment {
        name: "v24",
        bit: 13,
        start_time: 1_790_208_000,
        timeout: NO_TIMEOUT,
        ehf: true,
    },
    Deployment {
        name: "evo_shares",
        bit: 14,
        start_time: 1_790_208_000,
        timeout: NO_TIMEOUT,
        ehf: true,
    },
];

/// The verdict of `IsValidMNActivation`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activation {
    Valid,
    Invalid,
    /// The verdict depends on the median time past, which the caller did not have.
    Undecided,
}

/// `CChainParams::IsValidMNActivation(nBit, timePast)` (`chainparams.cpp:109-130`) over
/// `deployments`. Without `time_past` the verdict is evaluated at every time where some
/// deployment's window opens or closes (and at both ends of the 32-bit time range), and stands
/// only if they agree. `ehf_inverted` is the mutant that flips the `useEHF` test.
pub fn activation_in(
    deployments: &[Deployment],
    bit: u8,
    time_past: Option<i64>,
    ehf_inverted: bool,
) -> Activation {
    let verdict = |t: i64| -> bool {
        for d in deployments.iter().filter(|d| d.bit == bit) {
            if t > d.timeout || t < d.start_time {
                continue;
            }
            return d.ehf != ehf_inverted;
        }
        // an unknown bit is allowed
        true
    };
    let verdicts: Vec<bool> = match time_past {
        Some(t) => vec![verdict(t)],
        None => {
            let max = i64::from(u32::MAX);
            let mut times = vec![0, max];
            for d in deployments.iter().filter(|d| d.bit == bit) {
                for t in [d.start_time - 1, d.start_time, d.timeout, d.timeout.saturating_add(1)] {
                    times.push(t.clamp(0, max));
                }
            }
            times.into_iter().map(verdict).collect()
        }
    };
    if verdicts.iter().all(|v| *v) {
        Activation::Valid
    } else if verdicts.iter().all(|v| !*v) {
        Activation::Invalid
    } else {
        Activation::Undecided
    }
}

/// The consensus parameters of one network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetParams {
    pub net: Net,
    /// `DIP0003Height`.
    pub dip3: u32,
    /// `DIP0008Height`.
    pub dip8: u32,
    /// `DIP0020Height`.
    pub dip20: u32,
    /// `DIP0024Height`.
    pub dip24: u32,
    /// `DIP0024QuorumsHeight`.
    pub dip24_quorums: u32,
    /// `V20Height`.
    pub v20: u32,
    /// `nBudgetPaymentsStartBlock`.
    pub budget_start: u32,
    /// `nBudgetPaymentsCycleBlocks`.
    pub budget_cycle: u32,
    /// `nBudgetPaymentsWindowBlocks`.
    pub budget_window: u32,
    /// `nSuperblockStartBlock`.
    pub superblock_start: u32,
    /// `nSuperblockCycle`.
    pub superblock_cycle: u32,
    /// The LLMQ types `AddLLMQ`ed for the network (`chainparams.cpp:272-280`, `:465-474`).
    pub llmqs: &'static [u8],
    /// `vDeployments`.
    pub deployments: &'static [Deployment],
}

const MAIN_LLMQS: &[u8] =
    &[llmq::LLMQ_50_60, llmq::LLMQ_60_75, llmq::LLMQ_400_60, llmq::LLMQ_400_85, llmq::LLMQ_100_67];
const TEST_LLMQS: &[u8] = &[
    llmq::LLMQ_50_60,
    llmq::LLMQ_60_75,
    llmq::LLMQ_400_60,
    llmq::LLMQ_400_85,
    llmq::LLMQ_100_67,
    llmq::LLMQ_25_67,
];

impl NetParams {
    pub fn of(net: Net) -> Self {
        match net {
            // chainparams.cpp:168-194
            Net::Mainnet => Self {
                net,
                dip3: 1_028_160,
                dip8: 1_088_640,
                dip20: 1_516_032,
                dip24: 1_737_792,
                dip24_quorums: 1_738_698,
                v20: 1_987_776,
                budget_start: 328_008,
                budget_cycle: 16_616,
                budget_window: 100,
                superblock_start: 614_820,
                superblock_cycle: 16_616,
                llmqs: MAIN_LLMQS,
                deployments: MAIN_DEPLOYMENTS,
            },
            // chainparams.cpp:377-403
            Net::Testnet => Self {
                net,
                dip3: 7_000,
                dip8: 78_800,
                dip20: 414_100,
                dip24: 769_700,
                dip24_quorums: 770_730,
                v20: 905_100,
                budget_start: 4_100,
                budget_cycle: 50,
                budget_window: 10,
                superblock_start: 4_200,
                superblock_cycle: 24,
                llmqs: TEST_LLMQS,
                deployments: TEST_DEPLOYMENTS,
            },
        }
    }

    /// `IsValidMNActivation(bit, timePast)` for the network.
    pub fn mn_activation(&self, bit: u8, time_past: Option<i64>, ehf_inverted: bool) -> Activation {
        activation_in(self.deployments, bit, time_past, ehf_inverted)
    }

    /// `DeploymentActiveAfter(pindexPrev, DIP0024)` for a height-activated deployment.
    fn dip24_active_after(&self, prev_height: u32) -> bool {
        prev_height + 1 >= self.dip24
    }

    /// Whether `ty` is a configured type of the network.
    pub fn is_configured(&self, ty: u8) -> bool {
        self.llmqs.contains(&ty)
    }

    /// `ChainstateManager::IsQuorumTypeEnabled` for the mainnet/testnet types, evaluated on the
    /// previous block (`src/validation.cpp:6664-6715`). `None` for a type the network does not
    /// configure. `strict_quorums_height` is the `fHaveDIP0024Quorums` comparison, `>=` in Core.
    pub fn quorum_type_enabled(
        &self,
        ty: u8,
        prev_height: u32,
        strict_quorums_height: bool,
    ) -> Option<bool> {
        if !self.is_configured(ty) {
            return None;
        }
        let have_dip24_quorums = if strict_quorums_height {
            prev_height > self.dip24_quorums
        } else {
            prev_height >= self.dip24_quorums
        };
        Some(match ty {
            llmq::LLMQ_50_60 => {
                !self.dip24_active_after(prev_height)
                    || !have_dip24_quorums
                    || self.net == Net::Testnet
            }
            llmq::LLMQ_400_60 | llmq::LLMQ_400_85 => true,
            llmq::LLMQ_100_67 => prev_height + 1 >= self.dip20,
            llmq::LLMQ_60_75 => self.dip24_active_after(prev_height),
            llmq::LLMQ_25_67 => prev_height >= TESTNET_LLMQ_25_67_ACTIVATION_HEIGHT,
            _ => return None,
        })
    }

    /// `h` is in a window the old budget system leaves unchecked under `NoCheck`
    /// (`src/masternode/payments.cpp:262-287`, `:313-329`). `inclusive_window` is the `<=`
    /// variant the window mutant uses.
    pub fn in_old_budget_window(&self, h: u32, inclusive_window: bool) -> bool {
        if h < self.budget_start || h >= self.superblock_start {
            return false;
        }
        let offset = h % self.budget_cycle;
        if inclusive_window {
            offset <= self.budget_window
        } else {
            offset < self.budget_window
        }
    }

    /// `CSuperblock::IsValidBlockHeight`.
    pub fn is_superblock(&self, h: u32) -> bool {
        h >= self.superblock_start && h.is_multiple_of(self.superblock_cycle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dep(bit: u8, start_time: i64, timeout: i64, ehf: bool) -> Deployment {
        Deployment {
            name: "t",
            bit,
            start_time,
            timeout,
            ehf,
        }
    }

    #[test]
    fn activation_follows_is_valid_mn_activation() {
        let ds = [dep(5, 100, 200, false), dep(6, 100, 200, true)];
        // inside the window of a non-EHF deployment: rejected
        assert_eq!(activation_in(&ds, 5, Some(150), false), Activation::Invalid);
        // the window bounds are inclusive on both sides
        assert_eq!(activation_in(&ds, 5, Some(100), false), Activation::Invalid);
        assert_eq!(activation_in(&ds, 5, Some(200), false), Activation::Invalid);
        // outside it the loop `continue`s and the unknown bit is allowed
        assert_eq!(activation_in(&ds, 5, Some(99), false), Activation::Valid);
        assert_eq!(activation_in(&ds, 5, Some(201), false), Activation::Valid);
        // an EHF deployment is valid inside and outside its window
        assert_eq!(activation_in(&ds, 6, Some(150), false), Activation::Valid);
        assert_eq!(activation_in(&ds, 6, Some(300), false), Activation::Valid);
        // a bit nobody deploys
        assert_eq!(activation_in(&ds, 7, Some(150), false), Activation::Valid);
        // without the time the verdict stands only if both window states agree
        assert_eq!(activation_in(&ds, 5, None, false), Activation::Undecided);
        assert_eq!(activation_in(&ds, 6, None, false), Activation::Valid);
        assert_eq!(activation_in(&ds, 7, None, false), Activation::Valid);
        // the inverted-EHF mutant differs
        assert_eq!(activation_in(&ds, 6, Some(150), true), Activation::Invalid);
    }

    #[test]
    fn only_testdummy_is_rejected_on_both_networks() {
        for net in [Net::Mainnet, Net::Testnet] {
            let p = NetParams::of(net);
            for bit in 0..VERSIONBITS_NUM_BITS {
                let want = if bit == 28 {
                    Activation::Invalid
                } else {
                    Activation::Valid
                };
                assert_eq!(p.mn_activation(bit, None, false), want, "{net:?} bit {bit}");
                for t in [0, 1_790_207_999, 1_790_208_000, 1_800_000_000] {
                    assert_eq!(p.mn_activation(bit, Some(t), false), want, "{net:?} bit {bit} {t}");
                }
            }
        }
    }

    #[test]
    fn mainnet_50_60_is_disabled_from_1738699() {
        let p = NetParams::of(Net::Mainnet);
        // block h = prev + 1
        assert_eq!(p.quorum_type_enabled(llmq::LLMQ_50_60, 1_738_697, false), Some(true));
        assert_eq!(p.quorum_type_enabled(llmq::LLMQ_50_60, 1_738_698, false), Some(false));
        assert_eq!(p.quorum_type_enabled(llmq::LLMQ_50_60, 1_738_699, false), Some(false));
    }

    #[test]
    fn enable_boundaries() {
        let m = NetParams::of(Net::Mainnet);
        assert_eq!(m.quorum_type_enabled(llmq::LLMQ_100_67, m.dip20 - 2, false), Some(false));
        assert_eq!(m.quorum_type_enabled(llmq::LLMQ_100_67, m.dip20 - 1, false), Some(true));
        assert_eq!(m.quorum_type_enabled(llmq::LLMQ_60_75, m.dip24 - 2, false), Some(false));
        assert_eq!(m.quorum_type_enabled(llmq::LLMQ_60_75, m.dip24 - 1, false), Some(true));
        assert_eq!(m.quorum_type_enabled(llmq::LLMQ_25_67, 847_000, false), None);
        let t = NetParams::of(Net::Testnet);
        assert_eq!(t.quorum_type_enabled(llmq::LLMQ_25_67, 846_999, false), Some(false));
        assert_eq!(t.quorum_type_enabled(llmq::LLMQ_25_67, 847_000, false), Some(true));
        // testnet keeps 50_60 enabled for good
        assert_eq!(t.quorum_type_enabled(llmq::LLMQ_50_60, 2_000_000, false), Some(true));
    }

    #[test]
    fn budget_windows() {
        let m = NetParams::of(Net::Mainnet);
        assert!(m.in_old_budget_window(332_320, false));
        assert!(m.in_old_budget_window(332_419, false));
        assert!(!m.in_old_budget_window(332_420, false));
        assert!(!m.in_old_budget_window(614_820, false), "superblocks have started");
        assert!(!m.in_old_budget_window(314_000, false), "before the budget start");
        assert!(m.is_superblock(631_408), "the first superblock");
        assert!(!m.is_superblock(614_820), "the start is not itself a superblock");
        assert!(!m.is_superblock(631_409));
        assert!(!m.is_superblock(16_616), "before the start");
    }
}
