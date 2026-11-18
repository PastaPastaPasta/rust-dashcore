//! Constants from Dash Core v24.0.0 `policy/policy.h:23-83,95`.

use dash_consensus::script::ScriptFlags;
use dash_consensus::tx::locks::LockTimeFlags;

pub const DEFAULT_BLOCK_MAX_SIZE: u32 = 2_000_000;
pub const DEFAULT_BLOCK_MIN_TX_FEE: u32 = 1_000;
pub const MAX_STANDARD_TX_SIZE: u32 = 100_000;
pub const MIN_STANDARD_TX_SIZE: u32 = 65;
pub const MAX_P2SH_SIGOPS: u32 = 15;
pub const MAX_STANDARD_TX_SIGOPS: u32 = 4_000;
pub const DEFAULT_INCREMENTAL_RELAY_FEE: u32 = 1_000;
pub const DEFAULT_BYTES_PER_SIGOP: u32 = 20;
pub const DEFAULT_PERMIT_BAREMULTISIG: bool = true;
pub const MAX_STANDARD_SCRIPTSIG_SIZE: u32 = 1_650;
pub const DUST_RELAY_TX_FEE: u32 = 3_000;
pub const DEFAULT_MIN_RELAY_TX_FEE: u32 = 1_000;
pub const DEFAULT_ANCESTOR_LIMIT: u32 = 25;
pub const DEFAULT_ANCESTOR_SIZE_LIMIT_KVB: u32 = 101;
pub const DEFAULT_DESCENDANT_LIMIT: u32 = 25;
pub const DEFAULT_DESCENDANT_SIZE_LIMIT_KVB: u32 = 101;
pub const EXTRA_DESCENDANT_TX_SIZE_LIMIT: u32 = 10_000;
pub const STANDARD_SCRIPT_VERIFY_FLAGS: ScriptFlags = ScriptFlags::STANDARD;
pub const STANDARD_NOT_MANDATORY_VERIFY_FLAGS: ScriptFlags =
    STANDARD_SCRIPT_VERIFY_FLAGS.difference(ScriptFlags::MANDATORY_V24_0);
pub const STANDARD_LOCKTIME_VERIFY_FLAGS: LockTimeFlags = LockTimeFlags::VERIFY_SEQUENCE;
pub const TX_MAX_STANDARD_VERSION: i16 = 3;
