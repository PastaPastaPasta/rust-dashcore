//! CH7b's LLMQ integration (CH7 §2.2, §4): the process-wide members facade V validates with, and
//! the routes every other caller takes to it (EV addendum §2.6).
//!
//! - [`cache`]: Core's `unordered_lru_cache` and `PerLlmqTypeCache`, which the facade and the
//!   runtime rows keep (§4.2.1, §4.4);
//! - [`capability`]: `OnValidation`, the sealed proof of running on V under the gate (§2.6);
//! - [`facade`]: `MembersFacade`, its gate, V's attempt handle and route A's raw rows (§4.2.1);
//! - [`members`]: Core's members call over the facade's caches, and the precompute loop (§4.2.2);
//! - [`rows`]: the runtime rotation-snapshot rows, with Core's snapshot-cache sections (§4.4);
//! - [`source`]: `NodeLlmqSource`, the lists and block reads LQ1 computes members from (§4.2.5).

pub mod cache;
mod cap_probes;
pub mod capability;
pub mod facade;
pub mod members;
pub mod rows;
pub mod source;
