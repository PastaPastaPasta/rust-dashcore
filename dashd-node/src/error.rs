//! How an init step fails.

use dash_args::RuntimeError;
use dashd_masternode::active::ActiveMnInitError;

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

/// An invalid key is Core's `InitError`; an array or object setting is `GetArg`'s exception.
// Core: init.cpp:2156-2159; util/system.cpp:529-535 at v24.0.0
impl From<ActiveMnInitError> for InitFailure {
    fn from(e: ActiveMnInitError) -> InitFailure {
        match e {
            ActiveMnInitError::InvalidKey => InitFailure::Error(e.to_string()),
            ActiveMnInitError::Argument(e) => e.into(),
        }
    }
}
