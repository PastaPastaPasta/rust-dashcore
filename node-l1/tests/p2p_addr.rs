//! The address relay's single-command targets (P2a-net): each registered, the empty input out of
//! domain, generated scripts agree on both legs with the node's `NodeAddressRelay` installed
//! (P2a-net D24), and on Core's leg alone they relay, answer `getaddr` and keep their peers.

use std::collections::BTreeMap;

use dash_core_vectors::hex::parse_hex;
use dash_oracle_sys::{Chain, ConnType, Oracle, Pin, SessionConfig, Substitution, Transport};
use node_l1::p2p_addr::{
    ADDR_FETCH_TIMEOUT, ADDRV2_PROTO_VERSION, Ends, GENESIS_TIME, PORT, SELFADDR_MAX_TIP_AGE, T0,
};
use node_l1::p2p_node::{Session, Step};
use node_l1::{Outcome, target};

/// `count` deterministic pseudo-random inputs (splitmix64), as the other session tests.
fn inputs(count: u64) -> impl Iterator<Item = Vec<u8>> {
    (0..count).map(|seed| {
        let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        (0..48 + 64 * (seed % 16) as usize)
            .map(|_| {
                x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
                let mut z = x;
                z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                (z ^ (z >> 31)) as u8
            })
            .collect()
    })
}

/// Every script agrees on both legs, with the node's relay installed (P2a-net D29): 64 inputs
/// per target, `diff_p2p_multi_relay`'s too (its fuzzing stays parked, D32), and in `FIXED` the
/// inputs that once diverged on base-sdk's `fc00::/8` decoding (P2a-net D30).
#[test]
fn address_targets_agree() {
    const FIXED: [(&str, u64); 10] = [
        ("diff_p2p_addr", 198),
        ("diff_p2p_addrv2", 301),
        ("diff_p2p_getaddr", 184),
        ("diff_p2p_getaddr", 188),
        ("diff_p2p_getaddr", 206),
        ("diff_p2p_getaddr", 230),
        ("diff_p2p_getaddr", 231),
        ("diff_p2p_getaddr", 300),
        ("diff_p2p_getaddr", 301),
        ("diff_p2p_getaddr", 312),
    ];
    let all = inputs(313).collect::<Vec<_>>();
    let names = ["diff_p2p_addr", "diff_p2p_addrv2", "diff_p2p_getaddr", "diff_p2p_multi_relay"];
    let cases = names
        .iter()
        .flat_map(|&name| all[..64].iter().map(move |input| (name, input)))
        .chain(FIXED.iter().map(|&(name, k)| (name, &all[k as usize])));
    for (name, input) in cases {
        let t = target(name).expect("registered");
        let outcome = (t.compare)(input, Pin::V24_0);
        assert!(
            matches!(outcome, Ok(o) if o != Outcome::OutOfDomain),
            "{name} len {}: {outcome:?}",
            input.len()
        );
    }
    for name in names {
        let t = target(name).expect("registered");
        assert_eq!((t.compare)(&[], Pin::V24_0), Ok(Outcome::OutOfDomain), "{name}");
    }
}

/// `diff_p2p_selfaddr`'s scripts agree on both legs out of IBD (P2a-net BL-203): the
/// self-announcement frames, `m_next_local_addr_send` and its draw's place in the stream, the
/// filter reset when the timer fires again, and headers sync with every peer.
#[test]
fn selfaddr_targets_agree() {
    let t = target("diff_p2p_selfaddr").expect("registered");
    for input in inputs(64) {
        let outcome = (t.compare)(&input, Pin::V24_0);
        assert!(
            matches!(outcome, Ok(o) if o != Outcome::OutOfDomain),
            "diff_p2p_selfaddr len {}: {outcome:?}",
            input.len()
        );
    }
    assert_eq!((t.compare)(&[], Pin::V24_0), Ok(Outcome::OutOfDomain));
}

/// `diff_p2p_multi_relay` sends raw `addrv2` payloads that `encode_v2`'s remap never touches,
/// so it can still relay a BIP155 IPv6 address in `fc00::/8` that base-sdk reads as CJDNS: this
/// input (Opus, P2a-net D32) diverges on peer state until BS10 lands; the target stays parked.
#[test]
#[ignore = "base-sdk reads fc00::/8 as CJDNS until BS10 (P2a-net D32)"]
fn multi_relay_fc00_agrees() {
    let mut input = parse_hex(concat!(
        "07070707070707070200020200010001043030303030303030011500000500000101010431313131313131",
        "310115010005010001020104323232323232323201150200050200010301043333333333333333011503",
        "000503001103000d001a0139e88154010210fc0102030405060708090a0b0c0d0e0f4dbb",
    ));
    for _ in 0..16 {
        input.extend([0x04, 0x14, 0x00]);
    }
    let t = target("diff_p2p_multi_relay").expect("registered");
    let outcome = (t.compare)(&input, Pin::V24_0);
    assert!(matches!(outcome, Ok(o) if o != Outcome::OutOfDomain), "{outcome:?}");
}

/// Why Core ended a peer, as the script predicts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Ended {
    /// A feeler, at its `version`.
    Feeler,
    /// A `sendaddrv2` after `verack`, at [`ADDRV2_PROTO_VERSION`] or later.
    LateSendAddrV2,
    /// An ADDR_FETCH peer, at an address message of more than one entry.
    AddrFetchDone,
    /// An ADDR_FETCH peer, at a pass past [`ADDR_FETCH_TIMEOUT`] after the session's start once
    /// connected.
    AddrFetchTimeout,
}

/// What the script has sent a peer so far.
#[derive(Clone, Default)]
struct Seen {
    asked: bool,
    /// The protocol version of its `version`, once sent.
    version: Option<i32>,
    verack: bool,
    late_v2: bool,
    /// The step of the last address message claiming more than one entry, read or not.
    /// `AddrFetchDone` is credited in the pass right after it: Core ends no peer at one it cannot
    /// read, and the generator ends an ADDR_FETCH peer past its timeout before choosing a
    /// sender, so that pass never also crosses the timeout. Whether Core ends the peer there is
    /// checked exactly against the script's own ends.
    fetched: Option<usize>,
    ended: bool,
}

/// Why Core may end a peer of kind `conn` after what it was sent, seen at step `k`, a pass at
/// `pass_t` (`None` for a delivery: one that ends a peer by itself is unexpected).
fn end_reason(
    conn: ConnType,
    seen: &Seen,
    k: usize,
    pass_t: Option<i64>,
    t0: i64,
) -> Option<Ended> {
    let addr_fetch = conn == ConnType::AddrFetch;
    if conn == ConnType::Feeler && seen.version.is_some() {
        Some(Ended::Feeler)
    } else if seen.late_v2 {
        Some(Ended::LateSendAddrV2)
    } else if addr_fetch && pass_t.is_some() && seen.fetched == Some(k.wrapping_sub(1)) {
        // Only in the pass that processes the message.
        Some(Ended::AddrFetchDone)
    } else if addr_fetch && seen.verack && pass_t.is_some_and(|t| t - t0 > ADDR_FETCH_TIMEOUT) {
        Some(Ended::AddrFetchTimeout)
    } else {
        None
    }
}

/// What Core's own session did running a script.
#[derive(Default)]
struct CoreRun {
    /// `addr`/`addrv2` sent to a peer that never sent `getaddr`: relays.
    relays: usize,
    /// `addr`/`addrv2` sent to a peer that did: replies (or relays to it).
    replies: usize,
    /// Why Core ended each peer it ended.
    ended: Vec<Ended>,
    /// Per peer, the address messages that carried our own address as that peer sees us
    /// ([`announced`]).
    self_announced: Vec<usize>,
    /// Per peer, `m_next_local_addr_send` (µs) after the last step.
    next_local_addr_send: Vec<i64>,
    /// Per peer, whether Core sent it `getheaders` or `getheaders2`.
    getheaders: Vec<bool>,
}

/// The entry count an address message claims (its CompactSize), or 0 when the prefix is absent
/// or wider than three bytes.
fn claimed_entries(payload: &[u8]) -> u64 {
    match payload {
        [n @ 0..0xfd, ..] => u64::from(*n),
        [0xfd, a, b, ..] => u64::from(u16::from_le_bytes([*a, *b])),
        _ => 0,
    }
}

/// What Core advertises to a peer whose `version` said `addr_recv` (16-byte IP, port), if
/// anything: the IP at our listening port for an outbound peer, all of it for an inbound one
/// (net.cpp:262-272). Routability is Core's to decide; an unroutable one never matches a frame
/// because Core sends none.
fn self_address(conn: ConnType, addr_recv: &[u8]) -> Option<([u8; 16], u16)> {
    let ip: [u8; 16] = addr_recv.get(..16)?.try_into().ok()?;
    if ip == [0; 16] {
        return None;
    }
    let port = if conn == ConnType::Inbound {
        u16::from_be_bytes(addr_recv.get(16..18)?.try_into().ok()?)
    } else {
        PORT
    };
    Some((ip, port))
}

/// Whether an `addr` (v1) or `addrv2` payload holds `ip` at `port`.
fn announced(command: &str, payload: &[u8], (ip, port): ([u8; 16], u16)) -> bool {
    let mut needle = Vec::new();
    if command == "addr" {
        needle.extend(ip);
    } else if ip[..12] == [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff] {
        needle.extend([1, 4]);
        needle.extend(&ip[12..]);
    } else {
        needle.extend([2, 16]);
        needle.extend(ip);
    }
    needle.extend(port.to_be_bytes());
    payload.windows(needle.len()).any(|w| w == needle.as_slice())
}

/// Runs `s` on Core's session alone. Every delivery must reach a peer Core has not ended, every
/// peer Core ends must be one the script expects it to end, for that reason, and after every
/// pass the peers Core has ended must be exactly those `ends` lists: none the script stopped
/// using is still live in Core. Without `ends` (an IBD control of a script built for a session
/// out of IBD), deliveries to peers Core ended are skipped and its ends are not checked.
fn core_run(oracle: &Oracle, s: &Session, ends: Option<&Ends>) -> Result<CoreRun, String> {
    let config = SessionConfig {
        chain: Chain::regtest(),
        our_services: s.our_services,
        mocktime: s.mocktime,
        rng_seed: s.seed,
        blocks: Vec::new(),
        min_chain_work: None,
        max_tip_age: s.max_tip_age,
        check_block_index: false,
    };
    let mut session = oracle.peer_session_new(&config).expect("session").expect("tip");
    let mut selves: Vec<Option<([u8; 16], u16)>> = vec![None; s.peers.len()];
    let ids: Vec<i64> = s
        .peers
        .iter()
        .map(|p| session.add_peer_with(p.conn, Transport::V1, p.send_mode, &p.addr).expect("peer"))
        .collect();
    let mut seen = vec![Seen::default(); ids.len()];
    let mut run = CoreRun {
        self_announced: vec![0; ids.len()],
        next_local_addr_send: vec![0; ids.len()],
        getheaders: vec![false; ids.len()],
        ..CoreRun::default()
    };
    let strict = ends.is_some();
    let mut expected = ends.into_iter().flatten();
    for (k, step) in s.steps.iter().enumerate() {
        let pass_t = if let Step::Pass(t) = step {
            Some(*t)
        } else {
            None
        };
        match step {
            Step::Deliver {
                peer,
                command,
                payload,
                subs,
            } => {
                let p = &mut seen[*peer];
                if p.ended && !strict {
                    continue;
                }
                if p.ended {
                    return Err(format!("step {k}: {command} to peer {peer}, which Core ended"));
                }
                match *command {
                    "getaddr" => p.asked = true,
                    "version" => {
                        p.version =
                            payload.get(..4).map(|v| i32::from_le_bytes(v.try_into().unwrap()));
                        // After nVersion, nServices, nTime and addr_recv's services.
                        selves[*peer] =
                            payload.get(28..46).and_then(|a| self_address(s.peers[*peer].conn, a));
                    }
                    "verack" => p.verack = true,
                    // Core: net_processing.cpp:4292-4304, the version first.
                    "sendaddrv2"
                        if p.verack && p.version.is_some_and(|v| v >= ADDRV2_PROTO_VERSION) =>
                    {
                        p.late_v2 = true;
                    }
                    "addr" | "addrv2" if claimed_entries(payload) > 1 => p.fetched = Some(k),
                    _ => {}
                }
                let subs: Vec<Substitution> = subs
                    .iter()
                    .map(|x| Substitution {
                        src_peer: ids[x.src_peer as usize],
                        ..x.clone()
                    })
                    .collect();
                session
                    .deliver(ids[*peer], command, payload, &subs)
                    .map_err(|e| format!("step {k}: {command} to peer {peer}: {}", e.message))?;
            }
            Step::Pass(t) => session.send_pass(*t).expect("pass"),
            other => panic!("the address scripts have no {other:?}"),
        }
        for (i, id) in ids.iter().enumerate() {
            let o = session.peer_outcome(*id).expect("outcome");
            // `sent` holds what Core sent since the last call.
            let sends = o.sent.iter().filter(|(c, _)| c == "addr" || c == "addrv2").count();
            if let Some(me) = selves[i] {
                run.self_announced[i] += o
                    .sent
                    .iter()
                    .filter(|(c, p)| (c == "addr" || c == "addrv2") && announced(c, p, me))
                    .count();
            }
            run.getheaders[i] |= o.sent.iter().any(|(c, _)| c.starts_with("getheaders"));
            run.next_local_addr_send[i] = o.peer.as_ref().map_or(0, |p| p.next_local_addr_send_us);
            if seen[i].asked {
                run.replies += sends;
            } else {
                run.relays += sends;
            }
            if seen[i].ended || !(o.removed || o.disconnect) {
                continue;
            }
            seen[i].ended = true;
            if !strict {
                continue;
            }
            let conn = s.peers[i].conn;
            let Some(reason) = end_reason(conn, &seen[i], k, pass_t, s.mocktime) else {
                return Err(format!("step {k}: Core ended peer {i} ({conn:?}) unexpectedly"));
            };
            run.ended.push(reason);
        }
        if pass_t.is_some() && strict {
            let want = match expected.next() {
                Some((at, want)) if *at == k => want,
                other => return Err(format!("step {k}: the script's ends are at {other:?}")),
            };
            let got: Vec<bool> = seen.iter().map(|p| p.ended).collect();
            if &got != want {
                return Err(format!("step {k}: Core has ended {got:?}, the script {want:?}"));
            }
        }
    }
    match expected.next() {
        None => Ok(run),
        Some((at, _)) => Err(format!("the script's ends at step {at} are past its last step")),
    }
}

/// The scripts reach Core's address paths through peers Core keeps: on 64 inputs per target,
/// every delivery reaches a live peer, Core ends exactly the peers the script ends on purpose,
/// Core relays in a quarter of the scripts at least, and answers `getaddr` in half of
/// `diff_p2p_getaddr`'s (P2a-net D25).
#[test]
fn scripts_reach_core_address_paths() {
    let oracle = Oracle::new(Pin::V24_0);
    for name in ["diff_p2p_addr", "diff_p2p_addrv2", "diff_p2p_getaddr"] {
        let (mut relays, mut replies, mut relaying, mut replying) = (0, 0, 0, 0);
        let mut reasons = BTreeMap::<Ended, usize>::new();
        for input in inputs(64) {
            let (s, ends) = node_l1::p2p_addr::script(name, &input).expect("in domain");
            let run = core_run(&oracle, &s, Some(&ends))
                .unwrap_or_else(|e| panic!("{name} len {}: {e}", input.len()));
            relays += run.relays;
            replies += run.replies;
            relaying += usize::from(run.relays > 0);
            replying += usize::from(run.replies > 0);
            for reason in run.ended {
                *reasons.entry(reason).or_default() += 1;
            }
        }
        eprintln!(
            "{name}: relays {relays} in {relaying} scripts, replies {replies} in {replying}, \
             ended {reasons:?}"
        );
        // Measured at 07dbd6483: relays in 22-26 scripts of 64, getaddr replies in 47.
        assert!(relaying >= 16, "{name}: Core relayed in {relaying} of 64 scripts");
        if name == "diff_p2p_getaddr" {
            assert!(replying >= 32, "{name}: Core answered getaddr in {replying} of 64 scripts");
        }
    }
}

/// A manual peer at 70222 sends `sendaddrv2` after `verack`: Core ignores it below
/// `ADDRV2_PROTO_VERSION` (net_processing.cpp:4292-4304) and keeps the peer, so it still seeds
/// AddrMan and Core answers the inbound peer's `getaddr` from it (Sol r4 R4-M1).
#[test]
fn late_sendaddrv2_at_70222_keeps_the_manual_seeder() {
    // Seed; two peers: inbound (70242, no `sendaddrv2`, no blocks), manual (70222, late
    // `sendaddrv2`); both handshake now; the manual peer seeds 10 addresses; then, at no clock
    // move, the inbound peer's `getaddr`.
    let mut input = vec![0; 8];
    input.extend([1, 0x00, 0x00, 0x58, 0x00, 0x00, 0x00, 0x00, 0x21]);
    let (s, ends) = node_l1::p2p_addr::script("diff_p2p_getaddr", &input).expect("in domain");
    assert_eq!(s.peers[1].conn, ConnType::Manual);
    let sent: Vec<(&str, &Vec<u8>)> = s
        .steps
        .iter()
        .filter_map(|step| match step {
            Step::Deliver {
                peer: 1,
                command,
                payload,
                ..
            } => Some((*command, payload)),
            _ => None,
        })
        .collect();
    let commands: Vec<&str> = sent.iter().map(|(c, _)| *c).collect();
    let version = sent.iter().find(|(c, _)| *c == "version").expect("version").1;
    assert_eq!(&version[..4], &(ADDRV2_PROTO_VERSION - 1).to_le_bytes());
    let late = commands.iter().position(|c| *c == "sendaddrv2").expect("sendaddrv2");
    assert!(commands[..late].contains(&"verack"), "{commands:?}");
    assert!(commands[late..].contains(&"addr"), "the manual peer seeds after it: {commands:?}");
    assert!(ends.iter().all(|(_, e)| !e[1]), "the script keeps the manual peer");
    let run = core_run(&Oracle::new(Pin::V24_0), &s, Some(&ends)).unwrap_or_else(|e| panic!("{e}"));
    assert!(run.ended.is_empty(), "{:?}", run.ended);
    assert!(run.replies > 0, "Core answers the inbound peer's getaddr");
}

/// `diff_p2p_selfaddr`'s clock (P2a-net BL-203, D40): its `T0` is at least 24 h after genesis,
/// so at Core's default 6 h tip age the IBD control stays in IBD, and every script ends inside
/// `-maxtipage` = 30 days of it, so it is out of IBD and fans headers sync out throughout.
#[test]
fn selfaddr_clock_stays_inside_the_tip_age_window() {
    const DEFAULT_MAX_TIP_AGE: i64 = 6 * 3600;
    const { assert!(T0 - GENESIS_TIME >= 86_400 && T0 - GENESIS_TIME > DEFAULT_MAX_TIP_AGE) };
    const { assert!(T0 - GENESIS_TIME < SELFADDR_MAX_TIP_AGE) };
    for input in inputs(256) {
        let (s, _) = node_l1::p2p_addr::script("diff_p2p_selfaddr", &input).expect("in domain");
        assert_eq!(s.max_tip_age, Some(SELFADDR_MAX_TIP_AGE));
        let last = s
            .steps
            .iter()
            .filter_map(|step| match step {
                Step::Pass(t) => Some(*t),
                _ => None,
            })
            .max()
            .expect("a pass");
        assert!(last - GENESIS_TIME < SELFADDR_MAX_TIP_AGE, "len {}: ends at {last}", input.len());
    }
}

/// Out of IBD Core advertises our address to the peers that say they see us at a routable one,
/// and again after the self-announcement timer fires; in IBD (the same scripts at the default
/// tip age) it advertises nothing and never draws the timer (P2a-net BL-203). Every delivery
/// still reaches a live peer and Core ends exactly the peers the script ends.
#[test]
fn scripts_reach_core_self_announce() {
    let oracle = Oracle::new(Pin::V24_0);
    let (mut announcing, mut again, mut timed, mut announcements) = (0, 0, 0, 0);
    let (mut fanned, mut fanned_control) = (0, 0);
    let syncing = |run: &CoreRun| run.getheaders.iter().filter(|&&g| g).count();
    for input in inputs(64) {
        let (s, ends) = node_l1::p2p_addr::script("diff_p2p_selfaddr", &input).expect("in domain");
        let run = core_run(&oracle, &s, Some(&ends))
            .unwrap_or_else(|e| panic!("diff_p2p_selfaddr len {}: {e}", input.len()));
        announcements += run.self_announced.iter().sum::<usize>();
        announcing += usize::from(run.self_announced.iter().any(|&n| n > 0));
        again += usize::from(run.self_announced.iter().any(|&n| n > 1));
        timed += usize::from(run.next_local_addr_send.iter().any(|&t| t != 0));
        fanned += usize::from(syncing(&run) > 1);
        let control = Session {
            max_tip_age: None,
            ..s
        };
        let run = core_run(&oracle, &control, None)
            .unwrap_or_else(|e| panic!("IBD control len {}: {e}", input.len()));
        assert!(run.self_announced.iter().all(|&n| n == 0), "IBD control len {}", input.len());
        assert!(
            run.next_local_addr_send.iter().all(|&t| t == 0),
            "IBD control len {}",
            input.len()
        );
        // In IBD Core syncs headers from one peer at a time (net_processing.cpp:6381), and
        // from another only once a timeout ended the first (6797).
        fanned_control += usize::from(syncing(&run) > 1);
    }
    eprintln!(
        "diff_p2p_selfaddr: {announcements} self-announcements in {announcing} scripts, again \
         in {again}, timer drawn in {timed}, headers sync with several peers in {fanned} (IBD \
         control: {fanned_control})"
    );
    // Measured on the BL-203 branch (P2a-net D41): self-announcements in 51 scripts of 64, again
    // in 31, the timer drawn in 63, headers sync with several peers in 30 (IBD control: 9).
    assert!(announcing >= 32, "Core advertised our address in {announcing} of 64 scripts");
    assert!(again >= 16, "Core advertised it again in {again} of 64 scripts");
    assert!(timed >= 48, "Core drew the self-announcement timer in {timed} of 64 scripts");
    assert!(fanned >= 16, "Core synced headers from several peers in {fanned} of 64 scripts");
    assert!(fanned > fanned_control, "out of IBD {fanned}, in IBD {fanned_control}");
}
