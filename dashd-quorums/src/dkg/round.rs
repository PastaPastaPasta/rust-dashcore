//! A handler's DKG round (DK1 plan §4 L2): `InitNewQuorum`, then `HandleDKGRound`'s phases on
//! the phase thread, which owns the session.

use std::cell::RefCell;
use std::panic::resume_unwind;
use std::sync::Arc;

use dash_consensus::primitives::P2pCommitment;
use dash_dml::Dmn;
use dash_evo::types::ProTxHash;
use dash_llmq::dkg::bls::BlsCtx;
use dash_llmq::dkg::conn::ConnSnapshot;
use dash_llmq::dkg::rng::DkgRng;
use dash_llmq::dkg::session::{
    DkgEnv, DkgEvent, DkgFault, DkgMsgKind, DkgSession, InitRefusal, QuorumBase, SerialRunner,
    SessionInit,
};
use dash_primitives::QuorumHash;
use getrandom::SysRng;
use rand_core::UnwrapErr;
use tracing::{debug, info};

use super::debug::DebugManager;
use super::handler::Handler;
use super::runtime::RoundSeams;
use super::seams::BaseFacts;
use super::served::Served;
use super::types::QuorumPhase;
use super::wait::{Abort, PhaseCtx, handle_phase, wait_for_new_quorum, wait_for_next_phase};

/// Writes a session's lines as Core's logger writes them: each `CDKGLogger` record as one
/// `llmq-dkg` debug line holding its lines, each indented by four spaces; each line written on
/// its own as `<func> -- <text>`, under `llmq` for `GetVerifiedContributions` and `llmq-dkg`
/// otherwise. The cxxtimer fields (`time=`) are not in the session's lines.
// Core: v24.0.0 src/batchedlogger.h:26-33 CBatchedLogger::Batch
pub fn log_events(events: &[DkgEvent]) {
    let mut i = 0;
    while i < events.len() {
        let Some(record) = events[i].record else {
            let e = &events[i];
            if e.func == "GetVerifiedContributions" {
                debug!(target: "llmq", "{e}");
            } else {
                debug!(target: "llmq-dkg", "{e}");
            }
            i += 1;
            continue;
        };
        let mut text = String::new();
        while i < events.len() && events[i].record == Some(record) {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str("    ");
            text.push_str(&events[i].text);
            i += 1;
        }
        debug!(target: "llmq-dkg", "{text}");
    }
}

fn short(pro_tx: &ProTxHash) -> String {
    pro_tx.to_string().chars().take(4).collect()
}

fn short_list<'a>(pro_txs: impl IntoIterator<Item = &'a ProTxHash>) -> String {
    pro_txs.into_iter().map(|p| format!("{} | ", short(p))).collect()
}

// Core: v24.0.0 src/llmq/dkgsession.cpp:119-153 CDKGSession::Init
fn init_events(
    mut events: Vec<DkgEvent>,
    session: Option<&DkgSession<Arc<Dmn>>>,
    members: &[ProTxHash],
    cycle_height: Option<i32>,
    quorum_index: i32,
) -> Vec<DkgEvent> {
    if !tracing::enabled!(target: "llmq", tracing::Level::DEBUG) {
        return events;
    }
    let record = events.first().and_then(|e| e.record);
    if let Some((s, idx)) = session.and_then(|s| s.my_index().map(|i| (s, i))) {
        let relay = short_list(s.relay_members());
        let at = events.len().saturating_sub(1);
        events.insert(
            at,
            DkgEvent {
                record,
                func: "Init",
                text: format!("forMember[{}] relayMembers[{relay}]", short(&members[idx])),
            },
        );
    }
    if let Some(height) = cycle_height {
        let dkg = short_list(members);
        events.insert(
            0,
            DkgEvent {
                record,
                func: "Init",
                text: format!("DKGComposition h[{height}] i[{quorum_index}] DKG:[{dkg}]"),
            },
        );
    }
    events
}

struct Live {
    session: DkgSession<Arc<Dmn>>,
    rng: Box<dyn DkgRng + Send>,
}

// Core: v24.0.0 src/active/dkgsessionhandler.cpp:80-109 ActiveDKGSessionHandler::InitNewQuorum
fn init_new_quorum(seams: &RoundSeams, h: &Handler, base: &BaseFacts) -> Option<Live> {
    let key = h.key();
    let (t, qi) = (key.llmq_type, key.quorum_index);
    if seams.chain.snapshot_unvalidated() {
        debug!(
            target: "llmq-dkg",
            "InitNewQuorum -- refusing DKG participation while snapshot background validation is incomplete"
        );
        return None;
    }
    if !seams.chain.dip3_enforced(base.height) {
        return None;
    }
    // Core: v24.0.0 src/active/dkgsession.cpp:35 ActiveDKGSession::ActiveDKGSession
    let v19_after_base = seams.chain.v19_after(base);
    drop(h.take_session());
    let active_pro_tx = seams.active.pro_tx_hash();
    let members = seams.members.all_members(t, base);
    let rotation_enabled = seams.chain.rotation_enabled(h.params(), base);
    let pro_txs: Vec<ProTxHash> = members.iter().map(|m| m.pro_tx_hash).collect();
    let init = SessionInit {
        llmq_type: t,
        params: h.params(),
        base: QuorumBase {
            hash: QuorumHash::from(*base.hash.as_bytes()),
            height: base.height,
        },
        quorum_index: qi,
        members,
        active_pro_tx,
        rotation_enabled,
        v19_after_base,
    };
    let cycle_height = rotation_enabled.then_some(base.height - qi);
    let result = {
        let mut topology = seams.ports.topology();
        let mut debug: &DebugManager = &seams.debug;
        DkgSession::init(init, &mut *topology, &mut debug)
    };
    match result {
        Err(InitRefusal::NotEnoughMembers {
            events,
            ..
        }) => {
            log_events(&init_events(events, None, &pro_txs, cycle_height, qi));
            info!(
                "ActiveDKGSessionHandler::InitNewQuorum -- height[{}] quorum initialization failed for {t} qi[{qi}]",
                base.height
            );
            None
        }
        Ok(mut session) => {
            let events = session.take_events();
            log_events(&init_events(events, Some(&session), &pro_txs, cycle_height, qi));
            let served = Arc::new(Served::for_session(&session));
            let session = session.with_served(served.clone());
            h.set_session(served);
            info!(
                "ActiveDKGSessionHandler::InitNewQuorum -- height[{}] quorum initialization OK for {t} qi[{qi}]",
                base.height
            );
            Some(Live {
                session,
                rng: seams.ports.session_rng(key),
            })
        }
    }
}

// Core: v24.0.0 src/evo/specialtxman.cpp:1043-1047, 1067-1072 CSpecialTxProcessor
fn tip_scheme(seams: &RoundSeams) -> BlsCtx {
    match seams.chain.current_tip() {
        Some(tip) if seams.chain.v19_after(&tip) => BlsCtx::BASIC,
        _ => BlsCtx::LEGACY,
    }
}

fn step<R>(
    seams: &RoundSeams,
    live: &mut Live,
    f: impl FnOnce(&mut DkgSession<Arc<Dmn>>, &mut DkgEnv<'_>) -> R,
) -> R {
    let Live {
        session,
        rng,
    } = live;
    let bls = tip_scheme(seams);
    let sim = seams.sim.snapshot();
    let mut store = seams.ports.store();
    let mut verify = seams.ports.verifier();
    let mut debug: &DebugManager = &seams.debug;
    let mut weights = UnwrapErr(SysRng);
    let mut env = DkgEnv {
        bls,
        rng: &mut **rng,
        weights: &mut weights,
        key: seams.ports.operator_key(),
        store: &mut *store,
        debug: &mut debug,
        sim: &sim,
        runner: &SerialRunner,
        verify: &mut *verify,
    };
    let out = f(session, &mut env);
    log_events(&session.take_events());
    out
}

fn fault(f: DkgFault) -> ! {
    resume_unwind(Box::new(f))
}

// Core: v24.0.0 src/llmq/net_dkg.cpp:869-873, 942-947 NetDKG::HandleDKGRound
fn mine(seams: &RoundSeams, qc: &P2pCommitment) {
    if let Some(hash) = seams.mineable.add(qc) {
        seams.mineable.relay_inv(&hash);
    }
}

/// One DKG round of handler `h`, from the wait for Initialized to the final commitments.
///
/// # Errors
///
/// [`Abort`] from a wait, or after a failed init once the quorum hash has changed.
// Core: v24.0.0 src/llmq/net_dkg.cpp:846-949 NetDKG::HandleDKGRound
pub(crate) fn handle_round(seams: &RoundSeams, h: &Handler) -> Result<(), Abort> {
    let key = h.key();
    let (t, qi) = (key.llmq_type, key.quorum_index);
    let clock = seams.clock.as_ref();
    let ctx = PhaseCtx {
        handler: h,
        chain: seams.chain.as_ref(),
        marks: seams.debug.as_ref(),
    };
    let null = QuorumHash::default();
    wait_for_next_phase(&ctx, clock, None, QuorumPhase::Initialized, &null, &mut || false)?;

    seams.queues.clear(key);
    let hash = h.quorum_hash();
    let Some((base, mut live)) = seams
        .chain
        .lookup(&hash)
        .and_then(|base| init_new_quorum(seams, h, &base).map(|live| (base, live)))
    else {
        wait_for_new_quorum(&ctx, clock, &hash)?;
        return Err(Abort);
    };
    seams.debug.mark_phase_advanced(t, qi, QuorumPhase::Initialized);
    let (init, contribute) = (QuorumPhase::Initialized, QuorumPhase::Contribute);

    if h.params().size == 1 {
        let qc = step(seams, &mut live, |s, env| s.finalize_single_commitment(env));
        if let Some(qc) = qc.unwrap_or_else(|f| fault(f)) {
            mine(seams, &qc);
        }
        seams.debug.mark_phase_advanced(t, qi, QuorumPhase::Finalize);
        wait_for_next_phase(&ctx, clock, Some(init), contribute, &hash, &mut || false)?;
        return Ok(());
    }

    let session = &live.session;
    let me = session.my_index();
    let my_pro_tx = me.map_or_else(ProTxHash::default, |i| session.members()[i].pro_tx_hash);
    let we_are_member = me.is_some();
    seams.connections.on_initialized(t, h.params(), &base, &my_pro_tx, seams.watch, we_are_member);

    wait_for_next_phase(&ctx, clock, Some(init), contribute, &hash, &mut || false)?;

    let live = RefCell::new(live);
    let pop = |kind: DkgMsgKind| {
        let batch = seams.queues.pop_batch(key, kind, 8);
        if batch.is_empty() {
            return false;
        }
        let mut live = live.borrow_mut();
        step(seams, &mut live, |s, env| seams.batches.process_batch(s, env, kind, batch));
        true
    };
    let enqueue = |kind: DkgMsgKind, payload: Option<Vec<u8>>| {
        if let Some(payload) = payload {
            seams.queues.enqueue_own(key, kind, &my_pro_tx, payload);
        }
    };

    let kind = DkgMsgKind::Contribution;
    let start = || {
        let own = step(seams, &mut live.borrow_mut(), |s, env| {
            let qc = s.contribute(env)?;
            Ok::<_, DkgFault>(qc.map(|qc| qc.payload(env.bls.scheme())))
        });
        enqueue(kind, own.unwrap_or_else(|f| fault(f)));
    };
    let (cur, next) = (QuorumPhase::Contribute, QuorumPhase::Complain);
    handle_phase(&ctx, clock, cur, next, &hash, 0.05, start, &mut || pop(kind))?;

    let kind = DkgMsgKind::Complaint;
    let start = || {
        let mut live = live.borrow_mut();
        let conn = if we_are_member {
            let members: Vec<ProTxHash> =
                live.session.members().iter().map(|m| m.pro_tx_hash).collect();
            seams.conn_facts.snapshot(t, &members)
        } else {
            ConnSnapshot::default()
        };
        let own = step(seams, &mut live, |s, env| {
            let qc = s.verify_and_complain(&conn, env)?;
            Ok::<_, DkgFault>(qc.map(|qc| qc.payload(env.bls.scheme())))
        });
        drop(live);
        enqueue(kind, own.unwrap_or_else(|f| fault(f)));
    };
    let (cur, next) = (QuorumPhase::Complain, QuorumPhase::Justify);
    handle_phase(&ctx, clock, cur, next, &hash, 0.05, start, &mut || pop(kind))?;

    let kind = DkgMsgKind::Justification;
    let start = || {
        let own = step(seams, &mut live.borrow_mut(), |s, env| {
            let qj = s.verify_and_justify(env)?;
            Ok::<_, DkgFault>(qj.map(|qj| qj.payload(env.bls.scheme())))
        });
        enqueue(kind, own.unwrap_or_else(|f| fault(f)));
    };
    let (cur, next) = (QuorumPhase::Justify, QuorumPhase::Commit);
    handle_phase(&ctx, clock, cur, next, &hash, 0.05, start, &mut || pop(kind))?;

    let kind = DkgMsgKind::PrematureCommitment;
    let start = || {
        let own = step(seams, &mut live.borrow_mut(), |s, env| {
            s.verify_and_commit(env).map(|qc| qc.payload(env.bls.scheme()))
        });
        enqueue(kind, own);
    };
    let (cur, next) = (QuorumPhase::Commit, QuorumPhase::Finalize);
    handle_phase(&ctx, clock, cur, next, &hash, 0.1, start, &mut || pop(kind))?;

    let finals = step(seams, &mut live.borrow_mut(), |s, env| s.finalize_commitments(env));
    for qc in &finals {
        mine(seams, qc);
    }
    seams.debug.mark_phase_advanced(t, qi, QuorumPhase::Finalize);
    Ok(())
}

#[cfg(test)]
mod tests;
