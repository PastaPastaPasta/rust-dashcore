//! V tests.

// Test gates and waits use raw channels; LO-9's ban is for the crate's own wait paths.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]

mod abc;
mod actor;
mod attempt;
mod blocks;
mod connect;
mod connector;
mod deployments;
#[cfg(feature = "statedigest")]
mod digest_tap;
mod failure;
mod flush;
mod index;
mod locks;
mod node_evo;
mod node_stores;
mod queue_hooks;
mod signals;
mod stop;
mod support;
mod verify;
mod view;
