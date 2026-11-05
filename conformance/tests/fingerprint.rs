//! O13a's fingerprint lab v0, the sim leg (docs/node-program/designs/O13a-notes.md): each dashd
//! v24.0.0-rc.3 transcript under `tests/fixtures/fingerprint-v24/`
//! (`contrib/node/fingerprint/capture.py`; provenance in the directory's `PROVENANCE.md`) is
//! played against rdashd's processor and network core in `dashd_p2p::sim`, configured with the
//! fixture's own dashd arguments; rdashd's transcript is recorded and diffed with dashd's by
//! [`dash_conformance::fingerprint::diff`]. Each fixture's differences must equal its entries in
//! `KNOWN-DIFFS.json` exactly, each with its BACKLOG row: a new difference fails, and so does a
//! listed one that went away.
//!
//! `FP_LAB_OUT=DIR` also writes each recorded rdashd transcript to `DIR/<fixture>.rdashd.txt`.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use dash_conformance::fingerprint::{Chain, Profile, Transcript, diff};
use dash_p2p::conn::Permissions;
use dash_p2p::permissions::parse_whitelist;
use dash_p2p::{ConnectionType, Why};
use dash_params::{ChainParamsBuilder, Network, Release};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity, local_services};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::sim::transcript::{NoHooks, assert_whole_v1_frames, record, replay};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim};
use dashd_p2p::{Registry, Seams};
use serde::Deserialize;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fingerprint-v24");

/// One remaining difference and the row that owns it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Known {
    fixture: String,
    /// The BACKLOG row, `BL-<n>`, or `pending:O13a-<n>` until the manager assigns one (the
    /// design note's ruling requests).
    bl: String,
    /// The difference as [`dash_conformance::fingerprint::Difference`] displays it, by line.
    diff: Vec<String>,
}

fn known() -> Vec<Known> {
    let text =
        std::fs::read_to_string(format!("{DIR}/KNOWN-DIFFS.json")).expect("KNOWN-DIFFS.json");
    serde_json::from_str(&text).expect("KNOWN-DIFFS.json")
}

/// rdashd's node as the profile configures dashd, where `sim` departs from dashd's defaults too
/// (`-peertimeout` is 60 s, as in the P2a and P2b replays).
fn sim(p: &Profile) -> Sim {
    let net = match p.chain {
        Chain::Regtest => Network::Regtest,
        Chain::Testnet => Network::Testnet3,
        Chain::Mainnet => Network::Main,
    };
    let params = Arc::new(ChainParamsBuilder::new(net, Release::V24_0).build().expect("params"));
    let opts = IdentityOptions {
        v2_transport: p.v2_transport,
        ..IdentityOptions::new()
    };
    let id = local_identity(&params, &opts).expect("identity");
    let processor = ProcessorOptions {
        blocks_only: p.blocks_only,
        ..ProcessorOptions::default()
    };
    Sim::with(params, id, processor, Registry::new(), Seams::default(), |cfg| {
        cfg.peer_timeout = 60;
        cfg.local_services = local_services(&opts);
        cfg.listen = p.listen;
        if p.connect_none {
            cfg.connect = Some(Vec::new());
        }
        cfg.whitelist.extend(p.whitelist.iter().map(|w| parse_whitelist(w, false).expect(w)));
    })
}

fn lab(name: &str) {
    let text = std::fs::read_to_string(format!("{DIR}/{name}.txt")).expect("the fixture");
    let profile = Profile::from_fixture(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
    let node = record(name, &text, sim(&profile), &mut NoHooks);
    if let Some(out) = std::env::var_os("FP_LAB_OUT") {
        std::fs::write(Path::new(&out).join(format!("{name}.rdashd.txt")), &node)
            .expect("FP_LAB_OUT");
    }
    let dashd = Transcript::parse(&text).expect("dashd's transcript");
    let node = Transcript::parse(&node).expect("rdashd's transcript");
    let got: Vec<String> = diff(&dashd, &node).iter().map(ToString::to_string).collect();
    let want: Vec<String> =
        known().into_iter().filter(|k| k.fixture == name).map(|k| k.diff.join("\n")).collect();
    assert!(
        got == want,
        "{name}: the differences are not KNOWN-DIFFS.json's\n--- found:\n{}\n--- listed:\n{}",
        got.join("\n"),
        want.join("\n")
    );
}

/// The V2 fixtures also pass as P2a-style replays (each segment asserted, with `masked_hex`): the
/// replay reads the format's V2 records, type forms included, as the recording does.
#[test]
fn v2_fixtures_replay() {
    for name in ["fp-default-out-v2", "fp-default-in-v2", "fp-testnet-out-v2", "fp-whitelist-in-v2"]
    {
        let text = std::fs::read_to_string(format!("{DIR}/{name}.txt")).expect("the fixture");
        let profile = Profile::from_fixture(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
        replay(name, &text, sim(&profile), &mut NoHooks);
    }
}

/// A V2 attach returns once both ends finished the handshake, so the first record's segment
/// starts from a V2 node.
#[test]
fn a_v2_accept_returns_with_the_node_on_v2() {
    let text = std::fs::read_to_string(format!("{DIR}/fp-default-in-v2.txt")).expect("the fixture");
    let sim = sim(&Profile::from_fixture(&text).expect("the profile"));
    let peer = sim.accept_v2(1, Permissions::NONE).expect("a listening node");
    assert_eq!(peer.handle.transport_info().kind.as_str(), "v2");
}

/// The capture tool's V1 reader refuses what Core's `V1Transport` refuses (wrong or another
/// network's magic, a bad checksum, a malformed command, an oversized length) and an incomplete
/// frame left at the end of a scenario: `capture.py selftest`'s negative controls.
#[test]
fn the_capture_tools_v1_reader_refuses_bad_frames() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../contrib/node/p2a-transcripts/capture.py");
    // CI's Windows runners name the interpreter `python`.
    let python = if cfg!(windows) {
        "python"
    } else {
        "python3"
    };
    let out = std::process::Command::new(python)
        .args(["-I", script, "selftest"])
        .output()
        .expect("python");
    assert!(
        out.status.success(),
        "capture.py selftest: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `record`'s end check fails a recording whose node left a V1 frame cut short: the node's socket
/// closes after 10 bytes of its answer to the peer's `version`, which a Core peer never reads as a
/// message.
#[test]
#[should_panic(expected = "cut: peer 1: an incomplete V1 frame was left at the end")]
fn a_recording_that_ends_inside_a_v1_frame_fails() {
    let text =
        std::fs::read_to_string(format!("{DIR}/fp-default-out-v1.txt")).expect("the fixture");
    let mut sim = sim(&Profile::from_fixture(&text).expect("the profile"));
    let p = sim.connect(1, ConnectionType::OutboundFullRelay, Permissions::NONE);
    assert_eq!(sim.sent_messages(&p, 1)[0].cmd.as_str(), "version");
    p.read(0);
    sim.deliver(&p, &[("version", PeerVersionMsg::v24(SIM_START).payload())]);
    sim.pass();
    p.read(10);
    sim.core.disconnect(p.handle.id(), Why::Processor("O13a cut frame"));
    sim.remove_disconnected();
    assert!(sim.sent_messages(&p, 0).is_empty());
    assert_eq!(p.partial_bytes(), 10);
    assert_whole_v1_frames("cut", [(1, &p)]);
}

macro_rules! lab {
    ($($test:ident => $file:literal,)*) => {
        $(
            #[test]
            fn $test() {
                lab($file);
            }
        )*

        /// Every fixture in the directory has its test, and every listed difference names a
        /// fixture and a BACKLOG row.
        #[test]
        fn every_fixture_is_judged() {
            let tested: BTreeSet<&str> = [$($file),*].into_iter().collect();
            let on_disk: BTreeSet<String> = std::fs::read_dir(DIR)
                .expect("the fixtures")
                .map(|e| e.expect("an entry").file_name().into_string().expect("a name"))
                .filter_map(|n| n.strip_suffix(".txt").map(str::to_owned))
                .collect();
            assert_eq!(on_disk.iter().map(String::as_str).collect::<BTreeSet<_>>(), tested);
            for k in known() {
                assert!(tested.contains(k.fixture.as_str()), "KNOWN-DIFFS.json: {}", k.fixture);
                let n = k.bl.strip_prefix("BL-").or_else(|| k.bl.strip_prefix("pending:O13a-"));
                let n = n.and_then(|n| n.parse::<u32>().ok());
                assert!(n.is_some(), "KNOWN-DIFFS.json: {} has no BACKLOG row", k.fixture);
                assert!(!k.diff.is_empty(), "KNOWN-DIFFS.json: {}: an empty difference", k.fixture);
            }
        }
    };
}

lab! {
    default_out_v1 => "fp-default-out-v1",
    default_out_v2 => "fp-default-out-v2",
    default_in_v1 => "fp-default-in-v1",
    default_in_v2 => "fp-default-in-v2",
    default_framing => "fp-default-framing",
    default_rekey => "fp-default-rekey",
    blocksonly_out_v1 => "fp-blocksonly-out-v1",
    blocksonly_in_v1 => "fp-blocksonly-in-v1",
    listen0_out_v1 => "fp-listen0-out-v1",
    listen0_in_v1 => "fp-listen0-in-v1",
    v2off_out_v1 => "fp-v2off-out-v1",
    v2off_in_v1 => "fp-v2off-in-v1",
    v2off_in_v2 => "fp-v2off-in-v2",
    whitelist_in_v1 => "fp-whitelist-in-v1",
    whitelist_in_v2 => "fp-whitelist-in-v2",
    testnet_out_v1 => "fp-testnet-out-v1",
    testnet_out_v2 => "fp-testnet-out-v2",
    testnet_in_v1 => "fp-testnet-in-v1",
    testnet_in_v2 => "fp-testnet-in-v2",
    mainnet_out_v1 => "fp-mainnet-out-v1",
    mainnet_out_v2 => "fp-mainnet-out-v2",
    mainnet_in_v1 => "fp-mainnet-in-v1",
    mainnet_in_v2 => "fp-mainnet-in-v2",
}
