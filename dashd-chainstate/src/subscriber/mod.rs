//! Subscribers (CH2 §2.9, §4.4; CH2b b3): the indexes that follow V's tip under the
//! `SubscriberFeed` contract F1–F11, each on its own engine.
//!
//! - [`FeedTable`]: the producer side the hub and the drivers use, SB5's feeds in the node
//!   ([`ChainFeedTable`]) and `ScriptedFeeds` on the seam (feature `crash`).
//! - The hub that W and the drivers share is [`SubscriberHub`](crate::flush::SubscriberHub).

mod data;
mod driver;
mod error;
#[cfg(any(test, feature = "crash"))]
mod scripted;
mod table;
#[cfg(any(test, feature = "crash"))]
mod testing;
#[cfg(any(test, feature = "crash"))]
mod toy;
#[cfg(test)]
mod watchdog_tests;

pub use data::{
    BlockRows, BlockSource, ConnectedBlock, DisconnectedBlock, DurableBelow, NoBlocks, Row,
    RowSchema, SubscriberData, height_key,
};
#[cfg(feature = "crash")]
pub(crate) use driver::store_err;
pub use driver::{
    DriverConfig, LocatorKind, PRUNE_VIOLATION, STEPPING_THREAD, SubStep, SubscriberClass,
    SubscriberDriver, SyncProgress, decode_locator,
};
pub use error::SubscriberError;
#[cfg(any(test, feature = "crash"))]
pub use scripted::{ScriptedFeeds, ScriptedReader};
pub use table::{ChainFeedTable, FeedTable, SubscriberId};
#[cfg(any(test, feature = "crash"))]
pub use testing::{TreeIndex, tree_hash};
#[cfg(any(test, feature = "crash"))]
pub use toy::{
    TOY_ARCHIVE, TOY_BEST, TOY_DB, TOY_HEIGHT, TOY_INEXACT, TOY_KEYED, TOY_META, ToyIndex,
    ToyState, toy_archived, toy_check_at_l, toy_expected, toy_rows_above,
};
