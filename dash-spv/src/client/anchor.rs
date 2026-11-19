//! Initial anchor selection and coverage transitions, without storage or networking.

use std::sync::Arc;
use std::time::Duration;

use dash_chain::{
    anchor_candidate, anchor_fallback, Anchor, AnchorPack, AnchorState, ChainMeta, ClPersisted,
    CoverageMiss, CHAIN_SCHEMA,
};
use dash_consensus::BlockId;
use dash_mnlist::{Coverage, LlmqConfig, RequestKey};
use dash_primitives::BlockHash;

use super::ClientConfig;
use crate::error::SpvError;

pub const COVERAGE_ATTEMPTS: u32 = 3;
pub const COVERAGE_RETRY_DELAY: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Unopened,
    Candidate {
        anchor: Anchor,
        attempt: u32,
    },
    Coverage {
        anchor: Anchor,
    },
    Established,
}

#[derive(Clone, Debug)]
pub enum Input {
    Opened(Option<ChainMeta>),
    HeadersCaughtUp(BlockHash),
    Coverage(Coverage),
    FetchFailed(RequestKey),
    /// The host sends this after the requested retry delay.
    RetryTick,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output {
    /// The host durably resets the store before building the new chain.
    Reset(ChainMeta),
    BuildChain(Anchor),
    SyncHeaders,
    /// Each attempt in a retry round must use different peers.
    RunCoverage {
        attempt: u32,
    },
    SendCoverageRequests,
    Establish,
    StartSubscribers,
    Fallback(Anchor),
    RetryAfter(Duration),
    Diagnostic(String),
}

pub struct InitialSync {
    phase: Phase,
    packs: Vec<AnchorPack>,
    llmq: Arc<LlmqConfig>,
    config: ClientConfig,
    birth: Option<u32>,
    tip: Option<BlockHash>,
    attempts: u32,
    waiting: bool,
}

impl InitialSync {
    /// Creates an unwired driver. `birth` is the host's resolved wallet birth height.
    pub fn new(
        config: ClientConfig,
        llmq: Arc<LlmqConfig>,
        packs: Vec<AnchorPack>,
        birth: Option<u32>,
    ) -> Self {
        Self {
            phase: Phase::Unopened,
            packs,
            llmq,
            config,
            birth,
            tip: None,
            attempts: 0,
            waiting: false,
        }
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    pub fn advance(&mut self, input: Input) -> Result<Vec<Output>, SpvError> {
        if let Input::Opened(meta) = input {
            if self.phase != Phase::Unopened {
                return Err(SpvError::Config("initial sync was already opened".into()));
            }
            let mut out = Vec::new();
            let anchor = match meta {
                Some(meta) => {
                    let anchor = self.stored_anchor(&meta)?;
                    if meta.anchor_state == AnchorState::Established {
                        self.phase = Phase::Established;
                        return Ok(vec![
                            Output::BuildChain(anchor),
                            Output::StartSubscribers,
                            Output::SyncHeaders,
                        ]);
                    }
                    anchor
                }
                None => {
                    let anchor = anchor_candidate(
                        &self.packs,
                        self.llmq.params(),
                        self.config.start_from_height.or(self.birth),
                        self.config.clock.node_now(),
                        self.llmq.history_blocks(),
                    );
                    out.push(Output::Reset(self.candidate_meta(&anchor)));
                    anchor
                }
            };
            self.phase = Phase::Candidate {
                anchor: anchor.clone(),
                attempt: 0,
            };
            out.push(Output::BuildChain(anchor));
            if self.config.enable_masternodes {
                out.push(Output::SyncHeaders);
            } else {
                self.phase = Phase::Established;
                out.extend([Output::Establish, Output::StartSubscribers, Output::SyncHeaders]);
            }
            return Ok(out);
        }
        let anchor = match &self.phase {
            Phase::Candidate {
                anchor,
                ..
            }
            | Phase::Coverage {
                anchor,
            } => anchor.clone(),
            Phase::Established => return Ok(Vec::new()),
            Phase::Unopened => return Err(SpvError::Config("initial sync is not open".into())),
        };
        match input {
            Input::HeadersCaughtUp(tip) => {
                if self.tip != Some(tip) || matches!(self.phase, Phase::Candidate { .. }) {
                    self.tip = Some(tip);
                    self.attempts = 1;
                    self.waiting = false;
                    self.phase = Phase::Coverage {
                        anchor,
                    };
                    Ok(vec![Output::RunCoverage {
                        attempt: 1,
                    }])
                } else {
                    Ok(Vec::new())
                }
            }
            Input::Coverage(coverage)
                if matches!(self.phase, Phase::Coverage { .. }) && !self.waiting =>
            {
                match coverage {
                    Coverage::Covered => {
                        self.phase = Phase::Established;
                        Ok(vec![Output::Establish, Output::StartSubscribers])
                    }
                    Coverage::Pending => Ok(vec![Output::SendCoverageRequests]),
                    Coverage::Below {
                        lowest,
                    } => {
                        let miss =
                            lowest.map_or(CoverageMiss::UnknownBase, CoverageMiss::LowestUncovered);
                        let next = anchor_fallback(&self.packs, self.llmq.params(), &anchor, miss);
                        if next == anchor {
                            return Ok(vec![Output::Diagnostic(
                                "coverage is below genesis; waiting for headers".into(),
                            )]);
                        }
                        self.phase = Phase::Candidate {
                            anchor: next.clone(),
                            attempt: 0,
                        };
                        self.tip = None;
                        self.attempts = 0;
                        Ok(vec![
                            Output::Fallback(next.clone()),
                            Output::Reset(self.candidate_meta(&next)),
                            Output::BuildChain(next),
                            Output::SyncHeaders,
                        ])
                    }
                }
            }
            Input::FetchFailed(key)
                if matches!(self.phase, Phase::Coverage { .. }) && !self.waiting =>
            {
                if self.attempts < COVERAGE_ATTEMPTS {
                    self.attempts += 1;
                    Ok(vec![Output::RunCoverage {
                        attempt: self.attempts,
                    }])
                } else {
                    self.waiting = true;
                    Ok(vec![
                        Output::Diagnostic(format!(
                            "coverage fetch {key:?} failed after {} attempts",
                            self.attempts
                        )),
                        Output::RetryAfter(COVERAGE_RETRY_DELAY),
                    ])
                }
            }
            Input::RetryTick if self.waiting => {
                self.waiting = false;
                self.attempts = 1;
                Ok(vec![Output::RunCoverage {
                    attempt: 1,
                }])
            }
            _ => Ok(Vec::new()),
        }
    }

    fn candidate_meta(&self, anchor: &Anchor) -> ChainMeta {
        let (anchor, root) = match anchor {
            Anchor::Genesis => {
                ((self.llmq.params().consensus.hash_genesis_block.into(), 0), BlockId(0))
            }
            Anchor::Checkpoint {
                hash,
                height,
                window,
                ..
            } => ((*hash, *height), BlockId(window.len() as u32 - 1)),
        };
        ChainMeta {
            schema: CHAIN_SCHEMA,
            anchor,
            anchor_state: AnchorState::Candidate,
            next_id: BlockId(0),
            tip: root,
            best_header: root,
            sync_seq: 0,
            cl: ClPersisted {
                best: None,
                floor: None,
                enabled_observed: false,
            },
        }
    }

    fn stored_anchor(&self, meta: &ChainMeta) -> Result<Anchor, SpvError> {
        if meta.anchor == (self.llmq.params().consensus.hash_genesis_block.into(), 0) {
            return Ok(Anchor::Genesis);
        }
        let pack =
            self.packs.iter().find(|p| (p.hash, p.height) == meta.anchor).ok_or_else(|| {
                SpvError::Config("stored anchor has no matching anchor pack".into())
            })?;
        Ok(Anchor::Checkpoint {
            height: pack.height,
            hash: pack.hash,
            chain_work: pack.chain_work,
            window: Box::new(pack.headers),
        })
    }
}

#[cfg(test)]
mod tests {
    use dash_chain::{ClPersisted, Work, CHAIN_SCHEMA};
    use dash_clock::MockClock;
    use dash_primitives::BlockHeader;
    use dashcore::Network;

    use super::*;
    use crate::client::llmq::llmq_config;
    use crate::client::DevnetConfig;

    fn driver(mn: bool, birth: Option<u32>) -> InitialSync {
        configured_driver(mn, birth, None, true)
    }

    fn configured_driver(
        mn: bool,
        birth: Option<u32>,
        start: Option<u32>,
        with_packs: bool,
    ) -> InitialSync {
        let mut config =
            ClientConfig::new(Network::Devnet).with_devnet(DevnetConfig::new("sb15-driver"));
        config.enable_masternodes = mn;
        config.start_from_height = start;
        config.clock = Arc::new(MockClock::at(2_000_000));
        let llmq = llmq_config(&config).unwrap();
        let packs = [10000, 20000, 30000]
            .map(|height| {
                let mut header = BlockHeader::default();
                header.set_time(if height == 30000 {
                    2_000_000
                } else {
                    1_000_000
                });
                AnchorPack {
                    height,
                    hash: BlockHash::from([u8::try_from(height / 10000).unwrap(); 32]),
                    chain_work: Work::from(u64::from(height)),
                    headers: [header; 25],
                }
            })
            .to_vec();
        assert!(packs
            .iter()
            .all(|pack| pack.height >= llmq.params().consensus.pow_dgw_height.to_u32() + 24));
        InitialSync::new(
            config,
            llmq,
            if with_packs {
                packs
            } else {
                vec![]
            },
            birth,
        )
    }

    fn meta(anchor: &Anchor, state: AnchorState, llmq: &LlmqConfig) -> ChainMeta {
        ChainMeta {
            schema: CHAIN_SCHEMA,
            anchor: match anchor {
                Anchor::Genesis => (llmq.params().consensus.hash_genesis_block.into(), 0),
                Anchor::Checkpoint {
                    hash,
                    height,
                    ..
                } => (*hash, *height),
            },
            anchor_state: state,
            next_id: BlockId(99),
            tip: BlockId(98),
            best_header: BlockId(98),
            sync_seq: 7,
            cl: ClPersisted {
                best: None,
                floor: None,
                enabled_observed: false,
            },
        }
    }

    fn open(driver: &mut InitialSync) -> Anchor {
        let out = driver.advance(Input::Opened(None)).unwrap();
        let Output::BuildChain(anchor) = &out[1] else {
            panic!("{out:?}")
        };
        assert_eq!(
            out,
            vec![
                Output::Reset(driver.candidate_meta(anchor)),
                Output::BuildChain(anchor.clone()),
                Output::SyncHeaders
            ]
        );
        anchor.clone()
    }

    #[test]
    fn candidate_uses_birth_clock_and_genesis_without_packs() {
        let mut d = driver(true, None);
        let a = open(&mut d);
        assert!(matches!(
            a,
            Anchor::Checkpoint {
                height: 20000,
                ..
            }
        ));
        assert!(d.advance(Input::Coverage(Coverage::Covered)).unwrap().is_empty());
        assert!(matches!(d.phase(), Phase::Candidate { .. }));
        let mut d = driver(true, Some(15000));
        assert!(matches!(
            open(&mut d),
            Anchor::Checkpoint {
                height: 10000,
                ..
            }
        ));
        let mut d = driver(true, Some(5000));
        assert_eq!(open(&mut d), Anchor::Genesis);
        let mut d = configured_driver(true, None, None, false);
        assert_eq!(open(&mut d), Anchor::Genesis);
        let mut d = configured_driver(true, Some(25000), Some(15000), true);
        assert!(matches!(
            open(&mut d),
            Anchor::Checkpoint {
                height: 10000,
                ..
            }
        ));
    }

    #[test]
    fn stored_candidate_resumes_and_established_never_moves() {
        for state in [AnchorState::Candidate, AnchorState::Established] {
            let mut original = driver(true, None);
            let anchor = open(&mut original);
            let stored = meta(&anchor, state, &original.llmq);
            let mut resumed = driver(true, Some(1));
            let out = resumed.advance(Input::Opened(Some(stored))).unwrap();
            let expected = if state == AnchorState::Candidate {
                vec![Output::BuildChain(anchor.clone()), Output::SyncHeaders]
            } else {
                vec![
                    Output::BuildChain(anchor.clone()),
                    Output::StartSubscribers,
                    Output::SyncHeaders,
                ]
            };
            assert_eq!(out, expected);
            if state == AnchorState::Established {
                assert_eq!(resumed.phase(), &Phase::Established);
                assert!(resumed
                    .advance(Input::Coverage(Coverage::Below {
                        lowest: None
                    }))
                    .unwrap()
                    .is_empty());
            }
            assert!(resumed.advance(Input::Opened(None)).is_err());
        }
        let mut d = driver(true, None);
        let mut stored = meta(&Anchor::Genesis, AnchorState::Candidate, &d.llmq);
        stored.anchor = (BlockHash::from([99; 32]), 999);
        assert!(d.advance(Input::Opened(Some(stored))).is_err());
        assert_eq!(d.phase(), &Phase::Unopened);
    }

    #[test]
    fn coverage_pending_then_covered_establishes_once() {
        let mut d = driver(true, None);
        let anchor = open(&mut d);
        assert_eq!(
            d.advance(Input::HeadersCaughtUp(BlockHash::from([8; 32]))).unwrap(),
            vec![Output::RunCoverage {
                attempt: 1
            }]
        );
        assert_eq!(
            d.phase(),
            &Phase::Coverage {
                anchor
            }
        );
        assert_eq!(
            d.advance(Input::Coverage(Coverage::Pending)).unwrap(),
            vec![Output::SendCoverageRequests]
        );
        assert_eq!(
            d.advance(Input::Coverage(Coverage::Covered)).unwrap(),
            vec![Output::Establish, Output::StartSubscribers]
        );
        assert_eq!(d.phase(), &Phase::Established);
        assert!(d.advance(Input::Coverage(Coverage::Covered)).unwrap().is_empty());
    }

    #[test]
    fn below_and_unknown_base_fall_back_and_resume_after_reset() {
        for lowest in [Some(15000), None] {
            let mut d = driver(true, None);
            open(&mut d);
            d.advance(Input::HeadersCaughtUp(BlockHash::from([8; 32]))).unwrap();
            let out = d
                .advance(Input::Coverage(Coverage::Below {
                    lowest,
                }))
                .unwrap();
            let Output::Fallback(anchor) = &out[0] else {
                panic!("{out:?}")
            };
            assert!(matches!(
                anchor,
                Anchor::Checkpoint {
                    height: 10000,
                    ..
                }
            ));
            assert_eq!(
                out,
                vec![
                    Output::Fallback(anchor.clone()),
                    Output::Reset(d.candidate_meta(anchor)),
                    Output::BuildChain(anchor.clone()),
                    Output::SyncHeaders
                ]
            );
            let stored = meta(anchor, AnchorState::Candidate, &d.llmq);
            let mut reopened = driver(true, None);
            assert_eq!(
                reopened.advance(Input::Opened(Some(stored))).unwrap(),
                vec![Output::BuildChain(anchor.clone()), Output::SyncHeaders]
            );
            d.advance(Input::HeadersCaughtUp(BlockHash::from([8; 32]))).unwrap();
            let out = d
                .advance(Input::Coverage(Coverage::Below {
                    lowest: None,
                }))
                .unwrap();
            assert_eq!(out[0], Output::Fallback(Anchor::Genesis));
        }
        let mut d = driver(true, None);
        open(&mut d);
        d.advance(Input::HeadersCaughtUp(BlockHash::from([8; 32]))).unwrap();
        let out = d
            .advance(Input::Coverage(Coverage::Below {
                lowest: Some(5000),
            }))
            .unwrap();
        assert_eq!(out[0], Output::Fallback(Anchor::Genesis));
    }

    #[test]
    fn failed_fetches_retry_with_a_bound_and_never_fall_back() {
        let mut d = driver(true, None);
        open(&mut d);
        let tip = BlockHash::from([8; 32]);
        d.advance(Input::HeadersCaughtUp(tip)).unwrap();
        let failed = || Input::FetchFailed(RequestKey::QrInfo(BlockHash::from([9; 32])));
        for attempt in 2..=3 {
            assert_eq!(
                d.advance(failed()).unwrap(),
                vec![Output::RunCoverage {
                    attempt
                }]
            );
        }
        let out = d.advance(failed()).unwrap();
        assert!(matches!(
            &out[..],
            [Output::Diagnostic(_), Output::RetryAfter(COVERAGE_RETRY_DELAY)]
        ));
        assert!(d.advance(failed()).unwrap().is_empty());
        assert!(d.advance(Input::HeadersCaughtUp(tip)).unwrap().is_empty());
        assert_eq!(
            d.advance(Input::RetryTick).unwrap(),
            vec![Output::RunCoverage {
                attempt: 1
            }]
        );
        assert_eq!(
            d.advance(Input::HeadersCaughtUp(BlockHash::from([10; 32]))).unwrap(),
            vec![Output::RunCoverage {
                attempt: 1
            }]
        );
        assert!(matches!(
            d.phase(),
            Phase::Coverage {
                anchor: Anchor::Checkpoint {
                    height: 20000,
                    ..
                }
            }
        ));
    }

    #[test]
    fn mn_off_establishes_before_header_sync() {
        let mut d = driver(false, None);
        let out = d.advance(Input::Opened(None)).unwrap();
        assert!(matches!(
            &out[..],
            [
                Output::Reset(_),
                Output::BuildChain(_),
                Output::Establish,
                Output::StartSubscribers,
                Output::SyncHeaders
            ]
        ));
        assert_eq!(d.phase(), &Phase::Established);
    }
}
