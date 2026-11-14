//! Core-format net-state files (IM1a; A1 §2.21, FR-P2P-19, FR-MEM-07): detection and parsing of
//! the files a Dash Core v24 datadir keeps besides its databases:
//!
//! | file | module | form |
//! |---|---|---|
//! | `peers.dat` | [`peers`] | magic, `AddrMan` (format 0 to 4), SHA256d |
//! | `anchors.dat` | [`anchors`] | magic, `vector<CAddress>`, SHA256d |
//! | `banlist.json` | [`banlist`] | JSON, key `banned_nets` |
//! | `mempool.dat` | [`mempool`] | `u64` version 1, entries, deltas, unbroadcast |
//! | `fee_estimates.dat` | [`fee_estimates`] | version words, buckets, three stats blocks |
//! | `settings.json` | [`settings`] | JSON object |
//! | `governance.dat` | `governance` | `CFlatDB` envelope, objects, votes, the masternode list (IM1b) |
//! | `mncache.dat` | [`mncache`] | `CFlatDB` envelope, masternode meta records, `nDsqCount` (IM1b) |
//! | `netfulfilled.dat` | [`netfulfilled`] | `CFlatDB` envelope, per-service request expiries (IM1b) |
//! | `sporks.dat` | [`sporks`] | `CFlatDB` envelope, signed spork messages (IM1b) |
//!
//! The readers are the future owning crates' loaders' building blocks (`dashd-p2p`'s address
//! manager and ban manager, `dashd-mempool`, `dash-args`); they only decode. Each is a total
//! function over untrusted bytes: it never panics, returns a typed [`FormatError`], and reads the
//! bytes it is given and nothing else (no files, no clocks, no network). [`read_file_capped`] is
//! the one function that touches a file.
//!
//! # Core's semantics, with bounds only (DEC-137)
//!
//! A reader accepts and keeps what Core v24.0.0-rc.3's loader accepts and keeps, ported from its
//! source with the function cited at each reader (`addrdb.cpp`, `addrman.cpp`, `net_types.cpp`,
//! `netbase.cpp`, `kernel/mempool_persist.cpp`, `policy/fees.cpp`, `util/settings.cpp`,
//! `univalue_read.cpp`): UniValue's object semantics, the banlist's per-entry order of checks, the
//! mempool's retained prefix and its map and set duplicate handling, numbers kept as written.
//! `tests/netstate.rs` checks the readers against files a real regtest `dashd` wrote and the RPC
//! output it gave, and against Core's behaviour on the corner cases of each reader.
//!
//! The **only** differences are the hostile-input bounds below. Everything else that looks like a
//! difference is a bug.
//!
//! | Bound | Value | Core | Reason |
//! |---|---|---|---|
//! | Input size, `peers.dat` | 16 MiB (`peers::MAX_PEERS_DAT_SIZE`) | no cap; streams the file | files Core writes are under 10 MiB (65,536 new and 16,384 tried entries, 1,024 buckets of 64 indexes) |
//! | Input size, `anchors.dat` | 1 MiB (`anchors::MAX_ANCHORS_DAT_SIZE`) | no cap | at most a few dozen entries |
//! | Input size, `mempool.dat` | 1 GiB (`mempool::MAX_MEMPOOL_DAT_SIZE`) | no cap; loads the prefix of any size | the default `-maxmempool` is 300 MB in memory and its dump is smaller; a node run with a much larger `-maxmempool` can write a dump over the cap, which this reader refuses as a whole (Core would apply it) |
//! | Input size, `fee_estimates.dat` | 64 MiB (`fee_estimates::MAX_FEE_ESTIMATES_SIZE`) | no cap | at most 1,000 buckets by 1,008 confirms by 3 horizons of `f64` is under 50 MiB |
//! | Input size, `banlist.json` | 16 MiB ([`MAX_JSON_FILE_SIZE`]) | no cap | a few thousand bans of about 150 bytes (100,000 fit); the memory this admits is the next row's |
//! | Input size, `settings.json` | 1 MiB ([`MAX_SETTINGS_FILE_SIZE`]) | no cap | a few dozen keys; the same memory row applies |
//! | Memory of a parsed JSON file | at most **72 heap bytes per input byte** on glibc (64 requested bytes), plus a fixed 40 KiB per parse: a peak of at most 73 times the file with the input itself, so 1,168 MiB (1.14 GiB) for a 16 MiB `banlist.json` and 73 MiB for `settings.json`, plus the process's own few MiB | the same tree per node, with no cap on the file | derived, below the table, and checked by `tests/netstate_memory.rs`; the input size caps above are what make it finite |
//! | Memory of a parsed `mempool.dat` | at most **5 requested bytes per input byte** (derived below and checked by `tests/netstate_memory.rs`), so at most 6 times the file with the input itself (measured requested bytes at 1 MiB: 4.0 for records, 1.6 for the map and the set); measured RSS was 3.46 times at 256 MiB (905,744 KiB), about 3.5 GiB at the 1 GiB cap (extrapolated), 50 s at 256 MiB at load average 96 | Core streams the file and applies each record as it reads it, holding none | the reader returns the whole dump: a 64-byte `MempoolTx` for every record of at least 26 input bytes, and the `mapDeltas` and unbroadcast nodes |
//! | Input size, the `CFlatDB` files | `governance.dat` 128 MiB (`governance::MAX_GOVERNANCE_DAT_SIZE`) of which the masternode list at its end at most 8 MiB (`governance::MAX_MN_LIST_SIZE`), `mncache.dat` 64 MiB ([`mncache::MAX_MNCACHE_DAT_SIZE`]), `netfulfilled.dat` 16 MiB ([`netfulfilled::MAX_NETFULFILLED_DAT_SIZE`]), `sporks.dat` 16 MiB ([`sporks::MAX_SPORKS_DAT_SIZE`]) | no cap | mainnet's `governance.dat` is tens of MiB at most, a `mncache.dat` is under a few MiB for thousands of masternodes, a `netfulfilled.dat` and a `sporks.dat` are kilobytes, and a mainnet masternode list is about 400 bytes a masternode (a few MiB at most); the caps are what the next row's memory is multiplied by |
//! | Memory of a parsed `CFlatDB` file | everything but the masternode list inside `governance.dat`: at most **18 requested bytes per input byte** (`tests/flatfiles_memory.rs`, keys in random order and ascending, which is how Core writes a `std::map` and costs a `BTreeMap` more: 17.2 for a `netfulfilled.dat` of services with one request each, 7.1 for one governance object with a voter record of one signal per voter, 4.9 for the smallest `mncache.dat` meta records, 3.4 for `sporks.dat`), so at most about 0.29 GiB at the `netfulfilled.dat` cap, 0.9 GiB at the `governance.dat` cap and 0.31 GiB at the `mncache.dat` cap. **The masternode list in `governance.dat`** is decoded by `dash-dml`'s `decode_list_s3`, whose reader has no length budget, so it has its own row: at most **150 requested bytes per list byte** (measured 144.0 for one entry holding 2^21 + 1 one-byte invalid net-info entries, a `Vec` of 48-byte items just past a doubling; 21.3 for empty payout shares, 6.8 for empty collateral shares, 5.7 for the smallest entries, 8.2 for the real testnet file) and the list is capped at 8 MiB, so at most 1.2 GiB for the list on top of the rest. The decoder's work per entry is linear whatever the hashes (`dash-dml/tests/same_prefix_scaling.rs` and `tests/flatfiles_memory.rs`: keys that all share their first 64 bits cost 4.3 times the work for 4 times the entries) | the same maps and vectors per record | the shapes are the smallest records that each cost a heap node; the figure is the peak of requested bytes with a counting allocator, not an allocator's rounding |
//! | Memory of the other binary readers | measured, **not a bound**: `peers.dat` 3.3 times the file (65,536 entries and 3.75 M bucket references in 16 MiB), `fee_estimates.dat` 2.05 times (the largest valid shape, 48 MB), `anchors.dat` about 1 time | Core streams and builds its own structures | the input size caps above are the limit: about 53 MiB, 131 MiB and 2 MiB |
//! | Reservation before data is read | 4,096 elements, and never more than the bytes left can hold | `std::vector` growth in `Unserialize` | an element is several times its on-disk minimum in memory; counts that the file supports are still accepted, the `Vec` grows as items decode |
//! | File type | regular files only, checked on the opened descriptor | `fopen` any file type | a FIFO or device would block or never end; symlinks to regular files are followed, as `fopen` does |
//!
//! The JSON memory row is a derivation, not a measurement. A parsed value is a 32-byte `Value`
//! slot in its parent's `Vec`; a container with children owns one `Vec` of at least 4 slots (4 x 32
//! = 128 bytes for an array, 4 x 56 = 224 for an object, whose members carry a key); a number or
//! string owns at most 8 bytes per input byte. Charge every allocation to the input bytes of its
//! own syntax: a container with n children owns at least n + 1 input bytes (brackets and commas)
//! and one `Vec` of at most max(4, 2n) slots, which is 64 requested bytes per input byte at n = 1
//! (a chain of single-child arrays, `[[[[...]]]]`: 128 requested bytes, a 144-byte glibc chunk, per
//! two input bytes, i.e. 72 on glibc) and less for every larger n; an object with one member is 5
//! input bytes (`{"":}`) for 224, 45 per byte. The readers' own structures (`Value::values()`, the
//! ban map, the cloned addresses, the duplicate-key check) add under 20 per byte. Every other shape
//! therefore costs less per input byte (measured at 1 MiB, as requested bytes: flat arrays of
//! numbers, strings or nulls 8 to 20 when the root `Vec` is nearly full and up to about 36 just
//! past a power of two, flat one-element arrays and one-member objects 42, chains of single-member
//! objects 45). The depth limit of 512 only limits how many levels one chain has, not how many
//! chains a file holds. The measured figures agree: 65.0 times for four-array chains and 73.08
//! times (1,197,272 KiB of RSS for 16,777,180 bytes, Sol r2) for 510-array chains, which is the
//! bound plus the process. A different allocator changes the 72 (its rounding of a 128-byte
//! request), not the 64; `tests/netstate_memory.rs` checks the 64 against the code with a counting
//! allocator and pins the `Value` size the derivation uses.
//!
//! The reader also holds two fixed-size structures that do not scale with the file: its work stack
//! (at most 512 frames of 48 bytes, 24 KiB, still allocated while the tree is complete) and the
//! unwinding list of `Value`'s `Drop` (at most the depth, 512 x 32 = 16 KiB). They add at most
//! 40 KiB per parse to the figures above, which matters only for a file of a few hundred bytes
//! (a 520-byte document of depth 257 peaks at 111 requested bytes per input byte, 57,576 bytes);
//! `tests/netstate_memory.rs` checks `64 x file + 40 KiB`.
//!
//! The `mempool.dat` row's derivation: a record is at least 26 input bytes (a 10-byte transaction,
//! the time and the fee delta) and becomes one 64-byte `MempoolTx` in a `Vec` whose capacity is at
//! most twice its length: 2 x 64 / 26 = 4.9 requested bytes per input byte. An entry of the
//! `mapDeltas` is 40 input bytes and of the unbroadcast set 32; a `BTreeMap` or `BTreeSet` node
//! holds 5 to 11 of them (at least half full), at most about 3 times the entry (internal nodes
//! included), under the records' 4.9. A byte of the
//! file belongs to one of the three parts, so the bound is the largest of them, the records' 4.9,
//! rounded up to 5.
//!
//! Core's own limits (`MAX_SIZE` for compact sizes, 65,536 new and 16,384 tried entries, the
//! 512 JSON depth, 1,000 fee buckets, 512-byte BIP155 addresses) are the readers' limits too and
//! are not bounds in this sense.
//!
//! What a file does not decide, and the readers take from the importing host and Core's defaults:
//! the platform resolver for numeric ban hosts (`getaddrinfo` with `AI_NUMERICHOST`: glibc's
//! `inet_aton` forms and IPv6 syntax; a numeric scope id is accepted as glibc does, an interface
//! name as a scope is looked up among the importing host's interfaces, as Core would when it
//! loads the file there; the text-mode translation of non-Unix hosts is not reproduced) and
//! `-cjdnsreachable` ([`banlist::parse_with`]). These are inputs of Core's own decision, not
//! differences from it.
//!
//! What the readers leave to the owning loaders, which must not skip it:
//!
//! - `peers.dat`: the address manager's rebuild and `CheckAddrman`
//!   ([`peers::PeersDat::bucketing_restorable`] does not certify a usable manager).
//! - `mempool.dat`: the admission of each transaction record, and applying the fee deltas.
//! - `fee_estimates.dat`: **Core's staleness rule** (`policy/fees.cpp:570`-`575`,
//!   `policy/fees.h:31`). Core does not read the file at all when its age, the whole hours (the
//!   division truncates) between the file's modification time and now, is more than
//!   `MAX_FILE_AGE`, 60 hours (so from 61 hours on), unless `-acceptstalefeeestimates` is set
//!   (`init.cpp` allows that on regtest only). It logs that the file is too old and starts with
//!   empty estimates. [`fee_estimates::parse`] sees bytes, not the file's modification time or the
//!   clock, so a loader must read the time from the opened descriptor and **discard the parsed
//!   estimates** under that rule, or it serves estimates Core would not. A modification time in
//!   the future is not stale. Under `-blocksonly` Core creates no estimator and never reads the
//!   file (`init.cpp:1722`).
//! - `anchors.dat`: Core keeps at most `MAX_BLOCK_RELAY_ONLY_ANCHORS` = 2 of the anchors
//!   (`net.cpp:67`, `net.cpp:4028`), and `ReadAnchors` deletes the file after reading it, whether
//!   or not it parsed (`addrdb.cpp:249`). Core does both only when it uses addrman for outbound
//!   connections (`net.cpp:4025`-`4027`).
//!   [`anchors::parse`] returns every entry the file holds and removes nothing.
//! - `sporks.dat`: **the spork signature check.** Core's `LoadCache` runs `CheckAndRemove` right
//!   after loading, which drops every message whose signature does not verify against the
//!   network's spork key; the reader sees bytes, and the checksum anyone who can write the datadir
//!   can recompute. [`sporks::SporksDat::check_and_remove`] is that step, and a loader must apply
//!   it, with the network's spork key id, before it uses a spork.
//! - `netfulfilled.dat`: Core runs `CheckAndRemove` right after loading;
//!   [`netfulfilled::NetFulfilledDat::without_expired`] is that, with the clock the loader gives it.
//! - `governance.dat`: Core's `CGovernanceManager` re-validates objects and rebuilds its indexes
//!   (`mapPostponedObjects`, the vote index) after loading; the reader returns what the file holds.
//! - The `CFlatDB` files report a damaged object as [`Loaded::discarded`], not as an error: Core
//!   clears the object and starts with it empty in that case, and an importer should do the same
//!   (and may warn with the reason). A bad checksum, magic message or network magic is an error,
//!   as in Core.

pub mod anchors;
pub mod banlist;
#[cfg(feature = "full")]
mod detect;
mod envelope;
mod error;
pub mod fee_estimates;
mod flat;
#[cfg(feature = "full")]
pub mod governance;
mod json;
pub mod mempool;
pub mod mncache;
mod netaddr;
pub mod netfulfilled;
pub mod peers;
mod reader;
pub mod settings;
pub mod sporks;
pub mod subnet;

use std::io::Read;
use std::path::Path;

#[cfg(feature = "full")]
pub use detect::{Detected, Format, Version, detect_named, sniff};
pub use error::{FormatError, FormatResult, LoadError};
pub use flat::Loaded;
pub use json::{JsonStr, MAX_JSON_FILE_SIZE, MAX_SETTINGS_FILE_SIZE, Object, Value};
pub use netaddr::{Address, NetAddr, Service};

/// Opens the regular file at `path` for reading, as [`read_file_capped`] does: the file is opened
/// **first** (on Unix with `O_NONBLOCK`, so that opening a FIFO that was put in the file's place
/// cannot block, and `O_NOCTTY`), and the type is taken from the opened descriptor (`fstat`),
/// never from a prior look at the path; anything but a regular file is an [`LoadError::Io`] of
/// kind `InvalidInput`. Symlinks are followed, as Core's `fopen` does; a link to a regular file
/// opens, a link to anything else is refused by the same descriptor check. The returned
/// descriptor reads in blocking mode.
pub(crate) fn open_regular_file(path: &Path) -> Result<std::fs::File, LoadError> {
    let io = |source| LoadError::Io {
        path: path.to_path_buf(),
        source,
    };
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY);
    }
    let file = options.open(path).map_err(io)?;
    if !file.metadata().map_err(io)?.is_file() {
        return Err(io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        )));
    }
    // A regular file reads in blocking mode, as Core's `fopen` reads it.
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: `fd` is the open descriptor `file` owns for the length of both calls.
        unsafe {
            let fd = file.as_raw_fd();
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK);
            }
        }
    }
    Ok(file)
}

/// Opens the file at `path` and reads at most `limit` bytes of it: a longer file is
/// [`FormatError::TooLarge`] after reading `limit + 1` bytes, never the whole file. Only regular
/// files are read (see `open_regular_file`).
pub fn read_file_capped(
    path: &Path,
    limit: usize,
    what: &'static str,
) -> Result<Vec<u8>, LoadError> {
    let file = open_regular_file(path)?;
    let io = |source| LoadError::Io {
        path: path.to_path_buf(),
        source,
    };
    let mut bytes = Vec::new();
    file.take((limit as u64).saturating_add(1)).read_to_end(&mut bytes).map_err(io)?;
    if bytes.len() > limit {
        return Err(FormatError::TooLarge {
            what,
            size: bytes.len() as u64,
            limit: limit as u64,
        }
        .into());
    }
    Ok(bytes)
}
