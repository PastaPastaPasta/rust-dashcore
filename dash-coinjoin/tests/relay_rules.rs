//! The network side: received queues (`dsq`) on a masternode and a client, received DSTXes
//! (`ValidateDSTX`), and the relay rules of Dash Core v24.0.0-rc.3's `net_processing.cpp` that the
//! peer manager (P6a) applies.

mod common;

use std::collections::BTreeMap;

use common::*;
use dash_coinjoin::denom::SMALLEST_DENOMINATION;
use dash_coinjoin::dstx::{
    self, DSTX_PRIORITY_DELTA, DstxChain, DstxManager, DstxScore, DstxValidation,
    can_announce_dstx_to, validate_dstx,
};
use dash_coinjoin::queue::{
    ClientQueueSessions, DsqOutcome, MAX_MIXING_TXES, MasternodeMixing, MixingCounters,
    MixingMasternode, QueueManager, TipMasternodes, client_process_dsq,
};
use dash_coinjoin::relay::{
    DsqRelay, TxInvKind, WantsDsq, announce_inv_kind, dsq_relay, dstx_for_getdata, relay_inv_kind,
    wants_dsq_after_senddsq,
};
use dash_coinjoin::server::ServerSession;
use dash_p2p_core::{DsQueue, DsTx, ProtocolVersion};
use dash_primitives::{OutPoint, TxHash};
use dash_types::Hashable;

/// A masternode and its operator key.
struct Mn {
    op: Operator,
    info: MixingMasternode,
}

impl Mn {
    fn new(seed: u8) -> Self {
        let op = Operator::new(seed);
        let info = MixingMasternode {
            pro_tx_hash: u256(seed),
            collateral: outpoint(u256(0x80 | seed), 1),
            operator_key: op.public(),
        };
        Self {
            op,
            info,
        }
    }

    fn queue(&self, denom: i32, time: i64, ready: bool) -> DsQueue {
        let mut dsq = DsQueue {
            denom,
            pro_tx_hash: self.info.pro_tx_hash,
            time,
            ready,
            sig: Vec::new(),
        };
        dsq.sig = self.op.sign(&dsq.signature_hash());
        dsq
    }

    fn dstx(&self, ins: usize, outs: usize) -> DsTx {
        let inputs = (0..ins).map(|i| txin(outpoint(u256(2), i as u32))).collect();
        let outputs = (0..outs).map(|i| txout(SMALLEST_DENOMINATION, p2pkh(i as u8))).collect();
        let mut d = DsTx {
            tx: tx(inputs, outputs),
            pro_tx_hash: self.info.pro_tx_hash,
            sig: Vec::new(),
            sig_time: NOW,
        };
        d.sig = self.op.sign(&dstx::dstx_signature_hash(&d));
        d
    }
}

/// The tip's masternode list.
struct List {
    mns: Vec<MixingMasternode>,
    enabled: i32,
}

impl TipMasternodes for List {
    fn valid_mn(&self, pro_tx_hash: &TxHash) -> Option<MixingMasternode> {
        self.mns.iter().find(|m| m.pro_tx_hash == *pro_tx_hash).cloned()
    }

    fn enabled_count(&self) -> i32 {
        self.enabled
    }
}

fn list(mns: &[&Mn]) -> List {
    List {
        mns: mns.iter().map(|m| m.info.clone()).collect(),
        enabled: 10,
    }
}

// ---------------------------------------------------------------------------------------------
// CCoinJoinServer::ProcessDSQUEUE (server.cpp:129)
// ---------------------------------------------------------------------------------------------

/// A masternode's session, which keeps the queues it receives (`m_queueman`).
fn server() -> ServerSession<u8> {
    ServerSession::new(mainnet_limits())
}

#[test]
fn server_dsq_accepts_relays_and_counts() {
    let mn = Mn::new(1);
    let mns = list(&[&mn]);
    let mut session = server();
    let mut counters = MixingCounters::default();

    let dsq = mn.queue(SMALLEST, NOW, false);
    let hash = dsq.hash();
    assert_eq!(session.process_dsq(&mut counters, &mns, dsq.clone(), NOW), DsqOutcome::Relay(hash));
    assert_eq!(session.queues().len(), 1);
    assert_eq!(session.queues().queues()[0].masternode_outpoint, mn.info.collateral);
    assert_eq!(counters.dsq_count, 1);
    assert_eq!(
        counters.masternodes[&mn.info.pro_tx_hash],
        MasternodeMixing {
            last_dsq: 1,
            mixing_tx_count: 0
        }
    );

    // A `getdata` for `MSG_DSQ` is answered from the queue manager (`CCoinJoinServer::
    // ProcessGetData`, server.cpp:1305, through net_processing.cpp:3072).
    assert_eq!(session.queues().queue_from_hash(&hash).map(|q| &q.dsq), Some(&dsq));

    // The same masternode and readiness again: a duplicate, not kept and not served.
    let later = mn.queue(SMALLEST, NOW + 1, false);
    let later_hash = later.hash();
    assert_eq!(session.process_dsq(&mut counters, &mns, later, NOW), DsqOutcome::Ignored);
    assert_eq!(session.queues().len(), 1);
    assert!(session.queues().queue_from_hash(&later_hash).is_none());
}

#[test]
fn server_dsq_rejections() {
    let mn = Mn::new(1);
    let stranger = Mn::new(2);
    let mns = list(&[&mn]);
    let run = |dsq: DsQueue| server().process_dsq(&mut MixingCounters::default(), &mns, dsq, NOW);

    assert_eq!(run(mn.queue(3, NOW, false)), DsqOutcome::Misbehaving(10));
    let mut no_mn = mn.queue(SMALLEST, NOW, false);
    no_mn.pro_tx_hash = TxHash::from_lendian([0; 32]);
    assert_eq!(run(no_mn), DsqOutcome::Misbehaving(100));
    assert_eq!(run(stranger.queue(SMALLEST, NOW, false)), DsqOutcome::Misbehaving(10));
    assert_eq!(run(mn.queue(SMALLEST, NOW - 31, false)), DsqOutcome::Ignored);
    let mut forged = mn.queue(SMALLEST, NOW, false);
    forged.sig = stranger.queue(SMALLEST, NOW, false).sig;
    assert_eq!(run(forged), DsqOutcome::Misbehaving(10));
    // A masternode only relays the queues that open sessions.
    assert_eq!(run(mn.queue(SMALLEST, NOW, true)), DsqOutcome::Ignored);
}

/// `IsMixingThresholdExceeded` (meta.cpp:93): a masternode's next queue waits for a fifth of the
/// enabled masternodes' queues. Each queue goes to a fresh session so that only the counters
/// carry over.
#[test]
fn server_dsq_mixing_threshold() {
    let a = Mn::new(1);
    let b = Mn::new(2);
    let mns = List {
        mns: vec![a.info.clone(), b.info.clone()],
        enabled: 10,
    };
    let mut counters = MixingCounters::default();
    let mut run =
        |m: &Mn| server().process_dsq(&mut counters, &mns, m.queue(SMALLEST, NOW, false), NOW);

    assert!(matches!(run(&a), DsqOutcome::Relay(_)));
    // last_dsq 1 + 10 / 5 > 1: too soon.
    assert_eq!(run(&a), DsqOutcome::Ignored);
    assert!(matches!(run(&b), DsqOutcome::Relay(_)));
    // 1 + 2 > 2: still too soon; one more queue elsewhere and it is a's turn.
    assert_eq!(run(&a), DsqOutcome::Ignored);
    counters.dsq_count += 1;
    let mut run =
        |m: &Mn| server().process_dsq(&mut counters, &mns, m.queue(SMALLEST, NOW, false), NOW);
    assert!(matches!(run(&a), DsqOutcome::Relay(_)));

    assert!(!counters.is_mixing_threshold_exceeded(&u256(9), 10));
}

// ---------------------------------------------------------------------------------------------
// CJWalletManagerImpl::ProcessDSQueue (walletman.cpp:243)
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct Sessions {
    waiting_on: Option<TxHash>,
    mixing_on: Option<OutPoint>,
    submitted: Vec<TxHash>,
}

impl ClientQueueSessions for Sessions {
    fn try_submit_denominate(&mut self, pro_tx_hash: &TxHash) -> bool {
        if self.waiting_on == Some(*pro_tx_hash) {
            self.submitted.push(*pro_tx_hash);
            return true;
        }
        false
    }

    fn is_mixing_on(&self, collateral: &OutPoint) -> bool {
        self.mixing_on == Some(*collateral)
    }
}

#[test]
fn client_dsq() {
    let mn = Mn::new(1);
    let mns = list(&[&mn]);
    let mut queues = QueueManager::default();
    let mut counters = MixingCounters::default();
    let mut sessions = Sessions::default();

    // A new queue is added, untried.
    let dsq = mn.queue(SMALLEST, NOW, false);
    assert_eq!(
        client_process_dsq(&mut queues, &mut counters, &mns, &mut sessions, dsq.clone(), NOW),
        DsqOutcome::Added
    );
    assert!(!queues.queues()[0].tried);
    // Again, or another with the same readiness: ignored.
    assert_eq!(
        client_process_dsq(&mut queues, &mut counters, &mns, &mut sessions, dsq, NOW),
        DsqOutcome::Ignored
    );
    let other = mn.queue(SMALLEST, NOW + 1, false);
    assert_eq!(
        client_process_dsq(&mut queues, &mut counters, &mns, &mut sessions, other, NOW),
        DsqOutcome::Ignored
    );

    // The ready queue of the masternode a session waits on: the session submits.
    sessions.waiting_on = Some(mn.info.pro_tx_hash);
    let ready = mn.queue(SMALLEST, NOW, true);
    assert_eq!(
        client_process_dsq(&mut queues, &mut counters, &mns, &mut sessions, ready, NOW),
        DsqOutcome::Submitted
    );
    assert_eq!(sessions.submitted, vec![mn.info.pro_tx_hash]);
    assert_eq!(queues.len(), 1);
}

#[test]
fn client_dsq_marks_a_joined_queue_tried() {
    let mn = Mn::new(1);
    let mns = list(&[&mn]);
    let mut queues = QueueManager::default();
    let mut counters = MixingCounters::default();
    let mut sessions = Sessions {
        mixing_on: Some(mn.info.collateral),
        ..Sessions::default()
    };
    let dsq = mn.queue(SMALLEST, NOW, false);
    assert_eq!(
        client_process_dsq(&mut queues, &mut counters, &mns, &mut sessions, dsq, NOW),
        DsqOutcome::Added
    );
    assert!(queues.queues()[0].tried);
}

#[test]
fn client_dsq_rejections() {
    let mn = Mn::new(1);
    let stranger = Mn::new(2);
    let mns = list(&[&mn]);
    let run = |dsq: DsQueue| {
        client_process_dsq(
            &mut QueueManager::default(),
            &mut MixingCounters::default(),
            &mns,
            &mut Sessions::default(),
            dsq,
            NOW,
        )
    };
    assert_eq!(run(stranger.queue(SMALLEST, NOW, false)), DsqOutcome::Misbehaving(10));
    assert_eq!(run(mn.queue(SMALLEST, NOW + 31, false)), DsqOutcome::Ignored);
    let mut forged = mn.queue(SMALLEST, NOW, false);
    forged.sig = stranger.queue(SMALLEST, NOW, false).sig;
    assert_eq!(run(forged), DsqOutcome::Misbehaving(10));
}

// ---------------------------------------------------------------------------------------------
// ValidateDSTX (net_processing.cpp:3637)
// ---------------------------------------------------------------------------------------------

/// The chain: tip height, V24 activation, and which masternodes each block below the tip lists.
struct Chain24 {
    tip: Option<i32>,
    v24: bool,
    v24_next: bool,
    /// Masternodes listed at each depth below the tip.
    lists: BTreeMap<i32, Vec<MixingMasternode>>,
}

impl Chain24 {
    fn with(mn: &Mn, depths: impl IntoIterator<Item = i32>) -> Self {
        Self {
            tip: Some(1_000),
            v24: false,
            v24_next: false,
            lists: depths.into_iter().map(|d| (d, vec![mn.info.clone()])).collect(),
        }
    }
}

impl DstxChain for Chain24 {
    fn tip_height(&self) -> Option<i32> {
        self.tip
    }

    fn v24_active_at_tip(&self) -> bool {
        self.v24
    }

    fn v24_active_after_tip(&self) -> bool {
        self.v24_next
    }

    fn mn_at_depth(&self, depth: i32, pro_tx_hash: &TxHash) -> Option<MixingMasternode> {
        self.lists.get(&depth)?.iter().find(|m| m.pro_tx_hash == *pro_tx_hash).cloned()
    }
}

#[test]
fn validate_dstx_accepts_and_counts() {
    let mn = Mn::new(1);
    let chain = Chain24::with(&mn, [0]);
    let d = mn.dstx(3, 3);
    let txid = dstx::txid(&d.tx);
    let mut counters = MixingCounters::default();
    let v =
        validate_dstx(&DstxManager::default(), &mut counters, &chain, mainnet_limits(), &d, &txid);
    assert_eq!(
        v,
        DstxValidation::Continue {
            priority_delta: DSTX_PRIORITY_DELTA,
            masternode: mn.info.pro_tx_hash
        }
    );
    assert_eq!(DSTX_PRIORITY_DELTA, 10_000_000);
    assert_eq!(counters.masternodes[&mn.info.pro_tx_hash].mixing_tx_count, 1);

    // Known already: stop, no penalty.
    let mut kept = DstxManager::default();
    kept.add(d.clone());
    assert_eq!(
        validate_dstx(&kept, &mut counters, &chain, mainnet_limits(), &d, &txid),
        DstxValidation::Stop(DstxScore::None)
    );
}

#[test]
fn validate_dstx_finds_the_masternode_up_to_24_blocks_back() {
    let mn = Mn::new(1);
    let d = mn.dstx(3, 3);
    let txid = dstx::txid(&d.tx);
    let run = |chain: &Chain24| {
        validate_dstx(
            &DstxManager::default(),
            &mut MixingCounters::default(),
            chain,
            mainnet_limits(),
            &d,
            &txid,
        )
    };
    assert!(matches!(run(&Chain24::with(&mn, [23])), DstxValidation::Continue { .. }));
    assert_eq!(run(&Chain24::with(&mn, [24])), DstxValidation::Stop(DstxScore::UnknownMasternode));
    // A short chain: only the blocks there are.
    let mut short = Chain24::with(&mn, [3]);
    short.tip = Some(2);
    assert_eq!(run(&short), DstxValidation::Stop(DstxScore::UnknownMasternode));
    short.tip = Some(3);
    assert!(matches!(run(&short), DstxValidation::Continue { .. }));
    let mut none = Chain24::with(&mn, [0]);
    none.tip = None;
    assert_eq!(run(&none), DstxValidation::Stop(DstxScore::UnknownMasternode));
    assert_eq!(DstxScore::UnknownMasternode.score(), 1);
}

#[test]
fn validate_dstx_rejections() {
    let mn = Mn::new(1);
    let chain = Chain24::with(&mn, [0]);
    let run = |d: &DsTx, counters: &mut MixingCounters, chain: &Chain24| {
        validate_dstx(
            &DstxManager::default(),
            counters,
            chain,
            mainnet_limits(),
            d,
            &dstx::txid(&d.tx),
        )
    };

    // Malformed: invalid, 10.
    let unbalanced = mn.dstx(3, 2);
    assert_eq!(
        run(&unbalanced, &mut MixingCounters::default(), &chain),
        DstxValidation::Stop(DstxScore::Invalid)
    );
    assert_eq!(DstxScore::Invalid.score(), 10);
    // A post-V24 shape a block before V24 here: premature, 1.
    let promo = mn.dstx(19, 10);
    let mut eve = Chain24::with(&mn, [0]);
    eve.v24_next = true;
    assert_eq!(
        run(&promo, &mut MixingCounters::default(), &eve),
        DstxValidation::Stop(DstxScore::Premature)
    );
    assert_eq!(
        run(&promo, &mut MixingCounters::default(), &chain),
        DstxValidation::Stop(DstxScore::Invalid)
    );
    eve.v24 = true;
    assert!(matches!(
        run(&promo, &mut MixingCounters::default(), &eve),
        DstxValidation::Continue { .. }
    ));

    // Too many transactions since the masternode's last queue: stop, no penalty.
    let d = mn.dstx(3, 3);
    let mut busy = MixingCounters::default();
    busy.masternodes.insert(
        mn.info.pro_tx_hash,
        MasternodeMixing {
            last_dsq: 1,
            mixing_tx_count: MAX_MIXING_TXES + 1,
        },
    );
    assert_eq!(run(&d, &mut busy, &chain), DstxValidation::Stop(DstxScore::None));
    busy.masternodes.get_mut(&mn.info.pro_tx_hash).unwrap().mixing_tx_count = MAX_MIXING_TXES;
    assert!(matches!(run(&d, &mut busy, &chain), DstxValidation::Continue { .. }));

    // Signed by another key: invalid.
    let mut forged = d.clone();
    forged.sig = Mn::new(2).dstx(3, 3).sig;
    let mut counters = MixingCounters::default();
    assert_eq!(run(&forged, &mut counters, &chain), DstxValidation::Stop(DstxScore::Invalid));
    // Nothing was counted.
    assert!(counters.masternodes.is_empty());
}

#[test]
fn dstx_signature() {
    let mn = Mn::new(1);
    let d = mn.dstx(3, 3);
    assert_eq!(
        dstx::check_dstx_signature(&d, &mn.info.operator_key),
        dash_pkc::bls::core::BlsVerdict::Valid
    );
    let mut later = d.clone();
    later.sig_time += 1;
    assert_eq!(
        dstx::check_dstx_signature(&later, &mn.info.operator_key),
        dash_pkc::bls::core::BlsVerdict::Invalid
    );
}

// ---------------------------------------------------------------------------------------------
// The relay rules (P6a applies them)
// ---------------------------------------------------------------------------------------------

/// `SENDDSQUEUE` (net_processing.cpp:4485) and `RelayDSQ` (:2555).
#[test]
fn senddsq_and_dsq_relay() {
    let below = ProtocolVersion(ProtocolVersion::DSQ_INV.0 - 1);
    assert_eq!(wants_dsq_after_senddsq(false, ProtocolVersion::V24_0), WantsDsq::None);
    assert_eq!(wants_dsq_after_senddsq(true, below), WantsDsq::All);
    assert_eq!(wants_dsq_after_senddsq(true, ProtocolVersion::DSQ_INV), WantsDsq::Inv);
    assert_eq!(WantsDsq::default(), WantsDsq::None);

    assert_eq!(dsq_relay(WantsDsq::None), DsqRelay::Skip);
    assert_eq!(dsq_relay(WantsDsq::Inv), DsqRelay::Inv);
    assert_eq!(dsq_relay(WantsDsq::All), DsqRelay::Message);
}

/// `CanAnnounceDstxTo` (net_processing.cpp:3612), `_RelayTransaction` (:2652), the announcement
/// in `SendMessages` (:6635) and `ProcessGetData` (:2976).
#[test]
fn dstx_announcement() {
    let mn = Mn::new(1);
    let limits = mainnet_limits();
    let legacy = ProtocolVersion(ProtocolVersion::COINJOIN_REBALANCE.0 - 1);
    let rebalance = ProtocolVersion::COINJOIN_REBALANCE;

    let balanced = mn.dstx(3, 3);
    let promo = mn.dstx(19, 10);
    let oversized = mn.dstx(181, 181);
    assert!(can_announce_dstx_to(&balanced.tx, limits, legacy));
    assert!(!can_announce_dstx_to(&promo.tx, limits, legacy));
    assert!(!can_announce_dstx_to(&oversized.tx, limits, legacy));
    assert!(can_announce_dstx_to(&promo.tx, limits, rebalance));
    assert!(can_announce_dstx_to(&oversized.tx, limits, rebalance));
    assert!(can_announce_dstx_to(&mn.dstx(180, 180).tx, limits, legacy));

    let mut kept = DstxManager::default();
    for d in [&balanced, &promo] {
        kept.add((*d).clone());
    }
    let plain = u256(0x33);
    assert_eq!(relay_inv_kind(&kept, &dstx::txid(&balanced.tx)), TxInvKind::Dstx);
    assert_eq!(relay_inv_kind(&kept, &plain), TxInvKind::Tx);
    assert_eq!(announce_inv_kind(&kept, &dstx::txid(&promo.tx), limits, legacy), TxInvKind::Tx);
    assert_eq!(
        announce_inv_kind(&kept, &dstx::txid(&promo.tx), limits, rebalance),
        TxInvKind::Dstx
    );
    assert_eq!(
        announce_inv_kind(&kept, &dstx::txid(&balanced.tx), limits, legacy),
        TxInvKind::Dstx
    );
    assert_eq!(announce_inv_kind(&kept, &plain, limits, rebalance), TxInvKind::Tx);

    assert_eq!(
        dstx_for_getdata(&kept, TxInvKind::Dstx, &dstx::txid(&balanced.tx)),
        Some(&balanced)
    );
    assert_eq!(dstx_for_getdata(&kept, TxInvKind::Tx, &dstx::txid(&balanced.tx)), None);
    assert_eq!(dstx_for_getdata(&kept, TxInvKind::Dstx, &plain), None);
}
