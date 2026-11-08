//! NetInfo calls (validate and add_entry) and `oracle_netgroup` racing `oracle_set_network`
//! (ABI 16): each call reads one selection's chain parameters and `-allowprivatenet` throughout,
//! as it checks the selection (or, for the group, takes the lock) under the process-state lock
//! the selection is made under.
//!
//! The calls go to the C entry points directly, from several threads: `Oracle` serializes its
//! own callers, so it cannot race them. The selections alternate between mainnet and regtest,
//! both with `-allowprivatenet`. The NetInfo calls tell the two apart (the routable rule and
//! the mainnet port), but since rc.4 they refuse an RFC1918 address on mainnet whatever the flag
//! holds (netinfo.cpp:294, 456). The group of that address still reads the flag (netgroup.cpp:43),
//! so a call that read the flag at its default (as `oracle_set_network` holds it until it
//! assigns the option) returns a group neither selection gives, and the calibration below
//! checks that it does. This binary holds no other test: nothing else selects a network in its
//! process.

use std::ffi::{CString, c_char, c_int};
use std::sync::Barrier;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use dash_oracle_sys::{NetAddr, NetInfoKind, NetInfoStatus, Oracle, Pin, ffi};

/// Selections made at least, and (until the calls have read both kinds) at most.
const FLIPS: usize = 2000;
const MAX_FLIPS: usize = 200_000;
const READERS: usize = 3;
/// NetInfoPurpose::CORE_P2P.
const CORE_P2P: u8 = 0;

/// A selection: the chain and its options.
type Selection = (&'static str, &'static [&'static str]);

const MAIN_ALLOWED: Selection = ("main", &["-allowprivatenet"]);
const REGTEST: Selection = ("regtest", &["-allowprivatenet"]);
/// What a call that read mainnet's parameters with the default flag would see.
const MAIN_DEFAULT: Selection = ("main", &[]);
/// `GetGroup()`'s first byte, the address's network class: `NET_UNROUTABLE` and `NET_IPV4`.
const UNROUTABLE: u8 = 0;
const IPV4: u8 = 1;

fn select(selection: Selection) {
    let chain = CString::new(selection.0).unwrap();
    let args: Vec<CString> = selection.1.iter().map(|a| CString::new(*a).unwrap()).collect();
    let argv: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).collect();
    let status =
        unsafe { (ffi::v24_0::API.set_network)(chain.as_ptr(), argv.as_ptr(), argv.len()) };
    assert_eq!(status, ffi::ORACLE_OK, "set_network {selection:?}");
}

/// `MnNetInfo` 10.0.0.1:9999: an IPv4-mapped `CService`, big-endian port.
fn mn_private() -> Vec<u8> {
    let mut out = vec![0u8; 10];
    out.extend([0xff, 0xff, 10, 0, 0, 1]);
    out.extend(9999u16.to_be_bytes());
    out
}

/// `ExtNetInfo` version 1 with one CORE_P2P entry, the `CService` 10.0.0.1:9999.
fn ext_private() -> Vec<u8> {
    let mut out = vec![1, 1, 0, 1, 0x01, 1, 4, 10, 0, 0, 1];
    out.extend(9999u16.to_be_bytes());
    out
}

/// One NetInfo call, made through the raw entry point.
#[derive(Debug)]
enum Call {
    /// `oracle_netinfo_validate` on the object.
    Validate(NetInfoKind, Vec<u8>),
    /// `oracle_netinfo_add_entry` of a CORE_P2P entry to an empty object of the kind.
    Add(NetInfoKind, &'static str),
    /// `oracle_netgroup` of the address without an asmap; its "status" is the group's network
    /// class.
    Group(NetAddr),
}

impl Call {
    /// Its return code and `*status` (a group's class).
    fn run(&self) -> (c_int, u8) {
        let mut status = 0xffu8;
        let ret = match self {
            Call::Validate(kind, netinfo) => unsafe {
                (ffi::v24_0::API.netinfo_validate)(
                    *kind as c_int,
                    netinfo.as_ptr(),
                    netinfo.len(),
                    &mut status,
                )
            },
            Call::Add(kind, input) => {
                // Empty objects: an empty `CService`; `ExtNetInfo` version 1 with no purposes.
                let empty = match kind {
                    NetInfoKind::Mn => vec![0u8; 18],
                    NetInfoKind::Ext => vec![1, 0],
                };
                let (mut entry, mut netinfo) = ([0u8; 512], [0u8; 512]);
                let (mut entry_len, mut netinfo_len) = (entry.len(), netinfo.len());
                unsafe {
                    (ffi::v24_0::API.netinfo_add_entry)(
                        *kind as c_int,
                        empty.as_ptr(),
                        empty.len(),
                        CORE_P2P,
                        input.as_ptr(),
                        input.len(),
                        &mut status,
                        entry.as_mut_ptr(),
                        &mut entry_len,
                        netinfo.as_mut_ptr(),
                        &mut netinfo_len,
                    )
                }
            }
            Call::Group(addr) => {
                let mut encoded = Vec::new();
                addr.encode(&mut encoded);
                let (mut group, mut group_len, mut asn) = ([0u8; 64], 64, 0u32);
                let ret = unsafe {
                    (ffi::v24_0::API.netgroup)(
                        encoded.as_ptr(),
                        encoded.len(),
                        [].as_ptr(),
                        0,
                        group.as_mut_ptr(),
                        &mut group_len,
                        &mut asn,
                    )
                };
                status = group[0];
                ret
            }
        };
        (ret, status)
    }
}

/// Every call's status under `selection`, sequentially.
fn verdicts(selection: Selection, calls: &[Call]) -> Vec<u8> {
    select(selection);
    calls
        .iter()
        .map(|call| {
            let (ret, status) = call.run();
            assert_eq!(ret, ffi::ORACLE_OK, "{selection:?} {call:?}");
            status
        })
        .collect()
}

#[test]
fn netinfo_reads_one_selection_while_another_thread_selects() {
    let oracle = Oracle::new(Pin::V24_0);
    assert_eq!(oracle.abi_version(), ffi::ABI_VERSION);
    let calls = [
        Call::Validate(NetInfoKind::Mn, mn_private()),
        Call::Validate(NetInfoKind::Ext, ext_private()),
        Call::Add(NetInfoKind::Mn, "10.0.0.1:9999"),
        Call::Add(NetInfoKind::Ext, "10.0.0.1:9999"),
        Call::Group(NetAddr::ipv4([10, 0, 0, 1])),
    ];

    // Calibration: the NetInfo calls give each selection's verdicts, the same with the flag at
    // its default; only the group shows the torn state, a verdict neither selection gives.
    let main = verdicts(MAIN_ALLOWED, &calls);
    let regtest = verdicts(REGTEST, &calls);
    let torn = verdicts(MAIN_DEFAULT, &calls);
    let not_routable = NetInfoStatus::NotRoutable as u8;
    assert_eq!(main, [not_routable, not_routable, not_routable, not_routable, IPV4]);
    assert_eq!(torn, [not_routable, not_routable, not_routable, not_routable, UNROUTABLE]);
    for i in 0..4 {
        assert_ne!(main[i], regtest[i], "{:?}: both selections give {}", calls[i], main[i]);
    }
    assert_eq!(regtest[4], IPV4);

    // The first call's verdicts show which selections were read.
    select(MAIN_ALLOWED);
    let read = [AtomicBool::new(false), AtomicBool::new(false)];
    let done = AtomicBool::new(false);
    let start = Barrier::new(READERS + 1);
    let flips = thread::scope(|scope| {
        for _ in 0..READERS {
            scope.spawn(|| {
                start.wait();
                while !done.load(Ordering::Relaxed) {
                    for (i, call) in calls.iter().enumerate() {
                        let (ret, status) = call.run();
                        assert_eq!(ret, ffi::ORACLE_OK, "{call:?}: a call saw no selection");
                        assert!(
                            status == main[i] || status == regtest[i],
                            "{call:?}: status {status} is neither selection's"
                        );
                        if i == 0 {
                            read[usize::from(status == regtest[0])].store(true, Ordering::Relaxed);
                        }
                    }
                }
            });
        }
        // The readers stop however the selecting ends (a failed selection must not hang them).
        let _stop = Stop(&done);
        start.wait();
        let mut flips = 0;
        while flips < MAX_FLIPS
            && (flips < FLIPS || !read.iter().all(|r| r.load(Ordering::Relaxed)))
        {
            select(if flips % 2 == 0 {
                REGTEST
            } else {
                MAIN_ALLOWED
            });
            flips += 1;
        }
        flips
    });
    for (s, name) in ["mainnet", "regtest"].iter().enumerate() {
        assert!(read[s].load(Ordering::Relaxed), "no call read {name} in {flips} selections");
    }
}

/// Sets the flag when dropped.
struct Stop<'a>(&'a AtomicBool);

impl Drop for Stop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}
