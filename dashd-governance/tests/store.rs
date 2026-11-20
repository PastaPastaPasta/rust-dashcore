//! GV1b S2: rdashd's `governance.dat` ([`store`]) and the import of Dash Core's ([`from_core`]),
//! against the files and RPC answers Core v24 recorded.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::PathBuf;

use dash_consensus::hash::sha256d;
use dash_coreimport::netstate::governance::{
    self as core_gov, GovernanceDat, LastObjectRec, RateCheckBuffer as CoreBuffer, StoreVersion,
};
use dash_governance::manager::{Manager, StoreState};
use dash_governance::object;
use dash_governance::rate::{LastObjectRecord, RateCheckBuffer};
use dash_governance::tally::{ObjectState, VoteInstance};
use dash_network::Network;
use dashd_governance::import::rate_record;
use dashd_governance::store::{self, GovernanceStore, MAX_STORE_SIZE};
use dashd_governance::{ImportError, from_core};
use dashd_masternode::flatdb::{LoadError, ReadError};
use serde_json::Value;

const REGTEST: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];
const TESTNET: [u8; 4] = [0xce, 0xe2, 0xca, 0xff];
const RC3: &str = "v24.0.0-rc.3";
const RC4: &str = "v24.0.0-rc.4";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn read(rel: &str) -> Vec<u8> {
    let path = root().join(rel);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn flatfile(release: &str, dir: &str, file: &str) -> Vec<u8> {
    read(&format!("dash-coreimport/tests/data/flatfiles/{release}/{dir}/{file}"))
}

fn core_dat(bytes: &[u8]) -> GovernanceDat {
    let loaded = core_gov::parse(bytes, Network::Regtest).expect("Core's file");
    assert!(loaded.discarded.is_none(), "{:?}", loaded.discarded);
    loaded.value
}

fn r9_dat() -> GovernanceDat {
    core_dat(&read("dashd-governance/tests/fixtures/dashd-v24.0.0/R9.governance.dat"))
}

/// A hash as RPC prints it, in internal byte order.
fn hash(display: &str) -> [u8; 32] {
    assert_eq!(display.len(), 64, "{display}");
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        let at = 62 - 2 * i;
        *b = u8::from_str_radix(&display[at..at + 2], 16).unwrap();
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `data ‖ SHA256d(data)`.
fn seal(mut data: Vec<u8>) -> Vec<u8> {
    let sum = sha256d(&data);
    data.extend_from_slice(&sum);
    data
}

fn push_str(out: &mut Vec<u8>, s: &[u8]) {
    assert!(s.len() < 253);
    out.push(s.len() as u8);
    out.extend_from_slice(s);
}

/// An own-store file around `body` (which starts with the version string).
fn own_file(body: &[u8]) -> Vec<u8> {
    let mut file = Vec::new();
    push_str(&mut file, store::MAGIC.as_bytes());
    file.extend_from_slice(&REGTEST);
    file.extend_from_slice(body);
    seal(file)
}

fn body_of(file: &[u8]) -> &[u8] {
    &file[1 + store::MAGIC.len() + 4..file.len() - 32]
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gv1b-s2-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The persisted fields of two states, compared one by one (`MnList` has no `PartialEq`).
fn assert_same(a: &StoreState, b: &StoreState) {
    assert_eq!(a.erased, b.erased);
    assert_eq!(a.orphans, b.orphans);
    assert_eq!(a.objects.keys().collect::<Vec<_>>(), b.objects.keys().collect::<Vec<_>>());
    for ((oa, sa), (ob, sb)) in a.objects.values().zip(b.objects.values()) {
        assert_eq!(oa, ob);
        assert_persisted(sa, sb);
    }
    assert_eq!(a.last_object, b.last_object);
    match (&a.last_mn_list, &b.last_mn_list) {
        (Some(x), Some(y)) => assert!(x.is_equal(y)),
        (None, None) => {}
        _ => panic!("last_mn_list differs"),
    }
}

fn assert_persisted(a: &ObjectState, b: &ObjectState) {
    assert_eq!((a.obj_type, a.hash), (b.obj_type, b.hash));
    assert_eq!((a.expired, a.deletion_time), (b.expired, b.deletion_time));
    assert_eq!(a.current, b.current);
    assert_eq!(a.file.votes().collect::<Vec<_>>(), b.file.votes().collect::<Vec<_>>());
}

/// R9's state plus what R9 lacks: erased entries, orphan votes, a well-used rate record and a
/// masternode list (R9's node stored Core's default one, which imports as `None`).
fn populated() -> StoreState {
    let mut s = from_core(r9_dat()).expect("R9 imports");
    assert!(!s.objects.is_empty() && !s.last_object.is_empty() && s.last_mn_list.is_none());
    s.last_mn_list = Some(core_dat(&flatfile(RC4, "populated", "governance.dat")).mn_list);
    s.erased.insert([0xe1; 32], 1_700_000_100);
    s.erased.insert([0xe2; 32], i64::MAX);
    let votes: Vec<_> =
        s.objects.values().flat_map(|(_, st)| st.file.votes().cloned()).take(2).collect();
    assert_eq!(votes.len(), 2);
    s.orphans = vec![
        ([0xa1; 32], (votes[0].clone(), 1_700_000_600)),
        ([0xa2; 32], (votes[1].clone(), 1_700_000_500)),
    ];
    let mut buffer = RateCheckBuffer::new();
    for t in 0..7 {
        buffer.add(1_700_000_000 + t * 1000);
    }
    s.last_object.insert(
        ([0x0b; 32], 3),
        LastObjectRecord {
            buffer,
            status_ok: false,
        },
    );
    s
}

#[test]
fn own_store_round_trips_a_populated_manager() {
    let mut original = Manager::new();
    original.import_state(populated());
    let exported = original.export_state();
    assert_eq!(exported.orphans.len(), 2);
    assert_eq!(exported.erased.len(), populated().erased.len());

    let bytes = store::encode(exported.clone(), REGTEST);
    let loaded = store::decode(&bytes, REGTEST).expect("own file loads");
    assert!(loaded.repaired.is_empty());
    let mut restored = Manager::new();
    restored.import_state(loaded.state);
    assert_same(&restored.export_state(), &exported);

    // The same through the file: flatdb's atomic store, then load.
    let dir = scratch("round-trip");
    let path = dir.join(store::FILE);
    assert!(store::store(exported.clone(), &path, REGTEST).unwrap());
    assert!(!dir.join("governance.dat.new").exists());
    let mut from_file = Manager::new();
    from_file.import_state(store::load(&path, REGTEST).unwrap().state);
    assert_same(&from_file.export_state(), &exported);
    // And again over the file just written.
    assert!(store::store(from_file.export_state(), &path, REGTEST).unwrap());
    assert_same(&store::load(&path, REGTEST).unwrap().state, &exported);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn the_body_is_cores_v17_layout_read_by_the_shared_decoder() {
    let state = populated();
    let file = store::encode(state.clone(), REGTEST);
    let body = body_of(&file);
    let mut prefix = Vec::new();
    push_str(&mut prefix, store::VERSION.as_bytes());
    assert!(body.starts_with(&prefix));
    let mut tailed = body[prefix.len()..].to_vec();
    tailed.extend_from_slice(b"TAIL");
    let mut rest = &tailed[..];
    let dat = core_gov::decode_store_body_from(&mut rest, StoreVersion::V17).unwrap();
    assert_eq!(rest, b"TAIL");
    // A failed decode leaves the caller's cursor where it was.
    let cut = &tailed[..tailed.len() / 2];
    let mut at = cut;
    assert!(core_gov::decode_store_body_from(&mut at, StoreVersion::V17).is_err());
    assert_eq!(at.len(), cut.len());
    assert_eq!(dat.orphan_votes.max_size, core_gov::MAX_CACHE_SIZE);
    assert_eq!(dat.orphan_votes.items.len(), state.orphans.len());
    assert_same(&from_core(dat).unwrap(), &state);
}

#[test]
fn a_bad_body_is_recreated_never_loaded_as_an_empty_store() {
    let body = body_of(&store::encode(populated(), REGTEST)).to_vec();
    let version_len = 1 + store::VERSION.len();
    // Cut anywhere after the version string: every prefix is a format error, so Core's loader
    // recreates the store, with its log line, rather than returning an empty one as loaded.
    for cut in (version_len..body.len()).step_by(7).chain([body.len() - 1]) {
        let err = store::decode(&own_file(&body[..cut]), REGTEST).unwrap_err();
        assert!(matches!(err, LoadError::Format(_)), "cut {cut}: {err:?}");
        assert!(err.recreates());
    }
    // Five objects declared and none written, another version string (rdashd's next, Core's),
    // and no version string at all.
    let mut bad_count = body[..version_len].to_vec();
    bad_count.extend([0, 0x40, 0x42, 0x0f, 0x00, 0, 5]);
    let mut other_version = Vec::new();
    push_str(&mut other_version, b"GovernanceStore-2");
    other_version.extend_from_slice(&body[version_len..]);
    let mut core_version = Vec::new();
    push_str(&mut core_version, b"CGovernanceManager-Version-17");
    core_version.extend_from_slice(&body[version_len..]);
    for bad in [bad_count, other_version, core_version, Vec::new()] {
        let err = store::decode(&own_file(&bad), REGTEST).unwrap_err();
        assert!(matches!(err, LoadError::Format(_)), "{err:?}");
    }
    // At the path, a recreating error is the empty store and is not an error.
    let dir = scratch("bad-body");
    let path = dir.join(store::FILE);
    std::fs::write(&path, own_file(&body[..body.len() / 2])).unwrap();
    let loaded = store::load(&path, REGTEST).unwrap();
    assert!(loaded.state.objects.is_empty() && loaded.state.erased.is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn damaged_and_foreign_files_follow_recreates() {
    let good = store::encode(populated(), REGTEST);
    let mut corrupt = good.clone();
    let mid = corrupt.len() / 2;
    corrupt[mid] ^= 1;
    let other_network = store::encode(populated(), TESTNET);
    let short = good[..31].to_vec();
    let mut wrong_magic = Vec::new();
    push_str(&mut wrong_magic, b"rdashd-mncache");
    wrong_magic.extend_from_slice(&REGTEST);
    wrong_magic.extend_from_slice(body_of(&good));
    let wrong_magic = seal(wrong_magic);
    let core_magic = {
        let mut f = Vec::new();
        push_str(&mut f, b"magicGovernanceCache");
        f.extend_from_slice(&REGTEST);
        f.extend_from_slice(body_of(&good));
        seal(f)
    };
    let dir = scratch("damaged");
    let path = dir.join(store::FILE);
    for (name, bytes, expect) in [
        ("corrupt", corrupt, "Corrupt"),
        ("other network", other_network, "Network"),
        ("short", short, "Short"),
        ("wrong magic", wrong_magic, "Magic"),
        ("Core's magic", core_magic, "Magic"),
    ] {
        let err = store::decode(&bytes, REGTEST).unwrap_err();
        assert_eq!(format!("{err:?}"), expect, "{name}");
        assert!(!err.recreates(), "{name}");
        // At the path these refuse to start, as Core's `Load` does; the file stays.
        std::fs::write(&path, &bytes).unwrap();
        let err = store::load(&path, REGTEST).unwrap_err();
        assert_eq!(format!("{err:?}"), expect, "{name} at the path");
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "{name}");
        // And the store is not written over it.
        assert!(!store::store(StoreState::default(), &path, REGTEST).unwrap(), "{name}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "{name}");
    }
    // A missing file is recreated.
    std::fs::remove_file(&path).unwrap();
    assert!(store::load(&path, REGTEST).unwrap().state.objects.is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_file_over_the_cap_is_recreated_without_being_read() {
    assert_eq!(MAX_STORE_SIZE, 128 << 20);
    let dir = scratch("oversize");
    let path = dir.join(store::FILE);
    // Sparse: no data block is written. Read, it would be 128 MiB of zeros that fail the checksum
    // (`Corrupt`, which stops the node); refused on its size it is recreated.
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(MAX_STORE_SIZE as u64 + 1).unwrap();
    drop(file);
    let loaded = store::load(&path, REGTEST).expect("recreated, not refused");
    assert!(loaded.state.objects.is_empty());
    // At the cap the file is read (and fails its checksum).
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(MAX_STORE_SIZE as u64).unwrap();
    drop(file);
    assert!(matches!(store::load(&path, REGTEST), Err(LoadError::Corrupt)));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_file_over_the_cap_is_replaced_by_the_flush() {
    let dir = scratch("oversize-flush");
    let path = dir.join(store::FILE);
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(MAX_STORE_SIZE as u64 + 1).unwrap();
    drop(file);
    // Load recreates it, and the flush makes the same decision: it writes a valid store, which
    // loads back.
    assert!(store::load(&path, REGTEST).unwrap().state.objects.is_empty());
    let state = populated();
    assert!(store::store(state.clone(), &path, REGTEST).unwrap());
    assert!(std::fs::metadata(&path).unwrap().len() < MAX_STORE_SIZE as u64);
    assert_same(&store::load(&path, REGTEST).unwrap().state, &state);
    // A damaged file under the cap still refuses, and the flush leaves it.
    let mut damaged = store::encode(state.clone(), REGTEST);
    damaged[40] ^= 1;
    std::fs::write(&path, &damaged).unwrap();
    assert!(matches!(store::load(&path, REGTEST), Err(LoadError::Corrupt)));
    assert!(!store::store(state, &path, REGTEST).unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), damaged);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn bytes_over_the_cap_are_refused_before_any_check() {
    // All zeros: checked, they would fail the checksum (`Corrupt`, which stops the node).
    let err = store::decode(&vec![0; MAX_STORE_SIZE + 1], REGTEST).unwrap_err();
    assert!(
        matches!(&err, LoadError::Format(ReadError::Other(text)) if text.contains("is over")),
        "{err:?}"
    );
    assert!(err.recreates());
}

#[test]
fn declared_counts_past_the_bytes_left_or_max_size_are_format_errors() {
    let mut prefix = Vec::new();
    push_str(&mut prefix, store::VERSION.as_bytes());
    let big = |n: u32| {
        let mut v = vec![0xfe];
        v.extend_from_slice(&n.to_le_bytes());
        v
    };
    let cap = MAX_STORE_SIZE as u32;
    // Each count is refused on truncation or on Core's `MAX_SIZE`, not on the file cap: an erased
    // map of 2^25 entries (`MAX_SIZE`), then nothing; a count over `MAX_SIZE`; an object whose
    // data vector declares cap + 1 bytes; a vote file of 2^25 votes, then nothing.
    let mut erased = prefix.clone();
    erased.extend(big(0x0200_0000));
    let mut over_max = prefix.clone();
    over_max.extend(big(0x0200_0001));
    let mut data = prefix.clone();
    data.extend([0]); // erased
    data.extend_from_slice(&core_gov::MAX_CACHE_SIZE.to_le_bytes());
    data.extend([0, 1]); // no orphans, one object
    data.extend([7; 32]); // key
    data.extend([0; 32 + 4 + 8 + 32]); // parent, revision, time, collateral
    data.extend(big(cap + 1));
    let mut vote_file = prefix;
    vote_file.extend([0]);
    vote_file.extend_from_slice(&core_gov::MAX_CACHE_SIZE.to_le_bytes());
    vote_file.extend([0, 1]);
    vote_file.extend([7; 32]);
    vote_file.extend([0; 32 + 4 + 8 + 32]);
    vote_file.extend([0, 1, 0, 0, 0]); // data, type
    vote_file.extend([0; 36 + 1 + 8 + 1 + 1 + 4]); // outpoint, sig, deletion, expired, no votes, memory
    vote_file.extend(big(0x0200_0000));
    for (name, body) in
        [("erased", erased), ("over MAX_SIZE", over_max), ("data", data), ("votes", vote_file)]
    {
        let err = store::decode(&own_file(&body), REGTEST).unwrap_err();
        assert!(matches!(err, LoadError::Format(_)), "{name}: {err:?}");
    }
}

/// Core's `gobject list`/`get` fields and `getcurrentvotes` entries for one state.
fn assert_matches_rpc(state: &StoreState, objects: &serde_json::Map<String, Value>, votes: &Value) {
    let keys: BTreeSet<[u8; 32]> = objects.keys().map(|k| hash(k)).collect();
    assert_eq!(state.objects.keys().copied().collect::<BTreeSet<_>>(), keys);
    for (display, listed) in objects {
        let (obj, st) = &state.objects[&hash(display)];
        assert_eq!(st.hash, hash(display));
        assert_eq!(object::hash(obj), hash(display));
        assert_eq!(i64::from(st.obj_type), listed["ObjectType"].as_i64().unwrap());
        assert_eq!(obj.time(), listed["CreationTime"].as_i64().unwrap());
        assert_eq!(
            object::txid_bytes(&obj.collateral_hash()),
            hash(listed["CollateralHash"].as_str().unwrap())
        );
        assert_eq!(hex(obj.data()), listed["DataHex"].as_str().unwrap());
        // `GetCurrentVotes`: one entry per instance, `outpoint-n:creation time:outcome:signal:weight`.
        let mut expected = BTreeSet::new();
        for text in votes[display].as_object().map(|m| m.values().collect()).unwrap_or(vec![]) {
            let parts: Vec<&str> = text.as_str().unwrap().split(':').collect();
            let (txid, n) = parts[0].rsplit_once('-').unwrap();
            let outcome = ["none", "yes", "no", "abstain"].iter().position(|o| *o == parts[2]);
            let signal = ["none", "funding", "valid", "delete", "endorsed"]
                .iter()
                .position(|s| *s == parts[3]);
            expected.insert((
                (hash(txid), n.parse::<u32>().unwrap()),
                signal.unwrap() as i32,
                outcome.unwrap() as i32,
                parts[1].parse::<i64>().unwrap(),
            ));
        }
        let current: BTreeSet<_> = st
            .current
            .iter()
            .flat_map(|(op, rec)| {
                rec.iter().map(|(signal, i)| (*op, *signal, i.outcome, i.creation_time))
            })
            .collect();
        assert_eq!(current, expected, "{display}");
    }
}

#[test]
fn from_core_matches_what_core_answered() {
    for release in [RC3, RC4] {
        let exp: Value =
            serde_json::from_slice(&flatfile(release, "populated", "expected.json")).unwrap();
        let dat = core_dat(&flatfile(release, "populated", "governance.dat"));
        let records = dat.last_masternode_object.len();
        let state = from_core(dat).unwrap();
        assert_matches_rpc(
            &state,
            exp["gobject_list_all"].as_object().unwrap(),
            &exp["gobject_getcurrentvotes"],
        );
        let count = &exp["gobject_count"];
        assert_eq!(state.erased.len() as u64, count["erased"].as_u64().unwrap(), "{release}");
        let votes: usize = state.objects.values().map(|(_, s)| s.file.len()).sum();
        assert_eq!(votes as u64, count["votes"].as_u64().unwrap(), "{release}");
        assert_eq!(state.last_object.len(), records, "{release}");
        let list = state.last_mn_list.as_ref().expect("the node's list");
        assert_eq!(list.len(), exp["protx_list_valid"].as_array().unwrap().len(), "{release}");

        let empty = from_core(core_dat(&flatfile(release, "empty", "governance.dat"))).unwrap();
        assert!(empty.objects.is_empty() && empty.erased.is_empty() && empty.orphans.is_empty());
    }

    let gz = read("dash-oracle-sys/tests/data/gov-regtest-v24.0.json.gz");
    let mut text = String::new();
    flate2::read::GzDecoder::new(&gz[..]).read_to_string(&mut text).unwrap();
    let exp: Value = serde_json::from_str(&text).unwrap();
    let state =
        from_core(core_dat(&read("dash-oracle-sys/tests/data/gov-regtest-v24.0.governance.dat")))
            .unwrap();
    // The file was written at the stop, after the probe votes.
    assert_matches_rpc(
        &state,
        exp["gobject_get_after_probes"].as_object().unwrap(),
        &exp["gobject_getcurrentvotes_after_probes"],
    );
    assert_eq!(
        state.last_mn_list.as_ref().unwrap().len(),
        exp["masternodes"].as_array().unwrap().len()
    );
}

#[test]
fn r9_imports_as_the_restarted_node_reports_it() {
    let text =
        String::from_utf8(read("dashd-governance/tests/fixtures/dashd-v24.0.0/R9.txt")).unwrap();
    let restart = text.lines().position(|l| l == "restart").expect("R9 restarts");
    let checkpoint = text
        .lines()
        .skip(restart)
        .find_map(|l| l.strip_prefix("check gobject "))
        .expect("a checkpoint after the restart");
    let cp: Value = serde_json::from_str(checkpoint).unwrap();
    assert_eq!(cp["label"], "restart-start");
    let state = from_core(r9_dat()).unwrap();
    assert_eq!(state.objects.len(), 5);
    assert_matches_rpc(&state, cp["objects"].as_object().unwrap(), &cp["votes"]);
}

/// A vote instance as signal, outcome, last update and creation time.
type Instance = (i32, i32, i64, i64);

/// A `-16` file with one object whose current votes are `records` (outpoint byte, instances).
fn v16_file(records: &[(u8, &[Instance])]) -> Vec<u8> {
    let empty = flatfile(RC3, "empty", "governance.dat");
    let mut file = Vec::new();
    push_str(&mut file, b"magicGovernanceCache");
    file.extend_from_slice(&REGTEST);
    let header = file.len();
    push_str(&mut file, core_gov::SERIALIZATION_VERSION_STRING_V16.as_bytes());
    file.push(0); // erased
    file.extend([0; 5]); // the invalid-vote cache
    file.extend_from_slice(&core_gov::MAX_CACHE_SIZE.to_le_bytes());
    file.push(0); // orphans
    file.push(1);
    file.extend([7; 32]);
    file.extend([0; 32]); // parent
    file.extend(1i32.to_le_bytes());
    file.extend(1_700_000_000i64.to_le_bytes());
    file.extend([2; 32]); // collateral
    push_str(&mut file, br#"{"type":1}"#);
    file.extend(1i32.to_le_bytes());
    file.extend([3; 36]);
    push_str(&mut file, &[9; 70]);
    file.extend(1_700_000_500i64.to_le_bytes()); // deletion time
    file.push(1); // expired
    file.push(records.len() as u8);
    for (op, instances) in records {
        file.extend([*op; 32]);
        file.extend(0u32.to_le_bytes());
        file.push(instances.len() as u8);
        for (signal, outcome, last, created) in *instances {
            file.extend(signal.to_le_bytes());
            file.extend(outcome.to_le_bytes());
            file.extend(last.to_le_bytes());
            file.extend(created.to_le_bytes());
        }
    }
    file.extend(1i32.to_le_bytes());
    file.push(1);
    file.extend([5; 32]);
    file.extend(0u32.to_le_bytes());
    file.extend([7; 32]);
    file.extend(1i32.to_le_bytes());
    file.extend(2i32.to_le_bytes());
    file.extend(1_700_000_010i64.to_le_bytes());
    push_str(&mut file, &[8; 96]);
    file.push(0); // last objects
    // The empty capture's masternode list: after its version string and five empty sections.
    let empty_version_end = header + 1 + core_gov::SERIALIZATION_VERSION_STRING_V16.len();
    file.extend_from_slice(&empty[empty_version_end + 1 + 5 + 5 + 1 + 1..empty.len() - 32]);
    seal(file)
}

#[test]
fn v16_drops_none_instances_and_empty_records_as_the_declared_migration() {
    let none = |s| (s, 0, 0, 0);
    let records: &[(u8, &[Instance])] = &[
        (0xa1, &[none(1), (2, 1, 1_700_000_100, 1_700_000_090)]),
        (0xa2, &[none(1)]),
        (0xa3, &[(1, 0, 0, 7), (3, 0, 5, 0)]),
        (0xa4, &[]),
        (0xa5, &[(4, 2, 0, 0)]),
    ];
    let dat = core_dat(&v16_file(records));
    assert_eq!(dat.version, Some(StoreVersion::V16));
    let as_v17 = GovernanceDat {
        version: Some(StoreVersion::V17),
        ..dat.clone()
    };
    let state = from_core(dat).unwrap();
    let (obj, st) = &state.objects[&[7; 32]];
    let inst = |outcome, last_update, creation_time| VoteInstance {
        outcome,
        last_update,
        creation_time,
    };
    let key = |b: u8| ([b; 32], 0u32);
    let expected: BTreeMap<_, BTreeMap<_, _>> = [
        (key(0xa1), [(2, inst(1, 1_700_000_100, 1_700_000_090))].into()),
        (key(0xa3), [(1, inst(0, 0, 7)), (3, inst(0, 5, 0))].into()),
        (key(0xa5), [(4, inst(2, 0, 0))].into()),
    ]
    .into();
    assert_eq!(st.current, expected);
    // Every other field as the file has it.
    assert_eq!((st.obj_type, st.expired, st.deletion_time), (1, true, 1_700_000_500));
    assert_eq!(st.hash, object::hash(obj));
    assert_eq!(
        (obj.time(), obj.data().as_slice(), obj.sig().len()),
        (1_700_000_000, &br#"{"type":1}"#[..], 70)
    );
    assert_eq!(st.file.len(), 1);
    assert!(state.orphans.is_empty() && state.erased.is_empty() && state.last_object.is_empty());
    // A `-17` store maps field for field: the instances stay.
    let kept = from_core(as_v17).unwrap();
    let all: usize = kept.objects[&[7; 32]].1.current.values().map(BTreeMap::len).sum();
    assert_eq!(all, 6);
    assert_eq!(kept.objects[&[7; 32]].1.current.len(), 5);
}

#[test]
fn an_unknown_core_version_is_an_import_error() {
    let mut dat = r9_dat();
    dat.version = None;
    dat.version_matched = false;
    assert_eq!(from_core(dat).unwrap_err(), ImportError::UnknownVersion);
}

fn core_record(timestamps: &[i64], start: i32, end: i32, empty: bool, ok: bool) -> LastObjectRec {
    LastObjectRec {
        trigger_buffer: CoreBuffer {
            timestamps: timestamps.to_vec(),
            data_start: start,
            data_end: end,
            buffer_empty: empty,
        },
        status_ok: ok,
    }
}

#[test]
fn refused_rate_records_are_repaired_never_dropped() {
    // Core's own records load unchanged.
    for release in [RC3, RC4] {
        let dat = core_dat(&flatfile(release, "populated", "governance.dat"));
        assert!(!dat.last_masternode_object.is_empty(), "{release}");
        for rec in dat.last_masternode_object.values() {
            let (kept, repaired) = rate_record(rec);
            assert!(!repaired, "{release}");
            let b = &rec.trigger_buffer;
            assert_eq!(kept.buffer.timestamps().as_slice(), b.timestamps.as_slice());
            assert_eq!(
                (kept.buffer.start() as i32, kept.buffer.end() as i32),
                (b.data_start, b.data_end)
            );
            assert_eq!((kept.buffer.is_empty(), kept.status_ok), (b.buffer_empty, rec.status_ok));
        }
    }
    let ts = |b: &RateCheckBuffer| *b.timestamps();
    // Short and long vectors keep every slot Core can read; a position off the ring is the
    // constructor's buffer. `fStatusOK` is kept in every case.
    let short = rate_record(&core_record(&[10, 20, 30], 0, 3, false, false));
    assert!(short.1);
    assert_eq!(ts(&short.0.buffer), [10, 20, 30, 0, 0]);
    assert_eq!((short.0.buffer.start(), short.0.buffer.end(), short.0.status_ok), (0, 3, false));
    assert_eq!(short.0.buffer.count(), 3);
    let long = rate_record(&core_record(&[1, 2, 3, 4, 5, 6, 7], 2, 1, false, true));
    assert!(long.1);
    assert_eq!(ts(&long.0.buffer), [1, 2, 3, 4, 5]);
    assert_eq!((long.0.buffer.start(), long.0.buffer.end(), long.0.status_ok), (2, 1, true));
    for (start, end) in [(5, 0), (0, -1), (-3, 9)] {
        let off = rate_record(&core_record(&[1, 2, 3, 4, 5], start, end, false, false));
        assert!(off.1);
        assert_eq!(off.0.buffer, RateCheckBuffer::new(), "{start} {end}");
        assert!(!off.0.status_ok);
    }

    // In the own store: such a record loads, is reported, and does not recreate the store.
    let mut body = Vec::new();
    push_str(&mut body, store::VERSION.as_bytes());
    body.push(0);
    body.extend_from_slice(&core_gov::MAX_CACHE_SIZE.to_le_bytes());
    body.extend([0, 0, 1]);
    body.extend([0x0c; 32]);
    body.extend(7u32.to_le_bytes());
    body.push(3);
    for t in [100i64, 200, 300] {
        body.extend(t.to_le_bytes());
    }
    body.extend(0i32.to_le_bytes());
    body.extend(3i32.to_le_bytes());
    body.extend([0, 0]); // not empty, status false
    let mut list = Vec::new();
    dash_dml::codec::encode_canonical_list_unvalidated(
        &dash_dml::MnList::default_pre_dip3(),
        &mut list,
    );
    body.extend(list);
    let loaded: GovernanceStore = store::decode(&own_file(&body), REGTEST).expect("not recreated");
    assert_eq!(loaded.repaired, vec![([0x0c; 32], 7)]);
    let rec = &loaded.state.last_object[&([0x0c; 32], 7)];
    assert_eq!(ts(&rec.buffer), [100, 200, 300, 0, 0]);
    assert!(!rec.status_ok);
    // Through Core's importer too.
    let mut dat = r9_dat();
    dat.last_masternode_object.insert(
        core_gov::OutPoint {
            hash: [0x0d; 32],
            n: 1,
        },
        core_record(&[], 7, 7, false, false),
    );
    let records = dat.last_masternode_object.len();
    let state = from_core(dat).unwrap();
    assert_eq!(state.last_object.len(), records);
    assert_eq!(state.last_object[&([0x0d; 32], 1)].buffer, RateCheckBuffer::new());
    // Written back, a repaired record is a valid one.
    let reloaded = store::decode(&store::encode(state.clone(), REGTEST), REGTEST).unwrap();
    assert!(reloaded.repaired.is_empty());
    assert_eq!(reloaded.state.last_object, state.last_object);
}

#[test]
fn the_summary_and_errors_read_as_cores() {
    use dashd_masternode::flatdb::FlatObject as _;
    let mut state = populated();
    let types: Vec<i32> = state.objects.values().map(|(_, s)| s.obj_type).collect();
    let proposals = types.iter().filter(|t| **t == object::PROPOSAL).count();
    let triggers = types.iter().filter(|t| **t == object::TRIGGER).count();
    assert_eq!((proposals, triggers), (4, 1));
    let proposal = state.objects.values_mut().find(|(_, s)| s.obj_type == object::PROPOSAL);
    proposal.unwrap().1.obj_type = 7;
    let erased = state.erased.len();
    let store = GovernanceStore {
        state,
        repaired: Vec::new(),
    };
    assert_eq!(
        store.summary(),
        format!("Governance Objects: 5 (Proposals: 3, Triggers: 1, Other: 1; Erased: {erased})")
    );

    let mut hash = [0u8; 32];
    for (i, b) in hash.iter_mut().enumerate() {
        *b = i as u8;
    }
    let display = "1f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403020100";
    assert_eq!(
        ImportError::Object(hash).to_string(),
        format!("governance object {display} unreadable")
    );
    assert_eq!(ImportError::Vote(hash).to_string(), format!("a vote for {display} unreadable"));
    assert_eq!(ImportError::UnknownVersion.to_string(), "governance store of an unknown version");
}

#[test]
fn a_failed_read_clears_what_the_store_held() {
    let body = body_of(&store::encode(populated(), REGTEST)).to_vec();
    let mut held = store::decode(&store::encode(populated(), REGTEST), REGTEST).unwrap();
    held.repaired.push(([1; 32], 1));
    let err = dashd_masternode::flatdb::decode_into(
        &own_file(&body[..body.len() / 2]),
        REGTEST,
        &mut held,
    )
    .unwrap_err();
    assert!(matches!(err, LoadError::Format(_)));
    assert!(held.state.objects.is_empty() && held.state.erased.is_empty());
    assert!(held.state.orphans.is_empty() && held.state.last_object.is_empty());
    assert!(held.state.last_mn_list.is_none() && held.repaired.is_empty());
}

#[test]
fn compact_size_failures_are_cores_stream_errors() {
    let err = |body: &[u8]| match store::decode(&own_file(body), REGTEST).unwrap_err() {
        LoadError::Format(e) => e,
        e => panic!("{e:?}"),
    };
    // The version string's length: non-canonical, over `MAX_SIZE`, past the end.
    assert_eq!(err(&[0xfd, 17, 0]), ReadError::NonCanonical);
    assert_eq!(err(&[0xfe, 1, 0, 0, 2]), ReadError::SizeTooLarge);
    assert_eq!(err(&[0xfd]), ReadError::EndOfData);
    assert_eq!(err(&[20, b'G']), ReadError::EndOfData);
    // The boundaries are accepted, so only the missing bytes fail.
    assert_eq!(err(&[0xfd, 0xfd, 0]), ReadError::EndOfData);
    assert_eq!(err(&[0xfe, 0, 0, 0, 2]), ReadError::EndOfData);
    // The same inside the body (the erased map's count).
    let mut body = Vec::new();
    push_str(&mut body, store::VERSION.as_bytes());
    let at = body.len();
    body.extend([0xfd, 3, 0]);
    assert_eq!(err(&body), ReadError::NonCanonical);
    body.truncate(at);
    body.extend([0xfe, 1, 0, 0, 2]);
    assert_eq!(err(&body), ReadError::SizeTooLarge);
}
