//! Tracks whether block headers have caught up since every peer was last lost.

/// Whether block headers have caught up since every peer was last lost, for a manager that
/// waits on [`BlockHeaderSyncComplete`]. Both the completion's `connection_epoch` and the
/// epoch it is compared with are [`NetworkManager::disconnects`], read when the completion was
/// made and when it is checked, so the order a task handles sync and network events in does
/// not matter: a completion from before the last loss is stale.
///
/// [`NetworkManager::disconnects`]: crate::network::NetworkManager::disconnects
/// [`BlockHeaderSyncComplete`]: crate::sync::SyncEvent::BlockHeaderSyncComplete
#[derive(Debug, Default)]
pub(crate) struct HeaderCompletionGate {
    /// Highest `connection_epoch` of a completion handled so far.
    synced_epoch: Option<u64>,
}

impl HeaderCompletionGate {
    /// Records a handled completion, and returns whether it is from the
    /// `current_epoch` connection, i.e. whether it counts.
    pub(crate) fn record(&mut self, connection_epoch: u64, current_epoch: u64) -> bool {
        // The max, not the latest: a completion can only move the gate forward.
        self.synced_epoch = self.synced_epoch.max(Some(connection_epoch));
        connection_epoch >= current_epoch
    }

    /// Whether a completion from the `current_epoch` connection has been handled.
    ///
    /// When it was handled before this task got to the disconnect that preceded
    /// it, nothing emits it again, so a manager checks this in `start_sync` to
    /// resume.
    pub(crate) fn synced(&self, current_epoch: u64) -> bool {
        self.synced_epoch.is_some_and(|epoch| epoch >= current_epoch)
    }
}

#[cfg(test)]
mod tests {
    use super::HeaderCompletionGate;

    #[test]
    fn test_gate_keeps_a_completion_handled_before_its_disconnect() {
        let mut gate = HeaderCompletionGate::default();
        assert!(!gate.synced(1));
        assert!(gate.record(1, 1), "a completion from after the loss counts");
        assert!(gate.synced(1), "the loss, handled after it, does not discard it");
        assert!(!gate.synced(2), "a later loss does");
    }

    /// The stale-completion check: a completion whose epoch is older than the count is stale.
    #[test]
    fn test_gate_drops_a_completion_from_before_a_disconnect() {
        let mut gate = HeaderCompletionGate::default();
        assert!(gate.record(0, 0));
        assert!(!gate.synced(1));
        assert!(!gate.record(0, 1), "a late copy of the old completion does not count");
        assert!(!gate.synced(1));
        assert!(gate.record(1, 1));
        assert!(gate.synced(1));
    }

    #[test]
    fn test_gate_never_moves_back() {
        let mut gate = HeaderCompletionGate::default();
        assert!(gate.record(2, 2));
        assert!(!gate.record(1, 2), "a straggler from before the last loss is stale");
        assert!(gate.synced(2), "and must not lower the recorded epoch");
    }
}
