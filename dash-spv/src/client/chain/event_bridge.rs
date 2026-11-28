//! The chain's changes as [`SyncEvent`]s, for the managers not yet chain subscribers (PLAN-r1
//! §2.4.4).

use std::sync::Arc;

use dash_chain::AncestryError;
use tokio::sync::{broadcast, watch};

use super::status::tip_ref;
use super::sub::notice;
use super::{ChainNotice, ChainSource, ChainSub, HeaderSyncState, PinnedChain, SubError, SubItem};
use crate::network::NetworkManager;
use crate::sync::SyncEvent;

/// A [`SubKind::Progress`](super::SubKind::Progress) subscriber that publishes the chain's
/// changes as [`SyncEvent`]s:
/// - `BlockHeadersStored` at each new tip, after a `ChainReorganized` when the tip left its
///   branch;
/// - `BlockHeaderSyncComplete` when [`HeaderSyncState::caught_up`] turns true, its
///   `connection_epoch` the network's [`disconnects`](NetworkManager::disconnects) then.
///
/// It reads the chain only through each notice's pin. A `Resync` emits what changed since the
/// last pin it delivered: the reorg and the tip on that pin's generation, the tip alone on
/// another, and the completion when the state it carries is caught up and the last was not.
/// No lag or resync ends it; it ends once the chain has stopped.
pub struct ChainEventBridge {
    sub: ChainSub,
    source: watch::Receiver<ChainSource>,
    events: broadcast::Sender<SyncEvent>,
    network: Arc<dyn NetworkManager>,
    last: Option<PinnedChain>,
    caught_up: bool,
}

impl ChainEventBridge {
    pub(super) fn new(
        sub: ChainSub,
        source: watch::Receiver<ChainSource>,
        events: broadcast::Sender<SyncEvent>,
        network: Arc<dyn NetworkManager>,
    ) -> Self {
        Self {
            sub,
            source,
            events,
            network,
            last: None,
            caught_up: false,
        }
    }

    /// Publishes the chain's changes until it stops.
    pub async fn run(mut self) {
        while let Some(events) = self.next_events().await {
            for event in events {
                let _ = self.events.send(event);
            }
        }
    }

    /// The events of the subscriber's next item; `None` once the chain has stopped. A failed
    /// read is logged, and the next one waits for the chain's source to change.
    pub(super) async fn next_events(&mut self) -> Option<Vec<SyncEvent>> {
        loop {
            match self.sub.next().await {
                Ok(SubItem::Stop) | Err(SubError::Stopped) => return None,
                Ok(item) => return Some(self.on(item)),
                Err(e) => {
                    tracing::warn!("chain event bridge: {e}");
                    self.source.changed().await.ok()?;
                }
            }
        }
    }

    fn on(&mut self, item: SubItem) -> Vec<SyncEvent> {
        match item {
            SubItem::Notice(ChainNotice::Resync {
                pin,
                sync,
                ..
            }) => {
                let mut out = self.since_last(&pin);
                self.last = Some(pin);
                out.extend(self.sync(sync));
                out
            }
            SubItem::Notice(n) => {
                let out = changed(&n);
                if let ChainNotice::Advanced {
                    pin,
                    ..
                }
                | ChainNotice::Reorg {
                    pin,
                    ..
                } = n
                {
                    self.last = Some(pin);
                }
                out
            }
            SubItem::Sync(state) => self.sync(state).into_iter().collect(),
            SubItem::Stop => Vec::new(),
        }
    }

    /// What changed from the last pin delivered to `pin`, read through `pin`.
    fn since_last(&self, pin: &PinnedChain) -> Vec<SyncEvent> {
        let Some(last) = self.last.as_ref().filter(|l| l.gen == pin.gen) else {
            return vec![stored(tip_ref(&pin.snap).height)];
        };
        match notice(last, pin) {
            Ok(None) => Vec::new(),
            Ok(Some(n)) => changed(&n),
            Err(e) => {
                if !matches!(e, AncestryError::Retired) {
                    tracing::warn!("chain event bridge: no fork from the last tip: {e}");
                }
                vec![stored(tip_ref(&pin.snap).height)]
            }
        }
    }

    /// `BlockHeaderSyncComplete` when `state` is caught up and the last state was not.
    fn sync(&mut self, state: HeaderSyncState) -> Option<SyncEvent> {
        let rose = state.caught_up && !self.caught_up;
        self.caught_up = state.caught_up;
        rose.then(|| SyncEvent::BlockHeaderSyncComplete {
            tip_height: state.tip.height,
            connection_epoch: self.network.disconnects(),
        })
    }
}

fn changed(n: &ChainNotice) -> Vec<SyncEvent> {
    match n {
        ChainNotice::Advanced {
            to,
            ..
        } => vec![stored(to.height)],
        ChainNotice::Reorg {
            fork,
            new_tip,
            disconnected,
            connected,
            ..
        } => vec![
            SyncEvent::ChainReorganized {
                fork_height: fork.height,
                disconnected: *disconnected,
                connected: *connected,
            },
            stored(new_tip.height),
        ],
        ChainNotice::Resync {
            pin,
            ..
        } => vec![stored(tip_ref(&pin.snap).height)],
    }
}

fn stored(tip_height: u32) -> SyncEvent {
    SyncEvent::BlockHeadersStored {
        tip_height,
    }
}
