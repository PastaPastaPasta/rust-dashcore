//! A port of Dash Core's argument and configuration handling (`ArgsManager`) at v24.0.0-rc.3.
//!
//! It covers command-line parsing, `dash.conf` (network sections, `includeconf`), the
//! read-write `settings.json`, the precedence rules between them, chain selection, the datadir
//! and config paths, help output, and the option tables of Core's programs (generated from
//! Core's sources; see `gen/PROVENANCE.md`). See `ARCHITECTURE.md` for the mapping to Core's
//! files and the deviations.

pub mod args;
pub mod chain;
pub mod init;
pub mod options;
pub mod paths;
pub mod settings;
pub mod strencodings;
pub mod univalue;

pub use args::{
    ArgsManager, Command, KeyInfo, OptionsCategory, ReadConfigError, RegisteredArg, SectionInfo,
    help_requested, setup_help_options,
};
pub use init::{ConfigError, ConfigStatus, init_config};
pub use options::Program;
pub use settings::Settings;
pub use univalue::{RuntimeError, SettingsValue};
