//! BL-61 (O2c Sol r1 F1): an allocation failure in the oracle is `ErrorKind::Exception`, never a
//! verdict on the bytes, in `oracle_p2p_decode` and `oracle_payload_decode`: each call is
//! repeated with each of its `operator new(std::size_t)` calls failing in turn
//! (`support/alloc_fault.rs`), the input's copy, the read's and the write-back's. O1c Sol r1 F2:
//! the same for `oracle_addrman_new`, `oracle_peer_session_new` and `oracle_peer_session_deliver`,
//! whose failures never cross the C ABI (a failed session start leaves no session behind).

mod common;

#[path = "support/alloc_fault.rs"]
mod alloc_fault;

use std::fmt::Debug;

use common::{extra_payload, hex, trivial_vectors};
use dash_oracle_sys::{
    AddrManConfig, Chain, ConnType, ErrorKind, NetAddr, Oracle, P2pStream, P2pVerdict, PeerSession,
    Pin, Result, Service, SessionConfig,
};

/// Calls `call` with each of its allocations failing in turn. Each faulted call must be
/// `ErrorKind::Exception`, or, where Core catches the failure itself, the unfaulted result.
/// Returns the number of allocations and how many of them failed the call.
fn each_allocation_fails<T: Debug + PartialEq>(
    what: &str,
    call: impl Fn() -> Result<T>,
) -> (u32, u32) {
    let unfaulted = call();
    let (mut nth, mut exceptions) = (1, 0);
    while let Some(out) = alloc_fault::failing(nth, &call) {
        match (&out, &unfaulted) {
            (Err(e), _) if e.kind == ErrorKind::Exception => exceptions += 1,
            (Ok(a), Ok(b)) if a == b => {}
            (Err(a), Err(b)) if a.kind == b.kind => {}
            _ => panic!("{what}: allocation {nth} failed, and the oracle returned {out:?}"),
        }
        nth += 1;
    }
    (nth - 1, exceptions)
}

/// Sol r1's case: a getqrinfo with no base hashes, a zero request hash and `extraShare` 2, which
/// Core reads, writing the flag back as 1, and base-sdk refuses (a strict flag). A failed
/// allocation in the write-back read as Core refusing it, so overlay's comparison recorded
/// `BothReject`.
#[test]
fn p2p_decode_allocation_failure_is_no_verdict() {
    let v24 = Oracle::new(Pin::V24_0);
    let stream = P2pStream {
        version: 70242,
        bls_legacy: false,
        qdata_max_vvec: 0,
        qdata_max_contrib: 0,
    };
    let mut raw = [0u8; 34];
    raw[33] = 2;
    match v24.p2p_decode("getqrinfo", &raw, &stream).unwrap() {
        P2pVerdict::Read(r) => assert_eq!((r.consumed, r.reencoded[33]), (34, 1)),
        refused => panic!("{refused:?}"),
    }
    // The read of zero hashes allocates nothing: the input's copy, then the write-back's, and
    // Core catches none of their failures.
    let (n, failed) =
        each_allocation_fails("getqrinfo", || v24.p2p_decode("getqrinfo", &raw, &stream));
    assert!(n >= 2 && failed == n, "getqrinfo: {failed} of {n} allocations failed the call");

    // Without the flag byte Core refuses it, and a failed allocation still is no verdict.
    let short = &raw[..33];
    assert_eq!(v24.p2p_decode("getqrinfo", short, &stream).unwrap_err().kind, ErrorKind::Decode);
    each_allocation_fails("short getqrinfo", || v24.p2p_decode("getqrinfo", short, &stream));
}

/// The same for a special-transaction payload: Core's first trivially valid one
/// (`src/test/data/trivially_valid.json`), whole and one byte short.
#[test]
fn payload_decode_allocation_failure_is_no_verdict() {
    let row = &trivial_vectors("trivially_valid.json")[0];
    let tx = hex(&row[3]);
    let tx_type = u16::from_le_bytes(tx[2..4].try_into().unwrap());
    let legacy = row[2] == "legacy";
    let payload = extra_payload(&tx);
    let short = &payload[..payload.len() - 1];
    for oracle in Oracle::all() {
        let pin = oracle.pin();
        let decoded = oracle.payload_decode(tx_type, legacy, &payload).unwrap();
        assert!(decoded.consumed_all && decoded.reencoded == payload, "{pin}");
        let (n, failed) = each_allocation_fails(&format!("{pin} payload"), || {
            oracle.payload_decode(tx_type, legacy, &payload)
        });
        assert!(failed > 0, "{pin}: {failed} of {n} allocations failed the call");

        assert_eq!(
            oracle.payload_decode(tx_type, legacy, short).unwrap_err().kind,
            ErrorKind::Decode,
            "{pin}"
        );
        each_allocation_fails(&format!("{pin} short payload"), || {
            oracle.payload_decode(tx_type, legacy, short)
        });
    }
}

#[test]
fn addrman_new_allocation_failure_is_no_verdict() {
    for oracle in Oracle::all() {
        let pin = oracle.pin();
        for deterministic in [true, false] {
            let config = AddrManConfig {
                deterministic,
                ..AddrManConfig::default()
            };
            let (n, failed) = each_allocation_fails(&format!("{pin} addrman_new"), || {
                oracle.addrman_new(&config).map(|a| a.is_some())
            });
            assert!(n > 0 && failed > 0, "{pin}: {failed} of {n} allocations failed the call");
        }
    }
}

fn session_config() -> SessionConfig {
    SessionConfig {
        chain: Chain::regtest(),
        our_services: 1 | 4,
        // A day after regtest's genesis.
        mocktime: 1_417_713_337 + 86_400,
        rng_seed: 7,
        blocks: Vec::new(),
        min_chain_work: None,
        max_tip_age: None,
        check_block_index: true,
    }
}

/// A session start that fails part-way frees what it built: the next start succeeds.
///
/// Faults the shim's own work (before Core's `TestingSetup`, and after it: ConnMan, the
/// callbacks), not the Core code it calls that is not exception-safe, where dashd's
/// `new_handler_terminate` ends the process on any failed allocation: the node construction
/// (LevelDB's `DB::Open` holds its mutex across allocations, so `~DBImpl` deadlocks, and a failed
/// block read in `ConnectTip` aborts the node), and `RegisterValidationInterface` (its map entry
/// goes in before the list entry it points to).
#[test]
fn peer_session_new_allocation_failure_is_no_verdict() {
    // RegisterValidationInterface too, in case an archive inlines the shared one into it.
    const CORE: &[&str] =
        &["12TestingSetupC", "RegisterSharedValidationInterface", "RegisterValidationInterface"];
    for oracle in Oracle::all() {
        let pin = oracle.pin();
        let config = session_config();
        // A warm start: the first in a process also runs one-time initialisation (function
        // statics), so its allocations are not those of the starts below.
        drop(oracle.peer_session_new(&config).unwrap());
        let (started, own) = alloc_fault::outside(CORE, || oracle.peer_session_new(&config));
        // A start that survives its failed allocation is the start that had none.
        let unfaulted = started.unwrap().expect("a session").outcome_raw(-1).unwrap();
        let (again, total) = alloc_fault::counting(|| oracle.peer_session_new(&config));
        drop(again);
        assert_eq!(own.len(), total as usize, "{pin}: a warm start's allocations are fixed");
        let sample: Vec<u32> = (1..=total).filter(|nth| own[*nth as usize - 1]).collect();
        let core = own.iter().position(|own| !own).expect("TestingSetup allocates");
        assert!(core > 0 && sample.last() > Some(&(core as u32)), "{pin}: {sample:?}");
        let mut exceptions = 0;
        for nth in sample {
            match alloc_fault::failing(nth, || oracle.peer_session_new(&config)) {
                None => {}
                Some(Ok(Some(mut s))) => {
                    assert_eq!(s.outcome_raw(-1).unwrap(), unfaulted, "{pin}: allocation {nth}");
                }
                Some(Err(e)) if e.kind == ErrorKind::Exception => exceptions += 1,
                Some(out) => panic!(
                    "{pin}: allocation {nth} of {total} failed, and the oracle returned {:?}",
                    out.map(|s| s.is_some())
                ),
            }
            assert!(
                oracle.peer_session_new(&config).unwrap().is_some(),
                "{pin}: after allocation {nth} failed"
            );
        }
        assert!(exceptions > 0, "{pin}");
    }
}

/// A session with an outbound peer through version/verack (Core's reply pushed and read).
fn handshaken(oracle: &Oracle) -> (PeerSession, i64) {
    let mut s = oracle.peer_session_new(&session_config()).unwrap().unwrap();
    let peer = s
        .add_peer(ConnType::OutboundFullRelay, &Service::new(NetAddr::ipv4([1, 2, 3, 4]), 19899))
        .unwrap();
    let mut version = Vec::new();
    version.extend(70242i32.to_le_bytes());
    version.extend(5u64.to_le_bytes());
    version.extend(session_config().mocktime.to_le_bytes());
    for _ in 0..2 {
        version.extend([0u8; 18]);
        version.extend([0xff, 0xff, 127, 0, 0, 1, 0x4d, 0xbb]);
    }
    version.extend(9u64.to_le_bytes());
    version.extend([0, 0, 0, 0, 0, 1]);
    s.deliver(peer, "version", &version, &[]).unwrap();
    s.deliver(peer, "verack", &[], &[]).unwrap();
    for _ in 0..3 {
        s.send_pass(session_config().mocktime).unwrap();
    }
    assert!(s.peer_outcome(peer).unwrap().successfully_connected);
    (s, peer)
}

/// Each allocation of a delivered ping failing: no unwind across the ABI, and the session,
/// broken part-way, refuses further calls instead of reporting a run that did not happen.
#[test]
fn peer_session_deliver_allocation_failure_is_no_verdict() {
    for oracle in Oracle::all() {
        let pin = oracle.pin();
        let (mut s, peer) = handshaken(&oracle);
        let (ok, total) = alloc_fault::counting(|| s.deliver(peer, "ping", &[1; 8], &[]));
        ok.unwrap();
        // A delivery that survives its failed allocation is the delivery that had none.
        let processed = |s: &mut PeerSession| {
            s.send_pass(session_config().mocktime).unwrap();
            (s.outcome_raw(peer).unwrap(), s.outcome_raw(-1).unwrap())
        };
        let unfaulted = processed(&mut s);
        drop(s);
        assert!(total > 0, "{pin}");
        for nth in 1..=total {
            let (mut s, peer) = handshaken(&oracle);
            match alloc_fault::failing(nth, || s.deliver(peer, "ping", &[1; 8], &[])) {
                Some(Err(e)) if e.kind == ErrorKind::Exception => {
                    assert_eq!(
                        s.send_pass(session_config().mocktime).unwrap_err().kind,
                        ErrorKind::Exception,
                        "{pin}"
                    );
                }
                Some(Ok(())) => assert!(processed(&mut s) == unfaulted, "{pin}: allocation {nth}"),
                out => panic!(
                    "{pin}: allocation {nth} of {total} failed, and the oracle returned {out:?}"
                ),
            }
        }

        // A command longer than 12 bytes is refused before anything is allocated.
        let (mut s, peer) = handshaken(&oracle);
        let long = "a".repeat(32);
        let (refused, allocations) = alloc_fault::counting(|| s.deliver(peer, &long, &[], &[]));
        assert_eq!((refused.unwrap_err().kind, allocations), (ErrorKind::Argument, 0), "{pin}");
    }
}
