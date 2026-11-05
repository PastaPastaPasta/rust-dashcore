//! [`CacheSizes`]: Core's split of `-dbcache` among its databases and the coins cache, and its
//! `Cache configuration` log lines (CH2 §2.5; v24, DEC-82).

/// `-dbcache`'s default, in MiB (`txdb.h:31`).
pub const DEFAULT_DB_CACHE: i64 = 300;
/// `-dbcache`'s bounds, in MiB (`txdb.h:35-37`): the maximum is 1,024 on a 32-bit build.
pub const MIN_DB_CACHE: i64 = 4;
pub const MAX_DB_CACHE: i64 = if usize::BITS > 32 {
    16_384
} else {
    1_024
};
/// The shares' caps, in MiB (`txdb.h:39-47`).
const MAX_BLOCK_DB_CACHE: i64 = 2;
const MAX_TX_INDEX_CACHE: i64 = 1_024;
const MAX_FILTER_INDEX_CACHE: i64 = 1_024;
const MAX_COINS_DB_CACHE: i64 = 8;
/// The address, timestamp and spent index caps, in MiB (`node/caches.cpp:25-30`).
const MAX_ADDRESS_INDEX_CACHE: i64 = 256;
const MAX_TIMESTAMP_INDEX_CACHE: i64 = 8;
const MAX_SPENT_INDEX_CACHE: i64 = 128;

/// The index flags `CalculateCacheSizes` reads, and the block filter indexes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexArgs {
    pub txindex: bool,
    pub addressindex: bool,
    pub timestampindex: bool,
    pub spentindex: bool,
    /// The enabled block filter types by name (`BlockFilterTypeName`): Core's
    /// `g_enabled_filter_types`, from which it both sizes and logs the filter indexes.
    pub filters: &'static [&'static str],
}

impl Default for IndexArgs {
    /// Core's defaults: Dash's `-txindex` is on (`index/txindex.h:10`), the others off, and no
    /// block filter index.
    fn default() -> Self {
        Self {
            txindex: true,
            addressindex: false,
            timestampindex: false,
            spentindex: false,
            filters: &[],
        }
    }
}

/// Core's `CacheSizes` (`node/caches.h`), in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheSizes {
    pub block_tree_db: u64,
    pub tx_index: u64,
    pub address_index: u64,
    pub timestamp_index: u64,
    pub spent_index: u64,
    /// Each block filter index's share.
    pub filter_index: u64,
    pub coins_db: u64,
    /// The coins share: `nCoinCacheUsage`, the budget's base (§2.5).
    pub coins: u64,
}

impl CacheSizes {
    /// `CalculateCacheSizes` for `-dbcache=dbcache_mib` and the indexes in `args`, line for line.
    /// Core shifts the MiB in `int64_t`; the shift wraps as its build does.
    pub fn calculate(dbcache_mib: i64, args: &IndexArgs) -> Self {
        Self::calculate_capped(dbcache_mib, args, MAX_DB_CACHE)
    }

    // Core: node/caches.cpp:15-46 CalculateCacheSizes
    /// [`calculate`](Self::calculate) with `max_mib` for [`MAX_DB_CACHE`], which differs between
    /// 64-bit and 32-bit builds.
    fn calculate_capped(dbcache_mib: i64, args: &IndexArgs, max_mib: i64) -> Self {
        let mib = |n: i64| n << 20;
        let share = |on: bool, cap: i64| {
            if on {
                mib(cap)
            } else {
                0
            }
        };
        let mut total = dbcache_mib.wrapping_shl(20).clamp(mib(MIN_DB_CACHE), mib(max_mib));
        let block_tree_db = (total / 8).min(mib(MAX_BLOCK_DB_CACHE));
        total -= block_tree_db;
        let tx_index = (total / 8).min(share(args.txindex, MAX_TX_INDEX_CACHE));
        total -= tx_index;
        let address_index = (total / 8).min(share(args.addressindex, MAX_ADDRESS_INDEX_CACHE));
        total -= address_index;
        let timestamp_index =
            (total / 64).min(share(args.timestampindex, MAX_TIMESTAMP_INDEX_CACHE));
        total -= timestamp_index;
        let spent_index = (total / 8).min(share(args.spentindex, MAX_SPENT_INDEX_CACHE));
        total -= spent_index;
        let mut filter_index = 0;
        if !args.filters.is_empty() {
            let n = args.filters.len() as i64;
            filter_index = (total / 8).min(mib(MAX_FILTER_INDEX_CACHE)) / n;
            total -= filter_index * n;
        }
        // 25–50 % of the rest for the database's own cache, at most 8 MiB.
        let coins_db = (total / 2).min(total / 4 + (1 << 23)).min(mib(MAX_COINS_DB_CACHE));
        total -= coins_db;
        let bytes = |n: i64| u64::try_from(n).expect("a share of a clamped -dbcache");
        Self {
            block_tree_db: bytes(block_tree_db),
            tx_index: bytes(tx_index),
            address_index: bytes(address_index),
            timestamp_index: bytes(timestamp_index),
            spent_index: bytes(spent_index),
            filter_index: bytes(filter_index),
            coins_db: bytes(coins_db),
            coins: bytes(total),
        }
    }

    // Core: init.cpp:1985-2023
    /// Core's `Cache configuration` lines: a line for each enabled index and block filter index
    /// in `args` (the `args` these sizes were calculated from), and `max_mempool`, `-maxmempool`
    /// in bytes, beside the coins share. Core logs every line but the last, then checks
    /// `-maxmempool` (an `InitError` can stop it there), then logs the last: a caller that runs
    /// that check logs the other lines before it.
    pub fn log_lines(&self, args: &IndexArgs, max_mempool: u64) -> Vec<String> {
        let mib = |bytes: u64| bytes as f64 * (1.0 / 1024.0 / 1024.0);
        let mut lines = vec![
            "Cache configuration:".to_owned(),
            format!("* Using {:.1} MiB for block index database", mib(self.block_tree_db)),
        ];
        let indexes = [
            (args.txindex, self.tx_index, "transaction"),
            (args.addressindex, self.address_index, "address"),
            (args.timestampindex, self.timestamp_index, "timestamp"),
            (args.spentindex, self.spent_index, "spent"),
        ];
        for (_, bytes, name) in indexes.into_iter().filter(|(on, ..)| *on) {
            lines.push(format!("* Using {:.1} MiB for {name} index database", mib(bytes)));
        }
        for name in args.filters {
            lines.push(format!(
                "* Using {:.1} MiB for {name} block filter index database",
                mib(self.filter_index)
            ));
        }
        lines.push(format!("* Using {:.1} MiB for chain state database", mib(self.coins_db)));
        lines.push(format!(
            "* Using {:.1} MiB for in-memory UTXO set (plus up to {:.1} MiB of unused mempool space)",
            mib(self.coins),
            mib(max_mempool)
        ));
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `-dbcache=300` with txindex on: block tree 2, txindex 37.25, coins DB 8, coins 252.75 MiB
    /// (DEC-43); with it off, coins 290 MiB.
    #[test]
    fn the_default_split() {
        let on = CacheSizes::calculate(DEFAULT_DB_CACHE, &IndexArgs::default());
        assert_eq!(
            (on.block_tree_db, on.tx_index, on.coins_db, on.coins),
            (2 << 20, 39_059_456, 8 << 20, 265_027_584)
        );
        let off = IndexArgs {
            txindex: false,
            ..IndexArgs::default()
        };
        assert_eq!(CacheSizes::calculate(DEFAULT_DB_CACHE, &off).coins, 290 << 20);
    }

    /// `MAX_DB_CACHE` on a 64-bit build and on a 32-bit one (`txdb.h:35`).
    const MAX_DB_CACHE_64: i64 = 16_384;
    const MAX_DB_CACHE_32: i64 = 1_024;
    /// This build's, from the pointer width, not from `MAX_DB_CACHE`.
    const OWN_MAX_DB_CACHE: i64 = if cfg!(target_pointer_width = "32") {
        MAX_DB_CACHE_32
    } else {
        MAX_DB_CACHE_64
    };

    /// Core's grids in `tests/data`, from a 64-bit build and a 32-bit one, with each build's
    /// maximum `-dbcache`.
    const GRIDS: [(&str, &str, i64); 2] = [
        ("64-bit", include_str!("../tests/data/cache_sizes_v24.txt"), MAX_DB_CACHE_64),
        ("32-bit", include_str!("../tests/data/cache_sizes_v24_32bit.txt"), MAX_DB_CACHE_32),
    ];

    /// Core's numbers for each grid: `-dbcache` from `i64::MIN` to `i64::MAX`, every index flag,
    /// and 0–2 block filter indexes. `calculate` equals the grid of this build's pointer width;
    /// the split with the other width's maximum `-dbcache` equals the other grid.
    #[test]
    fn calculate_equals_cores_grids() {
        assert_eq!(MAX_DB_CACHE, OWN_MAX_DB_CACHE);
        for (width, grid, max_mib) in GRIDS {
            check_grid(width, grid, max_mib);
        }
    }

    /// `calculate` if `max_mib` is this build's maximum `-dbcache`, else the split with it.
    fn split_for(dbcache: i64, args: &IndexArgs, max_mib: i64) -> CacheSizes {
        if max_mib == OWN_MAX_DB_CACHE {
            CacheSizes::calculate(dbcache, args)
        } else {
            CacheSizes::calculate_capped(dbcache, args, max_mib)
        }
    }

    /// Up to two block filter indexes, as the grids count them. Dash defines one filter type;
    /// the split reads only how many there are.
    const FILTERS: [&str; 2] = ["basic"; 2];

    /// Every row of `grid` equals [`split_for`] with `max_mib`.
    fn check_grid(width: &str, grid: &str, max_mib: i64) {
        let mut rows = 0;
        for line in grid.lines() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let f: Vec<i64> = line.split_whitespace().map(|x| x.parse().unwrap()).collect();
            let (head, want) = f.split_at(6);
            let [dbcache, tx, addr, ts, spent, n_filter] = head[..] else {
                panic!("a malformed row: {line}");
            };
            let args = IndexArgs {
                txindex: tx == 1,
                addressindex: addr == 1,
                timestampindex: ts == 1,
                spentindex: spent == 1,
                filters: &FILTERS[..n_filter as usize],
            };
            let s = split_for(dbcache, &args, max_mib);
            let got = [
                s.block_tree_db,
                s.coins_db,
                s.coins,
                s.tx_index,
                s.address_index,
                s.timestamp_index,
                s.spent_index,
                s.filter_index,
            ]
            .map(|b| b as i64);
            assert_eq!(got[..], *want, "{width}: {line}");
            rows += 1;
        }
        assert_eq!(rows, 39 * 16 * 3, "{width}");
    }

    /// A 32-bit build caps `-dbcache` at 1,024 MiB: 2,048 splits as 1,024 does, leaving
    /// 1,063,256,064 bytes of coins with the indexes off.
    #[test]
    fn a_32_bit_build_caps_dbcache_at_1024_mib() {
        let off = IndexArgs {
            txindex: false,
            ..IndexArgs::default()
        };
        let split = |mib| CacheSizes::calculate_capped(mib, &off, MAX_DB_CACHE_32);
        assert_eq!(split(2_048), split(1_024));
        assert_eq!(split(2_048).coins, 1_063_256_064);
    }

    /// The lines a real dashd v24 logs (`tests/data/cache_log_v24.txt`), for the arguments in each
    /// section's header. That dashd is a 64-bit build (v24 ships no 32-bit one): a 64-bit build
    /// checks `calculate`'s split, a 32-bit one the 64-bit split.
    #[test]
    fn log_lines_equal_dashds() {
        let text = include_str!("../tests/data/cache_log_v24.txt");
        let mut sections = 0;
        for section in text.split("\n## ").skip(1) {
            let (header, body) = section.split_once('\n').unwrap();
            let (mut dbcache, mut args, mut mempool_mb) =
                (DEFAULT_DB_CACHE, IndexArgs::default(), 300);
            for arg in header.split_whitespace() {
                let (name, value) = arg.trim_start_matches('-').split_once('=').unwrap();
                let on = value != "0";
                match name {
                    "dbcache" => dbcache = value.parse().unwrap(),
                    "txindex" => args.txindex = on,
                    "addressindex" => args.addressindex = on,
                    "timestampindex" => args.timestampindex = on,
                    "spentindex" => args.spentindex = on,
                    // `1` enables every filter type; Dash has one, `basic`.
                    "blockfilterindex" if on => args.filters = &["basic"],
                    "blockfilterindex" => args.filters = &[],
                    "maxmempool" => mempool_mb = value.parse().unwrap(),
                    _ => panic!("an unknown argument: {arg}"),
                }
            }
            let sizes = split_for(dbcache, &args, MAX_DB_CACHE_64);
            let want: Vec<_> = body.lines().take_while(|l| !l.is_empty()).collect();
            assert_eq!(sizes.log_lines(&args, mempool_mb * 1_000_000), want, "## {header}");
            sections += 1;
        }
        assert_eq!(sections, 12);
    }
}
