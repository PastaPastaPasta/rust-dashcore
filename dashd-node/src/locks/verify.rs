//! Tip-time ChainLock verification (LK1a-2b).

use std::collections::BTreeMap;
use std::sync::Arc;

use dash_dml::StoreError;
use dash_evo::llmq::{MemberError, SIGN_HEIGHT_OFFSET, VerifiedQuorum, select_start_height};
use dash_evo::locks::{
    LockError, LockReject, LockSchemes, QuorumScan, QuorumScanView, QuorumStatus, ScanLookup,
    ScannedQuorum, verify_chainlock,
};
use dash_p2p_core::ClSig;
use dash_params::{ChainSetup, LlmqType};
use dash_primitives::{BlockHash, QuorumHash};
use dashd_chainstate::chain::{ChainHandle, ChainQuery, OnV, OnVMembersError};
use dashd_locks::chainlock::{ClVerdict, ClVerify, Read};

/// Verifies incoming ChainLocks against the active tip and quorum state.
///
/// `verify` blocks waiting for V's FIFO and must never be called from V itself.
#[derive(Clone)]
pub struct NodeClVerify {
    params: Arc<ChainSetup>,
    chain: ChainHandle,
}

impl NodeClVerify {
    /// Creates a verifier for the node's validation actor.
    pub fn new(params: Arc<ChainSetup>, chain: ChainHandle) -> Self {
        Self {
            params,
            chain,
        }
    }
}

impl ClVerify for NodeClVerify {
    fn verify(&self, read: &Read<ClSig>) -> ClVerdict {
        let params = Arc::clone(&self.params);
        let message = read.msg.clone();
        let read_scheme = read.read;
        match self
            .chain
            .on_v(move |on_v, query| {
                let view = match build_view(on_v, query, &params, &message) {
                    Ok(view) => view,
                    Err(verdict) => return verdict,
                };
                let check_scheme = query.bls();
                map_result(verify_chainlock(
                    &params,
                    &view,
                    LockSchemes {
                        read: read_scheme,
                        check: check_scheme,
                    },
                    &message,
                ))
            })
            .recv()
        {
            Ok(verdict) => verdict,
            Err(error) => ClVerdict::Unavailable(error.to_string()),
        }
    }
}

#[cfg(all(test, feature = "test-support"))]
impl NodeClVerify {
    /// The scan `verify` would hold for `message`, `None` with no scan: its rotation flag and its
    /// quorums' base blocks.
    pub(crate) fn scan_of(
        &self,
        message: &ClSig,
    ) -> Result<Option<(bool, Vec<BlockHash>)>, ClVerdict> {
        let params = Arc::clone(&self.params);
        let message = message.clone();
        self.chain
            .on_v(move |on_v, query| {
                let view = build_view(on_v, query, &params, &message)?;
                let mut bases = view.heights;
                bases.remove(&message.block_hash());
                Ok(view
                    .scan
                    .map(|(_, _, scan)| (scan.rotation_enabled, bases.into_keys().collect())))
            })
            .recv()
            .map_err(|error| ClVerdict::Unavailable(error.to_string()))?
    }
}

/// The immutable data collected in the verifier's one `OnV` turn.
struct NodeQuorumScanView {
    tip: u32,
    scan: Option<(LlmqType, u32, QuorumScan)>,
    quorums: BTreeMap<QuorumHash, QuorumStatus>,
    heights: BTreeMap<BlockHash, u32>,
}

impl QuorumScanView for NodeQuorumScanView {
    type Error = StoreError;

    fn tip_height(&self) -> u32 {
        self.tip
    }

    fn scan(&self, llmq_type: LlmqType, start_height: u32) -> Result<ScanLookup, Self::Error> {
        Ok(match &self.scan {
            Some((scan_type, scan_start, scan))
                if *scan_type == llmq_type && *scan_start == start_height =>
            {
                ScanLookup::Held(scan.clone())
            }
            _ => ScanLookup::Unavailable,
        })
    }

    fn scan_at_tip(&self, _: LlmqType, _: usize) -> Result<ScanLookup, Self::Error> {
        Ok(ScanLookup::Unavailable)
    }

    fn block_height(&self, hash: &BlockHash) -> Result<Option<u32>, Self::Error> {
        Ok(self.heights.get(hash).copied())
    }

    fn quorum(&self, _: LlmqType, hash: &QuorumHash) -> Result<Option<QuorumStatus>, Self::Error> {
        Ok(self.quorums.get(hash).cloned())
    }
}

fn build_view(
    on_v: &OnV<'_>,
    query: &ChainQuery<'_>,
    params: &ChainSetup,
    message: &ClSig,
) -> Result<NodeQuorumScanView, ClVerdict> {
    let snapshot = query.chain.snapshot();
    require_tip(snapshot.has_tip(), query.evo_view().tip.is_some())?;
    let tip = snapshot.tip.tip_height;
    let height = message.height();
    let start = scan_start(tip, height);
    let llmq_type = params.consensus.llmq_type_chain_locks;
    let binding = query.bind(on_v);
    let reader = query.chain.reader();
    let mut heights = BTreeMap::new();
    if let Some(id) = reader.lookup(&message.block_hash())
        && let Some(entry) = reader.entry(id)
    {
        heights.insert(message.block_hash(), entry.height);
    }

    let Some(start) = start else {
        return Ok(NodeQuorumScanView {
            tip,
            scan: None,
            quorums: BTreeMap::new(),
            heights,
        });
    };
    let Some(start_id) = snapshot.active.at(start) else {
        return Err(ClVerdict::Corrupt(format!(
            "active chain has no block at ChainLock scan height {start}"
        )));
    };
    let count = usize::from(
        params
            .consensus
            .llmq(llmq_type)
            .ok_or_else(|| {
                ClVerdict::Corrupt(format!("unknown ChainLock LLMQ type {llmq_type:?}"))
            })?
            .signing_active_quorum_count,
    );
    let (scanned, rotation_enabled) = binding
        .scan_quorums_with_rotation(llmq_type, start_id, count)
        .map_err(map_members_error)?;
    let mut entries = Vec::with_capacity(scanned.len());
    let mut quorums = BTreeMap::new();
    for scanned in scanned {
        let hash = scanned.commitment.quorum_hash();
        let quorum =
            binding.quorum(llmq_type, &hash).map_err(map_members_error)?.ok_or_else(|| {
                ClVerdict::Corrupt(format!(
                    "scan returned a quorum absent from the active index: {hash}"
                ))
            })?;
        let verified = VerifiedQuorum::mined(&quorum.commitment);
        let status = QuorumStatus::Verified(verified.clone());
        if let Some(entry) = reader.entry(quorum.base) {
            heights.insert(quorum.base_hash, entry.height);
        }
        quorums.insert(hash, status);
        entries.push(ScannedQuorum::verified(quorum.base, verified));
    }
    Ok(NodeQuorumScanView {
        tip,
        scan: Some((
            llmq_type,
            start,
            QuorumScan {
                rotation_enabled,
                entries,
            },
        )),
        quorums,
        heights,
    })
}

/// With no active tip or no evo tip there is nothing to verify against yet: a local readiness
/// gap, never a peer verdict or a fault.
fn require_tip(has_tip: bool, has_evo_tip: bool) -> Result<(), ClVerdict> {
    if has_tip && has_evo_tip {
        Ok(())
    } else {
        Err(ClVerdict::Unavailable("the active tip is not available".into()))
    }
}

/// The scan start `verify_chainlock` selects from: Core's `SelectQuorumForSigningStartBlock`, a
/// height of -1 being the tip's, with the tip read as `select_verified` reads it, so the view
/// holds the one scan the selection asks for.
fn scan_start(tip: u32, height: i32) -> Option<u32> {
    // Core: llmq/quorumsman.cpp:741-751 at v24.0.0.
    let tip = i32::try_from(tip).unwrap_or(i32::MAX);
    select_start_height(tip, height, SIGN_HEIGHT_OFFSET as i32)
}

fn map_members_error(error: OnVMembersError) -> ClVerdict {
    match error {
        OnVMembersError::Params(error) => ClVerdict::Corrupt(error.to_string()),
        OnVMembersError::Members(error) => match error {
            MemberError::View(error) => map_store_error(error),
            MemberError::Missing(block) => {
                ClVerdict::Corrupt(format!("missing quorum member input at block {}", block.0))
            }
            MemberError::Ehf(error) => ClVerdict::Corrupt(error.to_string()),
            MemberError::UnknownLlmqType(llmq_type) => {
                ClVerdict::Corrupt(format!("unknown LLMQ type {llmq_type}"))
            }
        },
    }
}

fn map_store_error(error: StoreError) -> ClVerdict {
    match error {
        StoreError::NotBuilt(capability) => ClVerdict::Unavailable(capability.to_owned()),
        StoreError::Read {
            what,
        }
        | StoreError::Inconsistent {
            what,
        } => ClVerdict::Corrupt(what.into_owned()),
        StoreError::ListUnavailable(block) => {
            ClVerdict::Corrupt(format!("masternode list unavailable at {block}"))
        }
    }
}

fn map_result(
    result: Result<dash_evo::locks::VerifiedChainLock, LockError<StoreError>>,
) -> ClVerdict {
    // Core: llmq/quorumsman.cpp:814-827 at v24.0.0.
    match result {
        Ok(_) => ClVerdict::Valid,
        Err(LockError::Rejected(reject)) => match reject {
            LockReject::NoQuorum => ClVerdict::NoQuorum,
            LockReject::InvalidSignature | LockReject::MalformedSignature => ClVerdict::Invalid,
            LockReject::QuorumUnavailable | LockReject::OutsideHorizon => {
                debug_assert!(false, "node ChainLock view produced an SPV-only rejection");
                ClVerdict::Unavailable(format!("{reject:?}"))
            }
            LockReject::Malformed | LockReject::UnknownCycle | LockReject::InvalidCycle => {
                debug_assert!(false, "node ChainLock view produced an InstantSend-only rejection");
                ClVerdict::Corrupt(format!("unexpected ChainLock rejection: {reject:?}"))
            }
        },
        Err(LockError::BlsUnsupported) => {
            ClVerdict::Corrupt("ChainLock BLS verification is unsupported".into())
        }
        Err(LockError::UnknownLlmqType(llmq_type)) => {
            ClVerdict::Corrupt(format!("unknown LLMQ type {llmq_type}"))
        }
        Err(LockError::View(error)) => map_store_error(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use dash_clock::{Clock, MockClock};
    use dash_p2p_core::ClSig;
    use dash_pkc::bls::BlsSigBytes;
    use dashd_chainstate::chain::RecordingAbort;

    use crate::chain::Chain;

    #[test]
    fn scan_start_matches_core_bounds() {
        assert_eq!(scan_start(100, 108), Some(100));
        assert_eq!(scan_start(100, 109), None);
        assert_eq!(scan_start(100, 7), None);
        assert_eq!(scan_start(100, -1), Some(92), "-1 signs at the tip");
        assert_eq!(scan_start(100, -2), None);
        assert_eq!(scan_start(100, i32::MIN), None);
    }

    #[test]
    fn local_store_faults_are_corrupt_and_not_peer_verdicts() {
        assert!(matches!(map_store_error(StoreError::NotBuilt("evo")), ClVerdict::Unavailable(_)));
        assert!(matches!(map_store_error(StoreError::read("read")), ClVerdict::Corrupt(_)));
        assert!(matches!(
            map_store_error(StoreError::ListUnavailable(BlockHash::from([1; 32]))),
            ClVerdict::Corrupt(_)
        ));
    }

    #[test]
    fn lock_rejections_have_exhaustive_node_mapping() {
        let reject = |r| map_result(Err(LockError::Rejected(r)));
        assert_eq!(reject(LockReject::NoQuorum), ClVerdict::NoQuorum);
        assert_eq!(reject(LockReject::InvalidSignature), ClVerdict::Invalid);
        assert_eq!(reject(LockReject::MalformedSignature), ClVerdict::Invalid);
        assert!(matches!(map_result(Err(LockError::BlsUnsupported)), ClVerdict::Corrupt(_)));
        assert!(matches!(map_result(Err(LockError::UnknownLlmqType(250))), ClVerdict::Corrupt(_)));
    }

    /// The SPV-only and InstantSend-only rejections cannot come from the node's view: a debug
    /// build stops on each, and a release build maps them as the table does.
    #[test]
    fn view_impossible_rejections_assert_in_debug_and_map_in_release() {
        let cases = [
            (LockReject::QuorumUnavailable, "SPV-only", true),
            (LockReject::OutsideHorizon, "SPV-only", true),
            (LockReject::Malformed, "InstantSend-only", false),
            (LockReject::UnknownCycle, "InstantSend-only", false),
            (LockReject::InvalidCycle, "InstantSend-only", false),
        ];
        for (reject, arm, unavailable) in cases {
            let mapped = std::panic::catch_unwind(|| map_result(Err(LockError::Rejected(reject))));
            if cfg!(debug_assertions) {
                let panic = mapped.expect_err("a debug assert");
                let text = panic.downcast_ref::<&str>().copied().unwrap_or_default();
                assert!(text.contains(arm), "{reject:?}: {text}");
            } else if unavailable {
                assert!(matches!(mapped, Ok(ClVerdict::Unavailable(_))), "{reject:?}");
            } else {
                assert!(matches!(mapped, Ok(ClVerdict::Corrupt(_))), "{reject:?}");
            }
        }
    }

    #[test]
    fn node_view_holds_only_the_computed_scan_start() {
        let view = NodeQuorumScanView {
            tip: 100,
            scan: Some((
                LlmqType::LlmqTest,
                92,
                QuorumScan {
                    rotation_enabled: false,
                    entries: Vec::new(),
                },
            )),
            quorums: BTreeMap::new(),
            heights: BTreeMap::new(),
        };
        assert!(matches!(view.scan(LlmqType::LlmqTest, 92), Ok(ScanLookup::Held(_))));
        assert!(matches!(view.scan(LlmqType::LlmqTest, 93), Ok(ScanLookup::Unavailable)));
    }

    #[test]
    fn no_tip_or_no_evo_tip_is_unavailable() {
        assert_eq!(require_tip(true, true), Ok(()));
        for (tip, evo) in [(false, true), (true, false), (false, false)] {
            assert!(matches!(require_tip(tip, evo), Err(ClVerdict::Unavailable(_))), "{tip} {evo}");
        }
    }

    /// A V with no LLMQ facade drops the `OnV` turn (a chain with no evo layer): the reply is
    /// lost, which `verify` maps to Unavailable, an unscored drop and never a stop.
    #[test]
    fn a_dropped_on_v_turn_is_unavailable_without_abort() {
        let params = crate::chain::tests::regtest();
        let genesis = dashd_chainstate::chain::genesis_block(&params);
        let clock: Arc<dyn Clock> =
            Arc::new(MockClock::at(i64::from(genesis.header().raw().time()) + 86_400));
        let abort = Arc::new(RecordingAbort::default());
        let mut chain = Chain::start(crate::chain::tests::fresh(
            &params,
            &clock,
            &abort,
            Box::new(crate::chain::tests::SharedFiles::default()),
        ))
        .expect("V starts");
        crate::chain::tests::startup(&mut chain);
        crate::chain::tests::imported(&mut chain);

        let verifier = NodeClVerify::new(params, chain.handle().clone());
        let message = ClSig::new(8, genesis.hash(), BlsSigBytes::from([0; 96]));
        let verdict = verifier.verify(&Read {
            msg: message,
            read: dash_consensus::BlsCtx::LEGACY,
        });
        assert_eq!(verdict, ClVerdict::Unavailable("the actor dropped the reply".into()));
        assert!(abort.messages().is_empty(), "a local readiness gap is not fail-stop");
        chain.stop().expect("V stops");
    }

    /// The real handler over the node's mapping of a view error: a `ListUnavailable` list is a
    /// fault, one `abort_node` and no peer score; `NotBuilt` is a readiness gap, no abort and no
    /// score, and the handler goes on taking ChainLocks.
    #[derive(Clone, Default)]
    struct Recorded(Arc<std::sync::Mutex<(Vec<String>, usize)>>);

    impl dashd_locks::chainlock::ClChain for Recorded {
        fn intake(&self) -> Result<i32, dashd_locks::chainlock::Stopped> {
            Ok(-1)
        }

        fn record(
            &self,
            _: Arc<dash_evo::locks::ChainLockSig>,
            _: &[u8; 32],
            _: i64,
        ) -> Result<bool, dashd_locks::chainlock::Stopped> {
            unreachable!("no verdict here is Valid")
        }

        fn reads(&self) -> dashd_locks::chainlock::ClSigsSnapshot {
            dashd_locks::chainlock::ClSigsSnapshot::default()
        }

        fn abort_node(&self, msg: String) {
            self.0.lock().expect("aborts").0.push(msg);
        }
    }

    struct ViewFails(Recorded, fn() -> StoreError);

    impl ClVerify for ViewFails {
        fn verify(&self, _: &Read<ClSig>) -> ClVerdict {
            self.0.0.lock().expect("verifies").1 += 1;
            map_result(Err(LockError::View((self.1)())))
        }
    }

    fn handled(error: fn() -> StoreError) -> (Recorded, Vec<bool>) {
        use dashd_p2p::processing::seams::ChainLocks as _;

        let chain = Recorded::default();
        let mut handler = dashd_locks::chainlock::ChainLockHandler::new(
            chain.clone(),
            ViewFails(chain.clone(), error),
            Arc::new(MockClock::new()),
            || true,
        );
        let scored = (1..=2_u8)
            .map(|n| {
                let message = ClSig::new(i32::from(n), [n; 32].into(), BlsSigBytes::from([0; 96]));
                let read = Read {
                    msg: message,
                    read: dash_consensus::BlsCtx::BASIC,
                };
                let result = handler.process_new_chainlock(7, read, &[n; 32]);
                assert!(result.inventory.is_empty(), "never relayed");
                result.error.is_some()
            })
            .collect();
        (chain, scored)
    }

    #[test]
    fn list_unavailable_aborts_once_per_lock_unscored_and_not_built_continues() {
        let (chain, scored) = handled(|| StoreError::ListUnavailable(BlockHash::from([9; 32])));
        assert_eq!(scored, [false, false], "a local fault never scores the peer");
        let state = chain.0.lock().expect("state");
        assert_eq!(state.1, 2);
        assert_eq!(state.0.len(), 2, "one abort per verified lock: {:?}", state.0);
        assert!(state.0[0].contains("masternode list unavailable"), "{:?}", state.0);
        drop(state);

        let (chain, scored) = handled(|| StoreError::NotBuilt("evo"));
        assert_eq!(scored, [false, false]);
        let state = chain.0.lock().expect("state");
        assert_eq!(state.1, 2, "the handler takes the next lock");
        assert!(state.0.is_empty(), "a readiness gap is not fail-stop");
    }
}
