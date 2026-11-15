//! `CGovernanceException`: what a refused vote reports, and the peer's penalty.

use std::fmt;

/// `governance_exception_type_enum_t`.
// Core: governance/object.h:27-38
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExceptionType {
    /// Not an error: the vote is ignored.
    None = 0,
    Warning = 1,
    /// The peer is punished.
    PermanentError = 2,
    /// The vote may be valid later.
    TemporaryError = 3,
    InternalError = 4,
}

impl ExceptionType {
    /// `operator<<(std::ostream&, governance_exception_type_enum_t)`.
    // Core: governance/object.cpp:304-325
    pub fn name(self) -> &'static str {
        match self {
            ExceptionType::None => "GOVERNANCE_EXCEPTION_NONE",
            ExceptionType::Warning => "GOVERNANCE_EXCEPTION_WARNING",
            ExceptionType::PermanentError => "GOVERNANCE_EXCEPTION_PERMANENT_ERROR",
            ExceptionType::TemporaryError => "GOVERNANCE_EXCEPTION_TEMPORARY_ERROR",
            ExceptionType::InternalError => "GOVERNANCE_EXCEPTION_INTERNAL_ERROR",
        }
    }
}

/// A `CGovernanceException`: its type, `GetMessage()` (`"<TYPE>:<message>"`) and
/// `GetNodePenalty()`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GovException {
    kind: ExceptionType,
    message: String,
    penalty: i32,
}

impl GovException {
    // Core: governance/object.cpp:327-333
    pub fn new(kind: ExceptionType, message: &str, penalty: i32) -> Self {
        GovException {
            kind,
            message: format!("{}:{message}", kind.name()),
            penalty,
        }
    }

    pub fn kind(&self) -> ExceptionType {
        self.kind
    }

    /// `GetMessage()`, with the type prefix.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// `GetNodePenalty()`: the misbehaviour score for the peer that sent the vote.
    pub fn penalty(&self) -> i32 {
        self.penalty
    }
}

impl fmt::Display for GovException {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for GovException {}
