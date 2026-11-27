//! Core's `NetGovernance::Schedule`: the periodic governance jobs, registered on the node
//! scheduler only while governance is valid. They are paced by the scheduler's steady time,
//! which `mockscheduler` advances and `setmocktime` does not; inside a job, governance reads
//! its own mockable clock.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use dash_governance::GovParams;
use dash_governance::manager::Effects;

use crate::exec::GovExec;
use crate::net::{NetGovernance, Shared, object_request};
use crate::rand::GovRng;

/// `MAX_ORPHAN_OBJECT_REQUESTS`: the most orphan parents one maintenance run requests.
// Core: governance/governance.h v24.0.2:366
pub const MAX_ORPHAN_OBJECT_REQUESTS: usize = 100;

/// The maintenance job's period.
// Core: governance/net_governance.cpp v24.0.2:67
pub const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Core's `CScheduler::scheduleEvery`, which dashd-node's scheduler implements (S5).
pub trait GovScheduler {
    /// Runs `job` every `every` of steady time, the first time after one `every`.
    fn schedule_every(&self, job: Box<dyn Fn() + Send + Sync>, every: Duration);
}

/// The relay flush's period: 1 s on a chain with mockable time, 5 s elsewhere.
// Core: governance/net_governance.cpp v24.0.2:76-77
pub fn relay_interval(params: &GovParams) -> Duration {
    Duration::from_secs(if params.chain().policy.is_mockable_chain {
        1
    } else {
        5
    })
}

/// The two jobs' bodies.
pub(crate) struct Jobs<E> {
    shared: Arc<Shared<E>>,
    rng: Mutex<Box<dyn GovRng + Send>>,
}

impl<E: GovExec + 'static> Jobs<E> {
    /// Requests the parents of orphan votes from every relaying peer, then posts the clean.
    // Core: governance/net_governance.cpp v24.0.2:45-66; governance/governance.cpp
    // v24.0.2:1149-1182
    pub(crate) fn maintenance(&self) {
        let s = &*self.shared;
        if !s.sync.is_synced() {
            return;
        }
        let now = s.handle.env().clock().now;
        let (mut hashes, leftover) = {
            let mut m = s.handle.lock();
            let hashes = m.orphan_parent_candidates(now);
            let relay = m.take_relay_queue();
            let leftover = Effects {
                relay_objects: relay.objects,
                relay_votes: relay.votes,
                meta: m.take_meta(),
                ..Effects::default()
            };
            (hashes, leftover)
        };
        s.apply(leftover);
        self.rng
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .sample(&mut hashes, MAX_ORPHAN_OBJECT_REQUESTS);
        if !hashes.is_empty() {
            tracing::debug!(target: "gobject", "NetGovernance::Schedule -- requesting {} orphan objects", hashes.len());
            let peers = s.peers.relay_peers();
            for hash in &hashes {
                for &peer in &peers {
                    s.peers.push(peer, object_request(*hash));
                }
            }
        }
        // Posted, not waited for: its effects go to the effects worker.
        drop(s.handle.clean());
    }

    /// Announces what `RelayObject` and `RelayVote` queued.
    // Core: governance/net_governance.cpp v24.0.2:69-75
    pub(crate) fn flush_relay(&self) {
        for inv in self.shared.relay.take() {
            self.shared.peers.relay_inv(&inv);
        }
    }
}

impl<E: GovExec + 'static> NetGovernance<E> {
    /// Registers the jobs on `scheduler`, unless governance is invalid.
    // Core: governance/net_governance.cpp v24.0.2:39-78
    pub fn schedule(&self, scheduler: &dyn GovScheduler, rng: Box<dyn GovRng + Send>) {
        if let Some(jobs) = self.jobs(rng) {
            let jobs = Arc::new(jobs);
            let relay = relay_interval(jobs.shared.handle.params());
            let maintenance = Arc::clone(&jobs);
            scheduler
                .schedule_every(Box::new(move || maintenance.maintenance()), MAINTENANCE_INTERVAL);
            scheduler.schedule_every(Box::new(move || jobs.flush_relay()), relay);
        }
    }

    pub(crate) fn jobs(&self, rng: Box<dyn GovRng + Send>) -> Option<Jobs<E>> {
        self.shared().sync.is_valid().then(|| Jobs {
            shared: Arc::clone(self.shared()),
            rng: Mutex::new(rng),
        })
    }
}

#[cfg(test)]
mod tests;
