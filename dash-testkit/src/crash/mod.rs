//! The device-level power-loss harness (ST3b, feature `crash`, Linux only): `dm-log-writes` logs,
//! the device states a power loss can leave, and their replay onto image files. The privileged
//! device setup and the checks against the node build on these.

pub mod baseline;
pub mod campaign;
pub mod checks;
pub mod devices;
pub mod fixture;
pub mod image;
pub mod inspect;
pub mod logwrites;
pub mod selftest;
