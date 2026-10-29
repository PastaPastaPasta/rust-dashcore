//! The memory the flat-file readers of IM1b take for a file: the evidence behind the "Memory of the
//! flat files" row of the bounds table in `netstate/mod.rs`.
//!
//! Each reader returns the whole object, so its memory follows the file. The cost per input byte is
//! highest for the smallest records that each cost a heap node: a service with one request, a
//! masternode meta record, an object with no votes, a voter record with one signal. This test
//! builds those shapes at about 2 MiB, with distinct keys so no entry is dropped as a duplicate,
//! and checks the peak of the *requested* live bytes against the bound the table states, with a
//! counting allocator (the bound is checked against the code, not against one allocator's
//! rounding). It also covers the masternode list inside `governance.dat`: the real testnet file,
//! the smallest entries, and the hostile shapes that expand most.
//!
//! It is one test in its own binary: the allocator counts the whole process.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use bitcoin_hashes::{Hash, sha256d};
use dash_coreimport::netstate::{governance, mncache, netfulfilled, sporks};
use dash_network::Network;

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
/// Every byte ever requested: the work a parse does, however much of it is freed again.
static TOTAL: AtomicUsize = AtomicUsize::new(0);

fn add(bytes: usize) {
    TOTAL.fetch_add(bytes, Relaxed);
    let now = LIVE.fetch_add(bytes, Relaxed) + bytes;
    PEAK.fetch_max(now, Relaxed);
}

// SAFETY: every method forwards to `System` with the arguments it was given.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        add(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        add(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size >= layout.size() {
            add(new_size - layout.size());
        } else {
            LIVE.fetch_sub(layout.size() - new_size, Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The bound the table states: requested bytes at the peak of a parse, per input byte.
const BOUND_PER_BYTE: f64 = 18.0;
/// The same for the masternode list inside `governance.dat`: the table's 150, over the worst
/// shape measured (144.0: one entry with 2^21 + 1 one-byte net-info entries, a `Vec` of 48-byte
/// items just past a doubling). A vector of n items at most doubles past n, so the shape that
/// crosses a power of two is the worst of its kind.
const LIST_BOUND_PER_BYTE: f64 = 150.0;
const SIZE: usize = 2 << 20;

/// Keys in random order, or ascending: Core writes a `std::map` in key order, and a `BTreeMap`
/// filled in ascending order keeps its leaves about half full, which costs more per entry.
struct Rng {
    state: u64,
    sorted: bool,
    counter: u64,
}

impl Rng {
    fn new(sorted: bool) -> Rng {
        Rng {
            state: 0x9e37_79b9_7f4a_7c15,
            sorted,
            counter: 0,
        }
    }

    fn next(&mut self) -> u64 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        self.state
    }

    /// A word that is the next of an ascending sequence when `sorted`, else a random one.
    fn ordered(&mut self) -> u64 {
        if self.sorted {
            self.counter += 1;
            self.counter
        } else {
            self.next()
        }
    }

    fn hash(&mut self) -> [u8; 32] {
        if self.sorted {
            let mut out = [0u8; 32];
            out[..8].copy_from_slice(&self.ordered().to_be_bytes());
            return out;
        }
        let mut out = [0u8; 32];
        for chunk in out.chunks_mut(8) {
            chunk.copy_from_slice(&self.next().to_le_bytes());
        }
        out
    }
}

fn compact(out: &mut Vec<u8>, n: usize) {
    match n {
        0..=252 => out.push(n as u8),
        253..=0xffff => {
            out.push(253);
            out.extend_from_slice(&(n as u16).to_le_bytes());
        }
        _ => {
            out.push(254);
            out.extend_from_slice(&(n as u32).to_le_bytes());
        }
    }
}

fn string(out: &mut Vec<u8>, s: &str) {
    compact(out, s.len());
    out.extend_from_slice(s.as_bytes());
}

fn header(magic_message: &str) -> Vec<u8> {
    let mut out = Vec::new();
    string(&mut out, magic_message);
    out.extend_from_slice(&[0xfc, 0xc1, 0xb7, 0xdc]);
    out
}

fn seal(mut data: Vec<u8>) -> Vec<u8> {
    let sum = sha256d::Hash::hash(&data).to_byte_array();
    data.extend_from_slice(&sum);
    data
}

/// `n` records of `size` bytes: `write` appends one.
fn repeated(size: usize, mut write: impl FnMut(&mut Vec<u8>)) -> (usize, Vec<u8>) {
    let n = SIZE / size;
    let mut out = Vec::with_capacity(n * size);
    for _ in 0..n {
        write(&mut out);
    }
    (n, out)
}

fn peak_of(parse: impl FnOnce()) -> usize {
    let base = LIVE.load(Relaxed);
    PEAK.store(base, Relaxed);
    parse();
    PEAK.load(Relaxed) - base
}

fn check(name: &str, file: &[u8], parse: impl FnOnce(&[u8])) {
    let peak = peak_of(|| parse(file));
    let per_byte = peak as f64 / file.len() as f64;
    eprintln!("{name}: {per_byte:.2} requested bytes per input byte ({} input bytes)", file.len());
    assert!(per_byte <= BOUND_PER_BYTE, "{name}: {per_byte:.2} per input byte");
}

fn vote(rng: &mut Rng, out: &mut Vec<u8>) {
    out.extend_from_slice(&rng.hash()); // masternode outpoint hash
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&rng.hash()); // parent hash
    out.extend_from_slice(&1i32.to_le_bytes());
    out.extend_from_slice(&1i32.to_le_bytes());
    out.extend_from_slice(&0i64.to_le_bytes());
    out.push(0); // empty signature
}

/// A governance object with nothing optional; `voters` is written between the `fExpired` flag and
/// the vote file.
fn object(rng: &mut Rng, out: &mut Vec<u8>, voters: impl FnOnce(&mut Rng, &mut Vec<u8>)) {
    out.extend_from_slice(&rng.hash()); // parent
    out.extend_from_slice(&1i32.to_le_bytes());
    out.extend_from_slice(&0i64.to_le_bytes());
    out.extend_from_slice(&rng.hash()); // collateral
    out.push(0); // data
    out.extend_from_slice(&1i32.to_le_bytes());
    out.extend_from_slice(&rng.hash());
    out.extend_from_slice(&0u32.to_le_bytes()); // masternode outpoint
    out.push(0); // signature
    out.extend_from_slice(&0i64.to_le_bytes());
    out.push(0); // expired
    voters(rng, out);
}

#[test]
fn flat_files_take_a_bounded_number_of_requested_bytes_per_input_byte() {
    for sorted in [false, true] {
        eprintln!(
            "-- keys {}",
            if sorted {
                "ascending"
            } else {
                "in random order"
            }
        );
        shapes(sorted);
    }
    real_testnet_file();
    masternode_lists();
}

fn shapes(sorted: bool) {
    let mut rng = Rng::new(sorted);
    let regtest = Network::Regtest;

    // netfulfilled.dat: a service with one request, and a service with many.
    let (n, body) = repeated(18 + 1 + 1 + 8 + 1, |out| {
        out.extend_from_slice(&[0u8; 10]);
        out.extend_from_slice(&[0xff, 0xff]);
        out.extend_from_slice(&(rng.ordered() as u32).to_be_bytes());
        out.extend_from_slice(&((rng.next() >> 40) as u16).to_be_bytes());
        out.push(1);
        out.push(0); // an empty request name
        out.extend_from_slice(&0i64.to_le_bytes());
    });
    let mut file = header("magicFulfilledCache");
    compact(&mut file, n);
    file.extend(body);
    check("netfulfilled.dat, one request per service", &seal(file), |f| {
        let _ = netfulfilled::parse(f, regtest).unwrap();
    });
    let mut file = header("magicFulfilledCache");
    file.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 1, 2, 3, 4, 0x20, 0x8d]);
    const REQUESTS: u16 = 32768;
    compact(&mut file, usize::from(REQUESTS));
    for request in 0..REQUESTS {
        // A distinct 2-byte name per request.
        file.push(2);
        file.extend_from_slice(&request.to_be_bytes());
        file.extend_from_slice(&0i64.to_le_bytes());
    }
    check("netfulfilled.dat, many requests", &seal(file), |f| {
        let _ = netfulfilled::parse(f, regtest).unwrap();
    });

    // sporks.dat: messages with an empty signature, under both maps.
    let mut ids = 0i32;
    let (n, by_hash) = repeated(32 + 21, |out| {
        out.extend_from_slice(&rng.hash());
        out.extend_from_slice(&[0; 4 + 8 + 8]);
        out.push(0);
    });
    let (m, active) = repeated(4 + 21, |out| {
        ids += 1;
        out.extend_from_slice(&ids.to_le_bytes());
        out.extend_from_slice(&ids.to_le_bytes());
        out.extend_from_slice(&[0; 8 + 8]);
        out.push(0);
    });
    let mut file = header("magicSporkCache");
    string(&mut file, "CSporkManager-Version-3");
    compact(&mut file, n);
    file.extend(by_hash);
    compact(&mut file, m);
    file.extend(active);
    check("sporks.dat", &seal(file), |f| {
        let _ = sporks::parse(f, regtest).unwrap();
    });

    // mncache.dat: the smallest meta records, one meta with many votes, and the used list.
    let (n, metas) = repeated(70, |out| {
        out.extend_from_slice(&rng.hash());
        out.extend_from_slice(&[0; 8 + 4]);
        out.push(0);
        out.extend_from_slice(&[0; 4 + 8 + 8 + 1 + 4]);
    });
    let mut file = header("magicMasternodeCache");
    string(&mut file, "CMasternodeMetaMan-Version-5");
    compact(&mut file, n);
    file.extend(metas);
    file.extend_from_slice(&0i64.to_le_bytes());
    file.push(0);
    check("mncache.dat, smallest metas", &seal(file), |f| {
        let _ = mncache::parse(f, regtest).unwrap();
    });
    let (n, votes) = repeated(36, |out| {
        out.extend_from_slice(&rng.hash());
        out.extend_from_slice(&1i32.to_le_bytes());
    });
    let mut file = header("magicMasternodeCache");
    string(&mut file, "CMasternodeMetaMan-Version-5");
    file.push(1);
    file.extend_from_slice(&[9; 32]);
    file.extend_from_slice(&[0; 8 + 4]);
    compact(&mut file, n);
    file.extend(votes);
    file.extend_from_slice(&[0; 4 + 8 + 8 + 1 + 4]);
    file.extend_from_slice(&0i64.to_le_bytes());
    file.push(0);
    check("mncache.dat, one meta with many votes", &seal(file), |f| {
        let _ = mncache::parse(f, regtest).unwrap();
    });
    let (n, used) = repeated(32, |out| out.extend_from_slice(&rng.hash()));
    let mut file = header("magicMasternodeCache");
    string(&mut file, "CMasternodeMetaMan-Version-5");
    file.push(0);
    file.extend_from_slice(&0i64.to_le_bytes());
    compact(&mut file, n);
    file.extend(used);
    check("mncache.dat, used masternodes", &seal(file), |f| {
        let _ = mncache::parse(f, regtest).unwrap();
    });

    // governance.dat: the masternode list tail is the empty fixture's, so each shape is the
    // section under test followed by a valid (default) list.
    let empty = std::fs::read(format!(
        "{}/tests/data/flatfiles/v24.0.0-rc.3/empty/governance.dat",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let mut prefix = header("magicGovernanceCache");
    string(&mut prefix, "CGovernanceManager-Version-16");
    // erased (1), invalid votes (4 + 1), orphans (4 + 1), objects (1), last objects (1)
    let tail = empty[prefix.len() + 13..empty.len() - 32].to_vec();
    assert_eq!(empty[..prefix.len()], prefix[..]);
    let gov = |erased: &[u8],
               erased_n: usize,
               orphans: &[u8],
               orphans_n: usize,
               objects: &[u8],
               objects_n: usize,
               last: &[u8],
               last_n: usize| {
        let mut file = prefix.clone();
        compact(&mut file, erased_n);
        file.extend_from_slice(erased);
        file.extend_from_slice(&[0; 4 + 1]);
        file.extend_from_slice(&0u32.to_le_bytes());
        compact(&mut file, orphans_n);
        file.extend_from_slice(orphans);
        compact(&mut file, objects_n);
        file.extend_from_slice(objects);
        compact(&mut file, last_n);
        file.extend_from_slice(last);
        file.extend_from_slice(&tail);
        seal(file)
    };
    let parse = |f: &[u8]| {
        let loaded = governance::parse(f, regtest).unwrap();
        assert!(loaded.discarded.is_none(), "{:?}", loaded.discarded);
    };
    let (n, erased) = repeated(40, |out| {
        out.extend_from_slice(&rng.hash());
        out.extend_from_slice(&0i64.to_le_bytes());
    });
    check("governance.dat, erased objects", &gov(&erased, n, &[], 0, &[], 0, &[], 0), parse);
    let (n, orphans) = repeated(32 + 85 + 8, |out| {
        out.extend_from_slice(&rng.hash());
        vote(&mut rng, out);
        out.extend_from_slice(&0i64.to_le_bytes());
    });
    check("governance.dat, orphan votes", &gov(&[], 0, &orphans, n, &[], 0, &[], 0), parse);
    let (n, last) = repeated(36 + 1 + 4 + 4 + 1 + 1, |out| {
        out.extend_from_slice(&rng.hash());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.push(0);
        out.extend_from_slice(&[0; 4 + 4 + 1 + 1]);
    });
    check("governance.dat, last masternode objects", &gov(&[], 0, &[], 0, &[], 0, &last, n), parse);
    let (n, objects) = repeated(32 + 133, |out| {
        out.extend_from_slice(&rng.hash());
        object(&mut rng, out, |_, out| {
            out.push(0); // no current votes
            out.extend_from_slice(&0i32.to_le_bytes());
            out.push(0); // no votes in the file
        });
    });
    check("governance.dat, smallest objects", &gov(&[], 0, &[], 0, &objects, n, &[], 0), parse);
    // One object holding many voters with one signal each, and many with none.
    for (signals, name) in [(1usize, "one signal"), (0, "no signal")] {
        let record = 36 + 1 + signals * 24;
        let (n, voters) = repeated(record, |out| {
            out.extend_from_slice(&rng.hash());
            out.extend_from_slice(&0u32.to_le_bytes());
            compact(out, signals);
            if signals == 1 {
                out.extend_from_slice(&1i32.to_le_bytes());
                out.extend_from_slice(&[0; 4 + 8 + 8]);
            }
        });
        let mut one = Vec::new();
        one.extend_from_slice(&[7; 32]);
        object(&mut rng, &mut one, |_, out| {
            compact(out, n);
            out.extend(&voters);
            out.extend_from_slice(&0i32.to_le_bytes());
            out.push(0);
        });
        check(
            &format!("governance.dat, one object with many voters ({name})"),
            &gov(&[], 0, &[], 0, &one, 1, &[], 0),
            parse,
        );
    }
    let (n, votes) = repeated(85, |out| vote(&mut rng, out));
    let mut one = Vec::new();
    one.extend_from_slice(&[7; 32]);
    object(&mut rng, &mut one, |_, out| {
        out.push(0);
        out.extend_from_slice(&0i32.to_le_bytes());
        compact(out, n);
        out.extend(&votes);
    });
    check(
        "governance.dat, one object with many votes",
        &gov(&[], 0, &[], 0, &one, 1, &[], 0),
        parse,
    );
}

fn real_testnet_file() {
    // The real testnet file: a masternode list of a few hundred entries inside.
    let real = std::fs::read(format!(
        "{}/tests/data/flatfiles/v24.0.0-rc.3/oracle-testnet/governance.dat",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    check("governance.dat, real testnet file", &real, |f| {
        let _ = governance::parse(f, Network::Testnet).unwrap();
    });
}

/// Core's `VARINT` (`serialize.h`): base 128, most significant group first, each group but the
/// last offset by one.
fn varint(out: &mut Vec<u8>, mut n: u64) {
    let mut groups = Vec::new();
    loop {
        groups.push(
            (n & 0x7f) as u8
                | if groups.is_empty() {
                    0
                } else {
                    0x80
                },
        );
        if n <= 0x7f {
            break;
        }
        n = (n >> 7) - 1;
    }
    out.extend(groups.iter().rev());
}

/// The smallest masternode a `governance.dat` list can hold, as `CDeterministicMN::Serialize`
/// writes it at `SER_DISK` (state version 1: legacy address, default operator key, empty
/// scripts). Everything that must be unique is numbered by `i`; `pro_tx` is its hash.
fn masternode(i: u32, pro_tx: [u8; 32]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&pro_tx);
    varint(&mut out, u64::from(i));
    out.extend_from_slice(&[(i % 251) as u8 + 1; 32]); // collateral hash
    out.extend_from_slice(&i.to_le_bytes()); // collateral index: unique with the hash
    out.extend_from_slice(&0u16.to_le_bytes()); // operator reward
    out.extend_from_slice(&1i32.to_le_bytes()); // state version
    out.extend_from_slice(&1_000_000i32.to_le_bytes()); // registered height
    out.extend_from_slice(&[0; 5 * 4]); // last paid .. ban height
    out.extend_from_slice(&0u16.to_le_bytes()); // revocation reason
    out.extend_from_slice(&[0; 64]); // confirmed hashes
    let key_id = |tag: u8| {
        let mut id = [tag; 20];
        id[..4].copy_from_slice(&(i + 1).to_le_bytes());
        id
    };
    out.extend_from_slice(&key_id(1)); // owner key id
    out.extend_from_slice(&[0; 48]); // operator key: the default
    out.extend_from_slice(&key_id(2)); // voting key id
    let mut address = [0u8; 16];
    address[10..12].copy_from_slice(&[0xff, 0xff]);
    address[12..].copy_from_slice(&(i + 1).to_be_bytes());
    out.extend_from_slice(&address);
    out.extend_from_slice(&9999u16.to_be_bytes());
    out.extend_from_slice(&[0, 0]); // payout and operator payout scripts
    out.extend_from_slice(&[0; 20 + 2 + 2]); // platform node id and ports
    out.extend_from_slice(&0u16.to_le_bytes()); // regular masternode
    out
}

/// One masternode with state version 3 (extended net info, payout and share vectors), the three
/// variable parts given as raw bytes: the entry whose vectors expand most per input byte.
fn masternode_v3(netinfo: &[u8], payouts: &[u8], shares: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&[9; 32]);
    varint(&mut out, 1);
    out.extend_from_slice(&[1; 32]);
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&3i32.to_le_bytes()); // state version
    out.extend_from_slice(&1_000_000i32.to_le_bytes());
    out.extend_from_slice(&[0; 5 * 4]);
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&[0; 64]);
    out.extend_from_slice(&[1; 20]);
    out.extend_from_slice(&[0; 48]);
    out.extend_from_slice(&[2; 20]);
    out.extend_from_slice(netinfo);
    out.extend_from_slice(payouts);
    out.extend_from_slice(shares);
    out.extend_from_slice(&0u32.to_le_bytes()); // early period
    out.extend_from_slice(&0i64.to_le_bytes()); // early penalty
    out.push(0); // operator payout script
    out.extend_from_slice(&[0; 20]); // platform node id
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// An otherwise empty `governance.dat` whose masternode list holds `entries`.
fn governance_with_list(entries: &[Vec<u8>]) -> Vec<u8> {
    let empty = std::fs::read(format!(
        "{}/tests/data/flatfiles/v24.0.0-rc.3/empty/governance.dat",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    // The empty file ends with the default list: null hash, height -1, counter 0, no entry.
    let list_at = empty.len() - 32 - (32 + 4 + 4 + 1);
    let mut file = empty[..list_at].to_vec();
    file.extend_from_slice(&[0; 32]);
    file.extend_from_slice(&(-1i32).to_le_bytes());
    file.extend_from_slice(&0u32.to_le_bytes());
    compact(&mut file, entries.len());
    for entry in entries {
        file.extend_from_slice(entry);
    }
    seal(file)
}

/// Requested bytes of a whole `governance.dat` parse (the work), and the peak of live ones. With
/// `Some(entries)` the list must come out with that many masternodes; with `None` the file may be
/// refused (a hostile entry usually fails `AddMN` only after it was decoded in full, which is
/// when its memory is spent).
fn work_and_peak(file: &[u8], entries: Option<usize>) -> (usize, usize) {
    let base = TOTAL.load(Relaxed);
    let peak = peak_of(|| {
        let loaded = governance::parse(file, Network::Regtest);
        if let Some(entries) = entries {
            let loaded = loaded.unwrap();
            assert!(loaded.discarded.is_none(), "{:?}", loaded.discarded);
            assert_eq!(loaded.value.mn_list.len(), entries);
        }
    });
    (TOTAL.load(Relaxed) - base, peak)
}

/// The masternode list inside `governance.dat`: its peak per
/// input byte for the smallest entries, and the work a hostile list of entries that all share
/// their first 64 bits takes.
fn masternode_lists() {
    // Distinct hashes: the honest shape, at about 2 MiB.
    let mut rng = Rng::new(false);
    let n = SIZE / masternode(0, [0; 32]).len();
    let entries: Vec<Vec<u8>> = (0..n as u32).map(|i| masternode(i, rng.hash())).collect();
    let file = governance_with_list(&entries);
    let (_, peak) = work_and_peak(&file, Some(n));
    let per_byte = peak as f64 / file.len() as f64;
    eprintln!(
        "governance.dat, smallest masternodes: {per_byte:.2} requested bytes per input byte \
         ({} input bytes)",
        file.len()
    );
    assert!(per_byte <= LIST_BOUND_PER_BYTE, "{per_byte:.2} per input byte");

    // One entry whose vectors hold as many tiny items as 2 MiB allows, each one past a power of two
    // (the worst point of a growing `Vec`): the reader of the list has no length budget, so these
    // are the shapes that expand most.
    // A vector of `n` items of the bytes `item`: its count, then the items.
    let counted = |n: usize, item: &[u8]| {
        let mut out = Vec::new();
        compact(&mut out, n);
        out.extend(item.repeat(n));
        out
    };
    // One valid service (the entry must pass `AddMN`'s address check), then the tiny items.
    let service = [0x01u8, 0x01, 4, 1, 2, 3, 4, 0x27, 0x0f];
    let m = SIZE;
    let mut invalid_net = vec![1u8, 1, 0]; // version 1, one group, purpose 0
    compact(&mut invalid_net, m + 1);
    invalid_net.extend_from_slice(&service);
    invalid_net.extend(std::iter::repeat_n(0xffu8, m)); // `NetInfoEntry` invalid: one byte
    let small_net = [vec![1u8, 1, 0, 1], service.to_vec()].concat();
    let empty_payouts = counted((1 << 19) + 1, &[0; 3]); // empty script, reward 0
    let empty_shares = counted((1 << 16) + 1, &[0; 30]); // amount, two empty scripts, owner key
    let none = vec![0u8];
    for (name, entry) in [
        ("invalid net info entries", masternode_v3(&invalid_net, &none, &none)),
        ("empty payout shares", masternode_v3(&small_net, &empty_payouts, &none)),
        ("empty collateral shares", masternode_v3(&small_net, &none, &empty_shares)),
    ] {
        let list_bytes = entry.len();
        let (_, peak) = work_and_peak(&governance_with_list(&[entry]), None);
        let per_byte = peak as f64 / list_bytes as f64;
        eprintln!(
            "governance.dat, one masternode with {name}: {per_byte:.2} requested bytes per list \
             byte ({list_bytes} list bytes)"
        );
        assert!(per_byte <= LIST_BOUND_PER_BYTE, "{name}: {per_byte:.2} per list byte");
    }

    // Every hash shares the first 64 bits: Core's collision node, which must not be copied once
    // per entry. Four times the entries must cost about four times the work.
    let shared = |count: usize| {
        let entries: Vec<Vec<u8>> = (0..count as u32)
            .map(|i| {
                let mut hash = [7u8; 32];
                hash[8..12].copy_from_slice(&i.to_be_bytes());
                masternode(i, hash)
            })
            .collect();
        governance_with_list(&entries)
    };
    let (small_work, small_peak) = work_and_peak(&shared(1_000), Some(1_000));
    let (large_work, large_peak) = work_and_peak(&shared(4_000), Some(4_000));
    let (work_ratio, peak_ratio) =
        (large_work as f64 / small_work as f64, large_peak as f64 / small_peak as f64);
    eprintln!("same-prefix list, 4x entries: {work_ratio:.2}x work, {peak_ratio:.2}x peak");
    assert!(work_ratio < 6.0, "4000 entries took {work_ratio:.1} times the work of 1000");
    assert!(peak_ratio < 6.0, "4000 entries took {peak_ratio:.1} times the memory of 1000");
}
