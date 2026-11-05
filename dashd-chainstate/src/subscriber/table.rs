//! The feed table the hub and the drivers use (CH2 §2.9.1): SB5's [`ChainFeeds`] in the node,
//! [`ScriptedFeeds`](super::ScriptedFeeds) on the seam. Keyed by the hub's [`SubscriberId`],
//! since SB5's `FeedId` is minted only by `dash-chain`.

use std::sync::{Mutex, MutexGuard, PoisonError};

use dash_chain::{
    BlockRef, ChainFeeds, ChainSnapshot, ControlMsg, FeedId, FeedSource, Handoff, Needs, Notify,
    Refused, SendControl, SubscribeError,
};

use super::SubscriberError;

/// A subscriber's id, assigned by the hub at `register`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SubscriberId(pub u32);

/// The producer side as CH2 uses it: SB5's `ChainFeeds` and `FeedSource` (F1–F11), with the
/// registry snapshot reduced to what step 0 and step 5 read. No call blocks on the tip lock
/// (DEC-157 (2)), and none is ever made under the hub's lock.
pub trait FeedTable: Send + Sync {
    /// P8. `HoldActive` is the caller's to retry ([`ChainFeeds::on_release`]).
    ///
    /// # Errors
    ///
    /// SB5's [`Refused`].
    fn register(&self, id: SubscriberId, needs: Needs) -> Result<(), Refused>;
    /// F11: final once it returns.
    fn deregister(&self, id: SubscriberId);
    fn on_release(&self, w: Notify);
    /// F4.
    fn send_control(&self, id: SubscriberId, c: ControlMsg) -> SendControl;
    /// True once `id` is closing, and for an id the table does not know.
    fn is_closing(&self, id: SubscriberId) -> bool;
    /// H2: the live (Synced) subscribers in one registry snapshot, filtered on closing after it
    /// is read (H7(b)).
    fn synced(&self) -> Vec<SubscriberId>;
    /// W's wake on any slot closing (H4). Set once.
    fn set_close_hook(&self, w: Notify);
    /// A coherent catch-up snapshot (F8).
    fn snapshot(&self) -> Box<dyn ChainSnapshot + '_>;
    /// F10.
    ///
    /// # Errors
    ///
    /// SB5's [`SubscribeError`], with `finish`'s error as `Finish`.
    fn subscribe_at(
        &self,
        id: SubscriberId,
        p: &BlockRef,
        finish: &mut dyn FnMut() -> Result<(), SubscriberError>,
    ) -> Result<Handoff, SubscribeError<SubscriberError>>;
}

/// SB5's feeds behind CH2's ids.
pub struct ChainFeedTable {
    feeds: ChainFeeds,
    /// A leaf, held only to look an id up.
    ids: Mutex<Vec<(SubscriberId, FeedId)>>,
}

impl ChainFeedTable {
    pub fn new(feeds: ChainFeeds) -> Self {
        Self {
            feeds,
            ids: Mutex::default(),
        }
    }

    fn ids(&self) -> MutexGuard<'_, Vec<(SubscriberId, FeedId)>> {
        self.ids.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn feed(&self, id: SubscriberId) -> Option<FeedId> {
        self.ids().iter().find(|(s, _)| *s == id).map(|&(_, f)| f)
    }
}

impl FeedTable for ChainFeedTable {
    fn register(&self, id: SubscriberId, needs: Needs) -> Result<(), Refused> {
        let feed = self.feeds.register(needs)?;
        self.ids().push((id, feed));
        Ok(())
    }

    fn deregister(&self, id: SubscriberId) {
        if let Some(feed) = self.feed(id) {
            self.feeds.deregister(feed);
            self.ids().retain(|(s, _)| *s != id);
        }
    }

    fn on_release(&self, w: Notify) {
        self.feeds.on_release(w);
    }

    fn send_control(&self, id: SubscriberId, c: ControlMsg) -> SendControl {
        match self.feed(id) {
            Some(feed) => self.feeds.send_control(feed, c),
            None => SendControl::Stale,
        }
    }

    fn is_closing(&self, id: SubscriberId) -> bool {
        self.feed(id).is_none_or(|feed| self.feeds.is_closing(feed))
    }

    fn synced(&self) -> Vec<SubscriberId> {
        let registry = self.feeds.registry();
        let ids = self.ids();
        registry
            .feeds
            .iter()
            .filter(|e| e.seq_synced.is_some() && !e.is_closing())
            .filter_map(|e| ids.iter().find(|(_, f)| *f == e.id).map(|&(s, _)| s))
            .collect()
    }

    fn set_close_hook(&self, w: Notify) {
        self.feeds.set_close_hook(w);
    }

    fn snapshot(&self) -> Box<dyn ChainSnapshot + '_> {
        FeedSource::<SubscriberError>::snapshot(&self.feeds)
    }

    fn subscribe_at(
        &self,
        id: SubscriberId,
        p: &BlockRef,
        finish: &mut dyn FnMut() -> Result<(), SubscriberError>,
    ) -> Result<Handoff, SubscribeError<SubscriberError>> {
        let feed = self.feed(id).ok_or(SubscribeError::Stale)?;
        self.feeds.subscribe_at(feed, p, finish)
    }
}
