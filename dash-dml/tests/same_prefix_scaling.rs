//! IM1b f1: a list of masternodes whose proTxHashes share their first 64 bits is built in linear
//! work. Core keeps such keys in one immer collision node; building that node must not copy it
//! once per key (a crafted `governance.dat` embeds exactly such a list).
//!
//! The work is counted as the bytes requested from the allocator on this thread: every copy of
//! a bucket shows up there, and the count does not depend on the machine's load (it drifts by
//! about 0.5 % between runs: `UniqueMap` hashes with a per-process random key). It sees copies,
//! not scans; a scan that allocates nothing needs an instruction count (`perf stat`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

use dash_dml::testkit::numbered;
use dash_dml::{Dmn, MnList, ProTxHash};

struct Counting;

thread_local! {
    static REQUESTED: Cell<u64> = const { Cell::new(0) };
}

// SAFETY: every call is forwarded to `System` unchanged; the counter is a thread-local `Cell`
// that is const-initialised and has no destructor, so touching it never allocates.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        REQUESTED.with(|r| r.set(r.get() + layout.size() as u64));
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

fn requested() -> u64 {
    REQUESTED.with(Cell::get)
}

/// Masternode `i` with a proTxHash of the shared prefix `7`, the rest numbering it.
fn same_prefix(i: u32) -> Arc<Dmn> {
    let mut dmn = (*numbered(i)).clone();
    let mut bytes = [7u8; 32];
    bytes[8..12].copy_from_slice(&i.to_be_bytes());
    dmn.pro_tx_hash = ProTxHash::from(bytes);
    Arc::new(dmn)
}

/// Bytes requested while adding `mns` one by one to an empty list, and the list.
fn build(mns: &[Arc<Dmn>]) -> (u64, MnList) {
    let mut list = MnList::default_pre_dip3();
    let before = requested();
    for dmn in mns {
        list.try_add(Arc::clone(dmn), true).expect("addable");
    }
    (requested() - before, list)
}

#[test]
fn a_list_of_keys_sharing_one_hash_is_built_in_linear_work() {
    const SMALL: usize = 500;
    const LARGE: usize = 2000;
    let mns: Vec<Arc<Dmn>> = (0..LARGE as u32).map(same_prefix).collect();

    let (small, _) = build(&mns[..SMALL]);
    let (large, list) = build(&mns);

    // Linear work makes the large build cost about 4 times the small one (a little more: the
    // maps are one level deeper). A copy of the bucket per key makes it 12 times or more.
    let ratio = large as f64 / small as f64;
    assert!(ratio < 6.0, "{LARGE} keys took {ratio:.1} times the work of {SMALL}");

    // The order is untouched: Core's collision node holds the keys newest first.
    list.check_invariants().expect("invariants");
    let expected: Vec<ProTxHash> = mns.iter().rev().map(|d| d.pro_tx_hash).collect();
    assert_eq!(&*list.core_order(), expected.as_slice());
    let (_, bucket) = list.collision_buckets().next().expect("one bucket");
    assert!(bucket.iter().eq(expected.iter()));
}
