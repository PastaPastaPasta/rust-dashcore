use std::cell::Cell;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::{Enrichment, PeerRpc, run_on_crit};
use crate::digest::extract::NodeRpc;
use anyhow::{Result, anyhow};
use dash_statedigest::compare::{Projector, ProjectorSnapshot};
use dash_statedigest::{
    Class as FieldClass, FieldSource, Hash256, Ident, Kind, Origin, Outcome, Record, Value, group,
};
use std::time::{Duration, Instant};
use tempfile::TempDir;

use super::alarm::{Class, Crit, Warn};
use super::live::CHAINLOCK_WARNING_PREFIX;
use super::settle::{ChainLock, Settle};
use super::{Config, NodeReply, NodeStatus, PeerStatus, RecordedPeer, Shadow};
use crate::digest::store::Records;

const NOW: u64 = 10_000;

fn sh() -> &'static Path {
    Path::new("sh")
}

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
    /// Retries for up to 5 s while the state lock is held: a hook another test thread spawns
    /// carries a copy of the lock's file description until its `exec`.
    fn open_named(&self, output: &str) -> Shadow {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let opened = Shadow::open(
                Config {
                    left: self.path("l"),
                    right: self.path("r"),
                    out: self.path(output),
                },
                NOW,
            );
            match opened {
                Err(e) if e.to_string().contains("already locked") && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                opened => return opened.unwrap(),
            }
        }
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
    h.node.warnings = format!("alert: {}", CHAINLOCK_WARNING_PREFIX);
    let mut shadow = h.run(&stream(&cs, true), &stream(&cs[..5], false));
    assert!(shadow.state.alarms.iter().any(|a| a.class == Class::Crit(Crit::D3)));
    h.peer.status.headers += 1;
    shadow.poll(&h.peer, &h.node, NOW + 7201, 1000).unwrap();
    assert!(shadow.state.alarms.iter().any(|a| a.class == Class::Crit(Crit::Stall)));
    assert_eq!(shadow.state.clock_start, NOW + 7201);
}

#[test]
fn d3_matches_the_fixed_prefix_exactly_or_with_a_suffix() {
    let cs = connects(8);
    for warning in [
        CHAINLOCK_WARNING_PREFIX.to_owned(),
        format!("{CHAINLOCK_WARNING_PREFIX} (latch not persisted)"),
        format!("Other warning; {CHAINLOCK_WARNING_PREFIX}"),
    ] {
        let mut h = Harness::new(&cs, 7);
        h.node.warnings = warning.clone();
        let shadow = h.run(&stream(&cs, true), &stream(&cs, false));
        let d3: Vec<_> =
            shadow.state.alarms.iter().filter(|a| a.class == Class::Crit(Crit::D3)).collect();
        assert_eq!(d3.len(), 1, "{warning:?}");
        assert_eq!(d3[0].detail, warning);
    }
}

#[test]
fn d3_does_not_match_other_warning_text() {
    let cs = connects(8);
    for warning in ["", "Warning: This node disagrees with another chain"] {
        let mut h = Harness::new(&cs, 7);
        h.node.warnings = warning.to_owned();
        let shadow = h.run(&stream(&cs, true), &stream(&cs, false));
        assert!(
            !shadow.state.alarms.iter().any(|a| a.class == Class::Crit(Crit::D3)),
            "unexpected D3 for {warning:?}"
        );
    }
}

#[test]
fn status_line_has_the_o15_format() {
    let cs = connects(8);
    let h = Harness::new(&cs, 7);
    let mut shadow = h.run(&stream(&cs, true), &stream(&cs, false));
    let line = shadow.status_line(NOW + 2 * 86_400);
    let hash12 = &cs[7].id.b.to_hex()[..12];
    assert_eq!(
        line,
        format!(
            "O15 shadow Sat Jan  3 02:46:40 UTC 1970 settled=7 {hash12} lagR=0 lagL=0 \
             clock=10000/2d alarms=0/0 last=none"
        )
    );
    shadow.write_status_line(NOW + 2 * 86_400).unwrap();
    assert_eq!(fs::read_to_string(h.path("out/STATUS.line")).unwrap(), format!("{line}\n"));
    assert!(fs::read_dir(h.path("out")).unwrap().all(|e| {
        let name = e.unwrap().file_name();
        !name.to_string_lossy().starts_with(".tmp")
    }));

    let mut h = Harness::new(&cs, 7);
    h.node.warnings = CHAINLOCK_WARNING_PREFIX.to_owned();
    let shadow = h.run(&stream(&cs, true), &stream(&cs, false));
    let line = fs::read_to_string(h.path("out/STATUS.line")).unwrap();
    let tip = cs[7].id.b.to_hex();
    assert!(line.ends_with(&format!(" alarms=1/0 last=Crit(D3)@7:{tip}\n")), "{line}");
    assert_eq!(line.trim_end(), shadow.status_line(NOW));
}

#[test]
fn status_line_date_is_date_u() {
    for (seconds, shown) in [
        (0, "Thu Jan  1 00:00:00 UTC 1970"),
        (951_782_400, "Tue Feb 29 00:00:00 UTC 2000"),
        (1_794_712_304, "Sun Nov 15 03:11:44 UTC 2026"),
        (4_107_542_399, "Sun Feb 28 23:59:59 UTC 2100"),
    ] {
        assert_eq!(super::utc_date(seconds), shown);
    }
}

#[test]
fn live_rpc_timeout_is_a_warn_with_backoff() {
    use std::net::TcpListener;

    use super::live::{LivePeer, LiveRdashd};
    use crate::rpc::Auth;

    // Accepts connections and never answers.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let open: Vec<_> = listener.incoming().collect();
        drop(open);
    });
    let auth = || Auth::UserPass("u".into(), "p".into());
    let peer = LivePeer::new(url.clone(), auth(), Duration::from_millis(200));
    let node = LiveRdashd::new(url, auth(), Duration::from_millis(200));

    let cs = connects(3);
    let h = Harness::new(&cs, 2);
    write(&h.path("l"), &stream(&cs, true));
    write(&h.path("r"), &stream(&cs, false));
    let mut shadow = h.open();
    let started = Instant::now();
    shadow.poll(&peer, &node, NOW, 100).unwrap();
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    let classes: Vec<_> = shadow.state.alarms.iter().map(|a| a.class).collect();
    assert_eq!(classes, [Class::Warn(Warn::NodeRpc), Class::Warn(Warn::PeerRpc)]);
    assert_eq!(shadow.status().crit, 0);
    assert_eq!(shadow.state.rpc_failures, 1);
    assert_eq!(shadow.state.retry_after, NOW + 2);
    // Inside the backoff the peer is not asked again.
    shadow.poll(&h.peer, &h.node, NOW + 1, 100).unwrap();
    assert_eq!(shadow.state.rpc_failures, 1);
    assert!(shadow.state.checkpoint.is_none());
    // After it, the recovered peer settles the range and clears the backoff.
    shadow.poll(&h.peer, &h.node, NOW + 2, 100).unwrap();
    assert_eq!(shadow.state.rpc_failures, 0);
    assert_eq!(shadow.state.checkpoint.unwrap().h, 2);
}

fn crit_count(shadow: &Shadow) -> usize {
    shadow.state.alarms.iter().filter(|a| matches!(a.class, Class::Crit(_))).count()
}

#[test]
fn on_crit_hook_runs_once_per_key_across_a_restart() {
    let cs = connects(8);
    let mut h = Harness::new(&cs, 7);
    h.node.warnings = CHAINLOCK_WARNING_PREFIX.to_owned();
    let marker = h.path("hook-count");
    let hook = Some(format!("printf '%s\\n' \"$O15_ALARM_CLASS\" >> '{}'", marker.display()));
    let runs =
        || fs::read_to_string(&marker).unwrap().lines().map(str::to_owned).collect::<Vec<_>>();
    write(&h.path("l"), &stream(&cs, true));
    write(&h.path("r"), &stream(&cs, false));
    let mut shadow = h.open();
    shadow.set_on_crit(hook.clone(), Duration::from_secs(10));
    shadow.poll(&h.peer, &h.node, NOW, 1000).unwrap();
    assert_eq!(runs(), ["Crit(D3)"]);
    // The same key on the next poll, and after a restart, does not run the hook again.
    shadow.poll(&h.peer, &h.node, NOW + 1, 1000).unwrap();
    drop(shadow);
    let mut shadow = h.open();
    shadow.set_on_crit(hook, Duration::from_secs(10));
    shadow.poll(&h.peer, &h.node, NOW + 2, 1000).unwrap();
    assert_eq!(runs(), ["Crit(D3)"]);
    // A new key after the restart runs it once more.
    let mut divergent = cs[0].clone();
    divergent.id.b = hash(999);
    append(&h.path("r"), &[restart(), Record::Outcome(divergent)]);
    shadow.poll(&h.peer, &h.node, NOW + 3, 1000).unwrap();
    assert!(crit_count(&shadow) > 1);
    assert_eq!(runs().len(), crit_count(&shadow));
}

#[test]
fn on_crit_runs_even_when_the_status_line_cannot_be_written() {
    let cs = connects(8);
    let mut h = Harness::new(&cs, 7);
    h.node.warnings = CHAINLOCK_WARNING_PREFIX.to_owned();
    let marker = h.path("hook-ran");
    write(&h.path("l"), &stream(&cs, true));
    write(&h.path("r"), &stream(&cs, false));
    let mut shadow = h.open();
    // A directory in STATUS.line's place makes its rename fail after the state is saved.
    fs::create_dir(h.path("out/STATUS.line")).unwrap();
    fs::write(h.path("out/STATUS.line/x"), "").unwrap();
    shadow.set_on_crit(Some(format!("touch '{}'", marker.display())), Duration::from_secs(10));
    assert!(shadow.poll(&h.peer, &h.node, NOW, 1000).is_err());
    assert!(marker.exists());
}

#[test]
fn failing_or_hanging_on_crit_does_not_stop_the_shadow() {
    for (hook, timeout) in
        [("exit 3", Duration::from_secs(10)), ("sleep 30", Duration::from_millis(200))]
    {
        let cs = connects(12);
        let mut h = Harness::new(&cs, 11);
        h.node.warnings = CHAINLOCK_WARNING_PREFIX.to_owned();
        write(&h.path("l"), &stream(&cs[..8], true));
        write(&h.path("r"), &stream(&cs[..8], false));
        let mut shadow = h.open();
        shadow.set_on_crit(Some(hook.to_owned()), timeout);
        let started = Instant::now();
        shadow.poll(&h.peer, &h.node, NOW, 1000).unwrap();
        assert!(started.elapsed() < Duration::from_secs(10), "{hook}: {:?}", started.elapsed());
        assert_eq!(crit_count(&shadow), 1, "{hook}");
        append(&h.path("l"), &stream(&cs[8..], true)[1..]);
        append(&h.path("r"), &stream(&cs[8..], false)[1..]);
        assert_eq!(shadow.poll(&h.peer, &h.node, NOW + 1, 1000).unwrap(), 8, "{hook}");
        assert_eq!(shadow.state.checkpoint.as_ref().unwrap().h, 11, "{hook}");
    }
}

#[test]
fn on_crit_gets_the_alarm_only_through_its_environment() {
    let dir = TempDir::new().unwrap();
    let injected = dir.path().join("injected");
    let alarm = super::alarm::Alarm {
        class: Class::Crit(Crit::D1),
        h: Some(7),
        b: Some("ab".into()),
        detail: format!("$(touch '{}'); `touch '{}'`", injected.display(), injected.display()),
    };
    let seen = dir.path().join("seen");
    let command = format!(
        "printf '%s|%s|%s|%s' \"$O15_ALARM_CLASS\" \"$O15_ALARM_HEIGHT\" \"$O15_ALARM_HASH\" \"$O15_ALARM_DETAIL\" > '{}'",
        seen.display()
    );
    run_on_crit(sh(), &command, &alarm, Duration::from_secs(10)).unwrap();
    assert_eq!(fs::read_to_string(&seen).unwrap(), format!("Crit(D1)|7|ab|{}", alarm.detail));
    assert!(!injected.exists());
}

#[test]
fn hanging_on_crit_is_killed_with_its_children() {
    let dir = TempDir::new().unwrap();
    let pid_file = dir.path().join("pid");
    let alarm = super::alarm::Alarm {
        class: Class::Crit(Crit::D3),
        h: None,
        b: None,
        detail: String::new(),
    };
    let command = format!("sleep 30 & echo $! > '{}'; wait", pid_file.display());
    let started = Instant::now();
    run_on_crit(sh(), &command, &alarm, Duration::from_millis(300)).unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    let pid: libc::pid_t = fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
    // The killed grandchild is reaped by init shortly after its parent died.
    let deadline = Instant::now() + Duration::from_secs(5);
    // SAFETY: kill(2) with signal 0 only probes.
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(Instant::now() < deadline, "the hook's child {pid} survived");
        std::thread::sleep(Duration::from_millis(20));
    }
    run_on_crit(sh(), "exit 3", &alarm, Duration::from_secs(10)).unwrap();
    assert!(run_on_crit(sh(), " ", &alarm, Duration::from_secs(1)).is_err());
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
    // A closed checkpoint gate still asks the peer, for the timing checks.
    assert_eq!(flaky.calls.get(), 1);
    assert!(shadow.state.alarms.iter().any(|a| a.class == Class::Crit(Crit::D1)));
    assert_eq!(shadow.state.alarms.last().unwrap().class, Class::Warn(Warn::PeerRpc));
}

#[test]
fn alarm_journal_repairs_outputs_and_preserves_permissions() {
    let cs = connects(3);
    let mut h = Harness::new(&cs, 2);
    h.node.warnings = CHAINLOCK_WARNING_PREFIX.into();
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

fn d3s(shadow: &Shadow) -> Vec<&super::alarm::Alarm> {
    shadow.state.alarms.iter().filter(|a| a.class == Class::Crit(Crit::D3)).collect()
}

#[test]
fn d3_fires_on_each_trip_across_restarts() {
    let cs = connects(12);
    let mut h = Harness::new(&cs, 7);
    let marker = h.path("hook-count");
    let hook = Some(format!("echo \"$O15_ALARM_HEIGHT\" >> '{}'", marker.display()));
    let runs = || fs::read_to_string(&marker).map_or(0, |s| s.lines().count());
    write(&h.path("l"), &stream(&cs[..9], true));
    write(&h.path("r"), &stream(&cs[..9], false));
    let open = |h: &Harness| {
        let mut shadow = h.open();
        shadow.set_on_crit(hook.clone(), Duration::from_secs(10));
        shadow
    };
    let mut shadow = open(&h);
    h.node.warnings = CHAINLOCK_WARNING_PREFIX.to_owned();
    shadow.poll(&h.peer, &h.node, NOW + 10, 1000).unwrap();
    assert_eq!(d3s(&shadow).len(), 1);
    assert_eq!(
        (d3s(&shadow)[0].h, d3s(&shadow)[0].b.clone()),
        (Some(8), Some(cs[8].id.b.to_hex()))
    );
    assert_eq!((runs(), shadow.state.clock_start), (1, NOW + 10));
    // Still tripped, on the next poll and after a restart: the same episode.
    shadow.poll(&h.peer, &h.node, NOW + 20, 1000).unwrap();
    drop(shadow);
    let mut shadow = open(&h);
    shadow.poll(&h.peer, &h.node, NOW + 30, 1000).unwrap();
    assert_eq!((d3s(&shadow).len(), runs(), shadow.state.clock_start), (1, 1, NOW + 10));
    // Cleared, then tripped again at the same tip: a second D3, hook run and clock reset.
    h.node.warnings.clear();
    shadow.poll(&h.peer, &h.node, NOW + 40, 1000).unwrap();
    assert!(!shadow.state.d3_active);
    h.node.warnings = CHAINLOCK_WARNING_PREFIX.to_owned();
    shadow.poll(&h.peer, &h.node, NOW + 50, 1000).unwrap();
    assert_eq!((d3s(&shadow).len(), runs(), shadow.state.clock_start), (2, 2, NOW + 50));
    // Cleared, a restart, then tripped at a new tip: a third.
    h.node.warnings.clear();
    shadow.poll(&h.peer, &h.node, NOW + 60, 1000).unwrap();
    drop(shadow);
    append(&h.path("l"), &stream(&cs[9..], true)[1..]);
    append(&h.path("r"), &stream(&cs[9..], false)[1..]);
    let mut shadow = open(&h);
    h.node.warnings = format!("{CHAINLOCK_WARNING_PREFIX} (latch not persisted)");
    shadow.poll(&h.peer, &h.node, NOW + 70, 1000).unwrap();
    assert_eq!((d3s(&shadow).len(), runs(), shadow.state.clock_start), (3, 3, NOW + 70));
    assert_eq!(d3s(&shadow)[2].h, Some(11));
    drop(shadow);
    // The journal with repeated keys reloads.
    let shadow = open(&h);
    assert_eq!(d3s(&shadow).len(), 3);
}

#[test]
fn rpc_warns_fire_on_each_outage() {
    struct Switch<'a> {
        down: Cell<bool>,
        peer: &'a RecordedPeer,
    }
    impl PeerRpc for Switch<'_> {
        fn status(&self) -> Result<PeerStatus> {
            if self.down.get() {
                return Err(anyhow!("peer down"));
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
    struct NodeSwitch(Cell<bool>, NodeStatus);
    impl super::RdashdRpc for NodeSwitch {
        fn status(&self) -> Result<NodeReply> {
            if self.0.get() {
                return Err(anyhow!("rdashd down"));
            }
            super::RdashdRpc::status(&self.1)
        }
    }
    let warns = |shadow: &Shadow, class| {
        shadow.state.alarms.iter().filter(|a| a.class == Class::Warn(class)).count()
    };
    let cs = connects(8);
    let h = Harness::new(&cs, 7);
    write(&h.path("l"), &stream(&cs, true));
    write(&h.path("r"), &stream(&cs, false));
    let peer = Switch {
        down: Cell::new(false),
        peer: &h.peer,
    };
    let node = NodeSwitch(Cell::new(false), h.node.clone());
    let mut shadow = h.open();
    let mut now = NOW;
    let mut step = |shadow: &mut Shadow, down: bool| {
        peer.down.set(down);
        node.0.set(down);
        // Past any backoff, so the peer is asked every time.
        now += 1000;
        shadow.poll(&peer, &node, now, 1000).unwrap();
    };
    for (round, outages) in [(0, 1), (1, 2)] {
        // An outage that lasts three polls is one WARN per side.
        for _ in 0..3 {
            step(&mut shadow, true);
        }
        assert_eq!(warns(&shadow, Warn::PeerRpc), outages, "round {round}");
        assert_eq!(warns(&shadow, Warn::NodeRpc), outages, "round {round}");
        if round == 0 {
            // A restart inside the outage keeps it one episode.
            drop(shadow);
            shadow = h.open();
            step(&mut shadow, true);
            assert_eq!(warns(&shadow, Warn::PeerRpc), 1);
            assert_eq!(warns(&shadow, Warn::NodeRpc), 1);
        }
        step(&mut shadow, false);
        assert!(!shadow.state.peer_rpc_down && !shadow.state.node_rpc_down);
    }
    assert_eq!(shadow.status().crit, 0);
    assert_eq!(shadow.state.checkpoint.as_ref().unwrap().h, 7);
}

#[test]
fn on_crit_survives_a_kill_between_the_two_saves() {
    let cs = connects(8);
    let mut h = Harness::new(&cs, 7);
    h.node.warnings = CHAINLOCK_WARNING_PREFIX.to_owned();
    let marker = h.path("hook-count");
    let hook = Some(format!("echo \"$O15_ALARM_CLASS\" >> '{}'", marker.display()));
    let runs = || fs::read_to_string(&marker).map_or(0, |s| s.lines().count());
    write(&h.path("l"), &stream(&cs, true));
    write(&h.path("r"), &stream(&cs, false));
    let mut shadow = h.open();
    shadow.set_on_crit(hook.clone(), Duration::from_secs(10));
    shadow.kill_before_drain = true;
    assert!(shadow.poll(&h.peer, &h.node, NOW, 1000).is_err());
    assert_eq!(runs(), 0);
    drop(shadow);
    let saved = super::state::State::read(&h.path("out/state.json")).unwrap().unwrap();
    assert_eq!(saved.pending_hooks.len(), 1);
    assert_eq!(saved.alarms.len(), 1);

    let mut shadow = h.open();
    shadow.set_on_crit(hook.clone(), Duration::from_secs(10));
    shadow.poll(&h.peer, &h.node, NOW + 1, 1000).unwrap();
    assert_eq!(runs(), 1);
    assert_eq!(crit_count(&shadow), 1);
    let saved = super::state::State::read(&h.path("out/state.json")).unwrap().unwrap();
    assert!(saved.pending_hooks.is_empty());
    shadow.poll(&h.peer, &h.node, NOW + 2, 1000).unwrap();
    drop(shadow);
    let mut shadow = h.open();
    shadow.set_on_crit(hook, Duration::from_secs(10));
    shadow.poll(&h.peer, &h.node, NOW + 3, 1000).unwrap();
    assert_eq!(runs(), 1);
}

#[test]
fn status_line_is_rewritten_on_alarms_and_every_minute() {
    let cs = connects(8);
    let mut h = Harness::new(&cs, 7);
    write(&h.path("l"), &stream(&cs, true));
    write(&h.path("r"), &stream(&cs, false));
    let status_path = h.path("out/STATUS.line");
    let line = || fs::read_to_string(&status_path).unwrap();
    let date = |t| super::utc_date(t);
    let mut shadow = h.open();
    shadow.poll(&h.peer, &h.node, NOW, 1000).unwrap();
    assert!(line().contains(&date(NOW)), "{}", line());
    // Ten seconds on, no alarm: unchanged.
    shadow.poll(&h.peer, &h.node, NOW + 10, 1000).unwrap();
    assert!(line().contains(&date(NOW)), "{}", line());
    // Twenty seconds on, a new alarm: rewritten at once.
    h.node.warnings = CHAINLOCK_WARNING_PREFIX.to_owned();
    shadow.poll(&h.peer, &h.node, NOW + 20, 1000).unwrap();
    assert!(line().contains(&date(NOW + 20)) && line().contains("alarms=1/0"), "{}", line());
    // No alarm: unchanged until sixty seconds after the last write, then rewritten.
    shadow.poll(&h.peer, &h.node, NOW + 79, 1000).unwrap();
    assert!(line().contains(&date(NOW + 20)), "{}", line());
    shadow.poll(&h.peer, &h.node, NOW + 80, 1000).unwrap();
    assert!(line().contains(&date(NOW + 80)), "{}", line());
    assert_eq!(shadow.status().crit, 1);
}

#[test]
fn recent_chainlock_survives_a_peer_restart() {
    let cs = connects(16);
    let mut h = Harness::new(&cs, 9);
    let mut shadow = h.run(&stream(&cs, true), &stream(&cs, false));
    let cl = Settle::Cl {
        h: 9,
        hash: cs[9].id.b.to_hex(),
    };
    assert_eq!(shadow.state.settle, Some(cl.clone()));
    // A restarted peer has no best ChainLock; the last one is 29 minutes old.
    h.peer.status.chainlock = None;
    shadow.poll(&h.peer, &h.node, NOW + 29 * 60, 1000).unwrap();
    assert_eq!(shadow.state.settle, Some(cl));
    assert!(!shadow.state.events.iter().any(|e| e["mode"] == "depth"), "{:?}", shadow.state.events);
    assert!(!shadow.state.alarms.iter().any(|a| a.class == Class::Warn(Warn::ChainLockSkew)));
    // Past 30 minutes the depth fallback applies.
    shadow.poll(&h.peer, &h.node, NOW + 30 * 60 + 1, 1000).unwrap();
    assert_eq!(
        shadow.state.settle,
        Some(Settle::Depth {
            h: 9
        })
    );
}

#[test]
fn last_read_reports_a_full_batch() {
    let cs = connects(8);
    let h = Harness::new(&cs, 7);
    write(&h.path("l"), &stream(&cs, true));
    write(&h.path("r"), &stream(&cs[..3], false));
    let mut shadow = h.open();
    shadow.poll(&h.peer, &h.node, NOW, 5).unwrap();
    assert_eq!(shadow.last_read, [5, 4]);
    shadow.poll(&h.peer, &h.node, NOW + 1, 5).unwrap();
    assert_eq!(shadow.last_read, [4, 0]);
}

fn hook_counter(h: &Harness) -> (Option<String>, impl Fn() -> usize + use<>) {
    let marker = h.path("hook-count");
    let hook = Some(format!("echo \"$O15_ALARM_CLASS\" >> '{}'", marker.display()));
    (hook, move || fs::read_to_string(&marker).map_or(0, |s| s.lines().count()))
}

fn stalls(shadow: &Shadow) -> Vec<&super::alarm::Alarm> {
    shadow.state.alarms.iter().filter(|a| a.class == Class::Crit(Crit::Stall)).collect()
}

#[test]
fn checkpoint_is_observed_after_an_append_only_restart() {
    let cs = connects(12);
    for idx in 0..2 {
        let mut h = Harness::new(&cs, 8);
        let (hook, runs) = hook_counter(&h);
        write(&h.path("l"), &stream(&cs[..9], true));
        write(&h.path("r"), &stream(&cs[..9], false));
        let mut shadow = h.open();
        shadow.set_on_crit(hook, Duration::from_secs(10));
        shadow.poll(&h.peer, &h.node, NOW, 1000).unwrap();
        assert_eq!(shadow.state.checkpoint.as_ref().unwrap().h, 8);
        // Side `idx` restarts with its log seeded: an `H`, then the tip's child.
        for (side, (name, raw)) in [("l", true), ("r", false)].into_iter().enumerate() {
            let mut next = stream(&cs[9..10], raw)[1..].to_vec();
            if side == idx {
                next.insert(0, restart());
            }
            append(&h.path(name), &next);
        }
        let cl = Some(ChainLock {
            h: 9,
            hash: cs[9].id.b.to_hex(),
        });
        h.peer.status.chainlock = cl.clone();
        h.node.chainlock = cl;
        h.node.warnings = CHAINLOCK_WARNING_PREFIX.to_owned();
        shadow.poll(&h.peer, &h.node, NOW + 1, 1000).unwrap();
        assert!(shadow.sides.iter().all(|s| s.checkpoint_seen), "side {idx}");
        assert_eq!(shadow.state.checkpoint.as_ref().unwrap().h, 9, "side {idx}");
        assert_eq!((d3s(&shadow).len(), runs()), (1, 1), "side {idx}");
        assert_eq!(shadow.status().crit, 1, "side {idx}");
    }
}

#[test]
fn replay_restart_waits_for_the_checkpoint_but_not_for_d3() {
    let cs = connects(14);
    let mut h = Harness::new(&cs, 11);
    let (hook, runs) = hook_counter(&h);
    write(&h.path("l"), &stream(&cs[..12], true));
    write(&h.path("r"), &stream(&cs[..12], false));
    let mut shadow = h.open();
    shadow.set_on_crit(hook, Duration::from_secs(10));
    shadow.poll(&h.peer, &h.node, NOW, 1000).unwrap();
    assert_eq!(shadow.state.checkpoint.as_ref().unwrap().h, 11);
    // rdashd replays from below the checkpoint and has not reached it yet.
    append(&h.path("r"), &replay_suffix(&cs, 3, 10, None, false));
    h.node.warnings = CHAINLOCK_WARNING_PREFIX.to_owned();
    shadow.poll(&h.peer, &h.node, NOW + 1, 1000).unwrap();
    assert!(!shadow.sides[1].checkpoint_seen);
    assert_eq!((d3s(&shadow).len(), runs()), (1, 1));
    // The replay's connect at the checkpoint observes it, and the comparison goes on.
    append(&h.path("l"), &stream(&cs[12..13], true)[1..]);
    append(&h.path("r"), &stream(&cs[11..13], false)[1..]);
    let cl = Some(ChainLock {
        h: 12,
        hash: cs[12].id.b.to_hex(),
    });
    h.peer.status.chainlock = cl.clone();
    h.node.chainlock = cl;
    shadow.poll(&h.peer, &h.node, NOW + 2, 1000).unwrap();
    assert!(shadow.sides[1].checkpoint_seen);
    assert_eq!(shadow.state.checkpoint.as_ref().unwrap().h, 12);
    assert_eq!(shadow.status().crit, 1);
}

#[test]
fn checkpoint_gate_closed_past_the_stall_window_is_a_stall() {
    let cs = connects(12);
    let h = Harness::new(&cs, 11);
    let mut shadow = h.run(&stream(&cs, true), &stream(&cs, false));
    append(&h.path("r"), &replay_suffix(&cs, 3, 5, None, false));
    shadow.poll(&h.peer, &h.node, NOW + 1, 1000).unwrap();
    shadow.poll(&h.peer, &h.node, NOW + 1 + 7200, 1000).unwrap();
    assert!(stalls(&shadow).is_empty());
    shadow.poll(&h.peer, &h.node, NOW + 2 + 7200, 1000).unwrap();
    let stall = stalls(&shadow);
    assert_eq!(stall.len(), 1);
    assert_eq!(stall[0].h, Some(11));
    assert!(stall[0].detail.contains("checkpoint gate"), "{}", stall[0].detail);
    assert_eq!(shadow.state.clock_start, NOW + 2 + 7200);
    // One Stall per closed episode.
    shadow.poll(&h.peer, &h.node, NOW + 9000, 1000).unwrap();
    assert_eq!(stalls(&shadow).len(), 1);
    // The replay reaches the checkpoint: the gate opens and its episode ends.
    append(&h.path("r"), &replay_suffix(&cs, 6, 11, None, false)[1..]);
    shadow.poll(&h.peer, &h.node, NOW + 9001, 1000).unwrap();
    assert!(shadow.state.gate_closed_since.is_none() && !shadow.state.gate_stalled);
    assert_eq!(stalls(&shadow).len(), 1);
}

#[test]
fn chainlock_failure_does_not_hide_the_tripwire() {
    use super::live::LiveRdashd;
    use super::live::tests::{auth, error, fake_rpc};

    let url = fake_rpc(|method| match method {
        "getblockchaininfo" => {
            (serde_json::json!({"warnings": CHAINLOCK_WARNING_PREFIX}), serde_json::Value::Null)
        }
        _ => error(-1),
    });
    let node = LiveRdashd::new(url, auth(), Duration::from_secs(5));
    let cs = connects(8);
    let h = Harness::new(&cs, 7);
    write(&h.path("l"), &stream(&cs, true));
    write(&h.path("r"), &stream(&cs, false));
    let mut shadow = h.open();
    for now in [NOW, NOW + 1] {
        shadow.poll(&h.peer, &node, now, 1000).unwrap();
        let classes = shadow.state.alarms.iter().map(|a| a.class).collect::<Vec<_>>();
        assert_eq!(classes, [Class::Crit(Crit::D3), Class::Warn(Warn::NodeRpc)]);
    }
    assert!(shadow.state.d3_active && shadow.state.node_rpc_down);
}

#[test]
fn hook_that_cannot_start_is_retried_until_it_runs() {
    let cs = connects(8);
    let mut h = Harness::new(&cs, 7);
    h.node.warnings = CHAINLOCK_WARNING_PREFIX.to_owned();
    let (hook, runs) = hook_counter(&h);
    write(&h.path("l"), &stream(&cs, true));
    write(&h.path("r"), &stream(&cs, false));
    let hook_warns = |shadow: &Shadow| {
        shadow.state.alarms.iter().filter(|a| a.class == Class::Warn(Warn::Hook)).count()
    };
    let mut shadow = h.open();
    shadow.set_on_crit(hook.clone(), Duration::from_secs(10));
    // The hook's shell is missing, so the hook cannot be started.
    shadow.shell = PathBuf::from("/nonexistent/o15-shell");
    for now in [NOW, NOW + 1] {
        shadow.poll(&h.peer, &h.node, now, 1000).unwrap();
        assert_eq!((runs(), hook_warns(&shadow)), (0, 1));
        let saved = super::state::State::read(&h.path("out/state.json")).unwrap().unwrap();
        assert_eq!(saved.pending_hooks.len(), 1);
        assert!(saved.pending_hooks[0].failed);
    }
    // Fixed after a restart: the next poll runs it exactly once.
    drop(shadow);
    let mut shadow = h.open();
    shadow.set_on_crit(hook, Duration::from_secs(10));
    for now in [NOW + 2, NOW + 3] {
        shadow.poll(&h.peer, &h.node, now, 1000).unwrap();
        assert_eq!((runs(), hook_warns(&shadow)), (1, 1));
        assert!(shadow.state.pending_hooks.is_empty());
    }
    assert_eq!(crit_count(&shadow), 1);
}

#[test]
fn hooks_stay_pending_without_on_crit() {
    let cs = connects(8);
    let mut h = Harness::new(&cs, 7);
    h.node.warnings = CHAINLOCK_WARNING_PREFIX.to_owned();
    let (hook, runs) = hook_counter(&h);
    write(&h.path("l"), &stream(&cs, true));
    write(&h.path("r"), &stream(&cs, false));
    let mut shadow = h.open();
    shadow.poll(&h.peer, &h.node, NOW, 1000).unwrap();
    shadow.poll(&h.peer, &h.node, NOW + 1, 1000).unwrap();
    drop(shadow);
    let saved = super::state::State::read(&h.path("out/state.json")).unwrap().unwrap();
    assert_eq!(saved.pending_hooks.len(), 1);
    let mut shadow = h.open();
    shadow.set_on_crit(hook, Duration::from_secs(10));
    shadow.poll(&h.peer, &h.node, NOW + 2, 1000).unwrap();
    assert_eq!(runs(), 1);
    assert!(shadow.state.pending_hooks.is_empty());
}

#[test]
fn hook_environment_escapes_nul() {
    let dir = TempDir::new().unwrap();
    let seen = dir.path().join("seen");
    let alarm = super::alarm::Alarm {
        class: Class::Crit(Crit::D3),
        h: Some(1),
        b: Some("a\0b".into()),
        detail: "x\0y".into(),
    };
    let command =
        format!("printf '%s|%s' \"$O15_ALARM_HASH\" \"$O15_ALARM_DETAIL\" > '{}'", seen.display());
    run_on_crit(sh(), &command, &alarm, Duration::from_secs(10)).unwrap();
    assert_eq!(fs::read_to_string(&seen).unwrap(), r"a\0b|x\0y");
}

#[test]
fn pending_hooks_run_before_a_peer_without_coinstatsindex_stops_the_poll() {
    use super::MissingCoinstatsIndex;
    use super::live::LivePeer;
    use super::live::tests::{auth, fake_rpc, no_coinstatsindex};

    let cs = connects(8);
    let mut h = Harness::new(&cs, 7);
    h.node.warnings = CHAINLOCK_WARNING_PREFIX.to_owned();
    let (hook, runs) = hook_counter(&h);
    write(&h.path("l"), &stream(&cs, true));
    write(&h.path("r"), &stream(&cs, false));
    let mut shadow = h.open();
    shadow.set_on_crit(hook.clone(), Duration::from_secs(10));
    shadow.kill_before_drain = true;
    assert!(shadow.poll(&h.peer, &h.node, NOW, 1000).is_err());
    drop(shadow);
    assert_eq!(runs(), 0);

    let mut shadow = h.open();
    shadow.set_on_crit(hook, Duration::from_secs(10));
    let peer = LivePeer::new(fake_rpc(no_coinstatsindex), auth(), Duration::from_secs(5));
    let e = shadow.poll(&peer, &h.node, NOW + 1, 1000).unwrap_err();
    assert!(e.is::<MissingCoinstatsIndex>(), "{e:#}");
    assert!(format!("{e:#}").contains("-coinstatsindex"), "{e:#}");
    assert_eq!(runs(), 1);
    let saved = super::state::State::read(&h.path("out/state.json")).unwrap().unwrap();
    assert!(saved.pending_hooks.is_empty());
}

#[test]
fn d3_is_saved_and_hooked_before_a_peer_without_coinstatsindex_stops_the_poll() {
    use super::MissingCoinstatsIndex;
    use super::live::LivePeer;
    use super::live::tests::{auth, fake_rpc, no_coinstatsindex};

    let cs = connects(8);
    let mut h = Harness::new(&cs, 7);
    h.node.warnings = CHAINLOCK_WARNING_PREFIX.to_owned();
    let (hook, runs) = hook_counter(&h);
    write(&h.path("l"), &stream(&cs, true));
    write(&h.path("r"), &stream(&cs, false));
    let mut shadow = h.open();
    shadow.set_on_crit(hook, Duration::from_secs(10));
    let peer = LivePeer::new(fake_rpc(no_coinstatsindex), auth(), Duration::from_secs(5));
    let e = shadow.poll(&peer, &h.node, NOW, 1000).unwrap_err();
    assert!(e.is::<MissingCoinstatsIndex>(), "{e:#}");
    assert_eq!(runs(), 1);
    drop(shadow);
    let saved = super::state::State::read(&h.path("out/state.json")).unwrap().unwrap();
    assert!(saved.alarms.iter().any(|a| a.class == Class::Crit(Crit::D3)));
    assert!(saved.pending_hooks.is_empty());
}

#[test]
fn loading_peer_does_not_hold_back_d3() {
    use super::live::LivePeer;
    use super::live::tests::{auth, error, fake_rpc};

    let peer = LivePeer::new(fake_rpc(|_| error(-28)), auth(), Duration::from_secs(5));
    let cs = connects(8);
    let mut h = Harness::new(&cs, 7);
    h.node.warnings = CHAINLOCK_WARNING_PREFIX.to_owned();
    write(&h.path("l"), &stream(&cs, true));
    write(&h.path("r"), &stream(&cs, false));
    let mut shadow = h.open();
    for now in [NOW, NOW + 1000, NOW + 2000] {
        shadow.poll(&peer, &h.node, now, 1000).unwrap();
    }
    let classes = shadow.state.alarms.iter().map(|a| a.class).collect::<Vec<_>>();
    assert_eq!(classes, [Class::Crit(Crit::D3), Class::Warn(Warn::PeerRpc)]);
    let journal = fs::read_to_string(h.path("out/alarms.jsonl")).unwrap();
    assert_eq!(journal.lines().count(), 2, "{journal}");
    assert!(journal.lines().next().unwrap().contains("D3"), "{journal}");
}

#[test]
fn enrichment_failures_are_one_peer_rpc_warn_per_outage() {
    struct EnrichFails<'a> {
        fail: Cell<bool>,
        peer: &'a RecordedPeer,
    }
    impl PeerRpc for EnrichFails<'_> {
        fn status(&self) -> Result<PeerStatus> {
            self.peer.status()
        }
        fn node(&self) -> Result<&dyn NodeRpc> {
            Err(anyhow!("offline"))
        }
        fn enrich(&self, record: &Outcome) -> Result<Enrichment> {
            if self.fail.get() {
                return Err(anyhow!("getblock timed out"));
            }
            self.peer.enrich(record)
        }
    }
    let peer_rpc = |shadow: &Shadow| {
        shadow.state.alarms.iter().filter(|a| a.class == Class::Warn(Warn::PeerRpc)).count()
    };
    let cs = connects(10);
    let mut h = Harness::new(&cs, 7);
    write(&h.path("l"), &stream(&cs[..8], true));
    write(&h.path("r"), &stream(&cs[..8], false));
    let mut shadow = h.open();
    let mut now = NOW;
    let mut step = |shadow: &mut Shadow, h: &Harness, fail: bool| {
        let peer = EnrichFails {
            fail: Cell::new(fail),
            peer: &h.peer,
        };
        // Past the backoff, so the peer is asked every time, and within the ChainLock's age.
        now += 60;
        shadow.poll(&peer, &h.node, now, 1000).unwrap();
    };
    for _ in 0..3 {
        step(&mut shadow, &h, true);
    }
    assert_eq!(peer_rpc(&shadow), 1);
    assert!(shadow.state.peer_rpc_down);
    step(&mut shadow, &h, false);
    assert!(!shadow.state.peer_rpc_down);
    assert_eq!(shadow.state.checkpoint.as_ref().unwrap().h, 7);
    // The next failure is a new outage.
    append(&h.path("l"), &stream(&cs[8..], true)[1..]);
    append(&h.path("r"), &stream(&cs[8..], false)[1..]);
    let cl = Some(ChainLock {
        h: 9,
        hash: cs[9].id.b.to_hex(),
    });
    h.peer.status.chainlock = cl.clone();
    h.node.chainlock = cl;
    step(&mut shadow, &h, true);
    assert_eq!(peer_rpc(&shadow), 2);
    assert_eq!(shadow.status().crit, 0);
}

#[test]
fn checkpoint_gate_held_by_d1_is_not_a_stall() {
    let cs = connects(12);
    let h = Harness::new(&cs, 11);
    let mut shadow = h.run(&stream(&cs, true), &stream(&cs, false));
    append(&h.path("r"), &replay_suffix(&cs, 3, 10, None, false));
    shadow.poll(&h.peer, &h.node, NOW + 1, 1000).unwrap();
    // The replay's block at the checkpoint height is another block: D1, both sides observed.
    let mut bad = cs[11].clone();
    bad.id.b = hash(999);
    bad.s_mut().set("prev", Value::string(cs[10].id.b.to_hex())).unwrap();
    append(&h.path("r"), &[Record::Outcome(bad)]);
    shadow.poll(&h.peer, &h.node, NOW + 2, 1000).unwrap();
    assert!(shadow.sides.iter().all(|s| s.checkpoint_seen));
    let d1 = |shadow: &Shadow| {
        shadow
            .state
            .alarms
            .iter()
            .filter(|a| a.class == Class::Crit(Crit::D1) && a.detail.contains("left the active"))
            .count()
    };
    assert_eq!((d1(&shadow), crit_count(&shadow)), (1, 2));
    for now in [NOW + 3 + 7200, NOW + 4 + 7200, NOW + 20_000] {
        shadow.poll(&h.peer, &h.node, now, 1000).unwrap();
    }
    assert!(stalls(&shadow).is_empty(), "{:?}", stalls(&shadow));
    assert_eq!((d1(&shadow), crit_count(&shadow)), (1, 2));
}

#[test]
fn hole_at_the_checkpoint_after_a_restart_is_d1() {
    let cs = connects(12);
    let h = Harness::new(&cs, 8);
    write(&h.path("l"), &stream(&cs[..9], true));
    write(&h.path("r"), &stream(&cs[..9], false));
    let mut shadow = h.open();
    shadow.poll(&h.peer, &h.node, NOW, 1000).unwrap();
    assert_eq!(shadow.state.checkpoint.as_ref().unwrap().h, 8);
    // rdashd restarts, its replay disconnects below the checkpoint, and its first connect
    // jumps above it, leaving a hole at the checkpoint's height.
    let mut suffix = vec![restart()];
    for o in cs[7..9].iter().rev() {
        let mut d = Outcome::disconnect(Ident {
            org: Origin::Replay,
            ..o.id.clone()
        });
        d.s_mut().set("res", Value::string("ok")).unwrap();
        suffix.push(Record::Outcome(d));
    }
    suffix.push(Record::Outcome(cs[10].clone()));
    append(&h.path("r"), &suffix);
    shadow.poll(&h.peer, &h.node, NOW + 1, 1000).unwrap();
    assert!(shadow.sides[1].checkpoint_seen);
    assert!(shadow.sides[1].slot(8).is_none());
    let crits = shadow
        .state
        .alarms
        .iter()
        .filter(|a| matches!(a.class, Class::Crit(_)))
        .map(|a| (a.class, a.h, a.detail.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(crits, [(Class::Crit(Crit::D1), Some(8), "checkpoint left the active chain")]);
}

#[test]
fn skew_warns_wait_for_an_open_checkpoint_gate() {
    let skews = |shadow: &Shadow| {
        shadow
            .state
            .alarms
            .iter()
            .filter(|a| matches!(a.class, Class::Warn(Warn::ChainLockSkew | Warn::TipSkew)))
            .map(|a| a.class)
            .collect::<Vec<_>>()
    };
    let cs = connects(12);
    let mut h = Harness::new(&cs, 11);
    let mut shadow = h.run(&stream(&cs, true), &stream(&cs, false));
    // rdashd replays and has no ChainLock yet; the peer has one.
    h.node.chainlock = None;
    append(&h.path("r"), &replay_suffix(&cs, 3, 5, None, false));
    shadow.poll(&h.peer, &h.node, NOW + 1, 1000).unwrap();
    assert!(!shadow.sides[1].checkpoint_seen);
    assert_eq!(skews(&shadow), []);
    // Once the gate is open again the skew is reported.
    append(&h.path("r"), &replay_suffix(&cs, 6, 11, None, false)[1..]);
    shadow.poll(&h.peer, &h.node, NOW + 2, 1000).unwrap();
    assert!(shadow.sides[1].checkpoint_seen);
    assert_eq!(skews(&shadow), [Class::Warn(Warn::ChainLockSkew)]);
}
