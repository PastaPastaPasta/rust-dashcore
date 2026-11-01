//! `OnValidation` (EV addendum §2.6): proof that a call runs on V with the facade's gate held, by
//! a dispatched command (route B, [`OnV`]) or an open attempt (route C). Sealed: only this module
//! implements it, and only the facade's own capability types can. What a capability reads through
//! is crate-private ([`Capability`]): no caller outside the crate reaches the caches or an ungated
//! row writer.

use dash_dml::StoreError;
use dash_evo::EffectSink;
use dash_evo::llmq::SnapshotRows;

pub use crate::chain::OnV;

use super::facade::{AttemptHandle, MembersFacade};
use super::members::{AttemptRows, InlineRows};

/// A capability of running on V under the facade's gate. The quorum reads (members, the
/// materializer, the composite builders) take one.
pub trait OnValidation: sealed::Sealed {}

/// The capability rules as code outside the crate sees them, each a `compile_fail` doctest.
#[cfg(doctest)]
#[doc = include_str!("capability_rules.md")]
pub struct CapabilityRules;

mod sealed {
    pub trait Sealed {}
}

/// What the facade's calls read through a capability.
pub(crate) trait Capability: OnValidation {
    /// The facade whose gate the capability holds.
    fn facade(&self) -> &MembersFacade;

    /// The rows a members call reads and writes: an attempt's go to its overlay and its log, a
    /// dispatched command's to the runtime rows at once.
    fn rows<'s>(
        &'s self,
        effects: &'s mut dyn EffectSink,
    ) -> Box<dyn SnapshotRows<Error = StoreError> + 's>;
}

impl sealed::Sealed for OnV<'_> {}

impl OnValidation for OnV<'_> {}

impl Capability for OnV<'_> {
    fn facade(&self) -> &MembersFacade {
        self.token().facade()
    }

    fn rows<'s>(
        &'s self,
        _: &'s mut dyn EffectSink,
    ) -> Box<dyn SnapshotRows<Error = StoreError> + 's> {
        Box::new(InlineRows {
            facade: self.token().facade(),
        })
    }
}

impl sealed::Sealed for AttemptHandle<'_> {}

impl OnValidation for AttemptHandle<'_> {}

impl Capability for AttemptHandle<'_> {
    fn facade(&self) -> &MembersFacade {
        AttemptHandle::facade(self)
    }

    fn rows<'s>(
        &'s self,
        effects: &'s mut dyn EffectSink,
    ) -> Box<dyn SnapshotRows<Error = StoreError> + 's> {
        Box::new(AttemptRows {
            overlay: self.overlay(),
            backing: self.facade().runtime_rows(),
            sink: effects,
        })
    }
}

/// Case 6's negative control: route B's reads on the caller's thread, with neither V's dispatch
/// nor the gate.
#[cfg(test)]
pub(crate) struct Ungated<'f>(pub(crate) &'f MembersFacade);

#[cfg(test)]
impl sealed::Sealed for Ungated<'_> {}

#[cfg(test)]
impl OnValidation for Ungated<'_> {}

#[cfg(test)]
impl Capability for Ungated<'_> {
    fn facade(&self) -> &MembersFacade {
        self.0
    }

    fn rows<'s>(
        &'s self,
        _: &'s mut dyn EffectSink,
    ) -> Box<dyn SnapshotRows<Error = StoreError> + 's> {
        Box::new(InlineRows {
            facade: self.0,
        })
    }
}
