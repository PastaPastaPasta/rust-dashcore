//! Tracks whether block headers have caught up since every peer was last lost.

/// Whether block headers have caught up since every peer was last lost, for a
/// manager that waits on [`BlockHeaderSyncComplete`].
///
/// A flag cleared on disconnect loses the completion when the task handles it
/// before the disconnect that preceded it: sync and network events arrive on
/// separate channels in no fixed order, the block-header task emits the
/// completion once per connection, and the stale disconnect would clear the
/// flag for good. The gate counts disconnects the way `BlockHeadersManager`
/// does and compares that count with the completion's `connection_epoch`, so
/// it keeps a completion from after the disconnect and drops one from before.
///
/// [`BlockHeaderSyncComplete`]: crate::sync::SyncEvent::BlockHeaderSyncComplete
#[derive(Debug, Default)]
pub(crate) struct HeaderCompletionGate {
    /// Times every peer has been lost, counted from `on_disconnect`.
    connection_epoch: u64,
    /// Highest `connection_epoch` of a completion handled so far.
    synced_epoch: Option<u64>,
}

impl HeaderCompletionGate {
    /// Counts a loss of every peer. Call from `SyncManager::on_disconnect`.
    pub(crate) fn on_disconnect(&mut self) {
        self.connection_epoch += 1;
    }

    /// Records a handled completion, and returns whether it is from the
    /// current connection (or a later one this task has not seen the
    /// disconnect for yet), i.e. whether it counts.
    pub(crate) fn record(&mut self, connection_epoch: u64) -> bool {
        // The max, not the latest: a completion can only move the gate forward.
        self.synced_epoch = self.synced_epoch.max(Some(connection_epoch));
        connection_epoch >= self.connection_epoch
    }

    /// Whether a completion from the current connection has been handled.
    ///
    /// When it was handled before the disconnect that preceded it, nothing
    /// emits it again, so a manager checks this in `start_sync` to resume.
    pub(crate) fn synced(&self) -> bool {
        self.synced_epoch.is_some_and(|epoch| epoch >= self.connection_epoch)
    }
}

#[cfg(test)]
mod tests {
    use super::HeaderCompletionGate;

    #[test]
    fn test_gate_keeps_a_completion_handled_before_its_disconnect() {
        let mut gate = HeaderCompletionGate::default();
        assert!(!gate.synced());
        assert!(gate.record(1), "a completion from the next connection counts");
        assert!(gate.synced());
        gate.on_disconnect();
        assert!(gate.synced(), "its disconnect must not discard it");
    }

    #[test]
    fn test_gate_drops_a_completion_from_before_a_disconnect() {
        let mut gate = HeaderCompletionGate::default();
        assert!(gate.record(0));
        gate.on_disconnect();
        assert!(!gate.synced());
        assert!(!gate.record(0), "a late copy of the old completion does not count");
        assert!(!gate.synced());
        assert!(gate.record(1));
        assert!(gate.synced());
    }

    #[test]
    fn test_gate_never_moves_back() {
        let mut gate = HeaderCompletionGate::default();
        assert!(gate.record(2));
        assert!(gate.record(1), "still at or past the current connection");
        gate.on_disconnect();
        gate.on_disconnect();
        assert!(gate.synced(), "the older straggler must not lower the recorded epoch");
    }
}
