//! How an init step fails.

use dash_args::RuntimeError;

/// Why init stopped: Core's `InitError` (`AppInitMain` and friends return `false`) or an
/// exception `AppInit` catches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InitFailure {
    /// The `InitError` text. The init driver shows it ([`crate::ui::Ui::init_error`]).
    Error(String),
    /// A `std::runtime_error` from the arguments (`GetArg` on a malformed setting); Core's
    /// `AppInit` prints it with `PrintExceptionContinue`.
    Exception(String),
    /// Already shown (Core's paths that call `InitError` themselves before more log lines).
    Reported,
}

impl From<RuntimeError> for InitFailure {
    fn from(e: RuntimeError) -> InitFailure {
        InitFailure::Exception(e.0)
    }
}
