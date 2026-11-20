//! Core v24's option defaults and limits that init reads (the headers are named per group).

// net.h, net_processing.h, netbase.h
pub const DEFAULT_LISTEN: bool = true;
pub const DEFAULT_LISTEN_ONION: bool = true;
pub const DEFAULT_MAX_PEER_CONNECTIONS: i64 = 125;
pub const DEFAULT_MAX_UPLOAD_TARGET: &str = "0M";
/// Core's `DEFAULT_BLOCKSONLY` is `false` (net.h:128). rdashd runs Core's `-blocksonly=1` until P5
/// brings the mempool (DEC-322, P5a D23); P5 restores `false` (BL-330).
pub const DEFAULT_BLOCKSONLY: bool = true;
pub const DEFAULT_PEER_CONNECT_TIMEOUT: i64 = 60;
pub const NUM_FDS_MESSAGE_CAPTURE: i32 = 1;
pub const MAX_ADDNODE_CONNECTIONS: i32 = 8;
pub const DEFAULT_FORCEDNSSEED: bool = false;
pub const DEFAULT_DNSSEED: bool = true;
pub const DEFAULT_V2_TRANSPORT: bool = true;
pub const DEFAULT_CONNECT_TIMEOUT: i64 = 5000;
pub const DEFAULT_NAME_LOOKUP: bool = true;
/// init.cpp:187.
pub const DEFAULT_PROXYRANDOMIZE: bool = true;
pub const DEFAULT_ALLOWPRIVATENET: bool = false;
pub const DEFAULT_WHITELISTRELAY: bool = true;
pub const DEFAULT_WHITELISTFORCERELAY: bool = false;
pub const DEFAULT_PEERBLOOMFILTERS: bool = true;
pub const DEFAULT_PEERBLOCKFILTERS: bool = false;

// init.cpp, index/*.h, validation.h, node/blockstorage.h
pub const DEFAULT_TXINDEX: bool = true;
pub const DEFAULT_ADDRESSINDEX: bool = false;
pub const DEFAULT_SPENTINDEX: bool = false;
pub const DEFAULT_TIMESTAMPINDEX: bool = false;
pub const DEFAULT_COINSTATSINDEX: bool = false;
pub const DEFAULT_BLOCKFILTERINDEX: &str = "0";
pub const DEFAULT_GOVERNANCE_ENABLE: bool = true;
/// Whether governance is on when `-disablegovernance` is not set. Core's is
/// [`DEFAULT_GOVERNANCE_ENABLE`]; rdashd runs Core's `-disablegovernance=1` until GV1b ports the
/// governance manager (DEC-510, SP1 D9), and GV1b restores Core's default (BL-423). Core's checks
/// that read the option keep Core's default.
pub const RDASHD_GOVERNANCE_ENABLE: bool = false;
pub const DEFAULT_BYTES_PER_SIGOP: i64 = 20;
pub const MIN_DISK_SPACE_FOR_BLOCK_FILES: u64 = 945 * 1024 * 1024;
pub const DEFAULT_ENABLE_QUORUM_DATA_RECOVERY: bool = true;
pub const MAX_SCRIPTCHECK_THREADS: i64 = 15;
pub const DEFAULT_SCRIPTCHECK_THREADS: i64 = 0;
/// `MIN_CORE_FILEDESCRIPTORS`: 0 on Windows, where `RaiseFileDescriptorLimit` is a constant.
pub const MIN_CORE_FILEDESCRIPTORS: i32 = if cfg!(windows) {
    0
} else {
    150
};

// kernel/mempool_options.h, policy/policy.h, policy/feerate.h
pub const DEFAULT_BLOCKSONLY_MAX_MEMPOOL_SIZE_MB: i64 = 5;
pub const DEFAULT_MAX_MEMPOOL_SIZE_MB: i64 = 300;
pub const DEFAULT_MEMPOOL_EXPIRY_HOURS: i64 = 336;
pub const DEFAULT_ACCEPT_NON_STD_TXN: bool = false;
pub const DEFAULT_INCREMENTAL_RELAY_FEE: i64 = 1000;
pub const DEFAULT_MIN_RELAY_TX_FEE: i64 = 1000;
pub const DUST_RELAY_TX_FEE: i64 = 3000;
pub const DEFAULT_PERMIT_BAREMULTISIG: bool = true;
pub const DEFAULT_ACCEPT_DATACARRIER: bool = true;
pub const MAX_OP_RETURN_RELAY: i64 = 83;
pub const DEFAULT_ANCESTOR_LIMIT: i64 = 25;
pub const DEFAULT_ANCESTOR_SIZE_LIMIT_KVB: i64 = 101;
pub const DEFAULT_DESCENDANT_LIMIT: i64 = 25;
pub const DEFAULT_DESCENDANT_SIZE_LIMIT_KVB: i64 = 101;

// script/sigcache.h
pub const DEFAULT_MAX_SIG_CACHE_BYTES: i64 = 32 << 20;
