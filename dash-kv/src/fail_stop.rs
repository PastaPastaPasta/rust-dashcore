//! The fail-stop latch every engine applies (the "Errors" contract of [`KvEngine`]): the API an
//! engine implementation uses to follow it.
//!
//! [`KvEngine`]: crate::KvEngine

use std::sync::OnceLock;

use crate::error::{ErrorKind, Result, StoreError};

/// A latched failure: its kind and how it displayed.
pub(crate) type Failure = (ErrorKind, String);

pub(crate) fn failure_of(err: &StoreError) -> Failure {
    (err.kind().unwrap_or(ErrorKind::Other), err.to_string())
}

/// What every write and barrier returns once `first` is latched.
pub(crate) fn refusal(engine: &'static str, (_, first): &Failure) -> StoreError {
    StoreError::engine(
        engine,
        ErrorKind::ReadOnly,
        format!(
            "refusing writes after an earlier failure ({first}); the database must be reopened"
        ),
    )
}

/// Remembers an engine's first write, barrier or background failure. Once set, every write and
/// barrier that starts later is refused with [`ErrorKind::ReadOnly`] until the engine is reopened
/// (a new engine value): nothing can be committed after a commit whose outcome is unknown.
#[derive(Debug, Default)]
pub struct FailStop {
    first: OnceLock<Failure>,
}

impl FailStop {
    /// Refused if a failure is latched. A write or barrier runs it first, and latches its own
    /// failure with [`latch`](Self::latch).
    pub fn check(&self, engine: &'static str) -> Result<()> {
        match self.first.get() {
            Some(first) => Err(refusal(engine, first)),
            None => Ok(()),
        }
    }

    /// Latches `err` unless a failure is latched already, and returns it.
    pub fn latch(&self, err: StoreError) -> StoreError {
        let _ = self.first.set(failure_of(&err));
        err
    }

    /// The latched failure's kind, if any.
    pub fn failed(&self) -> Option<ErrorKind> {
        self.first.get().map(|(kind, _)| *kind)
    }
}
