//! The memory the JSON readers and `mempool.dat`'s reader take for a file: the evidence behind the
//! "Memory of a parsed JSON file" and "Memory of a parsed `mempool.dat`" rows of the bounds table
//! in `netstate/mod.rs` (R2-m1, Opus-high m2).
//!
//! A parsed value costs a `Value` slot in its parent's `Vec` and, for a container with children,
//! one `Vec` allocation (capacity 4 at least: `RawVec`'s minimum for elements under 1 KiB) and for
//! a number or string its bytes. The worst shape per input byte is a chain of single-child arrays,
//! `[[[[...]]]]`: two input bytes per level (`[` and `]`) and one `Vec` of four `Value`s, 128
//! requested bytes, so **64 requested bytes per input byte**; glibc rounds a 128-byte request to a
//! 144-byte chunk, which is **72 bytes per input byte**. Every other shape is cheaper per input
//! byte (measured below: 8 to 45 requested; a single-member object is 5 input bytes, `{"":` and
//! `}`, for 4 x 56 requested). This test builds the shapes at about 1 MiB and checks
//! the peak of the *requested* live bytes of the parse against 64 per input byte, with a counting
//! allocator, so the bound is checked against the code and not against one allocator's rounding.
//!
//! `mempool.dat` (second test part): a record is 26 input bytes at least and becomes a 64-byte
//! `MempoolTx` in a `Vec` of at most twice its length, 4.9 requested bytes per input byte; the
//! `mapDeltas` and unbroadcast nodes are cheaper. The bound is 5.
//!
//! It is a single test in its own binary (both parts run in one `#[test]`): the allocator counts
//! the whole process.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use dash_coreimport::netstate::{JsonStr, Value, banlist, mempool, settings};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn add(bytes: usize) {
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

/// Under `settings.json`'s 1 MiB cap with room for the wrapper, so both readers parse the file.
const SIZE: usize = (1 << 20) - 1024;
/// Core's depth limit counts the root object, the member's array and every nested container.
const DEPTH: usize = 510;

/// Repeats `unit` (comma-terminated) to about `SIZE` bytes, then `0` to end the array.
fn repeat(unit: &str) -> String {
    let mut s = String::with_capacity(SIZE + unit.len());
    s.push('[');
    while s.len() + unit.len() < SIZE {
        s.push_str(unit);
    }
    s.push_str("0]");
    s
}

fn chain(depth: usize, inner: &str) -> String {
    format!("{}{inner}{},", "[".repeat(depth), "]".repeat(depth))
}

/// The peak requested bytes `parse` holds above what was live before it ran.
fn peak_of(parse: impl FnOnce()) -> usize {
    let base = LIVE.load(Relaxed);
    PEAK.store(base, Relaxed);
    parse();
    PEAK.load(Relaxed) - base
}

#[test]
fn parsed_files_take_a_bounded_number_of_requested_bytes_per_input_byte() {
    json_tree_takes_at_most_64_requested_bytes_per_input_byte();
    mempool_takes_at_most_5_requested_bytes_per_input_byte();
}

fn json_tree_takes_at_most_64_requested_bytes_per_input_byte() {
    // The per-node costs the derivation uses.
    assert_eq!(size_of::<Value>(), 32);
    assert_eq!(size_of::<(JsonStr, Value)>(), 56);

    let shapes: Vec<(&str, String)> = vec![
        ("chain of 510 arrays around a number", repeat(&chain(DEPTH, "0"))),
        ("chain of 510 arrays around nothing", repeat(&chain(DEPTH, ""))),
        ("chain of 4 arrays around a number", repeat(&chain(4, "0"))),
        ("chain of 1 array around a number", repeat(&chain(1, "0"))),
        ("chain of 255 objects", repeat(&format!("{}0{},", "{\"\":".repeat(255), "}".repeat(255)))),
        ("flat numbers", repeat("0,")),
        ("flat empty strings", repeat("\"\",")),
        ("flat one-byte strings", repeat("\"a\",")),
        ("flat empty arrays", repeat("[],")),
        ("flat one-element arrays", repeat("[0],")),
        ("flat one-member objects", repeat("{\"\":0},")),
        ("flat one-member objects with keys", repeat("{\"k\":0},")),
        ("flat nulls", repeat("null,")),
    ];
    // Valid bans with distinct addresses, so the ban map, the cloned addresses and the entry list
    // of the banlist reader are exercised too (the shapes above go into an unused member).
    let bans = {
        let mut s = String::from("[");
        let mut i = 0u32;
        while s.len() + 90 < SIZE {
            let [a, b, c, d] = i.to_be_bytes();
            s.push_str(&format!(
                "{{\"version\":1,\"address\":\"{a}.{b}.{c}.{d}\",\"ban_created\":1,\"banned_until\":2}},"
            ));
            i += 1;
        }
        s.push_str("{\"version\":2}]");
        s
    };
    let mut worst = 0.0f64;
    let mut check = |name: &str, reader: &str, json: &String, must_parse: bool| {
        let peak = peak_of(|| {
            let parsed = match reader {
                "banlist" => banlist::parse(json.as_bytes()).is_ok(),
                _ => settings::parse(json.as_bytes()).is_ok(),
            };
            // The file must really be parsed, not refused for its size.
            assert!(!must_parse || parsed, "{name} / {reader}");
        });
        let per_byte = peak as f64 / json.len() as f64;
        worst = worst.max(per_byte);
        eprintln!("{name} / {reader}: {per_byte:.2} requested bytes per input byte");
        // 64 per byte, plus the fixed 40 KiB of the work stack and the drop list.
        assert!(
            peak <= 64 * json.len() + 40 * 1024,
            "{name} / {reader}: {peak} requested bytes for {} input bytes",
            json.len()
        );
    };
    for (name, shape) in &shapes {
        // Sol's shape: an empty `banned_nets` and the stress value in an unused member.
        check(name, "banlist", &format!("{{\"banned_nets\":[],\"extra\":{shape}}}"), true);
        check(name, "settings", &format!("{{\"x\":{shape}}}"), true);
        // The stress value as the ban list itself: `values()` walks it and the first entry fails
        // the file (not an object), after the whole tree was built.
        check(name, "banlist-as-bans", &format!("{{\"banned_nets\":{shape}}}"), false);
    }
    check("valid bans", "banlist", &format!("{{\"banned_nets\":{bans}}}"), true);
    // Small deep documents: the fixed work stack (512 frames) dominates, which the per-byte bound
    // alone would miss.
    for depth in [257, 509, 510] {
        let deep = format!("{{\"x\":{}{}}}", "[".repeat(depth - 1), "]".repeat(depth - 1));
        check(&format!("deep {depth}"), "settings", &deep, true);
    }
    // The chain of single-child arrays is the worst shape and is close to the bound.
    assert!(worst > 60.0, "the worst shape should be the chain, got {worst:.2}");
}

/// A xorshift stream: txids that are not in order, so the map and set nodes fill as they would for
/// real data.
fn txid(state: &mut u64) -> [u8; 32] {
    let mut out = [0u8; 32];
    for chunk in out.chunks_mut(8) {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        chunk.copy_from_slice(&state.to_le_bytes());
    }
    out
}

fn mempool_takes_at_most_5_requested_bytes_per_input_byte() {
    // A transaction of 10 bytes (version, no inputs, no outputs, lock time), its time and delta.
    let record = {
        let mut r = vec![1, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        r.extend_from_slice(&[0; 16]);
        r
    };
    let header = |count: u64| {
        let mut h = 1u64.to_le_bytes().to_vec();
        h.extend_from_slice(&count.to_le_bytes());
        h
    };
    let mut state = 0x9e37_79b9_7f4a_7c15u64;

    let n = SIZE / record.len();
    let mut records = header(n as u64);
    for _ in 0..n {
        records.extend_from_slice(&record);
    }
    records.extend_from_slice(&[0, 0]);

    // Compact sizes below 2^16 are `0xfd` and a `u16`.
    let n = (SIZE - 12) / 40;
    let mut deltas = header(0);
    deltas.extend_from_slice(&[0xfd]);
    deltas.extend_from_slice(&(n as u16).to_le_bytes());
    for i in 0..n {
        deltas.extend_from_slice(&txid(&mut state));
        deltas.extend_from_slice(&(i as i64).to_le_bytes());
    }
    deltas.push(0);

    let n = (SIZE - 12) / 32;
    let mut set = header(0);
    set.push(0);
    set.extend_from_slice(&[0xfd]);
    set.extend_from_slice(&(n as u16).to_le_bytes());
    for _ in 0..n {
        set.extend_from_slice(&txid(&mut state));
    }

    for (name, file) in [("records", &records), ("map deltas", &deltas), ("unbroadcast set", &set)]
    {
        let peak = peak_of(|| {
            let loaded = mempool::load(file).is_ok();
            assert!(loaded, "{name}");
        });
        let per_byte = peak as f64 / file.len() as f64;
        eprintln!("mempool {name}: {per_byte:.2} requested bytes per input byte");
        assert!(peak <= 5 * file.len() + 4096, "mempool {name}: {peak} for {} bytes", file.len());
    }
}
