//! R (LK1a-2a, P24): a record → replay round trip across V19 on BL-648's R2 chain
//! (`tests/data/v19-window/r2`, V19 at 236, ChainLocks on, V20 from 100).
//!
//! The recording run is given only the lock on 234, received at tip 234 under legacy, so it is
//! the best ChainLock every CbTx's step 10 asks `cl_best` for across V19. A recorded `cl_best` is
//! `sig_bytes(attempt_ctx)`: block 235's attempt (the last under legacy) writes the lock's legacy
//! bytes and block 236's, the first under V19, writes the basic encoding of the point decoded
//! under legacy. The replay run is given no ChainLock: each attempt's `cl_best` decodes the
//! recorded bytes under that attempt's scheme, and the digest it writes is the recording's, byte
//! for byte.

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use dash_clock::MockClock;
use dash_consensus::BlsCtx;
use dash_p2p_core::ClSig;
use dash_pkc::bls::BlsGlobalSig;
use dash_statedigest::decisions::{Policy, RecordedDecisions};
use dash_statedigest::{FileKind, Lines, Query, Record};

use super::connector::digest::header;
use super::evo_chain::{decode, linked, recorded_config_with};
use super::locks::{cl_rig, intake, record, settle};
use super::support::RigConfig;
use super::v19_window::{R2_CHAIN, R2_SUMMARY, last_time, locks, params, scheme, summary};
use crate::chain::ChainLockSig;
use crate::connect::{
    AttemptChoice, DecisionTap, DigestLog, MissSlot, NodeHelper, RecordingHelper,
    ReplayChainHelper, TapedLog,
};

/// R2's V19 height.
const V19: u32 = 236;

/// A log in memory, read after the sink moves into the connector.
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<u8>>>);

impl Write for Log {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("log").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Log {
    fn bytes(&self) -> Vec<u8> {
        self.0.lock().expect("log").clone()
    }
}

#[test]
fn r2_recorded_across_v19_replays_to_the_same_digest() {
    let s = summary(R2_SUMMARY);
    assert_eq!(s["v19_height"], V19);
    let raw = decode(R2_CHAIN);
    let p = params(&s);
    let mocktime = last_time(&raw);

    let (digest, mut file) = (Log::default(), Log::default());
    file.write_all(&Record::Header(header(FileKind::Decisions)).to_line()).expect("the H");
    let tap = DecisionTap::new();
    let sink = TapedLog::new(
        DigestLog::new(digest.clone(), header(FileKind::Digest)).expect("the H"),
        file.clone(),
        tap.clone(),
    );
    let recorder = RecordingHelper::new(Box::new(NodeHelper), tap, Arc::new(MockClock::new()));
    let (cfg, kept) = recorded_config_with(
        RigConfig::default(),
        p.clone(),
        mocktime,
        Some(Box::new(sink)),
        Box::new(recorder),
    );
    let (rig, _) = cl_rig(cfg);
    let m = linked(&rig.genesis, raw.clone());
    let locks = locks(&m);
    let sig_of = |height: u32| locks.iter().find(|l| l.height == height).expect("a lock").sig;
    let mut fed = locks.iter().filter(|l| l.height == V19 - 2).peekable();
    for b in &m {
        rig.accept(b);
        while let Some(lock) = fed.next_if(|l| l.arrival == b.height) {
            intake(&rig);
            let wire = ClSig::new(
                i32::try_from(lock.height).expect("a height"),
                m[lock.height as usize - 1].hash(),
                lock.sig.into(),
            );
            let sig = Arc::new(ChainLockSig::from_wire(&wire, scheme(&rig)).expect("archive sig"));
            record(&rig, &sig);
            settle(&rig);
        }
    }
    assert!(fed.next().is_none(), "the lock fed");
    kept.assert_no_abort(&rig);

    // The recording: the lock on 234 as the best, in each attempt's own bytes.
    let decisions = file.bytes();
    let best_at = |height: u32| -> [u8; 96] {
        let found = Lines::new(&decisions).find_map(|(_, r)| match r.expect("a record") {
            Record::Attempt(a) if a.id.h == i64::from(height) => Some(a),
            _ => None,
        });
        let found = found.expect("the attempt");
        let best = found
            .q
            .iter()
            .find_map(|q| match q {
                Query::ClBest(best) => Some(best.clone()),
                _ => None,
            })
            .expect("the attempt asked cl_best")
            .expect("a best ChainLock");
        assert_eq!(best.height, i64::from(V19 - 2), "the best lock at {height}");
        best.sig
    };
    let point = BlsGlobalSig::decode(&sig_of(V19 - 2)).expect("a signature").get(BlsCtx::LEGACY);
    assert!(point.is_valid(), "234's signature is legacy's");
    let basic = point.to_bytes(BlsCtx::BASIC.scheme());
    assert_ne!(basic, sig_of(V19 - 2));
    assert_eq!(best_at(V19 - 1), sig_of(V19 - 2), "block 235 recorded the legacy bytes");
    assert_eq!(best_at(V19), basic, "block 236 recorded the basic bytes");
    assert_eq!(best_at(V19 + 1), basic, "and block 237");

    // The replay, with no ChainLock given to V.
    let recorded = Arc::new(RecordedDecisions::load(&digest.bytes(), &decisions).expect("loads"));
    let (replayed, miss) = (Log::default(), MissSlot::new());
    let replayer =
        ReplayChainHelper::new(recorded, Policy::Strict, AttemptChoice::FinalConnect, miss.clone());
    let (cfg, kept) = recorded_config_with(
        RigConfig::default(),
        p,
        mocktime,
        Some(Box::new(DigestLog::new(replayed.clone(), header(FileKind::Digest)).expect("the H"))),
        Box::new(replayer),
    );
    let (rig, _) = cl_rig(cfg);
    for b in &linked(&rig.genesis, raw) {
        rig.accept(b);
    }
    assert_eq!(miss.get(), None, "every recorded answer used");
    kept.assert_no_abort(&rig);
    assert!(rig.v.view().best_chainlock().is_none(), "nothing was fed");
    assert!(replayed.bytes() == digest.bytes(), "the replay's digest is the recording's");
}
