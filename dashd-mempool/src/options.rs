//! Mempool limits and options with Dash Core v24.0.0 defaults.

/// Package limits from Dash Core v24.0.0 `src/kernel/mempool_limits.h`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemPoolLimits {
    pub ancestor_count: i64,
    pub ancestor_size_vbytes: i64,
    pub descendant_count: i64,
    pub descendant_size_vbytes: i64,
}

impl Default for MemPoolLimits {
    fn default() -> MemPoolLimits {
        MemPoolLimits {
            ancestor_count: 25,
            ancestor_size_vbytes: 101 * 1_000,
            descendant_count: 25,
            descendant_size_vbytes: 101 * 1_000,
        }
    }
}

/// Options from Dash Core v24.0.0 `src/kernel/mempool_options.h`, without component pointers.
/// Fee rates are in duffs per kB (`CFeeRate`); the integer types are Core's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemPoolOptions {
    pub check_ratio: i32,
    /// The fixed vsize policy for a pool lifetime (Core v24.0.0 `src/policy/policy.h`, `src/init.cpp`).
    pub bytes_per_sigop: u32,
    pub max_size_bytes: i64,
    pub expiry_secs: i64,
    pub incremental_relay_feerate: i64,
    pub min_relay_feerate: i64,
    pub dust_relay_feerate: i64,
    pub max_datacarrier_bytes: Option<u32>,
    pub permit_bare_multisig: bool,
    pub require_standard: bool,
    pub address_index_enabled: bool,
    pub spent_index_enabled: bool,
    pub limits: MemPoolLimits,
}

impl Default for MemPoolOptions {
    fn default() -> MemPoolOptions {
        MemPoolOptions {
            check_ratio: 0,
            bytes_per_sigop: 20,
            max_size_bytes: 300 * 1_000_000,
            expiry_secs: 336 * 3600,
            incremental_relay_feerate: 1_000,
            min_relay_feerate: 1_000,
            dust_relay_feerate: 3_000,
            max_datacarrier_bytes: Some(83),
            permit_bare_multisig: true,
            require_standard: true,
            address_index_enabled: false,
            spent_index_enabled: false,
            limits: MemPoolLimits::default(),
        }
    }
}
