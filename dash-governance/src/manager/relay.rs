//! Relay inventory accumulated by pure manager transitions.

/// The two governance inventory classes queued for announcement.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RelayQueue {
    pub objects: Vec<[u8; 32]>,
    pub votes: Vec<[u8; 32]>,
}

impl super::Manager {
    /// Queue an object inventory for relay.
    // Core: governance/governance.cpp:122-131
    pub fn relay_object(&mut self, hash: [u8; 32]) {
        self.relay.objects.push(hash);
    }
    /// Queue a vote inventory for relay.
    // Core: governance/governance.cpp:134-151
    pub fn relay_vote(&mut self, hash: [u8; 32]) {
        self.relay.votes.push(hash);
    }
    /// Take and clear queued relay inventories.
    // Core: governance/governance.cpp:515-521
    pub fn take_relay_queue(&mut self) -> RelayQueue {
        std::mem::take(&mut self.relay)
    }
}
