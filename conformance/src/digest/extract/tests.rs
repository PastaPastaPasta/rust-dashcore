use std::cell::{Cell, RefCell};

use dash_statedigest::{Lines, Record};
use dashcore::hashes::{Hash, sha256d};

use super::checks::{
    COUNTER_ADDS, MnlAgreement, UndoIndex, mnl_agreement, ranges_hash, since_next,
};
use super::*;
use dash_statedigest::Ident;

/// The hash of block `h` in the fake chain.
fn bh(h: i64) -> Hash256 {
    let mut b = [0x11u8; 32];
    b[0] = u8::try_from(h).unwrap();
    Hash256::from_display_bytes(b)
}

fn fill(n: u8) -> Hash256 {
    Hash256::from_display_bytes([n; 32])
}

fn opts(dense: bool) -> Options {
    Options {
        dense,
        boundaries: BTreeSet::new(),
        wait: Duration::ZERO,
        poll: Duration::ZERO,
        blocks_dir: None,
        other_params_runs: BTreeSet::new(),
    }
}

/// What a fake block holds. A block with `mnl` is in the DIP0003 era, one with `cp` in the V20
/// era: the fake's buried heights are the first such blocks.
#[derive(Clone, Default)]
struct Spec {
    mnl: bool,
    cp: bool,
    ntx: i64,
    qc: Vec<(i64, Hash256)>,
    bur: i64,
    time: i64,
    /// `type 9` transaction indexes `getblock <hash> 2` reports.
    unlocks: Vec<u64>,
    /// Every unlock index mined up to this block: the credit pool's index set.
    pool: BTreeSet<u64>,
    /// MNHF signal bits `getblock <hash> 2` reports.
    signals: Vec<i64>,
    /// `s.ehf`'s rows: every signal mined up to this block, as `(bit, height)`.
    ehf: Vec<(i64, i64)>,
}

fn spec(mnl: bool, cp: bool) -> Spec {
    Spec {
        mnl,
        cp,
        ntx: 1,
        bur: 4095,
        time: 1000,
        ..Spec::default()
    }
}

/// Mines `qc` in block `h`, one transaction each.
fn mine_qc(specs: &mut [Spec], h: usize, qc: Vec<(i64, Hash256)>) {
    specs[h].ntx += qc.len() as i64;
    specs[h].qc = qc;
}

/// Mines asset unlocks in block `h`, one transaction each.
fn mine_unlocks(specs: &mut [Spec], h: usize, unlocks: Vec<u64>) {
    specs[h].ntx += unlocks.len() as i64;
    for sp in &mut specs[h..] {
        sp.pool.extend(&unlocks);
    }
    specs[h].unlocks = unlocks;
}

/// Mines an MNHF signal for `bit` in block `h`; it stays in `ehf` from there on.
fn mine_signal(specs: &mut [Spec], h: usize, bit: i64) {
    specs[h].ntx += 1;
    specs[h].signals.push(bit);
    for sp in &mut specs[h..] {
        sp.ehf.push((bit, h as i64));
    }
}

/// The commitment bytes of a fake `qc` transaction, after the payload's version and height.
fn commitment(t: i64, q: &Hash256) -> Vec<u8> {
    let mut b = vec![u8::try_from(t).unwrap()];
    b.extend_from_slice(&q.to_wire_bytes());
    b
}

fn qc_hash(t: i64, q: &Hash256) -> Hash256 {
    Hash256::from_wire_bytes(sha256d::Hash::hash(&commitment(t, q)).to_byte_array())
}

/// `s.dep` in Core's `VersionBitsDeploymentInfo` order, which is not the RPC's (the fake lists
/// `evo_shares` first, as `getdeploymentinfo` does): `testdummy` disabled (the fake does not list
/// it, as for feature_mnehf's first starts), `v24` in `state` since `since`.
fn dep_value(state: &str, since: i64) -> Value {
    let entry = |name: &str, state: &str, since: i64| {
        Value::Arr(vec![Value::string(name), Value::string(state), Value::Int(since)])
    };
    Value::Arr(vec![
        entry("testdummy", "failed", 0),
        entry("v24", state, since),
        entry("evo_shares", "defined", 0),
    ])
}

fn c_record(h: i64, seq: i64, org: Origin, sp: &Spec) -> Outcome {
    let mut c = Outcome::connect(Ident {
        h,
        b: bh(h),
        org,
    });
    let hash = |n: u8| Value::hash(&fill(n));
    let or_null = |on: bool, v: Value| {
        if on {
            v
        } else {
            Value::Null
        }
    };
    let mut s = c.s_mut();
    let items = [
        (
            "prev",
            if h > 0 {
                Value::hash(&bh(h - 1))
            } else {
                Value::Null
            },
        ),
        ("ver", Value::Int(1)),
        ("mrkl", hash(2)),
        ("time", Value::Int(sp.time)),
        ("bits", Value::Int(5)),
        ("nonce", Value::Int(6)),
        ("work", hash(3)),
        ("ntx", Value::Int(sp.ntx)),
        ("undo", or_null(h > 0, hash(8))),
        ("mnl", or_null(sp.mnl, hash(4))),
        (
            "qc",
            or_null(
                sp.mnl,
                Value::Arr(
                    sp.qc
                        .iter()
                        .map(|(t, q)| {
                            Value::Arr(vec![
                                Value::Int(*t),
                                Value::hash(q),
                                Value::hash(&qc_hash(*t, q)),
                            ])
                        })
                        .collect(),
                ),
            ),
        ),
        (
            "cp",
            or_null(
                sp.cp,
                Value::Arr(vec![
                    Value::Int(1),
                    Value::Int(2),
                    Value::Int(3),
                    Value::hash(&ranges_hash(&sp.pool)),
                ]),
            ),
        ),
        (
            "ehf",
            or_null(
                sp.cp,
                Value::Arr(
                    sp.ehf
                        .iter()
                        .map(|(bit, at)| Value::Arr(vec![Value::Int(*bit), Value::Int(*at)]))
                        .collect(),
                ),
            ),
        ),
        // `bls` is "V19 is not active after the block"; V19 is buried bit 11.
        ("bls", Value::Bool(sp.bur & (1 << 11) == 0)),
        ("bur", Value::Int(sp.bur)),
        ("dep", dep_value("defined", 0)),
    ];
    for (k, v) in items {
        s.set(k, v).unwrap();
    }
    c.o_mut().set_effects(&[]).unwrap();
    c.x_mut().set("seq", Value::Int(seq)).unwrap();
    c
}

fn header(chain: Chain) -> Record {
    Record::Header(Header {
        file: FileKind::Digest,
        src: Writer::Dashd,
        ver: b"v24.0.0-test".to_vec(),
        chain,
        run: b"1-2".to_vec(),
    })
}

fn digest_of(records: Vec<Record>) -> Vec<u8> {
    records.iter().flat_map(Record::to_line).collect()
}

/// A digest with one tip `C` per block of `specs`.
fn simple_digest(specs: &[Spec]) -> Vec<u8> {
    digest_with(specs, |_, _| {})
}

/// [`simple_digest`] with `edit` applied to each block's `C` record.
fn digest_with(specs: &[Spec], edit: impl Fn(i64, &mut Outcome)) -> Vec<u8> {
    let mut recs = vec![header(Chain::Regtest)];
    for (h, sp) in specs.iter().enumerate() {
        let h = h as i64;
        let mut c = c_record(h, h + 1, Origin::Tip, sp);
        edit(h, &mut c);
        recs.push(Record::Outcome(c));
    }
    digest_of(recs)
}

/// `Buried` order (spec §5.2), as in `checks`.
const BURIED: [&str; 15] = [
    "bip34",
    "bip66",
    "bip65",
    "bip147",
    "csv",
    "dip0001",
    "dip0003",
    "dip0008",
    "dip0020",
    "dip0024",
    "realloc",
    "v19",
    "v20",
    "mn_rr",
    "withdrawals",
];

/// Far past any fake chain: an era that never starts.
const NEVER: i64 = 1 << 40;

struct Fake {
    specs: Vec<Spec>,
    /// The buried DIP0003 and V20 heights, fixed when the fake is made.
    dip3: i64,
    v20: i64,
    log: RefCell<Vec<(String, serde_json::Value)>>,
    /// Calls to `getbestblockhash` so far.
    best_calls: Cell<u32>,
    /// From the second `getbestblockhash` on, report this block instead.
    moved_tip: Option<Hash256>,
    synced: bool,
    /// Fail this command (`method` or `method sub`) with an RPC error.
    fail: Option<&'static str>,
    /// The error code of the `fail` reply.
    fail_code: i64,
    /// `total_amount` text.
    amount: &'static str,
    /// `quorum info` reports every quorum mined in block 0.
    mined_elsewhere: bool,
    /// `getcanonicalmnlist`'s hash.
    canonical: Hash256,
    /// `getcanonicalmnlist`'s `hex`; "00" when unset.
    canonical_hex: Option<String>,
    /// The `v24` deployment's `status_next`: other `-vbparams` give other states.
    v24_next: &'static str,
    /// `(h, since)`: the reply for block `h` gives `v24` that `since`; every other one gives 0.
    v24_since: Option<(i64, i64)>,
}

impl Fake {
    fn new(specs: Vec<Spec>) -> Fake {
        let first = |f: fn(&Spec) -> bool| specs.iter().position(f).map_or(NEVER, |h| h as i64);
        Fake {
            dip3: first(|s| s.mnl),
            v20: first(|s| s.cp),
            specs,
            log: RefCell::default(),
            best_calls: Cell::new(0),
            moved_tip: None,
            synced: true,
            fail: None,
            fail_code: -8,
            amount: "25000.00000000",
            mined_elsewhere: false,
            canonical: fill(4),
            canonical_hex: None,
            v24_next: "defined",
            v24_since: None,
        }
    }

    fn tip(&self) -> i64 {
        self.specs.len() as i64 - 1
    }

    fn spec_of(&self, hash: &serde_json::Value) -> (i64, &Spec) {
        let h = h_of(&bh_inv(hash.as_str().unwrap()));
        (h, &self.specs[usize::try_from(h).unwrap()])
    }

    /// `getdeploymentinfo` after block `h`.
    fn deployments(&self, h: i64, sp: &Spec) -> String {
        let mut entries: Vec<String> = BURIED
            .iter()
            .enumerate()
            .map(|(bit, name)| {
                let height = match *name {
                    "dip0003" => self.dip3,
                    "v20" => self.v20,
                    _ => 0,
                };
                let active = sp.bur & (1 << bit) != 0;
                format!(r#""{name}":{{"type":"buried","active":{active},"height":{height}}}"#)
            })
            .collect();
        entries.push(
            r#""evo_shares":{"type":"bip9","bip9":{"status":"defined","start_time":0,"since":0,"status_next":"defined"},"active":false}"#.into(),
        );
        // The block keeps its state (status = status_next).
        let since = self.v24_since.filter(|(at, _)| *at == h).map_or(0, |(_, since)| since);
        entries.push(format!(
            r#""v24":{{"type":"bip9","bip9":{{"status":"{0}","start_time":0,"since":{since},"status_next":"{0}"}},"active":false}}"#,
            self.v24_next
        ));
        format!(r#"{{"hash":"x","deployments":{{{}}}}}"#, entries.join(","))
    }

    /// `getblock <hash> 0`: the header and the transaction count.
    fn raw_block(&self, h: i64, sp: &Spec) -> String {
        let mut b = 1i32.to_le_bytes().to_vec();
        b.extend_from_slice(&if h == 0 {
            [0; 32]
        } else {
            bh(h - 1).to_wire_bytes()
        });
        b.extend_from_slice(&fill(2).to_wire_bytes());
        for v in [u32::try_from(sp.time).unwrap(), 5, 6] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        match u16::try_from(sp.ntx).unwrap() {
            n @ 0..=0xfc => b.push(n as u8),
            n => {
                b.push(0xfd);
                b.extend_from_slice(&n.to_le_bytes());
            }
        }
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// `getblock <hash> 2`: the coinbase, a null commitment in DIP0003 blocks, then the block's
    /// commitments and asset unlocks.
    fn block(&self, sp: &Spec) -> String {
        let qc_tx = |t: i64, q: &str, n: u32, key: &str, payload: &str| {
            format!(
                r#"{{"type":6,"extraPayload":"{payload}","qcTx":{{"version":1,"height":9,"commitment":{{"version":1,"llmqType":{t},"quorumHash":"{q}","signersCount":{n},"validMembersCount":{n},"quorumVvecHash":"{key}","quorumPublicKey":"{key}","quorumSig":"{key}","membersSig":"{key}"}}}}}}"#
            )
        };
        let mut txs = vec![r#"{"type":5}"#.to_owned()];
        if sp.mnl {
            txs.push(qc_tx(100, &"00".repeat(32), 0, &"00".repeat(48), "0100090000000000"));
        }
        for (t, q) in &sp.qc {
            let payload: String = [1, 0, 9, 0, 0, 0]
                .iter()
                .chain(&commitment(*t, q))
                .map(|x| format!("{x:02x}"))
                .collect();
            txs.push(qc_tx(*t, &q.to_hex(), 3, &"ab".repeat(48), &payload));
        }
        txs.extend(
            sp.unlocks.iter().map(|i| format!(r#"{{"type":9,"assetUnlockTx":{{"index":{i}}}}}"#)),
        );
        txs.extend(sp.signals.iter().map(|bit| {
            format!(r#"{{"type":7,"mnhfTx":{{"version":1,"signal":{{"versionBit":{bit}}}}}}}"#)
        }));
        format!(r#"{{"tx":[{}]}}"#, txs.join(","))
    }

    fn result(&self, method: &str, p: &[serde_json::Value]) -> String {
        let h = p.first().and_then(serde_json::Value::as_i64).unwrap_or(-1);
        match method {
            "getindexinfo" => format!(
                r#"{{"coinstatsindex":{{"synced":{},"best_block_height":{}}}}}"#,
                self.synced,
                self.tip()
            ),
            "getblockchaininfo" => format!(
                r#"{{"chain":"regtest","blocks":{},"bestblockhash":"{}"}}"#,
                self.tip(),
                bh(self.tip())
            ),
            "getbestblockhash" => {
                self.best_calls.set(self.best_calls.get() + 1);
                match self.moved_tip {
                    Some(m) if self.best_calls.get() > 1 => format!(r#""{m}""#),
                    _ => format!(r#""{}""#, bh(self.tip())),
                }
            }
            "getnetworkinfo" => r#"{"buildversion":"v24.0.0-test","version":240000}"#.into(),
            "getblockhash" => format!(r#""{}""#, bh(p[0].as_i64().unwrap())),
            "gettxoutsetinfo" => {
                let h = p[1].as_i64().unwrap();
                format!(
                    r#"{{"height":{h},"bestblock":"{}","txouts":{},"bogosize":9,"muhash":"{}","total_amount":{},"total_unspendable_amount":50.00000000,"block_info":{{"prevout_spent":0.00000001,"coinbase":500.00000000,"new_outputs_ex_coinbase":1.50000000,"unspendable":0.00000000,"unspendables":{{"genesis_block":50.00000000,"scripts":0.00000000,"unclaimed_rewards":0.00000000}}}}}}"#,
                    bh(h),
                    h + 10,
                    "ab".repeat(32),
                    self.amount
                )
            }
            "getblockstats" => format!(
                r#"{{"subsidy":50000000000,"totalfee":{h},"txs":1,"ins":0,"outs":1,"utxo_increase":1,"utxo_size_inc":75}}"#
            ),
            "getblock" => {
                let (h, sp) = self.spec_of(&p[0]);
                match p[1].as_i64() {
                    Some(0) => format!(r#""{}""#, self.raw_block(h, sp)),
                    _ => self.block(sp),
                }
            }
            "getblockheader" => {
                let (_, sp) = self.spec_of(&p[0]);
                format!(r#"{{"nTx":{},"chainwork":"{}"}}"#, sp.ntx, fill(3))
            }
            "getdeploymentinfo" => match p.first() {
                Some(hash) => {
                    let (h, sp) = self.spec_of(hash);
                    self.deployments(h, sp)
                }
                None => self.deployments(self.tip(), &self.specs[self.specs.len() - 1]),
            },
            "getcreditpoolinfo" => {
                r#"{"locked":0.00000001,"currentLimit":0.00000002,"balance":0.00000001,"currentlimit":0.00000002,"window":{"unlocked":0.00000003,"indexes":[]}}"#.into()
            }
            "getcanonicalmnlist" => format!(
                r#"{{"hash":"{}","hex":"{}"}}"#,
                self.canonical,
                self.canonical_hex.as_deref().unwrap_or("00")
            ),
            // Sparse RPCs: keys deliberately unsorted, numbers with text that must survive.
            _ => {
                let sub = p.first().and_then(serde_json::Value::as_str).unwrap_or("");
                match (method, sub) {
                    ("quorum", "info") => {
                        let rotated = self.fail != Some("rotated-off");
                        let extra = if rotated {
                            r#","previousConsecutiveDKGFailures":0"#
                        } else {
                            ""
                        };
                        let quorum = (p[1].as_i64().unwrap(), bh_inv(p[2].as_str().unwrap()));
                        let mined = self.specs.iter().position(|s| s.qc.contains(&quorum)).unwrap();
                        let mined = if self.mined_elsewhere { 0 } else { mined as i64 };
                        format!(
                            r#"{{"quorumIndex":2,"height":9{extra},"type":"x","minedBlock":"{}"}}"#,
                            bh(mined)
                        )
                    }
                    ("protx", "list") => r#"[{"metaInfo":{"lastDSQ":0,"lastOutboundAttempt":7,"lastOutboundAttemptElapsed":1791777178,"lastOutboundSuccessElapsed":5},"x":{"metaInfo":{"keepElapsedNot":1}}}]"#.to_owned(),
                    _ => r#"{"z":[1.50,{"b":2,"a":1e-05}],"a":"é"}"#.to_owned(),
                }
            }
        }
    }
}

/// The height of a fake block hash.
fn h_of(h: &Hash256) -> i64 {
    i64::from(h.as_display_bytes()[0])
}

fn bh_inv(text: &str) -> Hash256 {
    Hash256::from_hex(text.as_bytes()).unwrap()
}

impl NodeRpc for Fake {
    fn call(&self, method: &str, params: serde_json::Value) -> Result<Reply> {
        self.log.borrow_mut().push((method.to_owned(), params.clone()));
        let p = params.as_array().unwrap();
        let key = match p.first().and_then(serde_json::Value::as_str) {
            Some(sub) if matches!(method, "protx" | "quorum" | "masternode") => {
                format!("{method} {sub}")
            }
            _ => method.to_owned(),
        };
        if self.fail == Some(key.as_str()) {
            return Ok(Err(RpcError {
                code: self.fail_code,
                message: "boom".into(),
            }));
        }
        let reply = format!(r#"{{"result":{},"error":null,"id":0}}"#, self.result(method, p));
        parse_reply(reply.as_bytes())
    }
}

fn run(fake: &Fake, digest: &[u8], dense: bool) -> Result<(Vec<u8>, Summary)> {
    let mut out = Vec::new();
    let summary = extract(fake, digest, &opts(dense), &mut out)?;
    Ok((out, summary))
}

fn records(out: &[u8]) -> Vec<Record> {
    Lines::new(out).map(|(n, r)| r.unwrap_or_else(|e| panic!("line {n}: {e}"))).collect()
}

fn sparse_of(recs: &[Record]) -> Vec<&Sparse> {
    recs.iter()
        .filter_map(|r| match r {
            Record::Sparse(s) => Some(s),
            _ => None,
        })
        .collect()
}

fn names(recs: &[Record]) -> Vec<(i64, String)> {
    sparse_of(recs).iter().map(|s| (s.h, String::from_utf8(s.rpc.clone()).unwrap())).collect()
}

fn err_text(r: Result<(Vec<u8>, Summary)>) -> String {
    format!("{:#}", r.expect_err("expected an error"))
}

#[test]
fn merges_the_last_tip_connect_and_appends_golden_keys() {
    let specs = vec![spec(false, false), spec(true, false), spec(true, true)];
    let mut recs = vec![header(Chain::Regtest)];
    recs.push(Record::Outcome(c_record(0, 1, Origin::Tip, &specs[0])));
    // A proposal at the same hash never merges, a stale branch block is ignored, and the later
    // of two agreeing tip connects supplies `o` and `x`.
    recs.push(Record::Outcome(c_record(1, 2, Origin::Proposal, &specs[1])));
    recs.push(Record::Outcome(c_record(1, 3, Origin::Tip, &specs[1])));
    let mut stale = c_record(1, 4, Origin::Tip, &specs[1]);
    stale.id.b = Hash256::from_display_bytes([0xee; 32]);
    recs.push(Record::Outcome(stale));
    recs.push(Record::Outcome(c_record(1, 9, Origin::Tip, &specs[1])));
    recs.push(Record::Outcome(c_record(2, 10, Origin::Tip, &specs[2])));
    let digest = digest_of(recs);

    let (out, summary) = run(&Fake::new(specs), &digest, true).unwrap();
    let recs = records(&out);
    assert_eq!(summary.blocks, 3);
    assert_eq!(summary.tip, 2);
    assert_eq!(summary.run, bh(2).to_hex());

    let Record::Header(h) = &recs[0] else {
        panic!("first record is not H")
    };
    assert_eq!((h.file, h.src, h.chain), (FileKind::Golden, Writer::Extract, Chain::Regtest));
    assert_eq!(h.ver, b"v24.0.0-test");
    assert_eq!(h.run, bh(2).to_hex().into_bytes());

    let cs: Vec<&Outcome> = recs
        .iter()
        .filter_map(|r| match r {
            Record::Outcome(o) => Some(o),
            _ => None,
        })
        .collect();
    assert_eq!(cs.len(), 3);
    assert_eq!(cs.iter().map(|c| c.id.h).collect::<Vec<_>>(), [0, 1, 2]);
    // `x` of the last tip connect.
    assert_eq!(cs[1].x().get("seq"), Some(&Value::Int(9)));

    // Golden keys follow the patch's keys, in table order.
    let keys: Vec<&str> = cs[1].s().iter().map(|(f, _)| f.key).collect();
    assert_eq!(
        keys[keys.len() - 6..],
        ["muhash", "txouts", "amount", "unspendable", "bi", "stats"]
    );
    assert_eq!(keys[0], "prev");
    let s = cs[1].s();
    assert_eq!(s.get("muhash"), Some(&Value::string("ab".repeat(32))));
    assert_eq!(s.get("txouts"), Some(&Value::Int(11)));
    assert_eq!(s.get("amount"), Some(&Value::Int(2_500_000_000_000)));
    assert_eq!(s.get("unspendable"), Some(&Value::Int(5_000_000_000)));
    assert_eq!(s.get("bi"), Some(&ints(&[1, 50_000_000_000, 150_000_000, 0, 5_000_000_000, 0, 0])));
    assert_eq!(
        s.get("stats"),
        Some(&ints(&[50_000_000_000, 1, 1, 0, 1, 1, 75])),
        "totalfee is the fake's height"
    );
    // Genesis: the RPC keys, but stats is null (§5.3).
    assert_eq!(cs[0].s().get("stats"), Some(&Value::Null));
    assert_eq!(cs[0].s().get("txouts"), Some(&Value::Int(10)));
}

#[test]
fn disagreeing_tip_connects_fail() {
    let specs = vec![spec(false, false), spec(false, false)];
    let mut other = specs[1].clone();
    other.time = 1001;
    let digest = digest_of(vec![
        header(Chain::Regtest),
        Record::Outcome(c_record(0, 1, Origin::Tip, &specs[0])),
        Record::Outcome(c_record(1, 2, Origin::Tip, &specs[1])),
        Record::Outcome(c_record(1, 3, Origin::Tip, &other)),
    ]);
    let msg = err_text(run(&Fake::new(specs), &digest, true));
    assert!(msg.contains("disagree") && msg.contains("\"time\""), "{msg}");
}

#[test]
fn a_block_without_a_tip_connect_fails() {
    let specs = vec![spec(false, false), spec(false, false)];
    // Block 1 only has a proposal record.
    let digest = digest_of(vec![
        header(Chain::Regtest),
        Record::Outcome(c_record(0, 1, Origin::Tip, &specs[0])),
        Record::Outcome(c_record(1, 2, Origin::Proposal, &specs[1])),
    ]);
    let msg = err_text(run(&Fake::new(specs), &digest, true));
    assert!(msg.contains("no tip C record for block 1"), "{msg}");
}

#[test]
fn a_digest_may_start_after_genesis() {
    // A node started from a copied datadir records from its first own connect.
    let specs = vec![spec(false, false), spec(false, false), spec(false, false)];
    let digest = digest_of(vec![
        header(Chain::Regtest),
        Record::Outcome(c_record(1, 1, Origin::Tip, &specs[1])),
        Record::Outcome(c_record(2, 2, Origin::Tip, &specs[2])),
    ]);
    let (out, summary) = run(&Fake::new(specs.clone()), &digest, true).unwrap();
    assert_eq!(summary.blocks, 2);
    let heights: Vec<i64> = records(&out)
        .iter()
        .filter_map(|r| match r {
            Record::Outcome(o) => Some(o.id.h),
            _ => None,
        })
        .collect();
    assert_eq!(heights, [1, 2]);

    let empty = digest_of(vec![header(Chain::Regtest)]);
    let msg = err_text(run(&Fake::new(specs), &empty, true));
    assert!(msg.contains("no tip C record of the active chain"), "{msg}");
}

#[test]
fn digest_problems_fail() {
    let specs = vec![spec(false, false)];
    let good = simple_digest(&specs);

    // Wrong chain.
    let mut bad = digest_of(vec![header(Chain::Main)]);
    bad.extend(&good[good.iter().position(|&b| b == b'\n').unwrap() + 1..]);
    assert!(err_text(run(&Fake::new(specs.clone()), &bad, true)).contains("chain main"));

    // No H.
    let no_h = &good[good.iter().position(|&b| b == b'\n').unwrap() + 1..];
    assert!(err_text(run(&Fake::new(specs.clone()), no_h, true)).contains("no H record"));

    // Not canonical.
    let mut spaced = good.clone();
    spaced.splice(1..1, *b" ");
    assert!(err_text(run(&Fake::new(specs.clone()), &spaced, true)).contains("digest line 1"));

    // Compressed.
    assert!(
        err_text(run(&Fake::new(specs.clone()), &[0x28, 0xb5, 0x2f, 0xfd, 0], true))
            .contains("zstd")
    );

    // A digest that already carries golden keys.
    let mut with_golden = c_record(0, 1, Origin::Tip, &specs[0]);
    with_golden.s_mut().set("muhash", Value::string("00")).unwrap();
    let digest = digest_of(vec![header(Chain::Regtest), Record::Outcome(with_golden)]);
    assert!(err_text(run(&Fake::new(specs), &digest, true)).contains("already has the golden key"));
}

#[test]
fn a_digest_for_another_chain_of_blocks_fails() {
    let specs = vec![spec(false, false), spec(false, false)];
    let mut wrong = c_record(1, 2, Origin::Tip, &specs[1]);
    wrong.s_mut().set("prev", Value::hash(&bh(77))).unwrap();
    let digest = digest_of(vec![
        header(Chain::Regtest),
        Record::Outcome(c_record(0, 1, Origin::Tip, &specs[0])),
        Record::Outcome(wrong),
    ]);
    let msg = err_text(run(&Fake::new(specs), &digest, true));
    assert!(msg.contains("prev of block 1"), "{msg}");
}

#[test]
fn two_extractions_are_byte_identical() {
    let specs = vec![spec(false, false), spec(true, false), spec(true, true), spec(true, true)];
    let digest = simple_digest(&specs);
    let a = run(&Fake::new(specs.clone()), &digest, true).unwrap().0;
    let b = run(&Fake::new(specs), &digest, true).unwrap().0;
    assert_eq!(a, b);
    assert!(!a.is_empty());
}

#[test]
fn dense_sparse_records_are_in_era_and_ordered() {
    let specs = vec![spec(false, false), spec(false, false), spec(true, false), spec(true, true)];
    let digest = simple_digest(&specs);
    let (out, summary) = run(&Fake::new(specs), &digest, true).unwrap();
    let recs = records(&out);
    let got = names(&recs);
    let want: Vec<(i64, &str)> = vec![
        (0, "deploymentinfo"),
        (1, "deploymentinfo"),
        (2, "protx_list"),
        // h = 2 is the first DIP3 block: listdiff <1> <2>, then the rest in table order.
        (2, "protx_listdiff"),
        (2, "mn_payments"),
        (2, "quorum_listextended"),
        (2, "deploymentinfo"),
        (3, "protx_list"),
        (3, "protx_listdiff"),
        (3, "mn_payments"),
        (3, "quorum_listextended"),
        (3, "deploymentinfo"),
        (3, "creditpool"),
    ];
    let want: Vec<(i64, String)> = want.into_iter().map(|(h, n)| (h, n.to_owned())).collect();
    // canonical_mnlist is dense-every-24: only heights divisible by 24 (none here).
    assert_eq!(got, want);
    assert_eq!(summary.sparse["deploymentinfo"], 4);
    assert!(!summary.sparse.contains_key("canonical_mnlist"));

    // Payload encoding: keys sorted bytewise, numbers as Core's text, non-ASCII byte-escaped.
    let s = sparse_of(&recs).into_iter().find(|s| s.rpc == b"protx_listdiff").unwrap();
    assert_eq!(s.args(), [Value::Int(1), Value::Int(2)]);
    let line = String::from_utf8(Record::Sparse(s.clone()).to_line()).unwrap();
    assert!(line.contains(r#""res":{"a":"\u00c3\u00a9","z":[1.50,{"a":1e-05,"b":2}]}}"#), "{line}");
}

fn info(h: i64) -> BlockInfo {
    BlockInfo {
        h,
        hash: bh(0),
        dip3: true,
        v20: true,
        qc: Vec::new(),
        unlocks: Vec::new(),
        transition: false,
    }
}

fn is_due(rpc: SparseRpc, h: i64, o: &Options) -> bool {
    due(rpc, &info(h), o)
}

#[test]
fn cadence_off_regtest() {
    use SparseRpc::*;
    let o = Options {
        boundaries: BTreeSet::from([1000]),
        ..opts(false)
    };
    // Every 576, plus boundaries.
    for rpc in [ProtxList, MnPayments, Creditpool] {
        assert!(is_due(rpc, 0, &o) && is_due(rpc, 576, &o) && is_due(rpc, 1152, &o), "{rpc:?}");
        assert!(!is_due(rpc, 575, &o) && !is_due(rpc, 577, &o), "{rpc:?}");
    }
    assert!(is_due(ProtxList, 1000, &o) && !is_due(MnPayments, 1000, &o));
    // listdiff: every 576, plus +-3 around boundaries; never with base block 0.
    assert!(is_due(ProtxListdiff, 576, &o) && !is_due(ProtxListdiff, 0, &o));
    for h in 997..=1003 {
        assert!(is_due(ProtxListdiff, h, &o), "{h}");
    }
    assert!(!is_due(ProtxListdiff, 996, &o) && !is_due(ProtxListdiff, 1004, &o));
    // quorum listextended: every 24.
    assert!(is_due(QuorumListextended, 48, &o) && !is_due(QuorumListextended, 49, &o));
    // deploymentinfo: every 576, boundaries and window transitions.
    assert!(is_due(Deploymentinfo, 0, &o) && is_due(Deploymentinfo, 1000, &o));
    assert!(!is_due(Deploymentinfo, 1001, &o));
    let mut t = info(1001);
    t.transition = true;
    assert!(due(Deploymentinfo, &t, &o));
    // canonical_mnlist (DEC-177 heights): every 576 and the boundaries off regtest.
    assert!(is_due(CanonicalMnlist, 576, &o) && is_due(CanonicalMnlist, 1000, &o));
    assert!(!is_due(CanonicalMnlist, 24, &o));
    // Driven by block contents, never by cadence.
    for rpc in [QuorumInfo, Rotationinfo, Assetunlockstatuses] {
        assert!(!is_due(rpc, 576, &o));
    }
}

#[test]
fn cadence_dense_and_eras() {
    use SparseRpc::*;
    let d = opts(true);
    for rpc in
        [ProtxList, ProtxListdiff, MnPayments, QuorumListextended, Deploymentinfo, Creditpool]
    {
        assert!(is_due(rpc, 7, &d), "{rpc:?}");
    }
    // canonical_mnlist: every 24 even when dense.
    assert!(is_due(CanonicalMnlist, 24, &d) && !is_due(CanonicalMnlist, 7, &d));
    // Eras: DIP0003 off, V20 off.
    let mut pre = info(576);
    pre.dip3 = false;
    pre.v20 = false;
    for rpc in
        [ProtxList, ProtxListdiff, MnPayments, QuorumListextended, CanonicalMnlist, Creditpool]
    {
        assert!(!due(rpc, &pre, &d) && !due(rpc, &pre, &opts(false)), "{rpc:?}");
    }
    assert!(due(Deploymentinfo, &pre, &opts(false)), "deploymentinfo has no era");
    let mut dip3_only = info(576);
    dip3_only.v20 = false;
    assert!(due(ProtxList, &dip3_only, &d) && !due(Creditpool, &dip3_only, &d));
}

#[test]
fn mainnet_boundaries_are_the_default_only_on_main() {
    assert!(Options::default_boundaries(Chain::Main).contains(&1028159));
    assert_eq!(Options::default_boundaries(Chain::Main).len(), 12);
    for c in [Chain::Test, Chain::Regtest, Chain::Devnet] {
        assert!(Options::default_boundaries(c).is_empty());
    }
}

#[test]
fn window_transitions_trigger_deploymentinfo_off_dense() {
    let mut changed = spec(false, false);
    changed.bur = 4031;
    let specs = vec![spec(false, false), spec(false, false), changed.clone(), changed];
    let digest = simple_digest(&specs);
    let (out, _) = run(&Fake::new(specs), &digest, false).unwrap();
    // Height 0 is a multiple of 576; the `bur` change at 2 is a window boundary.
    assert_eq!(
        names(&records(&out)),
        [(0, "deploymentinfo".to_owned()), (2, "deploymentinfo".to_owned())]
    );
}

#[test]
fn rotated_commitments_give_quorum_info_and_one_rotationinfo_per_cycle_base() {
    let (q1, q2, q3) = (bh(100), bh(101), bh(102));
    let mut specs: Vec<Spec> = (0..12).map(|_| spec(true, false)).collect();
    specs[0] = spec(false, false);
    mine_qc(&mut specs, 11, vec![(103, q1), (103, q2), (104, q3)]);
    let digest = simple_digest(&specs);
    let (out, summary) = run(&Fake::new(specs), &digest, false).unwrap();
    let recs = records(&out);
    let qi: Vec<&Sparse> =
        sparse_of(&recs).into_iter().filter(|s| s.rpc == b"quorum_info").collect();
    assert_eq!(qi.len(), 3);
    assert!(qi.iter().all(|s| s.h == 11 && s.b == bh(11)));
    assert_eq!(qi[0].args(), [Value::Int(103), Value::hash(&q1)]);
    assert_eq!(qi[2].args(), [Value::Int(104), Value::hash(&q3)]);
    // All three report height 9 and quorumIndex 2: one cycle base, height 7.
    let ri: Vec<&Sparse> =
        sparse_of(&recs).into_iter().filter(|s| s.rpc == b"rotationinfo").collect();
    assert_eq!(ri.len(), 1);
    assert_eq!((ri[0].h, ri[0].b), (7, bh(7)));
    assert_eq!(ri[0].args(), [Value::hash(&bh(7))]);
    assert_eq!(summary.sparse["rotationinfo"], 1);
    // Sorted by height: the rotationinfo at 7 precedes the quorum_info records at 11.
    let order = names(&recs);
    let at = |n: &str| order.iter().position(|(_, r)| r == n).unwrap();
    assert!(at("rotationinfo") < at("quorum_info"));
    assert!(order.windows(2).all(|w| w[0].0 <= w[1].0));
}

#[test]
fn unrotated_quorum_types_get_no_rotationinfo() {
    let mut specs: Vec<Spec> = (0..12).map(|_| spec(true, false)).collect();
    specs[0] = spec(false, false);
    mine_qc(&mut specs, 11, vec![(100, bh(100))]);
    let mut fake = Fake::new(specs.clone());
    fake.fail = Some("rotated-off");
    let (out, _) = run(&fake, &simple_digest(&specs), false).unwrap();
    let recs = records(&out);
    assert_eq!(sparse_of(&recs).iter().filter(|s| s.rpc == b"quorum_info").count(), 1);
    assert!(sparse_of(&recs).iter().all(|s| s.rpc != b"rotationinfo"));
}

#[test]
fn asset_unlocks_are_sent_as_decimal_strings_in_transaction_order() {
    let mut specs: Vec<Spec> = (0..5).map(|_| spec(true, true)).collect();
    specs[0] = spec(false, false);
    mine_unlocks(&mut specs, 3, vec![7, 3, 18_446_744_073_709_551_615]);
    // ntx = 1 blocks never fetch the block; a two-transaction block without unlocks adds none.
    specs[4].ntx = 2;
    let fake = Fake::new(specs.clone());
    let (out, _) = run(&fake, &simple_digest(&specs), false).unwrap();
    let recs = records(&out);
    let au: Vec<&Sparse> =
        sparse_of(&recs).into_iter().filter(|s| s.rpc == b"assetunlockstatuses").collect();
    assert_eq!(au.len(), 1);
    assert_eq!(au[0].h, 3);
    assert_eq!(
        au[0].args(),
        [
            Value::Arr(vec![
                Value::string("7"),
                Value::string("3"),
                Value::string("18446744073709551615")
            ]),
            Value::Int(3)
        ]
    );
    let fetched: Vec<_> = fake
        .log
        .borrow()
        .iter()
        .filter(|(m, p)| m == "getblock" && p[1] == 2)
        .map(|(_, p)| p.clone())
        .collect();
    assert_eq!(fetched.len(), 2, "only blocks 3 and 4 are fetched: {fetched:?}");
    let sent = fake
        .log
        .borrow()
        .iter()
        .find(|(m, _)| m == "getassetunlockstatuses")
        .map(|(_, p)| p.clone())
        .unwrap();
    assert_eq!(sent, json!([["7", "3", "18446744073709551615"], 3]));
}

#[test]
fn rpc_arguments_follow_the_table() {
    let specs = vec![spec(false, false), spec(false, false), spec(true, true)];
    let fake = Fake::new(specs.clone());
    run(&fake, &simple_digest(&specs), true).unwrap();
    let log = fake.log.borrow();
    let find = |method: &str, sub: &str| {
        log.iter()
            .find(|(m, p)| m == method && p[0] == json!(sub))
            .map(|(_, p)| p.clone())
            .unwrap_or_else(|| panic!("no {method} {sub}"))
    };
    assert_eq!(find("protx", "list"), json!(["list", "registered", true, 2]));
    assert_eq!(find("protx", "listdiff"), json!(["listdiff", 1, 2]));
    assert_eq!(find("masternode", "payments"), json!(["payments", bh(2).to_hex()]));
    assert_eq!(find("quorum", "listextended"), json!(["listextended", 2]));
    // The first call with arguments: the checks also call getdeploymentinfo without any.
    let find1 = |method: &str| {
        log.iter().find(|(m, p)| m == method && p != &json!([])).map(|(_, p)| p.clone()).unwrap()
    };
    assert_eq!(find1("getdeploymentinfo"), json!([bh(0).to_hex()]));
    assert_eq!(find1("getcreditpoolinfo"), json!([2]));
    assert_eq!(find1("gettxoutsetinfo"), json!(["muhash", 0, true]));
    assert_eq!(find1("getblockstats")[1], json!(STATS));
}

#[test]
fn an_rpc_error_in_era_fails_with_the_call_named() {
    let specs = vec![spec(false, false), spec(true, false)];
    for failing in ["protx list", "gettxoutsetinfo", "getblockstats", "getdeploymentinfo"] {
        let mut fake = Fake::new(specs.clone());
        fake.fail = Some(failing);
        let msg = err_text(run(&fake, &simple_digest(&specs), true));
        assert!(msg.contains("boom") && msg.contains("-8"), "{failing}: {msg}");
    }
}

#[test]
fn a_moving_tip_fails() {
    let specs = vec![spec(false, false), spec(false, false)];
    let mut fake = Fake::new(specs.clone());
    fake.moved_tip = Some(bh(40));
    let msg = err_text(run(&fake, &simple_digest(&specs), true));
    assert!(msg.contains("tip"), "{msg}");
}

#[test]
fn an_unsynced_index_fails() {
    let specs = vec![spec(false, false)];
    let mut fake = Fake::new(specs.clone());
    fake.synced = false;
    let msg = err_text(run(&fake, &simple_digest(&specs), true));
    assert!(msg.contains("coinstatsindex did not reach the tip"), "{msg}");
}

#[test]
fn malformed_amounts_fail() {
    let specs = vec![spec(false, false)];
    for bad in ["25000.0", "2.5e4", "25000"] {
        let mut fake = Fake::new(specs.clone());
        fake.amount = bad;
        let msg = err_text(run(&fake, &simple_digest(&specs), true));
        assert!(msg.contains("total_amount"), "{bad}: {msg}");
    }
}

#[test]
fn reply_parsing() {
    let ok = parse_reply(br#"{"result":{"a":1.0},"error":null,"id":0}"#).unwrap();
    assert_eq!(ok.unwrap().get(b"a").unwrap().as_num(), Some("1.0"));
    let err = parse_reply(
        br#"{"result":null,"error":{"code":-32601,"message":"Method not found"},"id":0}"#,
    )
    .unwrap();
    assert_eq!(
        err,
        Err(RpcError {
            code: -32601,
            message: "Method not found".into()
        })
    );
    assert!(parse_reply(b"Work queue depth exceeded").is_err());
    assert!(parse_reply(br#"{"error":null}"#).is_err());
}

#[test]
fn wall_clock_members_of_meta_info_are_dropped() {
    let specs = vec![spec(false, false), spec(false, false), spec(true, false)];
    let (out, _) = run(&Fake::new(specs.clone()), &simple_digest(&specs), true).unwrap();
    let recs = records(&out);
    let list = sparse_of(&recs).into_iter().find(|s| s.rpc == b"protx_list").unwrap();
    assert_eq!(
        list.res().to_vec(),
        br#"[{"metaInfo":{"lastDSQ":0,"lastOutboundAttempt":7},"x":{"metaInfo":{"keepElapsedNot":1}}}]"#
    );
}

#[test]
fn a_refused_rotationinfo_is_skipped_and_reported() {
    let mut specs: Vec<Spec> = (0..12).map(|_| spec(true, false)).collect();
    specs[0] = spec(false, false);
    mine_qc(&mut specs, 11, vec![(103, bh(100))]);
    let mut fake = Fake::new(specs.clone());
    fake.fail = Some("quorum rotationinfo");
    // Any other error than BuildQuorumRotationInfo's refusal fails.
    let msg = err_text(run(&fake, &simple_digest(&specs), false));
    assert!(msg.contains("rotationinfo") && msg.contains("-8"), "{msg}");
    fake.fail_code = -32600;
    let (out, summary) = run(&fake, &simple_digest(&specs), false).unwrap();
    let recs = records(&out);
    assert_eq!(sparse_of(&recs).iter().filter(|s| s.rpc == b"quorum_info").count(), 1);
    assert!(sparse_of(&recs).iter().all(|s| s.rpc != b"rotationinfo"));
    assert!(!summary.sparse.contains_key("rotationinfo"));
    assert_eq!(summary.skipped.len(), 1);
    assert!(
        summary.skipped[0].starts_with("rotationinfo at height 7")
            && summary.skipped[0].contains("code -32600: boom"),
        "{:?}",
        summary.skipped
    );
}

#[test]
fn more_than_a_hundred_unlocks_are_split_into_calls_of_a_hundred() {
    let mut specs: Vec<Spec> = (0..4).map(|_| spec(true, true)).collect();
    specs[0] = spec(false, false);
    mine_unlocks(&mut specs, 3, (0..250).collect());
    let fake = Fake::new(specs.clone());
    let (out, _) = run(&fake, &simple_digest(&specs), false).unwrap();
    let recs = records(&out);
    let au: Vec<&Sparse> =
        sparse_of(&recs).into_iter().filter(|s| s.rpc == b"assetunlockstatuses").collect();
    let sizes: Vec<usize> = au.iter().map(|s| s.args()[0].as_arr().unwrap().len()).collect();
    assert_eq!(sizes, [100, 100, 50]);
    assert_eq!(au[1].args()[0].as_arr().unwrap()[0], Value::string("100"));
    assert!(au.iter().all(|s| s.h == 3 && s.args()[1] == Value::Int(3)));
}

#[test]
fn listdiff_updated_mns_are_put_in_pro_tx_hash_order() {
    // Core fills updatedMNs from a salted unordered_map: its order changes per process.
    let reply = br#"{"updatedMNs":[{"bb":{"x":1}},{"aa":{}},{"cc":{"y":2}}],"addedMNs":[{"zz":1},{"yy":2}]}"#;
    let parse = |b: &[u8]| json::parse(b).unwrap();
    let mut v = parse(reply);
    order_updated_mns(SparseRpc::ProtxListdiff, &mut v);
    assert_eq!(
        v,
        parse(br#"{"updatedMNs":[{"aa":{}},{"bb":{"x":1}},{"cc":{"y":2}}],"addedMNs":[{"zz":1},{"yy":2}]}"#)
    );
    // Other RPCs are left alone.
    let mut other = parse(reply);
    order_updated_mns(SparseRpc::ProtxList, &mut other);
    assert_eq!(other, parse(reply));
}

/// A DIP0003 and V20 chain with a commitment and unlocks, which every check passes.
fn checked_chain() -> Vec<Spec> {
    let mut specs = vec![spec(false, false), spec(true, false), spec(true, true), spec(true, true)];
    mine_qc(&mut specs, 2, vec![(100, bh(50))]);
    mine_unlocks(&mut specs, 2, vec![4, 5]);
    mine_unlocks(&mut specs, 3, vec![9]);
    mine_signal(&mut specs, 2, 10);
    specs
}

/// A fake for [`checked_chain`]: its commitment is of an unrotated type, so no rotationinfo
/// reaches back before genesis.
fn checked_fake() -> Fake {
    let mut fake = Fake::new(checked_chain());
    fake.fail = Some("rotated-off");
    fake
}

fn extract_edited(edit: impl Fn(i64, &mut Outcome)) -> String {
    err_text(run(&checked_fake(), &digest_with(&checked_chain(), edit), true))
}

fn set(c: &mut Outcome, key: &'static str, v: Value) {
    c.s_mut().set(key, v).unwrap();
}

#[test]
fn the_checked_chain_extracts() {
    run(&checked_fake(), &simple_digest(&checked_chain()), true).unwrap();
}

#[test]
fn eras_come_from_the_node_not_the_digest() {
    // Sol r1 H1: a Core build without TapNewList writes `mnl: null` everywhere. The eras are the
    // node's buried heights, so the record is in era and fails, instead of dropping every
    // DIP0003-era S record.
    let msg = extract_edited(|_, c| set(c, "mnl", Value::Null));
    assert!(msg.contains("s.mnl is null at height 1, inside its era"), "{msg}");
    for key in ["qc", "cp", "ehf", "undo"] {
        let msg = extract_edited(|h, c| {
            if h == 3 {
                set(c, key, Value::Null)
            }
        });
        assert!(msg.contains(&format!("s.{key} is null at height 3, inside its era")), "{msg}");
    }
    // And a key set before its era.
    let msg = extract_edited(|h, c| {
        if h == 1 {
            set(
                c,
                "cp",
                Value::Arr(vec![
                    Value::Int(0),
                    Value::Int(0),
                    Value::Int(0),
                    Value::hash(&fill(1)),
                ]),
            )
        }
    });
    assert!(msg.contains("s.cp is set at height 1, outside its era"), "{msg}");
}

#[test]
fn header_keys_are_checked_against_the_block() {
    // Sol r1 H2: an encoder swapping nBits and nNonce.
    let msg = extract_edited(|_, c| {
        set(c, "bits", Value::Int(6));
        set(c, "nonce", Value::Int(5));
    });
    assert!(msg.contains("s.bits is Some(Int(6)), but the block says Int(5)"), "{msg}");
    for (key, v) in [
        ("ver", Value::Int(2)),
        ("mrkl", Value::hash(&fill(7))),
        ("time", Value::Int(999)),
        ("nonce", Value::Int(5)),
        ("work", Value::hash(&fill(7))),
        ("ntx", Value::Int(9)),
    ] {
        let msg = extract_edited(|h, c| {
            if h == 2 {
                set(c, key, v.clone())
            }
        });
        assert!(msg.contains(&format!("s.{key} is")) && msg.contains("block 2"), "{key}: {msg}");
    }
    // Genesis' prev is null: the header's zero hash.
    let msg = extract_edited(|h, c| {
        if h == 0 {
            set(c, "prev", Value::hash(&fill(0)))
        }
    });
    assert!(msg.contains("s.prev is"), "{msg}");
}

#[test]
fn deployment_keys_are_checked_against_getdeploymentinfo() {
    let msg = extract_edited(|h, c| {
        if h == 2 {
            set(c, "bur", Value::Int(4094))
        }
    });
    assert!(msg.contains("s.bur is 4094, but getdeploymentinfo gives 4095"), "{msg}");
    let msg = extract_edited(|_, c| set(c, "bls", Value::Bool(true)));
    assert!(msg.contains("s.bls is not !v19.active"), "{msg}");
    let dep = dep_value;
    let msg = extract_edited(|h, c| {
        if h == 2 {
            set(c, "dep", dep("started", 0))
        }
    });
    assert!(
        msg.contains("v24 is started, but getdeploymentinfo's status_next is defined"),
        "{msg}"
    );
    // A block's `since` is checked by its own reply and by its child's.
    let msg = extract_edited(|h, c| {
        if h == 1 {
            set(c, "dep", dep("defined", 1))
        }
    });
    assert!(
        msg.contains("block 1: s.dep: v24 since Int(1), but getdeploymentinfo gives 0"),
        "{msg}"
    );
    let mut fake = checked_fake();
    fake.v24_since = Some((2, 5));
    let msg = err_text(run(&fake, &simple_digest(&checked_chain()), true));
    assert!(
        msg.contains(
            "block 2: s.dep of block 1: v24 since Int(0), but the child's getdeploymentinfo says 5"
        ),
        "{msg}"
    );
    // The tip has no child; its `since` is checked against its own reply.
    let tip = checked_chain().len() as i64 - 1;
    let msg = extract_edited(|h, c| {
        if h == tip {
            set(c, "dep", dep("defined", 1))
        }
    });
    assert!(
        msg.contains(&format!(
            "block {tip}: s.dep: v24 since Int(1), but getdeploymentinfo gives 0"
        )),
        "{msg}"
    );
    let msg = extract_edited(|h, c| {
        if h == 2 {
            set(c, "dep", Value::Arr(Vec::new()))
        }
    });
    assert!(msg.contains("s.dep names [], but Core's BIP9 deployments are"), "{msg}");
    // §5.2 pins the names and their order: another order, a repeat, a foreign name and a missing
    // disabled deployment all fail.
    let edited = |edit: fn(&mut Vec<Value>)| {
        extract_edited(move |h, c| {
            if h == 2 {
                let Value::Arr(mut entries) = dep("defined", 0) else {
                    unreachable!()
                };
                edit(&mut entries);
                set(c, "dep", Value::Arr(entries))
            }
        })
    };
    for (edit, names) in [
        (
            (|e: &mut Vec<Value>| e.swap(1, 2)) as fn(&mut Vec<Value>),
            r#"["testdummy", "evo_shares", "v24"]"#,
        ),
        (|e| e[0] = e[1].clone(), r#"["v24", "v24", "evo_shares"]"#),
        (
            |e| {
                e[0] =
                    Value::Arr(vec![Value::string("v19"), Value::string("failed"), Value::Int(0)])
            },
            r#"["v19", "v24", "evo_shares"]"#,
        ),
        (|e| drop(e.remove(0)), r#"["v24", "evo_shares"]"#),
    ] {
        let msg = edited(edit);
        assert!(msg.contains(&format!("s.dep names {names}")), "{msg}");
    }
    // A deployment the node leaves out is disabled: failed since 0, nothing else.
    let msg = edited(|e| {
        e[0] = Value::Arr(vec![Value::string("testdummy"), Value::string("started"), Value::Int(0)])
    });
    assert!(
        msg.contains("s.dep: testdummy is started since Int(0), but the node does not list it"),
        "{msg}"
    );
}

#[test]
fn since_next_follows_the_childs_state() {
    let entry = |status: &str, since: i64, next: &str, height: Option<i64>| {
        let height = height.map(|h| format!(r#""height":{h},"#)).unwrap_or_default();
        let body = format!(
            r#"{{"type":"bip9",{height}"bip9":{{"status":"{status}","since":{since},"status_next":"{next}"}}}}"#
        );
        json::parse(body.as_bytes()).unwrap()
    };
    // The child keeps the block's state: the block's since.
    assert_eq!(since_next(&entry("defined", 0, "defined", None), 143).unwrap(), 0);
    assert_eq!(since_next(&entry("started", 144, "started", None), 200).unwrap(), 144);
    // The child, the first block of a period, enters another state.
    assert_eq!(since_next(&entry("started", 144, "locked_in", None), 287).unwrap(), 288);
    assert_eq!(since_next(&entry("locked_in", 288, "active", Some(432)), 431).unwrap(), 432);
    assert_eq!(since_next(&entry("active", 432, "active", Some(432)), 500).unwrap(), 432);
    // The RPC's `height` must agree.
    let msg =
        format!("{:#}", since_next(&entry("active", 432, "active", Some(433)), 500).unwrap_err());
    assert!(msg.contains("height Some(433), but status, since and status_next give 432"), "{msg}");
}

#[test]
fn special_transaction_keys_are_checked_against_the_block() {
    // A `qc` row whose commitment hash is not the mined commitment's.
    let msg = extract_edited(|h, c| {
        if h == 2 {
            let row =
                Value::Arr(vec![Value::Int(100), Value::hash(&bh(50)), Value::hash(&fill(9))]);
            set(c, "qc", Value::Arr(vec![row]))
        }
    });
    assert!(msg.contains("is not the block's non-null commitments"), "{msg}");
    // A missing row (the null commitment the block also holds is never a row).
    let msg = extract_edited(|h, c| {
        if h == 2 {
            set(c, "qc", Value::Arr(Vec::new()))
        }
    });
    assert!(msg.contains("is not the block's non-null commitments"), "{msg}");
    // Credit pool amounts, and the index set's hash.
    let cp = |amount: i64, indexes: &[u64]| {
        Value::Arr(vec![
            Value::Int(amount),
            Value::Int(2),
            Value::Int(3),
            Value::hash(&ranges_hash(&indexes.iter().copied().collect())),
        ])
    };
    let msg = extract_edited(|h, c| {
        if h == 3 {
            set(c, "cp", cp(7, &[4, 5, 9]))
        }
    });
    assert!(msg.contains("but the node's credit pool"), "{msg}");
    let msg = extract_edited(|h, c| {
        if h == 3 {
            set(c, "cp", cp(1, &[4, 5]))
        }
    });
    assert!(msg.contains("but the node's credit pool"), "{msg}");
    // EHF rows: the signal of bit 10 mined at 2 is in the ehf of 2 and 3.
    let ehf = |rows: &[(i64, i64)]| {
        Value::Arr(
            rows.iter().map(|(b, at)| Value::Arr(vec![Value::Int(*b), Value::Int(*at)])).collect(),
        )
    };
    // A row that no MNHF transaction mined.
    let msg = extract_edited(|h, c| {
        if h == 3 {
            set(c, "ehf", ehf(&[(10, 2), (28, 3)]))
        }
    });
    assert!(msg.contains("is no MNHF signal mined at its height"), "{msg}");
    // The block's own signal missing.
    let msg = extract_edited(|h, c| {
        if h == 2 {
            set(c, "ehf", ehf(&[]))
        }
    });
    assert!(msg.contains("s.ehf lacks the block's signal for bit 10"), "{msg}");
    // An expired row may go, but not come back.
    let mut specs = checked_chain();
    let mut last = spec(true, true);
    (last.pool, last.ehf) = (specs[3].pool.clone(), specs[3].ehf.clone());
    specs.push(last);
    let fake = || {
        let mut f = Fake::new(specs.clone());
        f.fail = Some("rotated-off");
        f
    };
    let digest = |rows: [&[(i64, i64)]; 2]| {
        digest_with(&specs, |h, c| match h {
            3 => set(c, "ehf", ehf(rows[0])),
            4 => set(c, "ehf", ehf(rows[1])),
            _ => {}
        })
    };
    run(&fake(), &digest([&[], &[]]), true).unwrap();
    let msg = err_text(run(&fake(), &digest([&[], &[(10, 2)]]), true));
    assert!(msg.contains("came from nowhere"), "{msg}");
}

#[test]
fn sparse_replies_must_agree_with_the_c_record() {
    let digest = simple_digest(&checked_chain());
    let mut fake = checked_fake();
    fake.mined_elsewhere = true;
    let msg = err_text(run(&fake, &digest, true));
    assert!(msg.contains("was mined in another block"), "{msg}");

    // A hook that taps another list than the one the node stores for the block.
    let mut fake = checked_fake();
    fake.canonical = fill(7);
    let msg = err_text(run(&fake, &digest, true));
    assert!(msg.contains("block 1") && msg.contains("but getcanonicalmnlist says"), "{msg}");
}

#[test]
fn dense_coverage_is_counted_from_the_eras() {
    let mut infos: Vec<BlockInfo> = (0..3).map(info).collect();
    infos[0].dip3 = false;
    infos[0].v20 = false;
    infos[2].qc = vec![(100, bh(1))];
    let full = BTreeMap::from([
        ("protx_list", 2),
        ("protx_listdiff", 1),
        ("mn_payments", 2),
        ("quorum_listextended", 2),
        ("quorum_info", 1),
        ("deploymentinfo", 3),
        ("creditpool", 2),
    ]);
    check_dense_coverage(&infos, &full).unwrap();
    for missing in full.keys() {
        let mut short = full.clone();
        *short.get_mut(missing).unwrap() -= 1;
        let msg = format!("{:#}", check_dense_coverage(&infos, &short).unwrap_err());
        assert!(msg.contains(missing) && msg.contains("but the eras call for"), "{msg}");
    }
}

#[test]
fn ranges_hash_serializes_runs_like_core() {
    // sha256d of the empty set's single compact-size byte, in Core's display order.
    assert_eq!(
        ranges_hash(&BTreeSet::new()).to_hex(),
        "9a538906e6466ebd2617d321f71bc94e56056ce213d366773699e28158e00614"
    );
    // {1, 2, 3, 7}: two runs, [1, 4) and [7, 8).
    let mut bytes = vec![2u8];
    for v in [1u64, 4, 7, 8] {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    assert_eq!(
        ranges_hash(&BTreeSet::from([1, 2, 3, 7])),
        Hash256::from_wire_bytes(sha256d::Hash::hash(&bytes).to_byte_array())
    );
}

#[test]
fn undo_keys_are_checked_against_the_rev_files_during_extraction() {
    // Block h > 0 of the checked chain has undo bytes "undo-<h>", stored for its parent.
    let undo = |h: i64| format!("undo-{h}").into_bytes();
    let sha = |b: &[u8]| sha256d::Hash::hash(b).to_byte_array();
    let dir = tempfile::tempdir().unwrap();
    let mut file = Vec::new();
    for h in 1..=3 {
        file.extend_from_slice(&[0xfc, 0xc1, 0xb7, 0xdc]);
        file.extend_from_slice(&(undo(h).len() as u32).to_le_bytes());
        file.extend_from_slice(&undo(h));
        file.extend_from_slice(&sha(&[&bh(h - 1).to_wire_bytes()[..], &undo(h)].concat()));
    }
    std::fs::write(dir.path().join("rev00000.dat"), &file).unwrap();
    let extract_with = |of: fn(i64) -> i64| {
        let digest = digest_with(&checked_chain(), |h, c| {
            if h > 0 {
                let hash = Hash256::from_wire_bytes(sha(&format!("undo-{}", of(h)).into_bytes()));
                set(c, "undo", Value::hash(&hash));
            }
        });
        let opts = Options {
            blocks_dir: Some(dir.path().to_path_buf()),
            ..opts(true)
        };
        let mut out = Vec::new();
        extract(&checked_fake(), &digest, &opts, &mut out).map(|summary| (out, summary))
    };
    extract_with(|h| h).unwrap();
    // Block 3 with block 2's undo bytes: stored, but bound to block 1.
    let msg = err_text(extract_with(|h| {
        if h == 3 {
            2
        } else {
            h
        }
    }));
    assert!(msg.contains("s.undo of block 3"), "{msg}");
    assert!(msg.contains("is not stored for a block whose parent is"), "{msg}");
    // And bytes the rev files do not hold.
    let msg = err_text(extract_with(|h| {
        if h == 2 {
            7
        } else {
            h
        }
    }));
    assert!(msg.contains("no undo record in the rev files"), "{msg}");
}

#[test]
fn undo_is_checked_against_the_rev_files() {
    let dir = tempfile::tempdir().unwrap();
    let sha = |b: &[u8]| sha256d::Hash::hash(b).to_byte_array();
    let (p1, p2) = (bh(1), bh(2));
    let undo_a = b"undo-a".to_vec();
    // Two blocks with the same undo bytes (no spends), each bound to its own parent.
    let mut file = Vec::new();
    for (prev, undo) in [(p1, &undo_a), (p2, &undo_a)] {
        file.extend_from_slice(&[0xfc, 0xc1, 0xb7, 0xdc]);
        file.extend_from_slice(&(undo.len() as u32).to_le_bytes());
        file.extend_from_slice(undo);
        file.extend_from_slice(&sha(&[&prev.to_wire_bytes()[..], undo].concat()));
    }
    file.resize(file.len() + 64, 0);
    std::fs::write(dir.path().join("rev00000.dat"), &file).unwrap();
    std::fs::write(dir.path().join("blk00000.dat"), b"ignored").unwrap();

    let index = UndoIndex::scan(dir.path()).unwrap();
    let a = Hash256::from_wire_bytes(sha(&undo_a));
    index.check(&a, &p1).unwrap();
    index.check(&a, &p2).unwrap();
    let msg = format!("{:#}", index.check(&a, &bh(3)).unwrap_err());
    assert!(msg.contains("is not stored for a block whose parent is"), "{msg}");
    let msg = format!("{:#}", index.check(&fill(7), &p1).unwrap_err());
    assert!(msg.contains("no undo record in the rev files"), "{msg}");

    // Magic must not change between records: the second record starts after 4 + 4 + 6 + 32.
    file[46..50].copy_from_slice(&[1, 2, 3, 4]);
    std::fs::write(dir.path().join("rev00000.dat"), &file).unwrap();
    assert!(UndoIndex::scan(dir.path()).is_err());
    let empty = tempfile::tempdir().unwrap();
    assert!(UndoIndex::scan(empty.path()).is_err());
}

#[test]
fn bip9_values_of_runs_with_other_parameters_are_left_unchecked() {
    // feature_mnehf restarts its nodes with other -vbparams; the copy runs with the last start's.
    let specs = checked_chain();
    let run_header = |run: &[u8]| {
        let Record::Header(mut h) = header(Chain::Regtest) else {
            unreachable!()
        };
        h.run = run.to_vec();
        Record::Header(h)
    };
    let mut recs = vec![run_header(b"old")];
    for (h, sp) in specs.iter().enumerate() {
        let h = h as i64;
        if h == 2 {
            recs.push(run_header(b"new"));
        }
        let mut c = c_record(h, h + 1, Origin::Tip, sp);
        if h < 2 {
            set(&mut c, "dep", dep_value("started", 0));
        }
        recs.push(Record::Outcome(c));
    }
    let digest = digest_of(recs);
    let msg = err_text(run(&checked_fake(), &digest, true));
    assert!(msg.contains("v24 is started"), "{msg}");

    let with_runs = |runs: &[&[u8]]| Options {
        other_params_runs: runs.iter().map(|r| r.to_vec()).collect(),
        ..opts(true)
    };
    let summary =
        extract(&checked_fake(), &digest, &with_runs(&[b"old"]), &mut Vec::new()).unwrap();
    assert_eq!(summary.bip9_unchecked, [0, 1]);
    // check_bip9 checks those blocks on a copy with the old run's parameters, and only those.
    let mut old_params = checked_fake();
    old_params.v24_next = "started";
    let runs = |r: &[u8]| BTreeSet::from([r.to_vec()]);
    let check = check_bip9(&old_params, &digest, &runs(b"old")).unwrap();
    assert_eq!((check.tip, check.checked), (bh(3), vec![0, 1]));
    let msg = format!("{:#}", check_bip9(&checked_fake(), &digest, &runs(b"old")).unwrap_err());
    assert!(
        msg.contains("v24 is started, but getdeploymentinfo's status_next is defined"),
        "{msg}"
    );
    let msg = format!("{:#}", check_bip9(&old_params, &digest, &runs(b"typo")).unwrap_err());
    assert!(msg.contains("the digest has no run typo"), "{msg}");
    // Buried values are still checked in those runs.
    let mut bad = digest.clone();
    let at = bad.windows(10).position(|w| w == br#""bur":4095"#).unwrap();
    bad.splice(at..at + 10, br#""bur":4094"#.iter().copied());
    let msg = format!(
        "{:#}",
        extract(&checked_fake(), &bad, &with_runs(&[b"old"]), &mut Vec::new()).unwrap_err()
    );
    assert!(msg.contains("s.bur is 4094"), "{msg}");
    // A run the digest lacks.
    let msg = format!(
        "{:#}",
        extract(&checked_fake(), &digest, &with_runs(&[b"old", b"typo"]), &mut Vec::new())
            .unwrap_err()
    );
    assert!(msg.contains("run typo has other parameters, but the digest has no such run"), "{msg}");

    // The last block of a run before the restart: its `since` is checked against its own reply
    // and its child's, which comes from the other run. Block 1 (old run) with v24 since 99.
    let mut wrong_since = Vec::new();
    for line in String::from_utf8(digest.clone()).unwrap().lines() {
        let fixed = if line.contains(r#""h":1,"#) && line.contains(r#""k":"C""#) {
            line.replace(r#"["v24","started",0]"#, r#"["v24","started",99]"#)
        } else {
            line.to_owned()
        };
        wrong_since.extend_from_slice(fixed.as_bytes());
        wrong_since.push(b'\n');
    }
    assert_ne!(wrong_since, digest);
    let msg = format!("{:#}", check_bip9(&old_params, &wrong_since, &runs(b"old")).unwrap_err());
    assert!(
        msg.contains("block 1: s.dep: v24 since Int(99), but getdeploymentinfo gives 0"),
        "{msg}"
    );
    // check_bip9 asks block 2 for block 1's since.
    old_params.v24_since = Some((2, 99));
    let msg = format!("{:#}", check_bip9(&old_params, &digest, &runs(b"old")).unwrap_err());
    assert!(msg.contains("block 2: s.dep of block 1: v24 since Int(0)"), "{msg}");
    // And the other order: block 1 from the copy's run, block 2 from a run with other
    // parameters. The extraction checks block 1's since with block 2's reply.
    let mut recs = vec![run_header(b"main")];
    for (h, sp) in specs.iter().enumerate() {
        let h = h as i64;
        if h == 2 {
            recs.push(run_header(b"other"));
        }
        recs.push(Record::Outcome(c_record(h, h + 1, Origin::Tip, sp)));
    }
    let mut fake = checked_fake();
    fake.v24_since = Some((2, 99));
    let msg = format!(
        "{:#}",
        extract(&fake, &digest_of(recs), &with_runs(&[b"other"]), &mut Vec::new()).unwrap_err()
    );
    assert!(msg.contains("block 2: s.dep of block 1: v24 since Int(0)"), "{msg}");
}

/// A canonical list (`SerializeCanonicalMNList`): block hash, height, registration counter, entry
/// count, entries. The entries are opaque bytes: only the head is read.
fn canonical_list(counter: u32, entries: &[&[u8]]) -> Vec<u8> {
    let mut b = vec![0x22u8; 32];
    b.extend(7i32.to_le_bytes());
    b.extend(counter.to_le_bytes());
    b.push(u8::try_from(entries.len()).unwrap());
    entries.iter().for_each(|e| b.extend(*e));
    b
}

fn list_hash(list: &[u8]) -> Hash256 {
    Hash256::from_wire_bytes(sha256d::Hash::hash(list).to_byte_array())
}

fn mnl_reply(list: &[u8]) -> Value {
    let json = format!(r#"{{"hash":"{}","hex":"{}"}}"#, list_hash(list), hex_of(list));
    parse_reply(format!(r#"{{"result":{json},"error":null,"id":0}}"#).as_bytes()).unwrap().unwrap()
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn mnl_agrees_on_equal_hashes_or_a_counter_difference_of_one_k() {
    let entries: [&[u8]; 2] = [b"entry-a", b"entry-b"];
    let stored = canonical_list(1000, &entries);
    let reply = mnl_reply(&stored);
    // k = 0: equal hashes.
    let equal = Value::hash(&list_hash(&stored));
    assert_eq!(mnl_agreement(&equal, &reply).unwrap(), MnlAgreement::Equal);
    // The range is 1..=16, pinned by its literal ends rather than through the constant.
    assert_eq!(COUNTER_ADDS, 1..=16);
    for counter in [1001, 1016] {
        let tapped = Value::hash(&list_hash(&canonical_list(counter, &entries)));
        assert_eq!(
            mnl_agreement(&tapped, &reply).unwrap(),
            MnlAgreement::CounterAdjusted,
            "{counter}"
        );
    }
    // Every k of the range: the tap's list has the counter k higher.
    for k in COUNTER_ADDS {
        let tapped = Value::hash(&list_hash(&canonical_list(1000 + k, &entries)));
        assert_eq!(mnl_agreement(&tapped, &reply).unwrap(), MnlAgreement::CounterAdjusted, "k {k}");
    }
    // Outside the range, and below the stored counter.
    for counter in [1000 + 17, 999] {
        let tapped = Value::hash(&list_hash(&canonical_list(counter, &entries)));
        let msg = format!("{:#}", mnl_agreement(&tapped, &reply).unwrap_err());
        assert!(msg.contains("but getcanonicalmnlist says"), "{msg}");
    }
    // The counter plus k overflows: no k can match, and nothing panics.
    let top = canonical_list(u32::MAX, &entries);
    let tapped = Value::hash(&list_hash(&canonical_list(5, &entries)));
    assert!(mnl_agreement(&tapped, &mnl_reply(&top)).is_err());
}

#[test]
fn mnl_counter_adjustment_rejects_any_other_difference() {
    let stored = canonical_list(1000, &[b"entry-a", b"entry-b"]);
    let reply = mnl_reply(&stored);
    // An entry differs, with the counter equal and with it adjusted.
    for counter in [1000, 1001] {
        let tapped = Value::hash(&list_hash(&canonical_list(counter, &[b"entry-a", b"entry-X"])));
        assert!(mnl_agreement(&tapped, &reply).is_err(), "counter {counter}");
    }
    // So do the height and the block hash (the head's other fields).
    let mut moved = canonical_list(1001, &[b"entry-a", b"entry-b"]);
    moved[32] ^= 1;
    assert!(mnl_agreement(&Value::hash(&list_hash(&moved)), &reply).is_err());
    moved[32] ^= 1;
    moved[0] ^= 1;
    assert!(mnl_agreement(&Value::hash(&list_hash(&moved)), &reply).is_err());
    // A reply whose hex is cut inside the head cannot be adjusted.
    let short = parse_reply(
        format!(
            r#"{{"result":{{"hash":"{}","hex":"{}"}},"error":null,"id":0}}"#,
            fill(9),
            hex_of(&stored[..38])
        )
        .as_bytes(),
    )
    .unwrap()
    .unwrap();
    let tapped = Value::hash(&list_hash(&canonical_list(1001, &[b"entry-a", b"entry-b"])));
    assert!(mnl_agreement(&tapped, &short).is_err());
    // Two ks cannot both reproduce `mnl`: each k gives a different counter, hence a different hash.
}

#[test]
fn the_summary_counts_the_blocks_that_agreed_by_counter_adjustment() {
    let stored = canonical_list(1000, &[b"entry-a"]);
    let tapped = Value::hash(&list_hash(&canonical_list(1003, &[b"entry-a"])));
    let mut fake = checked_fake();
    fake.canonical = list_hash(&stored);
    fake.canonical_hex = Some(hex_of(&stored));
    let digest = digest_with(&checked_chain(), |h, c| {
        if h > 0 && h <= 3 {
            set(c, "mnl", tapped.clone());
        }
    });
    let (_, summary) = run(&fake, &digest, true).unwrap();
    // The DIP0003-era blocks of the fake chain (heights 1 to 3).
    assert_eq!(summary.mnl_counter_adjusted, 3);
    let (_, summary) = run(&checked_fake(), &simple_digest(&checked_chain()), true).unwrap();
    assert_eq!(summary.mnl_counter_adjusted, 0);
}
