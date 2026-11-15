//! Arena residency (SB4 §3.4): the node keeps every chunk; SPV evicts below its window.

mod sealed {
    pub trait Sealed {}
}

/// How arena chunks are held. Sealed: [`Permanent`] and [`Evicting`] are the only two.
pub trait Residency: sealed::Sealed + Send + Sync + 'static {
    /// Whether entries can leave the arena (the cold branch is a never-taken `if` otherwise).
    const EVICTS: bool;
}

/// The node: chunks are never released, and readers borrow entries for the arena's lifetime.
#[derive(Clone, Copy, Debug, Default)]
pub struct Permanent;

/// SPV: a chunk is released once all its entries are evicted; an evicted id resolves through
/// the [`crate::HeaderStore`] (SB0 §4.4).
#[derive(Clone, Copy, Debug, Default)]
pub struct Evicting;

impl sealed::Sealed for Permanent {}
impl sealed::Sealed for Evicting {}

impl Residency for Permanent {
    const EVICTS: bool = false;
}

impl Residency for Evicting {
    const EVICTS: bool = true;
}
