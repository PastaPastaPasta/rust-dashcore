use std::cell::Cell;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::{Enrichment, PeerRpc};
use crate::digest::extract::NodeRpc;
use anyhow::{Result, anyhow};
use dash_statedigest::compare::{Projector, ProjectorSnapshot};
use dash_statedigest::{
    Class as FieldClass, FieldSource, Hash256, Ident, Kind, Origin, Outcome, Record, Value, group,
};
use tempfile::TempDir;

use super::alarm::{Class, Crit, Warn};
use super::settle::{ChainLock, Settle};
use super::{Config, NodeStatus, PeerStatus, RecordedPeer, Shadow};
use crate::digest::store::Records;

const NOW: u64 = 10_000;

fn vector(node: usize, file: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../test-vectors/statedigest/regtest-v24/feature_llmq_chainlocks/node{node}/{file}.jsonl.zst"))
}

fn records(path: &Path) -> Vec<Record> {
    Records::open(path).unwrap().map(|r| r.unwrap().1).collect()
}

fn header() -> Record {
    records(&vector(0, "digest")).into_iter().find(|r| matches!(r, Record::Header(_))).unwrap()
}

fn hash(h: i64) -> Hash256 {
    let mut bytes = [0; 32];
    bytes[24..].copy_from_slice(&h.to_be_bytes());
    Hash256::from_display_bytes(bytes)
}

fn connects(count: i64) -> Vec<Outcome> {
    let seed = records(&vector(0, "golden"))
        .into_iter()
        .find_map(|r| match r {
            Record::Outcome(o) if o.kind() == Kind::Connect => Some(o),
            _ => None,
        })
        .unwrap();
    assert!(
        group(Kind::Connect, FieldClass::Semantic).iter().all(|f| seed.s().get(f.key).is_some())
    );
    (0..count)
        .map(|h| {
            let mut o = seed.clone();
            o.id = Ident {
                h,
                b: hash(h),
                org: Origin::Tip,
            };
            o.s_mut().set("mnl", Value::string(hash(h).to_hex())).unwrap();
            o.s_mut()
                .set(
                    "prev",
                    if h == 0 {
                        Value::Null
                    } else {
                        Value::string(hash(h - 1).to_hex())
                    },
                )
                .unwrap();
            o
        })
        .collect()
}

fn stream(cs: &[Outcome], raw: bool) -> Vec<Record> {
    let mut rs = vec![header()];
    rs.extend(cs.iter().cloned().map(|mut c| {
        if raw {
            for f in group(Kind::Connect, FieldClass::Semantic)
                .iter()
                .filter(|f| f.source == FieldSource::Golden)
            {
                c.s_mut().remove(f.key);
            }
        }
        Record::Outcome(c)
    }));
    rs
}

fn reject(o: &Outcome, res: &str) -> Record {
    let mut r = Outcome::reject(o.id.clone());
    r.s_mut().set("res", Value::string(res)).unwrap();
    Record::Outcome(r)
}

fn restart() -> Record {
    let Record::Header(mut h) = header() else {
        unreachable!()
    };
    h.run = b"2-2000000".to_vec();
    Record::Header(h)
}

fn encode(rs: &[Record]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for r in rs {
        r.encode(&mut bytes);
    }
    bytes
}

fn write(path: &Path, rs: &[Record]) {
    fs::write(path, encode(rs)).unwrap();
}
fn append(path: &Path, rs: &[Record]) {
    OpenOptions::new().append(true).open(path).unwrap().write_all(&encode(rs)).unwrap();
}

struct Harness {
    dir: TempDir,
    peer: RecordedPeer,
    node: NodeStatus,
}

impl Harness {
    fn new(cs: &[Outcome], settled: i64) -> Self {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/shadow-tests");
        fs::create_dir_all(&base).unwrap();
        let dir = tempfile::tempdir_in(base).unwrap();
        let cl = Some(ChainLock {
            h: settled,
            hash: cs[settled as usize].id.b.to_hex(),
        });
        let peer = RecordedPeer {
            status: PeerStatus {
                index_height: cs.last().unwrap().id.h,
                headers: cs.len() as i64,
                chainlock: cl.clone(),
            },
            golden: cs.iter().cloned().map(|o| ((o.id.h, o.id.b), o)).collect(),
        };
        let node = NodeStatus {
            warnings: String::new(),
            chainlock: cl,
        };
        Self {
            dir,
            peer,
            node,
        }
    }
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
    fn open(&self) -> Shadow {
        self.open_named("out")
    }
    fn open_named(&self, output: &str) -> Shadow {
        Shadow::open(
            Config {
                left: self.path("l"),
                right: self.path("r"),
                out: self.path(output),
                tripwire: "ChainLock conflict".into(),
            },
            NOW,
        )
        .unwrap()
    }
    fn run(&self, l: &[Record], r: &[Record]) -> Shadow {
        write(&self.path("l"), l);
        write(&self.path("r"), r);
        let mut shadow = self.open();
        shadow.poll(&self.peer, &self.node, NOW, 100_000).unwrap();
        shadow
    }
}

#[test]
fn a_recorded_peers_match_at_tip() {
    let l_digest = records(&vector(0, "digest"));
    let r_digest = records(&vector(1, "digest"));
    let final_chain = |rs: Vec<Record>| {
        let mut p = Projector::new();
        for (i, r) in rs.iter().enumerate() {
            p.feed(i + 1, r).unwrap();
        }
        let mut selected = vec![header()];
        selected.extend(p.chain().iter().map(|s| rs[s.at - 1].clone()));
        selected
    };
    // The recordings have different starts and histories; their final recorded chains agree.
    let l = final_chain(l_digest);
    let r_digest = final_chain(r_digest);
    let Record::Outcome(tip) = l.last().unwrap() else {
        unreachable!()
    };
    let tip = tip.id.clone();
    let cs = connects(1);
    let mut h = Harness::new(&cs, 0);
    h.peer = RecordedPeer::open(
        PeerStatus {
            index_height: tip.h,
            headers: tip.h,
            chainlock: Some(ChainLock {
                h: tip.h,
                hash: tip.b.to_hex(),
            }),
        },
        &vector(0, "golden"),
    )
    .unwrap();
    h.node.chainlock = h.peer.status.chainlock.clone();
    let right_golden = RecordedPeer::open(h.peer.status.clone(), &vector(1, "golden")).unwrap();
    let r = r_digest
        .into_iter()
        .map(|rec| match rec {
            Record::Outcome(mut o) => {
                let g = &right_golden.golden[&(o.id.h, o.id.b)];
                for f in group(Kind::Connect, FieldClass::Semantic)
                    .iter()
                    .filter(|f| f.source == FieldSource::Golden)
                {
                    o.s_mut().set(f.key, g.s().get(f.key).unwrap().clone()).unwrap();
                }
                Record::Outcome(o)
            }
            rec => rec,
        })
        .collect::<Vec<_>>();
    let shadow = h.run(&l, &r);
    assert!(shadow.state.alarms.is_empty(), "{:?}", shadow.state.alarms);
    assert_eq!(shadow.state.checkpoint.as_ref().unwrap().h, tip.h);
    assert_eq!(shadow.state.checkpoint.as_ref().unwrap().hash, tip.b.to_hex());
    assert!(shadow.sides.iter().all(|s| s.pending.is_empty()));
}

#[test]
fn b_semantic_flip_is_d1_at_exact_height() {
    let cs = connects(12);
    let mut rs = stream(&cs, false);
    let Record::Outcome(o) = &mut rs[6] else {
        unreachable!()
    };
    let mut mnl = o.s().get("mnl").unwrap().as_bytes().unwrap().to_vec();
    mnl[0] = b'1';
    o.s_mut().set("mnl", Value::Str(mnl)).unwrap();
    let h = Harness::new(&cs, 11);
    let shadow = h.run(&stream(&cs, true), &rs);
    assert_eq!(shadow.status().crit, 1);
    assert_eq!(shadow.state.alarms[0].class, Class::Crit(Crit::D1));
    assert_eq!(shadow.state.alarms[0].h, Some(5));
    let dump = shadow.dump_mismatch(None, Some(&h.path("dumps"))).unwrap().unwrap();
    assert!(dump.join("mismatch.json").exists());
    assert!(dump.join("left.jsonl").exists());
    assert_eq!(shadow.state.checkpoint.unwrap().h, 4);
}

#[test]
fn c_settled_invalid_rejection_is_d2() {
    let cs = connects(12);
    let mut right = stream(&cs, false);
    right.push(reject(&cs[5], "invalid"));
    let h = Harness::new(&cs, 11);
    let shadow = h.run(&stream(&cs, true), &right);
    assert_eq!(
        shadow
            .state
            .alarms
            .iter()
            .filter(|a| a.class == Class::Crit(Crit::D2) && a.h == Some(5))
            .count(),
        1
    );
    assert!(shadow.state.alarms.iter().any(|a| a.class == Class::Crit(Crit::D6)));
}

#[test]
fn d_restart_gap_is_d5() {
    let cs = connects(12);
    let mut right = stream(&cs[..4], false);
    right.push(restart());
    right.extend(stream(&cs[6..], false).into_iter().skip(1));
    let h = Harness::new(&cs, 11);
    let shadow = h.run(&stream(&cs, true), &right);
    let alarms =
        shadow.state.alarms.iter().filter(|a| a.class == Class::Crit(Crit::D5)).collect::<Vec<_>>();
    assert_eq!(alarms.len(), 1);
    assert_eq!(alarms[0].h, Some(4));
    assert!(shadow.state.checkpoint.as_ref().is_none_or(|c| c.h < 4));
}

#[test]
fn e_rewind_above_settlement_is_warn_only() {
    let cs = connects(12);
    let mut right = stream(&cs, false);
    right.push(restart());
    right.extend(stream(&cs[9..], false).into_iter().skip(1));
    let h = Harness::new(&cs, 5);
    let shadow = h.run(&stream(&cs, true), &right);
    assert_eq!(shadow.status().crit, 0);
    assert_eq!(shadow.state.alarms.len(), 1);
    assert_eq!(shadow.state.alarms[0].class, Class::Warn(Warn::Rewind));
    assert_eq!(shadow.state.checkpoint.unwrap().h, 5);

    let h = Harness::new(&cs, 5);
    let mut shadow = h.run(&stream(&cs, true), &stream(&cs, false));
    let mut suffix = vec![restart()];
    suffix.extend(stream(&cs[3..], false).into_iter().skip(1));
    append(&h.path("r"), &suffix);
    shadow.poll(&h.peer, &h.node, NOW + 1, 1000).unwrap();
    assert_eq!(shadow.status().crit, 0);
    assert_eq!(shadow.state.checkpoint.unwrap().h, 5);
}

fn replay_suffix(
    cs: &[Outcome],
    from: usize,
    to: usize,
    changed: Option<usize>,
    missing: bool,
) -> Vec<Record> {
    let mut out = vec![restart()];
    for (h, source) in cs.iter().enumerate().take(to + 1).skip(from) {
        let mut o = source.clone();
        if changed == Some(h) {
            o.s_mut().set("mnl", Value::string(hash(999).to_hex())).unwrap();
        }
        if missing && h == 5 {
            let field = group(Kind::Connect, FieldClass::Semantic)
                .iter()
                .find(|f| o.s().get(f.key).is_some())
                .unwrap();
            o.s_mut().remove(field.key);
        }
        out.push(Record::Outcome(o));
    }
    out
}

#[test]
fn checkpoint_replay_split_waits_until_observed() {
    let cs = connects(12);
    for reopen in [false, true] {
        let h = Harness::new(&cs, 11);
        let mut shadow = h.run(&stream(&cs, true), &stream(&cs, false));
        let suffix = replay_suffix(&cs, 3, 5, None, false);
        append(&h.path("r"), &suffix);
        shadow.poll(&h.peer, &h.node, NOW + 1, 1000).unwrap();
        assert_eq!(shadow.status().crit, 0);
        if reopen {
            drop(shadow);
            shadow = h.open();
        }
        append(&h.path("r"), &replay_suffix(&cs, 6, 11, None, false)[1..]);
        shadow.poll(&h.peer, &h.node, NOW + 2, 1000).unwrap();
        assert_eq!(shadow.status().crit, 0);
    }

    let h = Harness::new(&cs, 11);
    let mut shadow = h.run(&stream(&cs, true), &stream(&cs, false));
    append(&h.path("r"), &replay_suffix(&cs, 3, 10, None, false));
    shadow.poll(&h.peer, &h.node, NOW + 1, 1000).unwrap();
    let mut bad = cs[11].clone();
    bad.id.b = hash(999);
    bad.s_mut().set("prev", Value::string(cs[10].id.b.to_hex())).unwrap();
    append(&h.path("r"), &[Record::Outcome(bad)]);
    shadow.poll(&h.peer, &h.node, NOW + 2, 1000).unwrap();
    assert!(
        shadow.state.alarms.iter().any(|a| a.class == Class::Crit(Crit::D1) && a.h == Some(11))
    );
}

#[test]
fn settled_replay_semantics_are_checked_across_restart() {
    let cs = connects(12);
    for (changed, missing) in [(true, false), (false, true)] {
        for reopen in [false, true] {
            let h = Harness::new(&cs, 11);
            let mut shadow = h.run(&stream(&cs, true), &stream(&cs, false));
            if reopen {
                drop(shadow);
                shadow = h.open();
            }
            let suffix = replay_suffix(&cs, 3, 11, changed.then_some(5), missing);
            append(&h.path("r"), &suffix);
            shadow.poll(&h.peer, &h.node, NOW + 1, 1000).unwrap();
            let class = if missing {
                Crit::D4
            } else {
                Crit::D1
            };
            assert!(
                shadow.state.alarms.iter().any(|a| a.class == Class::Crit(class) && a.h == Some(5))
            );
        }
    }
}

#[test]
fn stale_error_rejection_warns_after_both_sides_rewind() {
    let original = connects(12);
    let mut alternate = original.clone();
    for (i, o) in alternate.iter_mut().enumerate().skip(9) {
        o.id.b = hash(100 + i as i64);
        if i > 9 {
            o.s_mut().set("prev", Value::string(hash(99 + i as i64).to_hex())).unwrap();
        }
    }
    let h = Harness::new(&alternate, 11);
    let mut shadow = h.run(&stream(&original, true), &stream(&original, false));
    let mut left = vec![restart()];
    left.extend(stream(&alternate[9..], true).into_iter().skip(1));
    left.push(reject(&original[10], "error"));
    let mut right = vec![restart()];
    right.extend(stream(&alternate[9..], false).into_iter().skip(1));
    append(&h.path("l"), &left);
    append(&h.path("r"), &right);
    shadow.poll(&h.peer, &h.node, NOW + 1, 1000).unwrap();
    let b = original[10].id.b.to_hex();
    assert!(
        shadow
            .state
            .alarms
            .iter()
            .any(|a| a.class == Class::Warn(Warn::Rejection) && a.b.as_deref() == Some(&b))
    );
    assert!(
        !shadow
            .state
            .alarms
            .iter()
            .any(|a| a.class == Class::Crit(Crit::D2) && a.b.as_deref() == Some(&b))
    );
}

#[test]
fn shadow_state_lock_and_tail_identity_are_enforced() {
    let cs = connects(3);
    let h = Harness::new(&cs, 2);
    write(&h.path("l"), &stream(&cs, true));
    write(&h.path("r"), &stream(&cs, false));
    let shadow = h.open();
    let second = Shadow::open(
        Config {
            left: h.path("l"),
            right: h.path("r"),
            out: h.path("out"),
            tripwire: "ChainLock conflict".into(),
        },
        NOW,
    );
    let error = match second {
        Ok(_) => panic!("second shadow unexpectedly acquired the state lock"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("already locked"));
    drop(shadow);
    let mut shadow = h.open();
    shadow.poll(&h.peer, &h.node, NOW, 100).unwrap();
    let replacement = h.path("r-replacement");
    fs::write(&replacement, fs::read(h.path("r")).unwrap()).unwrap();
    fs::rename(replacement, h.path("r")).unwrap();
    let error = shadow.poll(&h.peer, &h.node, NOW + 1, 100).unwrap_err();
    assert!(error.to_string().contains("changed (device/inode"));
}

#[test]
fn f_partial_last_line_waits_for_completion() {
    let cs = connects(2);
    let h = Harness::new(&cs, 1);
    let left = encode(&stream(&cs, true));
    let right = encode(&stream(&cs, false));
    fs::write(h.path("l"), &left[..left.len() - 3]).unwrap();
    fs::write(h.path("r"), &right).unwrap();
    let mut shadow = h.open();
    shadow.poll(&h.peer, &h.node, NOW, 100).unwrap();
    assert!(shadow.state.checkpoint.is_none());
    let offset = shadow.sides[0].tail.offset;
    assert!(offset < left.len() as u64);
    drop(shadow);
    OpenOptions::new()
        .append(true)
        .open(h.path("l"))
        .unwrap()
        .write_all(&left[left.len() - 3..])
        .unwrap();
    let mut shadow = h.open();
    shadow.poll(&h.peer, &h.node, NOW, 100).unwrap();
    assert_eq!(shadow.state.checkpoint.as_ref().unwrap().h, 1);
    assert_eq!(shadow.sides[0].tail.offset, left.len() as u64);
    assert_eq!(shadow.status().crit, 0);
}

fn resumed_equivalence(cs: &[Outcome], l: &[Record], r: &[Record], split: usize, stuck: bool) {
    let mut h = Harness::new(cs, cs.last().unwrap().id.h);
    if stuck {
        h.peer.status.index_height = 0;
    }
    write(&h.path("l"), &l[..split.min(l.len())]);
    write(&h.path("r"), &r[..split.min(r.len())]);
    if stuck {
        let mut resumed = h.open_named("out-resume");
        resumed.poll(&h.peer, &h.node, NOW, 100_000).unwrap();
        append(&h.path("l"), &l[split.min(l.len())..]);
        append(&h.path("r"), &r[split.min(r.len())..]);
        resumed.poll(&h.peer, &h.node, NOW, 100_000).unwrap();
        h.peer.status.index_height = cs.last().unwrap().id.h;
        drop(resumed);
        resumed = h.open_named("out-resume");
        resumed.poll(&h.peer, &h.node, NOW, 100_000).unwrap();
        h.peer.status.index_height = 0;
        let mut continuous = h.open_named("out-cont");
        continuous.poll(&h.peer, &h.node, NOW, 100_000).unwrap();
        assert!(continuous.sides[0].pending.len() > 1000);
        h.peer.status.index_height = cs.last().unwrap().id.h;
        continuous.poll(&h.peer, &h.node, NOW, 100_000).unwrap();
        assert_eq!(continuous.state.alarms, resumed.state.alarms, "split {split}");
        assert_eq!(continuous.status(), resumed.status(), "split {split}");
        assert_eq!(continuous.state.checkpoint, resumed.state.checkpoint, "split {split}");
        assert_eq!(continuous.state.emitted, resumed.state.emitted, "split {split}");
        return;
    }
    let mut continuous = h.open_named("out-cont");
    continuous.poll(&h.peer, &h.node, NOW, 100_000).unwrap();
    let mut resumed = h.open_named("out-resume");
    resumed.poll(&h.peer, &h.node, NOW, 100_000).unwrap();
    append(&h.path("l"), &l[split.min(l.len())..]);
    append(&h.path("r"), &r[split.min(r.len())..]);
    continuous.poll(&h.peer, &h.node, NOW, 100_000).unwrap();
    resumed.poll(&h.peer, &h.node, NOW, 100_000).unwrap();
    assert_eq!(continuous.state.alarms, resumed.state.alarms, "split {split}");
    assert_eq!(continuous.status(), resumed.status(), "split {split}");
    assert_eq!(continuous.state.checkpoint, resumed.state.checkpoint, "split {split}");
    assert_eq!(continuous.state.emitted, resumed.state.emitted, "split {split}");
}

#[test]
fn g_every_split_resumes_equivalently() {
    let cs = connects(12);
    let left = stream(&cs, true);
    let mut right = stream(&cs, false);
    right.push(restart());
    right.extend(stream(&cs[8..], false).into_iter().skip(1));
    right.push(reject(&cs[10], "invalid"));
    for split in 0..=right.len() {
        resumed_equivalence(&cs, &left, &right, split, false);
    }
}

#[test]
fn g_recovery_keeps_two_thousand_pending_blocks() {
    let cs = connects(2001);
    let left = stream(&cs, true);
    let right = stream(&cs, false);
    let mut splits = vec![0, 1, 2, 999, 1000, 1001, 1998, 1999, 2000, 2001, 2002];
    let mut seed = 0x6f15_0a11_u64;
    while splits.len() < 43 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        splits.push((seed as usize) % (left.len() + 1));
    }
    splits.sort_unstable();
    splits.dedup();
    for split in splits {
        resumed_equivalence(&cs, &left, &right, split, true);
    }
}

#[test]
#[ignore = "the full boundary sweep is run as a final release-profile gate"]
fn g_recovery_exhaustive_two_thousand_boundaries() {
    let cs = connects(2001);
    let left = stream(&cs, true);
    let right = stream(&cs, false);
    for split in 0..=left.len() {
        resumed_equivalence(&cs, &left, &right, split, true);
    }
}

#[test]
fn h_missing_required_key_is_d4() {
    let cs = connects(12);
    for field in group(Kind::Connect, FieldClass::Semantic) {
        let mut right = stream(&cs, false);
        let Record::Outcome(o) = &mut right[6] else {
            unreachable!()
        };
        o.s_mut().remove(field.key);
        let h = Harness::new(&cs, 11);
        let shadow = h.run(&stream(&cs, true), &right);
        assert_eq!(shadow.status().crit, 1, "{}", field.key);
        assert_eq!(shadow.state.alarms[0].class, Class::Crit(Crit::D4), "{}", field.key);
        assert_eq!(shadow.state.alarms[0].h, Some(5));
        assert_eq!(shadow.state.checkpoint.unwrap().h, 4);
    }
}

#[test]
fn i_rewound_connect_and_invalid_reject_are_d6_across_restart() {
    let cs = connects(12);
    for resume in [false, true] {
        let h = Harness::new(&cs, 5);
        let mut right = stream(&cs, false);
        right.push(restart());
        let mut alternate = cs[9..].to_vec();
        for (i, o) in alternate.iter_mut().enumerate() {
            o.id.b = hash(100 + i as i64);
            if i > 0 {
                o.s_mut().set("prev", Value::string(hash(99 + i as i64).to_hex())).unwrap();
            }
        }
        right.extend(stream(&alternate, false).into_iter().skip(1));
        let mut shadow = h.run(&stream(&cs, true), &right);
        if resume {
            drop(shadow);
            shadow = h.open();
        }
        // L also leaves the original block before the invalid rejection arrives.
        let mut left_suffix = vec![restart()];
        left_suffix.extend(stream(&alternate, true).into_iter().skip(1));
        left_suffix.push(reject(&cs[10], "invalid"));
        append(&h.path("l"), &left_suffix);
        shadow.poll(&h.peer, &h.node, NOW + 1, 1000).unwrap();
        let b = cs[10].id.b.to_hex();
        let matching =
            shadow.state.alarms.iter().filter(|a| a.b.as_ref() == Some(&b)).collect::<Vec<_>>();
        assert_eq!(matching.len(), 1, "{matching:?}");
        assert_eq!(matching[0].class, Class::Crit(Crit::D6));
        assert_eq!(shadow.status().crit, 1);
        assert_eq!(shadow.state.clock_start, NOW + 1);
        drop(shadow);
        let mut shadow = h.open();
        shadow.poll(&h.peer, &h.node, NOW + 2, 1000).unwrap();
        assert_eq!(
            shadow.state.alarms.iter().filter(|a| a.class == Class::Crit(Crit::D6)).count(),
            1
        );
    }
}

#[test]
fn projector_snapshot_property() {
    let cs = connects(16);
    let mut synthetic = stream(&cs[..10], false);
    synthetic.push(restart());
    synthetic.extend(stream(&cs[7..12], false).into_iter().skip(1));
    synthetic.push(restart());
    synthetic.extend(stream(&cs[14..], false).into_iter().skip(1));
    // Exercise an emptied-chain floor, which a chain-only snapshot would lose.
    let mut floor = vec![header(), Record::Outcome(cs[0].clone())];
    let mut disconnect = Outcome::disconnect(cs[0].id.clone());
    disconnect.s_mut().set("res", Value::string("ok")).unwrap();
    floor.push(Record::Outcome(disconnect));
    floor.push(Record::Outcome(cs[0].clone()));
    for rs in [records(&vector(0, "digest")), records(&vector(1, "digest")), synthetic, floor] {
        let mut whole = Projector::new();
        for (at, r) in rs.iter().enumerate() {
            whole.feed(at + 1, r).unwrap();
        }
        let expected = whole.finish();
        for split in 0..=rs.len() {
            for chunk in [1, 7, rs.len().max(1)] {
                let mut p = Projector::new();
                for (at, r) in rs[..split].iter().enumerate() {
                    p.feed(at + 1, r).unwrap();
                }
                let before = p.snapshot();
                let wire = serde_json::to_vec(&before).unwrap();
                let snapshot: ProjectorSnapshot = serde_json::from_slice(&wire).unwrap();
                p = Projector::restore(snapshot).unwrap();
                assert_eq!(p.snapshot(), before);
                for (batch, rs) in rs[split..].chunks(chunk).enumerate() {
                    for (offset, r) in rs.iter().enumerate() {
                        p.feed(split + batch * chunk + offset + 1, r).unwrap();
                    }
                }
                assert_eq!(p.finish(), expected, "split {split} chunk {chunk}");
            }
        }
    }
    let mut snapshot = Projector::new().snapshot();
    snapshot.version = 99;
    assert!(Projector::restore(snapshot).is_err());
}

#[test]
fn settlement_waits_for_alignment_then_falls_back_to_depth() {
    let cs = connects(16);
    let mut h = Harness::new(&cs, 15);
    h.peer.status.chainlock.as_mut().unwrap().hash = hash(999).to_hex();
    let mut shadow = h.run(&stream(&cs, true), &stream(&cs, false));
    assert!(shadow.state.checkpoint.is_none());
    shadow.poll(&h.peer, &h.node, NOW + 1801, 1000).unwrap();
    assert_eq!(
        shadow.state.settle,
        Some(Settle::Depth {
            h: 9
        })
    );
    assert_eq!(shadow.state.checkpoint.unwrap().h, 9);

    let mut h = Harness::new(&cs, 15);
    h.peer.status.chainlock.as_mut().unwrap().hash = hash(999).to_hex();
    let mut shadow = h.run(&stream(&cs, true), &stream(&cs[..5], false));
    shadow.poll(&h.peer, &h.node, NOW + 7201, 1000).unwrap();
    assert_eq!(shadow.status().crit, 0);

    let mut h = Harness::new(&cs, 5);
    let mut shadow = h.run(&stream(&cs, true), &stream(&cs, false));
    h.peer.status.chainlock.as_mut().unwrap().hash = hash(999).to_hex();
    shadow.poll(&h.peer, &h.node, NOW + 1, 1000).unwrap();
    assert!(shadow.state.settle.is_none());
}

#[test]
fn tripwire_and_stalls_reset_the_clock() {
    let cs = connects(20);
    let mut h = Harness::new(&cs, 19);
    h.node.warnings = "alert: ChainLock conflict".into();
    let mut shadow = h.run(&stream(&cs, true), &stream(&cs[..5], false));
    assert!(shadow.state.alarms.iter().any(|a| a.class == Class::Crit(Crit::D3)));
    h.peer.status.headers += 1;
    shadow.poll(&h.peer, &h.node, NOW + 7201, 1000).unwrap();
    assert!(shadow.state.alarms.iter().any(|a| a.class == Class::Crit(Crit::Stall)));
    assert_eq!(shadow.state.clock_start, NOW + 7201);
}

#[test]
fn rpc_backoff_survives_restart_and_recovers() {
    struct Flaky<'a> {
        calls: Cell<usize>,
        peer: &'a RecordedPeer,
    }
    impl PeerRpc for Flaky<'_> {
        fn status(&self) -> Result<PeerStatus> {
            let calls = self.calls.get();
            self.calls.set(calls + 1);
            if calls == 0 {
                return Err(anyhow!("timeout"));
            }
            self.peer.status()
        }
        fn node(&self) -> Result<&dyn NodeRpc> {
            Err(anyhow!("offline"))
        }
        fn enrich(&self, record: &Outcome) -> Result<Enrichment> {
            self.peer.enrich(record)
        }
    }
    let cs = connects(3);
    let h = Harness::new(&cs, 2);
    write(&h.path("l"), &stream(&cs, true));
    write(&h.path("r"), &stream(&cs, false));
    let flaky = Flaky {
        calls: Cell::new(0),
        peer: &h.peer,
    };
    let mut shadow = h.open();
    shadow.poll(&flaky, &h.node, NOW, 100).unwrap();
    assert_eq!(shadow.status().crit, 0);
    assert_eq!(shadow.state.alarms[0].class, Class::Warn(Warn::PeerRpc));
    drop(shadow);
    let mut shadow = h.open();
    shadow.poll(&flaky, &h.node, NOW + 1, 100).unwrap();
    assert_eq!(flaky.calls.get(), 1);
    shadow.poll(&flaky, &h.node, NOW + 2, 100).unwrap();
    assert_eq!(flaky.calls.get(), 2);
    assert_eq!(shadow.state.checkpoint.unwrap().h, 2);

    let h = Harness::new(&cs, 2);
    let mut shadow = h.run(&stream(&cs, true), &stream(&cs, false));
    let flaky = Flaky {
        calls: Cell::new(0),
        peer: &h.peer,
    };
    let mut divergent = cs[0].clone();
    divergent.id.b = hash(999);
    append(&h.path("r"), &[restart(), Record::Outcome(divergent)]);
    shadow.poll(&flaky, &h.node, NOW + 1, 100).unwrap();
    assert_eq!(flaky.calls.get(), 0);
    assert_eq!(shadow.state.alarms.last().unwrap().class, Class::Crit(Crit::D1));
}

#[test]
fn alarm_journal_repairs_outputs_and_preserves_permissions() {
    let cs = connects(3);
    let mut h = Harness::new(&cs, 2);
    h.node.warnings = "ChainLock conflict".into();
    let shadow = h.run(&stream(&cs, true), &stream(&cs, false));
    let expected = fs::read(h.path("out/alarms.jsonl")).unwrap();
    drop(shadow);
    fs::write(h.path("out/alarms.jsonl"), b"partial").unwrap();
    let mut shadow = h.open();
    assert_eq!(fs::read(h.path("out/alarms.jsonl")).unwrap(), expected);
    shadow.poll(&h.peer, &h.node, NOW + 1, 100).unwrap();
    assert_eq!(shadow.status().crit, 1);
    assert_eq!(fs::metadata(h.path("out")).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(fs::metadata(h.path("out/state.json")).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn rdashd_stall_keeps_its_timer_without_a_projection_tip() {
    let cs = connects(20);
    let h = Harness::new(&cs, 19);
    let mut shadow = h.run(&stream(&cs, true), &[header()]);
    shadow.poll(&h.peer, &h.node, NOW + 1801, 100).unwrap();
    assert_eq!(shadow.status().crit, 0);
    shadow.poll(&h.peer, &h.node, NOW + 7201, 100).unwrap();
    assert_eq!(shadow.status().crit, 1);
    assert_eq!(shadow.state.alarms.last().unwrap().class, Class::Crit(Crit::Stall));
}

#[test]
fn enrichment_reorg_does_not_hide_an_earlier_settled_mismatch() {
    let cs = connects(12);
    let mut h = Harness::new(&cs, 5);
    h.peer.golden.remove(&(8, cs[8].id.b));
    let mut right = stream(&cs, false);
    let Record::Outcome(o) = &mut right[4] else {
        unreachable!()
    };
    o.s_mut().set("mnl", Value::string(hash(999).to_hex())).unwrap();
    let shadow = h.run(&stream(&cs, true), &right);
    assert_eq!(shadow.status().crit, 1);
    assert_eq!(shadow.state.alarms[0].class, Class::Crit(Crit::D1));
    assert_eq!(shadow.state.alarms[0].h, Some(3));
    assert_eq!(shadow.state.checkpoint.unwrap().h, 2);
}

#[test]
fn d6_keeps_an_unsettled_hash_below_the_settled_height() {
    let original = connects(12);
    let mut active = original.clone();
    for (i, o) in active.iter_mut().enumerate().skip(9) {
        o.id.b = hash(100 + i as i64);
        if i > 9 {
            o.s_mut().set("prev", Value::string(hash(99 + i as i64).to_hex())).unwrap();
        }
    }
    let h = Harness::new(&active, 11);
    let mut left = stream(&active, true);
    left.push(reject(&original[10], "invalid"));
    let mut shadow = h.run(&left, &stream(&original[..9], false));
    assert!(shadow.state.evidence.contains_key(&original[10].id.b.to_hex()));
    drop(shadow);
    shadow = h.open();
    let mut suffix = stream(&original[9..], false).into_iter().skip(1).collect::<Vec<_>>();
    suffix.push(restart());
    suffix.extend(stream(&active[9..], false).into_iter().skip(1));
    append(&h.path("r"), &suffix);
    shadow.poll(&h.peer, &h.node, NOW + 1, 1000).unwrap();
    assert_eq!(
        shadow
            .state
            .alarms
            .iter()
            .filter(|a| a.class == Class::Crit(Crit::D6)
                && a.b.as_deref() == Some(&original[10].id.b.to_hex()))
            .count(),
        1
    );
}

#[test]
fn ignored_disconnect_preserves_active_pending_outcomes() {
    let cs = connects(12);
    for org in [Origin::Proposal, Origin::Verifydb] {
        let h = Harness::new(&cs, 11);
        let mut right = stream(&cs, false);
        let mut d = Outcome::disconnect(Ident {
            h: 5,
            b: hash(999),
            org,
        });
        d.s_mut().set("res", Value::string("ok")).unwrap();
        right.push(Record::Outcome(d));
        let shadow = h.run(&stream(&cs, true), &right);
        assert_eq!(shadow.status().crit, 0);
        assert_eq!(shadow.state.checkpoint.unwrap().h, 11);
    }
}
