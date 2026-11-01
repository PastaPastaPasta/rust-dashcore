//! The power-loss harness (ST3b, feature `crash`, Linux only). The device level: `dm-log-writes`
//! logs, the device states a power loss can leave, and their replay onto image files, with the
//! privileged device setup and the checks against the node built on these. The VM level
//! ([`vm`], S7): rdashd in a KVM guest whose QEMU is killed, checked through the guest's RPC.

pub mod baseline;
pub mod campaign;
pub mod checks;
pub mod devices;
pub mod faults;
pub mod fixture;
pub mod image;
pub mod inspect;
pub mod logwrites;
pub mod selftest;
pub mod trace;
pub mod vm;
