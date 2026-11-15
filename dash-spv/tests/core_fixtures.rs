//! The bincode fixtures of the masternode engine tests are Core wire captures, decoded.
//!
//! `qrinfo_*.dat` and `mnlistdiffs_2240504.dat` hold `dashcore` types serialized with bincode. They
//! used to come from an unverified tool run; they are now exactly `bincode(decode(core wire))`
//! (see `dash/tests/data/test_DML_diffs/PROVENANCE.md`), and this test keeps it so. Run
//! `cargo test -p dash-spv --test core_fixtures -- --ignored regenerate` to rewrite them from the
//! wire captures.
//!
//! A `qrinfo` reply carries `mnListDiffTip`, built against the *responding node's current tip*.
//! The fixtures describe a node at tip 2240504 and 2518986, so that one diff is the Core
//! `mnlistdiff` for that tip block, which is a function of the chain alone.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use dashcore::consensus::{deserialize, serialize};
use dashcore::network::message_qrinfo::QRInfo;
use dashcore::network::message_sml::MnListDiff;

const DATA: &str = "../dash/tests/data/test_DML_diffs";

fn path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(DATA).join(rel)
}

fn read(rel: &str) -> Vec<u8> {
    std::fs::read(path(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn diff(rel: &str) -> MnListDiff {
    deserialize(&read(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn encode<T: bincode::Encode>(value: &T) -> Vec<u8> {
    bincode::encode_to_vec(value, bincode::config::standard()).expect("encodes")
}

/// `mnlistdiffs_2240504.dat`: every `(base height, height)` diff the engine fixture applies.
fn mnlistdiffs_2240504() -> BTreeMap<(u32, u32), MnListDiff> {
    let mut map = BTreeMap::new();
    map.insert((0, 2227096), diff("mn_list_diff_0_2227096.bin"));
    let wire_dir = path("core_wire");
    for entry in std::fs::read_dir(&wire_dir).expect("core_wire dir") {
        let name = entry.unwrap().file_name().into_string().unwrap();
        let Some(heights) = name.strip_prefix("mnlistdiff_").and_then(|n| n.strip_suffix(".bin"))
        else {
            continue;
        };
        let Some((base, height)) = heights.split_once('_') else {
            continue;
        };
        let (Ok(base), Ok(height)) = (base.parse::<u32>(), height.parse::<u32>()) else {
            continue; // `mnlistdiff_tip_*.bin`
        };
        map.insert((base, height), diff(&format!("core_wire/{name}")));
    }
    map
}

/// `qrinfo_<tip>.dat`: Core's `qrinfo` with the tip diff of the fixture's own tip block.
fn qrinfo(tip: u32, tip_diff: &str) -> QRInfo {
    let mut info: QRInfo = deserialize(&read(&format!("core_wire/qrinfo_{tip}.bin"))).unwrap();
    info.mn_list_diff_tip = diff(tip_diff);
    info
}

fn fixtures() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("mnlistdiffs_2240504.dat", encode(&mnlistdiffs_2240504())),
        (
            "qrinfo_2240504.dat",
            encode(&qrinfo(2240504, "core_wire/mnlistdiff_2227096_2240504.bin")),
        ),
        ("qrinfo_2518986.dat", encode(&qrinfo(2518986, "core_wire/mnlistdiff_tip_2518986.bin"))),
    ]
}

#[test]
fn bincode_fixtures_are_the_decoded_core_wire() {
    for (name, expected) in fixtures() {
        assert!(
            read(name) == expected,
            "{name} is not bincode(decode(core wire)); regenerate it (see the module docs)"
        );
    }
}

#[test]
fn core_wire_captures_decode_and_reencode_exactly() {
    let mut checked = 0;
    for entry in std::fs::read_dir(path("core_wire")).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        let bytes = read(&format!("core_wire/{name}"));
        if name.starts_with("qrinfo_") {
            let info: QRInfo = deserialize(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(serialize(&info), bytes, "{name}");
        } else if name.starts_with("mnlistdiff_") {
            let d: MnListDiff = deserialize(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(serialize(&d), bytes, "{name}");
        } else {
            continue;
        }
        checked += 1;
    }
    assert_eq!(checked, 34);
}

#[test]
#[ignore = "rewrites the bincode fixtures from the Core wire captures"]
fn regenerate() {
    for (name, bytes) in fixtures() {
        std::fs::write(path(name), bytes).unwrap();
        eprintln!("wrote {name}");
    }
}
