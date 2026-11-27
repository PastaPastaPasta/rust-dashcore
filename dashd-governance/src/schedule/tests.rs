//! The periodic jobs over the serial executor, with recorded sends and a steady-time scheduler.
// Core: governance/net_governance.cpp v24.0.2:39-78

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use dash_governance::GovParams;
use dash_governance::manager::GovClock;
use dash_p2p::OutMsg;
use dash_params::{ChainParamsBuilder, Network, Release};

use super::*;
use crate::exec::fakes::{ExecEvent, SerialExec};
use crate::net::tests::Parts;
use crate::net::{object_inv, object_request, vote_inv};
use crate::rand::{Draw, ReplayRng};
use crate::tests::{chain_at, clock, operator_vote, params, three_masternodes};

type Serial = Arc<SerialExec>;

/// The parts over a serial executor at 100 with `testkit::numbered(1..=3)` listed, synced, and
/// two relaying peers.
fn parts() -> (Parts<Serial>, Serial) {
    let exec = Arc::new(SerialExec::new(chain_at(100)));
    exec.chain().list = Arc::new(three_masternodes());
    let parts = Parts::new(Arc::clone(&exec));
    *parts.peers.relay_peers.lock().unwrap() = vec![1, 2];
    (parts, exec)
}

/// `n` orphan votes, each for its own unknown parent; their parents in order.
fn orphans(parts: &Parts<Serial>, n: u8) -> Vec<[u8; 32]> {
    let list = three_masternodes();
    let parents: Vec<_> = (0..n).map(|i| [i.wrapping_add(1); 32]).collect();
    for parent in &parents {
        let (outcome, _) = parts.handle.process_vote(&operator_vote(1, *parent), &list);
        assert_eq!(outcome.to_request, Some(*parent));
    }
    parents
}

fn pushed(parts: &Parts<Serial>) -> Vec<(i64, OutMsg)> {
    std::mem::take(&mut *parts.peers.pushed.lock().unwrap())
}

/// Each command the executor ran.
fn commands(exec: &SerialExec) -> usize {
    exec.log().iter().filter(|e| matches!(e, ExecEvent::Enter(_))).count()
}

fn jobs(parts: &Parts<Serial>, rng: ReplayRng) -> Jobs<Serial> {
    parts.net().jobs(Box::new(rng)).expect("valid")
}

#[test]
fn the_maintenance_job_does_nothing_until_synced() {
    let (parts, exec) = parts();
    orphans(&parts, 3);
    parts.set_sync(true, true, false);
    jobs(&parts, ReplayRng::default()).maintenance();
    assert!(pushed(&parts).is_empty());
    assert_eq!(commands(&exec), 0, "no clean posted");
}

#[test]
fn the_maintenance_job_asks_every_relaying_peer_for_each_orphans_parent_then_cleans() {
    let (parts, exec) = parts();
    let parents = orphans(&parts, 3);
    jobs(&parts, ReplayRng::default()).maintenance();
    let want: Vec<_> =
        parents.iter().flat_map(|h| [1, 2].map(|peer| (peer, object_request(*h)))).collect();
    assert_eq!(pushed(&parts), want);
    assert_eq!(commands(&exec), 1, "the clean");
    // Expired orphans are no longer asked for.
    parts.handle.lock().cleanup_orphans(clock().now + 601);
    jobs(&parts, ReplayRng::default()).maintenance();
    assert!(pushed(&parts).is_empty());
    assert_eq!(commands(&exec), 2);
}

#[test]
fn the_orphan_request_is_a_sample_of_at_most_100() {
    let (parts, _) = parts();
    let parents = orphans(&parts, 101);
    let mut recorded = parents.clone();
    recorded.reverse();
    recorded.truncate(MAX_ORPHAN_OBJECT_REQUESTS);
    let rng = ReplayRng::new([Draw::Sample(recorded.clone())]);
    jobs(&parts, rng).maintenance();
    let asked: Vec<_> =
        pushed(&parts).into_iter().filter(|(peer, _)| *peer == 1).map(|(_, msg)| msg).collect();
    let want: Vec<_> = recorded.iter().map(|h| object_request(*h)).collect();
    assert_eq!(asked, want);
}

#[test]
fn the_relay_flush_announces_the_queue_once() {
    let (parts, _) = parts();
    let invs = [object_inv([1; 32]), vote_inv([2; 32])];
    for inv in invs {
        parts.relay.push(inv);
    }
    let jobs = jobs(&parts, ReplayRng::default());
    jobs.flush_relay();
    jobs.flush_relay();
    assert_eq!(*parts.peers.relayed.lock().unwrap(), invs);
}

fn params_of(network: Network) -> GovParams {
    let chain = ChainParamsBuilder::new(network, Release::V24_0).build().expect("params");
    GovParams::from_chain_params(&chain).expect("its parameters")
}

#[test]
fn the_relay_flush_runs_every_second_on_regtest_and_every_5_s_on_mainnet() {
    assert_eq!(relay_interval(&params()), Duration::from_secs(1));
    assert_eq!(relay_interval(&params_of(Network::Main)), Duration::from_secs(5));
    assert_eq!(relay_interval(&params_of(Network::Testnet3)), Duration::from_secs(5));
}

/// A job's period, the time left until it runs, and its body.
type Entry = (Duration, Duration, Box<dyn Fn() + Send + Sync>);

/// A steady clock that runs each job when its period has passed since the last run.
#[derive(Default)]
struct Steady {
    jobs: Mutex<Vec<Entry>>,
}

impl Steady {
    fn advance(&self, by: Duration) {
        let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        for (every, until, job) in jobs.iter_mut() {
            *until = until.saturating_sub(by);
            while until.is_zero() {
                job();
                *until = *every;
            }
        }
    }

    fn periods(&self) -> Vec<Duration> {
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner).iter().map(|j| j.0).collect()
    }
}

impl GovScheduler for Steady {
    fn schedule_every(&self, job: Box<dyn Fn() + Send + Sync>, every: Duration) {
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner).push((every, every, job));
    }
}

#[test]
fn the_jobs_run_on_steady_time_and_the_mock_clock_alone_runs_nothing() {
    let (parts, exec) = parts();
    let steady = Steady::default();
    parts.net().schedule(&steady, Box::new(ReplayRng::default()));
    assert_eq!(steady.periods(), [MAINTENANCE_INTERVAL, Duration::from_secs(1)]);
    parts.relay.push(object_inv([1; 32]));
    let later = clock().now + 3600;
    parts.env.set(GovClock::new(later, later), true, true);
    assert!(parts.peers.relayed.lock().unwrap().is_empty());
    assert_eq!(commands(&exec), 0);

    steady.advance(Duration::from_secs(1));
    assert_eq!(*parts.peers.relayed.lock().unwrap(), [object_inv([1; 32])]);
    assert_eq!(commands(&exec), 0);
    steady.advance(MAINTENANCE_INTERVAL - Duration::from_secs(2));
    assert_eq!(commands(&exec), 0);
    steady.advance(Duration::from_secs(1));
    assert_eq!(commands(&exec), 1, "the clean");
}

#[test]
fn nothing_is_scheduled_while_governance_is_invalid() {
    let (parts, _) = parts();
    parts.set_sync(false, true, true);
    let steady = Steady::default();
    parts.net().schedule(&steady, Box::new(ReplayRng::default()));
    assert!(steady.periods().is_empty());
}
