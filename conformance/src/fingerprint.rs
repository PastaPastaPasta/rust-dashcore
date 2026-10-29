//! O13a's fingerprint lab v0 (docs/node-program/designs/O13a-notes.md): the judge of
//! protocol-parity handshake transcripts.
//!
//! A transcript is P2a's fixture format (`contrib/node/p2a-transcripts/capture.py`, extended by
//! `contrib/node/fingerprint/capture.py`): each input opens a segment, and the `<` records after
//! it are what the node sent each peer in that segment, in order. [`diff`] compares dashd's
//! transcript with rdashd's segment by segment and peer by peer, after [`mask`]; the inputs must
//! be the same script. Nothing is dropped. [`Profile`] reads the dashd arguments a fixture was
//! captured with.
//!
//! The masks, each with the Core line (tag v24.0.0-rc.3) that makes the field random; nothing
//! else is masked:
//! - `version`'s nonce: net.cpp:492 (outbound) and :1970 (inbound) derive it from the
//!   connection manager's random seeds (`GetDeterministicRandomizer(RANDOMIZER_ID_LOCALHOSTNONCE)`),
//!   and net_processing.cpp:1565 sends it;
//! - `version`'s MNAUTH challenge: net_processing.cpp:1573-1574 `GetRandBytes`;
//! - `version`'s user agent: not random, but identity, which DEC-83 leaves out; the fields after
//!   it are compared at their own places;
//! - `ping`'s nonce: net_processing.cpp:6177 `GetRand<uint64_t>()` (`MaybeSendPing`).
//!
//! V2's key, garbage, garbage terminator and decoys (net.cpp:1061-1065, :1101) never reach a
//! transcript: the capture records messages after the transport. `version`'s time is not masked:
//! both legs freeze the mock time.

use std::collections::BTreeMap;
use std::fmt;

/// `version`'s fixed fields before the user agent: version, services, time, `addr_recv`,
/// `addr_from` (26 bytes each, without a time) and the nonce.
const VERSION_UA_OFFSET: usize = 80;
/// Where the nonce lies in `version`.
const VERSION_NONCE: std::ops::Range<usize> = 72..80;
/// `version`'s fields from the start height to the MNAUTH challenge: the start height and
/// `fRelay`.
const VERSION_BEFORE_CHALLENGE: usize = 5;
/// The MNAUTH challenge's length.
const MNAUTH_CHALLENGE_LEN: usize = 32;

/// What stands in for the user agent in a masked `version`.
pub const USER_AGENT: &str = "<user agent>";

/// Lowercase hex.
fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

/// A compact size at `p[at..]`: its value and its length; `None` when cut short.
fn compact_size(p: &[u8], at: usize) -> Option<(usize, usize)> {
    let first = *p.get(at)?;
    let (len, bytes) = match first {
        0xfd => (3, p.get(at + 1..at + 3)?.to_vec()),
        0xfe => (5, p.get(at + 1..at + 5)?.to_vec()),
        0xff => (9, p.get(at + 1..at + 9)?.to_vec()),
        n => return Some((usize::from(n), 1)),
    };
    let mut v = [0u8; 8];
    v[..bytes.len()].copy_from_slice(&bytes);
    Some((usize::try_from(u64::from_le_bytes(v)).ok()?, len))
}

/// `cmd`'s `payload` after the masks (module docs), as text: hex, with the user agent of a
/// `version` replaced by [`USER_AGENT`]. A payload too short for a masked field keeps what it
/// has, so a cut message still differs from a whole one.
pub fn mask(cmd: &str, payload: &[u8]) -> String {
    let mut p = payload.to_vec();
    match cmd {
        "version" => {
            if let Some(nonce) = p.get_mut(VERSION_NONCE) {
                nonce.fill(0);
            }
            let Some((ua_len, len_bytes)) = compact_size(&p, VERSION_UA_OFFSET) else {
                return hex(&p);
            };
            let after = VERSION_UA_OFFSET + len_bytes + ua_len;
            if after > p.len() {
                return hex(&p);
            }
            let mut tail = p.split_off(after);
            let challenge =
                VERSION_BEFORE_CHALLENGE..VERSION_BEFORE_CHALLENGE + MNAUTH_CHALLENGE_LEN;
            if let Some(c) = tail.get_mut(challenge) {
                c.fill(0);
            }
            p.truncate(VERSION_UA_OFFSET);
            return format!("{}{USER_AGENT}{}", hex(&p), hex(&tail));
        }
        "ping" => p.fill(0),
        _ => {}
    }
    hex(&p)
}

/// Decodes hex; `None` when it is not hex.
fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// A `<` record's message after the masks: `cmd payload [form]` (`-` for an empty payload), or
/// `closed`.
fn masked_record(rest: &[&str]) -> String {
    match rest {
        ["closed"] => "closed".to_owned(),
        [cmd, payload, form @ ..] => {
            let body = unhex(payload).map_or_else(|| (*payload).to_owned(), |p| mask(cmd, &p));
            let body = if body.is_empty() {
                "-"
            } else {
                &body
            };
            let mut out = format!("{cmd} {body}");
            for f in form {
                out.push(' ');
                out.push_str(f);
            }
            out
        }
        [cmd] => (*cmd).to_owned(),
        [] => String::new(),
    }
}

/// One input and what the node sent each peer after it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Segment {
    /// The line number of the input (1-based).
    pub line: usize,
    /// The input record.
    pub input: String,
    /// Per peer, the masked messages in order.
    pub sent: BTreeMap<u8, Vec<String>>,
}

/// A transcript: its segments, in order. Comment and empty lines are not part of it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Transcript {
    /// The segments.
    pub segments: Vec<Segment>,
}

impl Transcript {
    /// Parses transcript `text`.
    ///
    /// # Errors
    ///
    /// A `<` record before any input, or with no peer number.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut segments: Vec<Segment> = Vec::new();
        for (i, line) in text.lines().enumerate() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let f: Vec<&str> = line.split(' ').collect();
            if f[0] == "<" {
                let peer = f
                    .get(1)
                    .and_then(|n| n.parse::<u8>().ok())
                    .ok_or_else(|| format!("line {}: a `<` record without a peer", i + 1))?;
                let seg = segments
                    .last_mut()
                    .ok_or_else(|| format!("line {}: a `<` record before any input", i + 1))?;
                seg.sent.entry(peer).or_default().push(masked_record(&f[2..]));
            } else {
                segments.push(Segment {
                    line: i + 1,
                    input: line.to_owned(),
                    sent: BTreeMap::new(),
                });
            }
        }
        Ok(Self {
            segments,
        })
    }
}

/// A difference between dashd's transcript and the node's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Difference {
    /// The scripts part at this segment (different inputs, or one transcript ends first);
    /// nothing after it is compared.
    Input {
        /// dashd's segment index.
        at: usize,
        /// dashd's input, if it has one there.
        dashd: Option<String>,
        /// The node's input, if it has one there.
        node: Option<String>,
    },
    /// After the same input, the node sent a peer something other than dashd did.
    Sent {
        /// The input.
        input: String,
        /// The segment index.
        at: usize,
        /// The peer.
        peer: u8,
        /// dashd's masked messages.
        dashd: Vec<String>,
        /// The node's masked messages.
        node: Vec<String>,
    },
}

impl fmt::Display for Difference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let none = || "(none)".to_owned();
        match self {
            Self::Input {
                at,
                dashd,
                node,
            } => write!(
                f,
                "segment {at}: the inputs differ\n  dashd: {}\n  node:  {}",
                dashd.clone().unwrap_or_else(none),
                node.clone().unwrap_or_else(none)
            ),
            Self::Sent {
                input,
                at,
                peer,
                dashd,
                node,
            } => {
                write!(f, "segment {at} `{input}`, peer {peer}")?;
                for m in dashd {
                    write!(f, "\n  dashd: {m}")?;
                }
                for m in node {
                    write!(f, "\n  node:  {m}")?;
                }
                Ok(())
            }
        }
    }
}

/// The differences between dashd's transcript and the node's, in order (module docs).
pub fn diff(dashd: &Transcript, node: &Transcript) -> Vec<Difference> {
    let mut out = Vec::new();
    let n = dashd.segments.len().max(node.segments.len());
    for at in 0..n {
        let (d, r) = (dashd.segments.get(at), node.segments.get(at));
        let (Some(d), Some(r)) = (d, r) else {
            out.push(Difference::Input {
                at,
                dashd: d.map(|s| s.input.clone()),
                node: r.map(|s| s.input.clone()),
            });
            return out;
        };
        if d.input != r.input {
            out.push(Difference::Input {
                at,
                dashd: Some(d.input.clone()),
                node: Some(r.input.clone()),
            });
            return out;
        }
        let peers: std::collections::BTreeSet<u8> =
            d.sent.keys().chain(r.sent.keys()).copied().collect();
        for peer in peers {
            let (ds, rs) = (d.sent.get(&peer), r.sent.get(&peer));
            let (ds, rs) = (ds.cloned().unwrap_or_default(), rs.cloned().unwrap_or_default());
            if ds != rs {
                out.push(Difference::Sent {
                    input: d.input.clone(),
                    at,
                    peer,
                    dashd: ds,
                    node: rs,
                });
            }
        }
    }
    out
}

/// The chain a fixture's node ran on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chain {
    /// `-regtest`.
    Regtest,
    /// `-testnet`.
    Testnet,
    /// Neither flag.
    Mainnet,
}

/// The dashd arguments a fixture was captured with (O13a D7): the profile flags the lab sets,
/// on top of the capture tool's base arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    /// The chain.
    pub chain: Chain,
    /// `-blocksonly`.
    pub blocks_only: bool,
    /// `-v2transport` (default on).
    pub v2_transport: bool,
    /// `-listen` (on: the base arguments bind a listener; off with `-listen=0`).
    pub listen: bool,
    /// `-whitelist` values, in order.
    pub whitelist: Vec<String>,
    /// `-connect=0`: no connection of the node's own.
    pub connect_none: bool,
}

/// The capture tool's base arguments (`p2a.Node`), with values that do not change what the node
/// sends: whatever follows `=` is not read.
const BASE_ARGS: &[&str] = &[
    "-server",
    "-listen=1",
    "-listenonion=0",
    "-discover=0",
    "-dnsseed=0",
    "-fixedseeds=0",
    "-upnp=0",
    "-natpmp=0",
    "-debug=net",
    "-logips",
    "-printtoconsole=0",
];

impl Profile {
    /// Reads a fixture's `# args:` line.
    ///
    /// # Errors
    ///
    /// No `# args:` line, or an argument the lab does not map (a fixture cannot be replayed
    /// under a configuration it was not captured with).
    pub fn from_fixture(text: &str) -> Result<Self, String> {
        let args = text
            .lines()
            .find_map(|l| l.strip_prefix("# args: "))
            .ok_or("the fixture has no `# args:` line")?;
        let mut p = Self {
            chain: Chain::Mainnet,
            blocks_only: false,
            v2_transport: true,
            listen: true,
            whitelist: Vec::new(),
            connect_none: false,
        };
        for a in args.split(' ') {
            match a {
                "-regtest" => p.chain = Chain::Regtest,
                "-testnet" => p.chain = Chain::Testnet,
                "-blocksonly" => p.blocks_only = true,
                "-v2transport=0" => p.v2_transport = false,
                "-listen=0" => p.listen = false,
                "-connect=0" => p.connect_none = true,
                a if BASE_ARGS.contains(&a) => {}
                a if a.starts_with("-port=") || a.starts_with("-bind=127.0.0.1:") => {}
                a if a.starts_with("-mocktime=") => {}
                a if let Some(w) = a.strip_prefix("-whitelist=") => p.whitelist.push(w.to_owned()),
                other => return Err(format!("argument {other} is not one the lab maps")),
            }
        }
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// dashd's `version` to an inbound peer, as captured in `fp-default-in-v1` (a release
    /// dashd v24.0.0-rc.3; `tests/fixtures/fingerprint-v24/PROVENANCE.md`).
    fn version() -> Vec<u8> {
        let fixture = include_str!("../tests/fixtures/fingerprint-v24/fp-default-in-v1.txt");
        let line =
            fixture.lines().find(|l| l.starts_with("< 1 version ")).expect("dashd's version");
        unhex(line.split(' ').nth(3).expect("a payload")).expect("hex")
    }

    /// The byte ranges [`mask`] hides in dashd's `version`: the nonce, the user agent (its
    /// length byte too) and the challenge.
    fn masked_ranges(p: &[u8]) -> Vec<std::ops::Range<usize>> {
        let ua = usize::from(p[80]);
        let tail = 81 + ua;
        vec![72..80, 80..tail, tail + 5..tail + 37]
    }

    #[test]
    fn the_fixture_version_is_whole() {
        let p = version();
        // 4+8+8+26+26+8, the user agent, start height, relay, challenge, masternode flag.
        assert_eq!(p.len(), 80 + 1 + usize::from(p[80]) + 4 + 1 + 32 + 1);
        assert_eq!(&p[81..81 + usize::from(p[80])], b"/Dash Core:24.0.0/");
    }

    /// Masking never hides a protocol difference: every byte of a real `version` outside the
    /// three masked ranges changes the masked text when flipped, and no byte inside them does.
    #[test]
    fn a_flip_outside_the_masks_always_shows() {
        let p = version();
        let base = mask("version", &p);
        let ranges = masked_ranges(&p);
        for i in 0..p.len() {
            let mut q = p.clone();
            q[i] ^= 0x01;
            let hidden = ranges.iter().any(|r| r.contains(&i));
            // The user agent's bytes are identity; its length byte moves the fields after it,
            // so a changed length must still show (it is outside the masks' meaning).
            let shows = mask("version", &q) != base;
            if i == 80 {
                assert!(shows, "a changed user agent length must move what follows");
            } else {
                assert_eq!(shows, !hidden, "byte {i}: hidden {hidden}, shows {shows}");
            }
        }
    }

    #[test]
    fn masks_hide_only_random_fields() {
        let p = version();
        let mut q = p.clone();
        q[72..80].copy_from_slice(&[9; 8]);
        let ua = 81 + usize::from(p[80]);
        q[ua + 5..ua + 37].fill(7);
        assert_eq!(mask("version", &p), mask("version", &q));
        // A different user agent of the same length is identity, not protocol.
        q[81] = b'/';
        q[82] = b'r';
        assert_eq!(mask("version", &p), mask("version", &q));
        // A longer one keeps every later field in place.
        let mut long = p[..80].to_vec();
        long.push(20);
        long.extend_from_slice(b"/rdashd:0.40.0-node/");
        long.extend_from_slice(&p[ua..]);
        assert_eq!(mask("version", &p), mask("version", &long));
        // The tail is still compared: a different masternode flag shows.
        let last = long.len() - 1;
        long[last] = 1;
        assert_ne!(mask("version", &p), mask("version", &long));
        assert_eq!(mask("ping", &[1, 2, 3, 4, 5, 6, 7, 8]), "0000000000000000");
        assert_ne!(mask("ping", &[0; 8]), mask("ping", &[0; 9]), "the length is compared");
        assert_eq!(mask("pong", &[1; 8]), "0101010101010101", "pong is not masked");
        assert_eq!(mask("verack", &[]), "");
    }

    #[test]
    fn cut_versions_are_never_masked_together_with_whole_ones() {
        let p = version();
        for cut in [0, 10, 75, 80, 81, 90, 100, p.len() - 1] {
            assert_ne!(mask("version", &p[..cut]), mask("version", &p), "cut at {cut}");
        }
        // Cut right after the user agent (an old peer's `version`): still masked.
        let end = 81 + usize::from(p[80]);
        assert_eq!(
            mask("version", &p[..end]),
            format!("{}{}{USER_AGENT}", hex(&p[..72]), "0".repeat(16))
        );
        // A user agent length past the end.
        let mut q = p[..82].to_vec();
        q[80] = 0xfd;
        assert_eq!(
            mask("version", &q),
            hex(&{
                let mut z = q.clone();
                z[72..80].fill(0);
                z
            })
        );
    }

    #[test]
    fn compact_sizes() {
        assert_eq!(compact_size(&[5], 0), Some((5, 1)));
        assert_eq!(compact_size(&[0xfd, 0x00, 0x01], 0), Some((256, 3)));
        assert_eq!(compact_size(&[0xfe, 1, 0, 0, 0], 0), Some((1, 5)));
        assert_eq!(compact_size(&[0xff, 2, 0, 0, 0, 0, 0, 0, 0], 0), Some((2, 9)));
        assert_eq!(compact_size(&[0xfd, 0x00], 0), None);
        assert_eq!(compact_size(&[], 0), None);
    }

    const DASHD: &str = "# c\nmocktime 1\nibd 1\naccept 1 v2\n> 1 version 00\n\
                         < 1 version 00 long\n< 1 verack  long\n> 1 verack \n\
                         < 1 ping 0102030405060708 short\n< 2 closed\n";

    #[test]
    fn equal_transcripts_have_no_differences() {
        let d = Transcript::parse(DASHD).unwrap();
        // Another ping nonce and other comments.
        let node = DASHD.replace("0102030405060708", "ffffffffffffffff").replace("# c", "# rdashd");
        assert_eq!(diff(&d, &Transcript::parse(&node).unwrap()), vec![]);
        assert_eq!(d.segments.len(), 5);
        assert_eq!(d.segments[3].sent[&1], ["version 00 long", "verack - long"]);
    }

    #[test]
    fn differences_are_reported_per_peer_and_segment() {
        let d = Transcript::parse(DASHD).unwrap();
        // Segments keep their 1-based line numbers, the comment counted.
        let lines: Vec<usize> = d.segments.iter().map(|s| s.line).collect();
        assert_eq!(lines, [2, 3, 4, 5, 8]);
        // A changed form, a dropped message, a closing the node did not do, an extra message.
        for (from, to) in [
            ("< 1 verack  long", "< 1 verack  short"),
            ("< 1 verack  long\n", ""),
            ("< 2 closed\n", ""),
            ("< 2 closed\n", "< 2 closed\n< 2 ping 00 short\n"),
            ("< 1 version 00 long\n< 1 verack  long", "< 1 verack  long\n< 1 version 00 long"),
        ] {
            let node = Transcript::parse(&DASHD.replace(from, to)).unwrap();
            let got = diff(&d, &node);
            assert_eq!(got.len(), 1, "{from:?} -> {to:?}: {got:?}");
            assert!(matches!(got[0], Difference::Sent { .. }), "{got:?}");
        }
        let node = Transcript::parse(&DASHD.replace("< 2 closed\n", "")).unwrap();
        assert_eq!(
            diff(&d, &node),
            vec![Difference::Sent {
                input: "> 1 verack ".to_owned(),
                at: 4,
                peer: 2,
                dashd: vec!["closed".to_owned()],
                node: vec![],
            }]
        );
    }

    #[test]
    fn different_scripts_stop_the_comparison() {
        let d = Transcript::parse(DASHD).unwrap();
        let node = Transcript::parse(&DASHD.replace("accept 1 v2", "accept 1")).unwrap();
        let got = diff(&d, &node);
        assert_eq!(
            got,
            vec![Difference::Input {
                at: 2,
                dashd: Some("accept 1 v2".to_owned()),
                node: Some("accept 1".to_owned()),
            }]
        );
        let short = Transcript::parse("mocktime 1\nibd 1\n").unwrap();
        assert_eq!(
            diff(&d, &short)[0],
            Difference::Input {
                at: 2,
                dashd: Some("accept 1 v2".to_owned()),
                node: None,
            }
        );
        assert_eq!(diff(&short, &d).len(), 1);
        assert!(Transcript::parse("< 1 ping 00\n").is_err());
        assert!(Transcript::parse("ibd 1\n< x ping 00\n").is_err());
    }

    #[test]
    fn profiles_come_from_the_args_line() {
        let base = "# args: -regtest -server -listen=1 -listenonion=0 -discover=0 -dnsseed=0 \
                    -fixedseeds=0 -upnp=0 -natpmp=0 -debug=net -logips -printtoconsole=0 \
                    -port=32000 -bind=127.0.0.1:32000 -mocktime=1700000000";
        let p = Profile::from_fixture(base).unwrap();
        assert_eq!(p.chain, Chain::Regtest);
        assert!(p.v2_transport && p.listen && !p.blocks_only && !p.connect_none);
        let p = Profile::from_fixture(&format!(
            "{base} -blocksonly -v2transport=0 -whitelist=127.0.0.1 -whitelist=noban@::1 -connect=0"
        ))
        .unwrap();
        assert!(p.blocks_only && !p.v2_transport && p.connect_none);
        assert_eq!(p.whitelist, ["127.0.0.1", "noban@::1"]);
        assert_eq!(
            Profile::from_fixture(&base.replace("-regtest", "-testnet")).unwrap().chain,
            Chain::Testnet
        );
        assert_eq!(
            Profile::from_fixture(&base.replace("-regtest ", "")).unwrap().chain,
            Chain::Mainnet
        );
        assert!(!Profile::from_fixture(&base.replace("-listen=1", "-listen=0")).unwrap().listen);
        assert!(Profile::from_fixture(&format!("{base} -maxconnections=8")).is_err());
        assert!(Profile::from_fixture("mocktime 1").is_err());
    }
}
