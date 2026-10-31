//! `AppInitMain`'s argument checks before step 4a (the upload target and the validation cache
//! sizes, CONS-23), and the validation caches and script-check threads step 4a starts.

use dash_args::ArgsManager;
use dashd_chainstate::caches::{CacheSizes, DEFAULT_DB_CACHE, IndexArgs};
use dashd_chainstate::checkqueue::{CheckQueue, DEFAULT_SCRIPTCHECK_THREADS, script_check_threads};
use dashd_chainstate::sigcache::{
    CacheError, CacheSizes as SigCacheSizes, ScriptCaches, ScriptExecCache, SigCache,
};
use dashd_chainstate::tuning::DEFAULT_DB_BATCH_SIZE;

use super::NetSetup;
use super::consts::{
    DEFAULT_ADDRESSINDEX, DEFAULT_MAX_SIG_CACHE_BYTES, DEFAULT_MAX_UPLOAD_TARGET,
    DEFAULT_SPENTINDEX, DEFAULT_TIMESTAMPINDEX, DEFAULT_TXINDEX,
};
use super::parse::{ByteUnit, parse_byte_units};
use crate::InitFailure;
use crate::logging::Logger;

/// The indexes step 7 sizes caches for, and `CalculateCacheSizes` over `-dbcache`.
// Core: init.cpp:1983-1992
pub fn cache_sizes(
    args: &ArgsManager,
    net: &NetSetup,
) -> Result<(IndexArgs, CacheSizes), InitFailure> {
    let indexes = IndexArgs {
        txindex: args.get_bool_arg("-txindex", DEFAULT_TXINDEX)?,
        addressindex: args.get_bool_arg("-addressindex", DEFAULT_ADDRESSINDEX)?,
        timestampindex: args.get_bool_arg("-timestampindex", DEFAULT_TIMESTAMPINDEX)?,
        spentindex: args.get_bool_arg("-spentindex", DEFAULT_SPENTINDEX)?,
        filters: if net.basic_filter_index {
            &["basic"]
        } else {
            &[]
        },
    };
    let sizes = CacheSizes::calculate(args.get_int_arg("-dbcache", DEFAULT_DB_CACHE)?, &indexes);
    Ok((indexes, sizes))
}

/// `-dbbatchsize`: the coins flush's sub-batch limit in bytes. Core reads it as a `size_t`, so a
/// negative value wraps to the maximum and 0 closes a sub-batch after every entry. The flush's
/// sub-batch is [`tuning::sub_batch`](dashd_chainstate::tuning::sub_batch) of it, which also caps
/// it by the coins share (CH2 §2.5).
// Core: txdb.cpp:117
pub fn db_batch_size(args: &ArgsManager) -> Result<u64, InitFailure> {
    let value = args.get_int_arg("-dbbatchsize", DEFAULT_DB_BATCH_SIZE as i64)?;
    Ok(value as usize as u64)
}

/// `-maxuploadtarget` in bytes (0: no limit).
// Core: init.cpp:1520-1523
pub fn max_upload_target(args: &ArgsManager) -> Result<u64, InitFailure> {
    let value = args.get_arg("-maxuploadtarget", DEFAULT_MAX_UPLOAD_TARGET)?;
    parse_byte_units(&value, ByteUnit::M)
        .ok_or_else(|| InitFailure::Error(format!("Unable to parse -maxuploadtarget: '{value}'")))
}

/// One cache as `CuckooCache::cache::setup_bytes` sizes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheSetup {
    pub num_elems: u32,
    pub approx_size_bytes: u64,
}

/// Both caches hold `uint256` entries.
const ELEMENT_BYTES: u64 = 32;

/// Core's `setup_bytes`: at least two elements; `None` if more than `u32::MAX` are asked for.
// Core: cuckoocache.h:337-376
pub fn cache_setup(bytes: u64) -> Option<CacheSetup> {
    let requested = u32::try_from(bytes / ELEMENT_BYTES).ok()?;
    let num_elems = requested.max(2);
    Some(CacheSetup {
        num_elems,
        approx_size_bytes: u64::from(num_elems) * ELEMENT_BYTES,
    })
}

/// Core's `ApplyArgsManOptions(args, validation_cache_sizes)` over `-maxsigcachesize` (`mib`)
/// and the sizing of `InitSignatureCache` and `InitScriptExecutionCache`, with their log lines:
/// the sizes, or Core's error for a cache it cannot set up. Core logs each line once its table
/// is allocated; both are logged first here, which differs only when an allocation fails.
// Core: init.cpp:1543-1552, node/validation_cache_args.cpp:21-35, script/sigcache.cpp:82-91,
// validation.cpp:2115-2132
pub fn log_validation_cache_sizes(
    logger: &Logger,
    mib: Option<i64>,
) -> Result<SigCacheSizes, InitFailure> {
    let sizes = SigCacheSizes::from_max_sig_cache_size(mib);
    for (bytes, name) in [
        (sizes.signature_cache_bytes, "signature cache"),
        (sizes.script_execution_cache_bytes, "script execution cache"),
    ] {
        let setup = cache_setup(bytes as u64).ok_or_else(|| sig_cache_error(mib))?;
        logger.log_printf(&format!(
            "Using {} MiB out of {} MiB requested for {name}, able to store {} elements\n",
            setup.approx_size_bytes >> 20,
            bytes >> 20,
            setup.num_elems
        ));
    }
    Ok(sizes)
}

/// The two caches [`log_validation_cache_sizes`] sizes, each with a random salt, as init builds
/// them at step 4a: V's connector checks scripts against them (CH3a).
///
/// # Errors
///
/// [`log_validation_cache_sizes`]'s, Core's error for a cache that cannot be allocated, and an
/// exception for a salt the OS cannot give.
// Core: init.cpp:1546-1552
pub fn init_validation_caches(
    logger: &Logger,
    args: &ArgsManager,
) -> Result<ScriptCaches, InitFailure> {
    let mib = args.get_int_arg_opt("-maxsigcachesize")?;
    let sizes = log_validation_cache_sizes(logger, mib)?;
    let mut nonces = [[0; 32]; 2];
    getrandom::fill(nonces.as_flattened_mut())
        .map_err(|_| InitFailure::Exception(CacheError::Rng.to_string()))?;
    let [sig_nonce, exec_nonce] = nonces;
    // `log_validation_cache_sizes` refused a cache too large for `with_nonce`: only an
    // allocation fails here.
    let refused = |_: CacheError| sig_cache_error(mib);
    Ok(ScriptCaches {
        sig: SigCache::with_nonce(sizes.signature_cache_bytes, sig_nonce).map_err(refused)?,
        exec: ScriptExecCache::with_nonce(sizes.script_execution_cache_bytes, exec_nonce)
            .map_err(refused)?,
    })
}

/// Core's `GetNumCores()`, `std::thread::hardware_concurrency()`: libstdc++ returns
/// `get_nprocs()`, the online processors, whatever the affinity mask or the cgroup's quota.
/// The parity is Linux's: on macOS and the BSDs libc++ reads `hw.ncpu`, which can exceed
/// `_SC_NPROCESSORS_ONLN` (`hw.activecpu`) while CPUs are disabled, and off unix
/// `available_parallelism` may honour the affinity mask.
// Core: util/system.cpp:1211-1214
pub fn get_num_cores() -> usize {
    #[cfg(unix)]
    {
        // SAFETY: `sysconf` reads a constant.
        let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
        usize::try_from(n).unwrap_or(0)
    }
    #[cfg(not(unix))]
    std::thread::available_parallelism().map_or(0, |n| n.get())
}

/// Core's script-check threads: `-par`'s count on [`get_num_cores`] ([`script_check_threads`]),
/// its log line, and the queue V's connector checks scripts on, without threads at 0 (`-par=1`).
/// The threads are told to exit when the queue's last holder drops it, after V stops (rayon does
/// not join them); Core joins them at `Shutdown` (`init.cpp:356`), after the scheduler and before
/// the flush.
///
/// # Errors
///
/// An exception for threads the OS cannot start (Core's `std::thread` throws).
// Core: init.cpp:1554-1570
pub fn start_script_check_threads(
    logger: &Logger,
    args: &ArgsManager,
) -> Result<CheckQueue, InitFailure> {
    let par = args.get_int_arg("-par", DEFAULT_SCRIPTCHECK_THREADS)?;
    let threads = script_check_threads(par, get_num_cores());
    logger.log_printf(&format!("Script verification uses {threads} additional threads\n"));
    CheckQueue::new(threads).map_err(|e| InitFailure::Exception(e.to_string()))
}

/// Core's error for `-maxsigcachesize`, with the value as `GetIntArg` reads it.
fn sig_cache_error(mib: Option<i64>) -> InitFailure {
    let mib = mib.unwrap_or(DEFAULT_MAX_SIG_CACHE_BYTES >> 20);
    InitFailure::Error(format!("Unable to allocate memory for -maxsigcachesize: '{mib}' MiB"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init::{test_args, test_logger};

    #[test]
    fn upload_target() {
        assert_eq!(max_upload_target(&test_args(&[])).unwrap(), 0);
        assert_eq!(max_upload_target(&test_args(&["-maxuploadtarget=2"])).unwrap(), 2 << 20);
        assert_eq!(max_upload_target(&test_args(&["-maxuploadtarget=5k"])).unwrap(), 5000);
        assert_eq!(
            max_upload_target(&test_args(&["-maxuploadtarget=1.5G"])),
            Err(InitFailure::Error("Unable to parse -maxuploadtarget: '1.5G'".to_owned()))
        );
    }

    /// The caches init builds are the size `-maxsigcachesize` asks for, and log Core's lines.
    #[test]
    fn default_caches_log_core_s_lines() {
        let (logger, lines) = test_logger();
        let caches = init_validation_caches(&logger, &test_args(&[])).unwrap();
        assert_eq!(
            *lines.lock().unwrap(),
            [
                "Using 16 MiB out of 16 MiB requested for signature cache, able to store 524288 elements\n",
                "Using 16 MiB out of 16 MiB requested for script execution cache, able to store 524288 elements\n",
            ]
        );
        let table = caches.sig.cache().table_bytes();
        assert!(table > 15 << 20 && table <= 16 << 20, "{table}");
        let caches = init_validation_caches(&logger, &test_args(&["-maxsigcachesize=2"])).unwrap();
        let table = caches.sig.cache().table_bytes();
        assert!(table > 0 && table <= 1 << 20, "-maxsigcachesize sizes the caches: {table}");
    }

    /// `GetNumCores()` counts the online processors (`/sys/devices/system/cpu/online`, which
    /// glibc's `get_nprocs()` reads first), also on a thread pinned to one CPU, the first this
    /// process may use. Skipped without `/sys`.
    #[cfg(target_os = "linux")]
    #[test]
    fn num_cores_are_the_online_processors() {
        let Ok(online) = std::fs::read_to_string("/sys/devices/system/cpu/online") else {
            eprintln!("skipped: no /sys/devices/system/cpu/online");
            return;
        };
        let count = |range: &str| match range.split_once('-') {
            Some((a, b)) => b.parse::<usize>().unwrap() - a.parse::<usize>().unwrap() + 1,
            None => 1,
        };
        let online: usize = online.trim().split(',').map(count).sum();
        assert_eq!(get_num_cores(), online);
        let pinned = std::thread::spawn(|| {
            // SAFETY: a zeroed `cpu_set_t` is empty; `sched_getaffinity` fills `allowed`, and
            // `sched_setaffinity` changes this thread's mask only.
            unsafe {
                let size = size_of::<libc::cpu_set_t>();
                let mut allowed: libc::cpu_set_t = std::mem::zeroed();
                assert_eq!(libc::sched_getaffinity(0, size, &mut allowed), 0);
                let first = (0..libc::CPU_SETSIZE as usize)
                    .find(|&cpu| libc::CPU_ISSET(cpu, &allowed))
                    .unwrap();
                let mut set: libc::cpu_set_t = std::mem::zeroed();
                libc::CPU_SET(first, &mut set);
                assert_eq!(libc::sched_setaffinity(0, size, &set), 0);
            }
            assert_eq!(std::thread::available_parallelism().unwrap().get(), 1);
            get_num_cores()
        });
        assert_eq!(pinned.join().unwrap(), online);
    }

    /// Step 4a's script-check threads: `-par`'s count on the online processors, Core's line, and
    /// a queue with that many threads, none at `-par=1`.
    // Core: init.cpp:1554-1570
    #[test]
    fn script_check_threads_follow_par() {
        // Auto: one fewer than the cores, at most 15.
        let auto = get_num_cores().saturating_sub(1).min(15);
        let cases: [(&[&str], usize); 7] = [
            (&[], auto),
            (&["-par=0"], auto),
            (&["-nopar"], auto),
            (&["-par=1"], 0),
            (&["-par=3"], 2),
            (&["-par=99"], 15),
            (&["-par=-1000"], 0),
        ];
        for (argv, want) in cases {
            let (logger, lines) = test_logger();
            let queue = start_script_check_threads(&logger, &test_args(argv)).unwrap();
            assert_eq!((queue.threads(), queue.has_threads()), (want, want > 0), "{argv:?}");
            let line = format!("Script verification uses {want} additional threads\n");
            assert_eq!(*lines.lock().unwrap(), [line], "{argv:?}");
        }
    }

    /// `-dbbatchsize` in bytes, read as Core's `size_t`: the default is 16 MiB, 0 stays 0 (a
    /// sub-batch after every entry) and a negative value wraps.
    // Core: txdb.cpp:117, txdb.h:33
    #[test]
    fn db_batch_size_is_a_size_t() {
        let size = |argv: &[&str]| db_batch_size(&test_args(argv)).unwrap();
        assert_eq!(size(&[]), 16 << 20);
        assert_eq!(size(&["-dbbatchsize=200000"]), 200_000);
        assert_eq!(size(&["-dbbatchsize=0"]), 0);
        assert_eq!(size(&["-dbbatchsize=-1"]), usize::MAX as u64);
        assert_eq!(size(&["-nodbbatchsize"]), 0);
    }

    /// `-maxsigcachesize` in MiB, split evenly, clamped at 0; Core's error past `u32::MAX`
    /// elements, before any cache is allocated.
    // Core: node/validation_cache_args.cpp:21-35, cuckoocache.h:368
    #[test]
    fn cache_size_bounds() {
        let (logger, lines) = test_logger();
        let sizes = |mib: Option<i64>| log_validation_cache_sizes(&logger, mib);
        let each = |bytes: usize| SigCacheSizes {
            signature_cache_bytes: bytes,
            script_execution_cache_bytes: bytes,
        };
        assert_eq!(sizes(None).unwrap(), each(16 << 20));
        assert_eq!(sizes(Some(3)).unwrap(), each(3 << 19));
        assert_eq!(sizes(Some(-5)).unwrap(), each(0));
        assert!(
            lines.lock().unwrap().iter().any(|l| l
                == "Using 0 MiB out of 0 MiB requested for signature cache, able to store 2 elements\n")
        );
        // 256 GiB gives each cache 2^32 elements, one too many.
        assert_eq!(sizes(Some(262143)).unwrap(), each(262143 << 19));
        let refused = |mib: &str| {
            Err(InitFailure::Error(format!(
                "Unable to allocate memory for -maxsigcachesize: '{mib}' MiB"
            )))
        };
        assert_eq!(sizes(Some(262144)), refused("262144"));
        assert_eq!(sizes(Some(i64::MAX)), refused("9223372036854775807"));
    }
}
