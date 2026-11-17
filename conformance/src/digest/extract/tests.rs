use std::cell::{Cell, RefCell};

use dash_statedigest::{Lines, Record};
use dashcore::hashes::sha256d;

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
        from: None,
        to: None,
        sample: None,
        sparse_only: false,
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
    /// Extra transactions appended to `getspecialtxes` after count and skip, so a reply can be
    /// longer than `nTx`.
    extra_specials: usize,
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
            extra_specials: 0,
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

    /// Transactions of `getblock <hash> 2`, in block order: the coinbase, the block's commitments,
    /// asset unlocks and signals, then a null commitment in DIP0003 blocks. The null commitment
    /// is last so `getspecialtxes` with `count = nTx` (which does not count it) still returns
    /// every transaction the parser keeps.
    fn tx_json(&self, sp: &Spec) -> Vec<String> {
        let qc_tx = |t: i64, q: &str, n: u32, key: &str, payload: &str| {
            format!(
                r#"{{"type":6,"extraPayload":"{payload}","qcTx":{{"version":1,"height":9,"commitment":{{"version":1,"llmqType":{t},"quorumHash":"{q}","signersCount":{n},"validMembersCount":{n},"quorumVvecHash":"{key}","quorumPublicKey":"{key}","quorumSig":"{key}","membersSig":"{key}"}}}}}}"#
            )
        };
        let mut txs = vec![r#"{"type":5}"#.to_owned()];
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
        if sp.mnl {
            txs.push(qc_tx(100, &"00".repeat(32), 0, &"00".repeat(48), "0100090000000000"));
        }
        txs
    }

    /// `getblock <hash> 2`.
    fn block(&self, sp: &Spec) -> String {
        format!(r#"{{"tx":[{}]}}"#, self.tx_json(sp).join(","))
    }

    /// `getspecialtxes`: every transaction whose type is not 0, in block order, then `count` and
    /// `skip`. `type == -1` keeps every special transaction.
    fn special_txes(&self, sp: &Spec, ty: i64, count: i64, skip: i64) -> String {
        let mut txs: Vec<String> =
            self.tx_json(sp).into_iter().filter(|tx| ty < 0 || tx_type_of(tx) == ty).collect();
        let skip_n = usize::try_from(skip.max(0)).unwrap_or(usize::MAX);
        let count_n = usize::try_from(count.max(0)).unwrap_or(0);
        txs = txs.into_iter().skip(skip_n).take(count_n).collect();
        for _ in 0..self.extra_specials {
            txs.push(r#"{"type":1}"#.to_owned());
        }
        format!("[{}]", txs.join(","))
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
            "getspecialtxes" => {
                let (_, sp) = self.spec_of(&p[0]);
                let ty = p.get(1).and_then(serde_json::Value::as_i64).unwrap_or(-1);
                let count = p.get(2).and_then(serde_json::Value::as_i64).unwrap_or(10);
                let skip = p.get(3).and_then(serde_json::Value::as_i64).unwrap_or(0);
                self.special_txes(sp, ty, count, skip)
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

/// `{"type":N...}` → N.
fn tx_type_of(tx: &str) -> i64 {
    let rest = tx.strip_prefix(r#"{"type":"#).expect("special tx json");
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().expect("special tx type")
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

/// Multi-era chain (0..=15) for X1/X2 tests: pre-DIP3 (0..=1), DIP3 (2..=15), V20 (5..=15),
/// window-boundary `bur` transitions at 6 and 11, rotated quorums mined at 9 and 12 sharing cycle
/// base 7, asset unlocks at 5, 8 and 13, and MNHF signals at 5 and 13.
fn rich_chain() -> Vec<Spec> {
    let mut specs: Vec<Spec> = (0..=15)
        .map(|h| {
            let mut sp = spec(h >= 2, h >= 5);
            if (6..11).contains(&h) {
                sp.bur = 4031;
            }
            sp
        })
        .collect();
    mine_unlocks(&mut specs, 5, vec![10, 11]);
    mine_signal(&mut specs, 5, 10);
    mine_unlocks(&mut specs, 8, vec![20]);
    mine_qc(&mut specs, 9, vec![(103, bh(100))]);
    mine_qc(&mut specs, 12, vec![(103, bh(101)), (104, bh(102))]);
    mine_unlocks(&mut specs, 13, vec![30, 31]);
    mine_signal(&mut specs, 13, 12);
    specs
}

#[test]
fn base_extract_without_new_flags_is_byte_identical_and_rejects_bad_ranges() {
    let specs = rich_chain();
    let digest = simple_digest(&specs);
    let (mem_out, mem_summary) = run(&Fake::new(specs.clone()), &digest, false).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let out_path = dir.path().join("golden.jsonl");
    let file_summary =
        extract_to_file(&Fake::new(specs.clone()), &digest, &opts(false), None, &out_path).unwrap();
    assert_eq!(file_summary, mem_summary);
    assert_eq!(std::fs::read(&out_path).unwrap(), mem_out);

    let sc =
        crate::digest::sample::RangeSidecar::read(&crate::digest::sample::sidecar_path(&out_path))
            .unwrap();
    assert_eq!(sc.from, 0);
    assert_eq!(sc.to, 15);
    assert!(!sc.sparse_only);
    assert_eq!(sc.sample, None);
    assert_eq!(sc.sampled, 0);

    // Invalid ranges: A < 0, A > B, B > tip.
    for (from, to) in [(Some(-1), None), (Some(8), Some(7)), (Some(0), Some(16))] {
        let bad_opts = Options {
            from,
            to,
            ..opts(false)
        };
        let bad_path = dir.path().join("bad.jsonl");
        assert!(
            extract_to_file(&Fake::new(specs.clone()), &digest, &bad_opts, None, &bad_path)
                .is_err(),
            "expected ({from:?}, {to:?}) to fail"
        );
        assert!(!bad_path.exists());
    }
}

#[test]
fn warmup_at_a_minus_1_carries_transition_and_checker_state_without_coin_stats() {
    let specs = rich_chain();
    let digest = simple_digest(&specs);

    // Height 6 is a window-boundary transition (bur changes 4095 -> 4031 between 5 and 6).
    // Extracting 6..=8 must warm up at 5 (no C record and no coin-stat RPC at 5) and emit
    // deploymentinfo at 6.
    let fake = Fake::new(specs.clone());
    let mut shard = Vec::new();
    let o = Options {
        from: Some(6),
        to: Some(8),
        ..opts(false)
    };
    let summary = extract(&fake, &digest, &o, &mut shard).unwrap();
    assert_eq!(summary.blocks, 3);
    let recs = records(&shard);
    let c_heights: Vec<i64> = recs
        .iter()
        .filter_map(|r| match r {
            Record::Outcome(c) => Some(c.id.h),
            _ => None,
        })
        .collect();
    assert_eq!(c_heights, [6, 7, 8]);
    // Because 5 -> 6 transitioned `bur`, deploymentinfo is due at 6 even off --dense!
    assert_eq!(
        names(&recs),
        [(6, "deploymentinfo".to_owned()), (8, "assetunlockstatuses".to_owned())]
    );
    // No coin-stat RPC calls were made for warm-up height 5.
    let txout_heights: Vec<i64> = fake
        .log
        .borrow()
        .iter()
        .filter(|(m, _)| m == "gettxoutsetinfo")
        .map(|(_, p)| p[1].as_i64().unwrap())
        .collect();
    assert_eq!(txout_heights, [6, 7, 8]);

    // Warm-up at A - 1 also carries Checker.prev (`dep` since check at A): if Fake says block 6's
    // parent (block 5) had v24 since=99 while digest's block 5 has since=0, extracting 6..=8
    // catches it!
    let mut bad_parent_since = Fake::new(specs.clone());
    bad_parent_since.v24_since = Some((6, 99));
    assert!(extract(&bad_parent_since, &digest, &o, &mut Vec::new()).is_err());
}

#[test]
fn checker_state_stays_correct_across_skipped_rpc_checks() {
    let mut specs = rich_chain();
    // Add one more V20 block 16 so we can test EHF expiry across an unsampled block.
    let mut b16 = spec(true, true);
    (b16.pool, b16.ehf) = (specs[15].pool.clone(), specs[15].ehf.clone());
    specs.push(b16);

    // Rev files covering blocks 1..=16 so undo checks run at every height, sampled or not.
    let sha = |b: &[u8]| sha256d::Hash::hash(b).to_byte_array();
    let undo_bytes = |h: i64| format!("undo-{h}").into_bytes();
    let rev_dir = tempfile::tempdir().unwrap();
    let mut rev_file = Vec::new();
    for h in 1..=16 {
        let u = undo_bytes(h);
        rev_file.extend_from_slice(&[0xfc, 0xc1, 0xb7, 0xdc]);
        rev_file.extend_from_slice(&(u.len() as u32).to_le_bytes());
        rev_file.extend_from_slice(&u);
        rev_file.extend_from_slice(&sha(&[&bh(h - 1).to_wire_bytes()[..], &u].concat()));
    }
    std::fs::write(rev_dir.path().join("rev00000.dat"), &rev_file).unwrap();

    let make_digest = |custom: &dyn Fn(i64, &mut Outcome)| {
        digest_with(&specs, |h, c| {
            if h > 0 {
                let hash = Hash256::from_wire_bytes(sha(&undo_bytes(h)));
                set(c, "undo", Value::hash(&hash));
            }
            custom(h, c);
        })
    };

    // Span 4..16 with every=12,edges=0,windows=0x1,seed=1: ONLY height 12 is in the set? Wait:
    // P=4 and Q=16 are +-0 edges too, so sampled heights in [4, 16] are {4, 12, 16}!
    // Heights 0..=3 are outside [4, 16] (dense); heights 5, 8, 13 (which mine unlocks & signals!)
    // and 15 (parent of sampled 16) are UNSAMPLED!
    let sample_spec =
        crate::digest::sample::SampleSpec::parse("every=12,edges=0,windows=0x1,seed=1,span=4..16")
            .unwrap();
    let sampled_heights = crate::digest::sample::sample_set(&sample_spec, &BTreeSet::new());
    assert_eq!(sampled_heights, BTreeSet::from([4, 12, 16]));

    let sample_opts = Options {
        blocks_dir: Some(rev_dir.path().to_path_buf()),
        sample: Some(sample_spec),
        ..opts(false)
    };

    let fake = Fake::new(specs.clone());
    let mut out = Vec::new();
    extract(&fake, &make_digest(&|_, _| {}), &sample_opts, &mut out).unwrap();

    // Coin-stat RPCs ran ONLY at dense heights outside [4, 16] (0, 1, 2, 3) and sampled {4, 12, 16}.
    let txout_heights: Vec<i64> = fake
        .log
        .borrow()
        .iter()
        .filter(|(m, _)| m == "gettxoutsetinfo")
        .map(|(_, p)| p[1].as_i64().unwrap())
        .collect();
    assert_eq!(txout_heights, [0, 1, 2, 3, 4, 12, 16]);

    // `getblockheader` runs at every height: dense heights read `chainwork` from it, and
    // unsampled heights take `nTx` from it before `getspecialtxes`.
    let header_heights: Vec<i64> = fake
        .log
        .borrow()
        .iter()
        .filter(|(m, _)| m == "getblockheader")
        .map(|(_, p)| h_of(&bh_inv(p[0].as_str().unwrap())))
        .collect();
    assert_eq!(header_heights, (0..=16).collect::<Vec<_>>());

    // C records exist for all 0..=16; coin-stat keys present iff h in {0, 1, 2, 3, 4, 12, 16}.
    for r in records(&out) {
        if let Record::Outcome(c) = r {
            let want_coins = [0, 1, 2, 3, 4, 12, 16].contains(&c.id.h);
            assert_eq!(c.s().get("muhash").is_some(), want_coins, "h={}", c.id.h);
            assert_eq!(c.s().get("stats").is_some(), want_coins, "h={}", c.id.h);
            assert!(c.s().get("undo").is_some(), "non-coin keys stay dense at h={}", c.id.h);
        }
    }

    // 1. Rev-file `undo` check STILL runs at unsampled height 7!
    let bad_undo = make_digest(&|h, c| {
        if h == 7 {
            set(c, "undo", Value::hash(&fill(9)));
        }
    });
    let msg = format!(
        "{:#}",
        extract(&Fake::new(specs.clone()), &bad_undo, &sample_opts, &mut Vec::new()).unwrap_err()
    );
    assert!(msg.contains("s.undo of block 7"), "{msg}");

    // 2. `unlock_indexes` from unsampled heights 5, 8, 13 is checked at sampled height 16:
    //    if block 16's `cp` omits height 13's unlocks (30, 31), check_credit_pool at 16 fails!
    let bad_cp = make_digest(&|h, c| {
        if h == 16 {
            let wrong_pool = BTreeSet::from([10, 11, 20]);
            set(
                c,
                "cp",
                Value::Arr(vec![
                    Value::Int(1),
                    Value::Int(2),
                    Value::Int(3),
                    Value::hash(&ranges_hash(&wrong_pool)),
                ]),
            );
        }
    });
    let msg = format!(
        "{:#}",
        extract(&Fake::new(specs.clone()), &bad_cp, &sample_opts, &mut Vec::new()).unwrap_err()
    );
    assert!(msg.contains("block 16") && msg.contains("credit pool"), "{msg}");

    // 3. `Prev.dep` from unsampled height 15 is checked at sampled height 16 via check_parent_since!
    let mut bad_since_fake = Fake::new(specs.clone());
    bad_since_fake.v24_since = Some((16, 77));
    let msg = format!(
        "{:#}",
        extract(&bad_since_fake, &make_digest(&|_, _| {}), &sample_opts, &mut Vec::new())
            .unwrap_err()
    );
    assert!(msg.contains("s.dep of block 15"), "{msg}");

    // 4. `Prev.ehf` expiry across unsampled height 15 is enforced at sampled height 16:
    //    if signal (10, 5) expired at unsampled 15 and reappears at sampled 16, it fails!
    let resurrected_ehf = make_digest(&|h, c| {
        if h == 15 {
            set(c, "ehf", Value::Arr(vec![Value::Arr(vec![Value::Int(12), Value::Int(13)])]));
        }
    });
    let msg = format!(
        "{:#}",
        extract(&Fake::new(specs.clone()), &resurrected_ehf, &sample_opts, &mut Vec::new())
            .unwrap_err()
    );
    assert!(msg.contains("came from nowhere"), "{msg}");
}

#[test]
fn join_reproduces_single_pass_across_layouts_samples_and_sparse_only_shards() {
    use crate::digest::join::{CLinesSpec, join};
    use crate::digest::sample::sidecar_path;

    let specs = rich_chain();
    let digest = simple_digest(&specs);
    let boundaries = BTreeSet::from([8]);

    // Three cut layouts over 0..=15:
    // - Layout 1: cuts at 8|9 and 10|11 (both 9..=10 and 11..=15 emit rotationinfo at base 7 < A,
    //   and 10|11 cuts right at a window-boundary transition!).
    // - Layout 2: cuts at 5|6 and 11|12 (5|6 cuts right at a window-boundary transition, and both
    //   6..=11 and 12..=15 emit rotationinfo at base 7!).
    // - Layout 3: 4 shards cutting across DIP3, V20, unlocks, signals and quorums.
    // V20 starts at height 5, so a shard at A = 5 has A−1 = V20−1. The last layout is a
    // single-height shard at the tip.
    let layouts: [&[(i64, i64)]; 5] = [
        &[(0, 8), (9, 10), (11, 15)],
        &[(0, 5), (6, 11), (12, 15)],
        &[(0, 3), (4, 7), (8, 10), (11, 15)],
        &[(0, 4), (5, 15)],
        &[(0, 14), (15, 15)],
    ];

    for dense in [false, true] {
        for sample_raw in [None, Some("every=4,edges=1,windows=2x2,seed=42,span=3..14")] {
            let dir = tempfile::tempdir().unwrap();
            let single_path = dir.path().join("single.jsonl");
            let base_opts = Options {
                dense,
                boundaries: boundaries.clone(),
                ..opts(dense)
            };
            extract_to_file(
                &Fake::new(specs.clone()),
                &digest,
                &base_opts,
                sample_raw,
                &single_path,
            )
            .unwrap();
            let want_golden = std::fs::read(&single_path).unwrap();
            let want_sidecar = std::fs::read(sidecar_path(&single_path)).unwrap();

            for (li, layout) in layouts.iter().enumerate() {
                let mut shard_paths = Vec::new();
                for (si, &(from, to)) in layout.iter().enumerate() {
                    let spath = dir.path().join(format!("l{li}-s{si}.jsonl"));
                    let sopts = Options {
                        from: Some(from),
                        to: Some(to),
                        ..base_opts.clone()
                    };
                    extract_to_file(&Fake::new(specs.clone()), &digest, &sopts, sample_raw, &spath)
                        .unwrap();
                    shard_paths.push(spath);
                }

                // Confirm in Layout 0 & 1 that two shards actually emitted rotationinfo at base 7!
                if li < 2 {
                    let ri_shards = shard_paths
                        .iter()
                        .filter(|p| {
                            let recs = records(&std::fs::read(p).unwrap());
                            sparse_of(&recs).iter().any(|s| s.rpc == b"rotationinfo" && s.h == 7)
                        })
                        .count();
                    assert_eq!(ri_shards, 2, "layout {li}: both shards must emit rotationinfo@7");
                }

                let joined_path = dir.path().join(format!("joined-{li}.jsonl"));
                join(&shard_paths, &[], &boundaries, &joined_path).unwrap();
                assert_eq!(
                    std::fs::read(&joined_path).unwrap(),
                    want_golden,
                    "golden mismatch: dense={dense}, sample={sample_raw:?}, layout={li}"
                );
                assert_eq!(
                    std::fs::read(sidecar_path(&joined_path)).unwrap(),
                    want_sidecar,
                    "sidecar mismatch: dense={dense}, sample={sample_raw:?}, layout={li}"
                );
            }

            // Sparse-only middle shard (6..=11) + --c-lines from single-pass golden truncated
            // after height 11 with a torn last line!
            let s0 = dir.path().join("sp-0.jsonl");
            let s1_sparse = dir.path().join("sp-1.jsonl");
            let s2 = dir.path().join("sp-2.jsonl");
            extract_to_file(
                &Fake::new(specs.clone()),
                &digest,
                &Options {
                    from: Some(0),
                    to: Some(5),
                    ..base_opts.clone()
                },
                sample_raw,
                &s0,
            )
            .unwrap();
            let fake_sparse = Fake::new(specs.clone());
            extract_to_file(
                &fake_sparse,
                &digest,
                &Options {
                    from: Some(6),
                    to: Some(11),
                    sparse_only: true,
                    ..base_opts.clone()
                },
                sample_raw,
                &s1_sparse,
            )
            .unwrap();
            // Verify --sparse-only made zero coin-stat calls and wrote zero C records!
            assert!(
                fake_sparse
                    .log
                    .borrow()
                    .iter()
                    .all(|(m, _)| m != "gettxoutsetinfo" && m != "getblockstats")
            );
            assert!(
                records(&std::fs::read(&s1_sparse).unwrap())
                    .iter()
                    .all(|r| !matches!(r, Record::Outcome(_)))
            );
            extract_to_file(
                &Fake::new(specs.clone()),
                &digest,
                &Options {
                    from: Some(12),
                    to: Some(15),
                    ..base_opts.clone()
                },
                sample_raw,
                &s2,
            )
            .unwrap();

            // Truncate `want_golden` partway through height 12's C line (no trailing '\n').
            // Line 0 is H, lines 1..=12 are C records for heights 0..=11, line 13 is C for h=12.
            let raw_lines: Vec<&[u8]> = want_golden.split_inclusive(|&b| b == b'\n').collect();
            let mut truncated: Vec<u8> = raw_lines[..=12].concat();
            let line13 = raw_lines[13];
            truncated.extend_from_slice(&line13[..line13.len() / 2]);
            assert_ne!(truncated.last(), Some(&b'\n'), "must end with a torn line");
            let trunc_path = dir.path().join("truncated.jsonl");
            std::fs::write(&trunc_path, &truncated).unwrap();

            let joined_sp = dir.path().join("joined-sp.jsonl");
            let cl_spec = CLinesSpec {
                path: trunc_path,
                from: 6,
                to: 11,
            };
            join(&[s0, s1_sparse, s2], &[cl_spec], &boundaries, &joined_sp).unwrap();
            assert_eq!(std::fs::read(&joined_sp).unwrap(), want_golden);
            assert_eq!(std::fs::read(sidecar_path(&joined_sp)).unwrap(), want_sidecar);

            // Sparse-only shard at A = 0, with --c-lines from the single-pass golden.
            let z0 = dir.path().join("z-sparse0.jsonl");
            let z1 = dir.path().join("z-rest.jsonl");
            extract_to_file(
                &Fake::new(specs.clone()),
                &digest,
                &Options {
                    from: Some(0),
                    to: Some(4),
                    sparse_only: true,
                    ..base_opts.clone()
                },
                sample_raw,
                &z0,
            )
            .unwrap();
            extract_to_file(
                &Fake::new(specs.clone()),
                &digest,
                &Options {
                    from: Some(5),
                    to: Some(15),
                    ..base_opts.clone()
                },
                sample_raw,
                &z1,
            )
            .unwrap();
            let joined_z = dir.path().join("joined-z.jsonl");
            join(
                &[z0, z1],
                &[CLinesSpec {
                    path: single_path.clone(),
                    from: 0,
                    to: 4,
                }],
                &boundaries,
                &joined_z,
            )
            .unwrap();
            assert_eq!(std::fs::read(&joined_z).unwrap(), want_golden);
            assert_eq!(std::fs::read(sidecar_path(&joined_z)).unwrap(), want_sidecar);
        }
    }
}

#[test]
fn join_refuses_every_invalid_shard_or_c_lines_condition() {
    use crate::digest::join::{CLinesSpec, join};
    use crate::digest::sample::sidecar_path;

    let specs = rich_chain();
    let digest = simple_digest(&specs);
    let sample_str = "every=4,edges=1,windows=1x2,seed=7,span=2..14";
    let dir = tempfile::tempdir().unwrap();

    let make_shard = |name: &str, from: i64, to: i64, sample: Option<&str>, sparse_only: bool| {
        let p = dir.path().join(format!("{name}.jsonl"));
        let o = Options {
            from: Some(from),
            to: Some(to),
            sparse_only,
            ..opts(false)
        };
        extract_to_file(&Fake::new(specs.clone()), &digest, &o, sample, &p).unwrap();
        p
    };

    let a = make_shard("a", 0, 8, Some(sample_str), false);
    let b = make_shard("b", 9, 15, Some(sample_str), false);
    let out = dir.path().join("out.jsonl");
    // Baseline succeeds.
    join(&[a.clone(), b.clone()], &[], &BTreeSet::new(), &out).unwrap();
    let stamp_g = dir.path().join("stamp-golden");
    let stamp_s = dir.path().join("stamp-sidecar");
    let remember = || {
        std::fs::copy(&out, &stamp_g).unwrap();
        std::fs::copy(sidecar_path(&out), &stamp_s).unwrap();
    };
    remember();
    let refuse = |shards: &[std::path::PathBuf], lines: &[CLinesSpec], needle: &str| {
        let err = join(shards, lines, &BTreeSet::new(), &out).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains(needle), "expected {needle:?} in:\n{msg}");
        assert_eq!(std::fs::read(&out).unwrap(), std::fs::read(&stamp_g).unwrap(), "{needle}");
        assert_eq!(
            std::fs::read(sidecar_path(&out)).unwrap(),
            std::fs::read(&stamp_s).unwrap(),
            "{needle} sidecar"
        );
    };

    // 1. Different H record.
    let bad_h = dir.path().join("bad_h.jsonl");
    let mut b_lines: Vec<Vec<u8>> =
        std::fs::read(&b).unwrap().split_inclusive(|&x| x == b'\n').map(<[u8]>::to_vec).collect();
    let Record::Header(mut hdr) = dash_statedigest::parse_line(&b_lines[0]).unwrap() else {
        unreachable!()
    };
    hdr.ver = b"other-ver".to_vec();
    b_lines[0] = Record::Header(hdr).to_line();
    std::fs::write(&bad_h, b_lines.concat()).unwrap();
    std::fs::copy(sidecar_path(&b), sidecar_path(&bad_h)).unwrap();
    refuse(&[a.clone(), bad_h], &[], "not byte-identical");

    // 2. Gap between ranges (0..=7, 9..=15).
    let gap_a = make_shard("gap_a", 0, 7, Some(sample_str), false);
    refuse(&[gap_a, b.clone()], &[], "not contiguous");

    // 3. Overlap / wrong order (0..=9, 9..=15 and b before a).
    let over_a = make_shard("over_a", 0, 9, Some(sample_str), false);
    refuse(&[over_a, b.clone()], &[], "0..=9");
    refuse(&[b.clone(), a.clone()], &[], "not contiguous");

    // 4. Non-sparse-only shard missing a C record, or holding an extra C record outside range.
    let missing_c = dir.path().join("missing_c.jsonl");
    let mut a_lines: Vec<Vec<u8>> =
        std::fs::read(&a).unwrap().split_inclusive(|&x| x == b'\n').map(<[u8]>::to_vec).collect();
    a_lines.remove(2); // drop C at height 1
    std::fs::write(&missing_c, a_lines.concat()).unwrap();
    std::fs::copy(sidecar_path(&a), sidecar_path(&missing_c)).unwrap();
    refuse(&[missing_c, b.clone()], &[], "expected 9 C records for 0..=8, got 8");

    // 5. Shards with different `sample` strings.
    let unsampled_b = make_shard("unsampled_b", 9, 15, None, false);
    refuse(&[a.clone(), unsampled_b], &[], "has sample");

    // 6. Tampered `sampled_sha256` in sidecar.
    let bad_sc = dir.path().join("bad_sc.jsonl");
    std::fs::copy(&b, &bad_sc).unwrap();
    let sc_text = std::fs::read_to_string(sidecar_path(&b)).unwrap();
    let tampered =
        sc_text.replace(&sc_text[sc_text.find("sampled_sha256\":\"").unwrap() + 17..][..4], "ffff");
    std::fs::write(sidecar_path(&bad_sc), tampered).unwrap();
    refuse(&[a.clone(), bad_sc], &[], "sampled_sha256");

    // 7. Conflicting `rotationinfo` across two shards (0..=10 and 11..=15 both emit rotationinfo@7).
    let r0 = make_shard("r0", 0, 10, Some(sample_str), false);
    let r1 = make_shard("r1", 11, 15, Some(sample_str), false);
    join(&[r0.clone(), r1.clone()], &[], &BTreeSet::new(), &out).unwrap();
    remember();
    let r1_diff = dir.path().join("r1_diff.jsonl");
    let r1_mut: Vec<u8> = std::fs::read(&r1)
        .unwrap()
        .split_inclusive(|&x| x == b'\n')
        .flat_map(|line| match dash_statedigest::parse_line(line).unwrap() {
            Record::Sparse(s) if s.rpc == b"rotationinfo" => {
                let changed = Sparse::new(
                    s.h,
                    s.b,
                    "rotationinfo",
                    s.args().to_vec(),
                    Value::obj([("different", Value::Num("1".into()))]),
                )
                .unwrap();
                Record::Sparse(changed).to_line()
            }
            _ => line.to_vec(),
        })
        .collect();
    std::fs::write(&r1_diff, r1_mut).unwrap();
    std::fs::copy(sidecar_path(&r1), sidecar_path(&r1_diff)).unwrap();
    refuse(&[r0.clone(), r1_diff], &[], "rotationinfo at height 7 differs");

    // 8. --c-lines refusals: range not matching a sparse-only shard, wrong H in --c-lines, and
    //    --c-lines truncated before B.
    let full = make_shard("full", 0, 15, Some(sample_str), false);
    // Range matches non-sparse-only shard `b`:
    let cl_non_sparse = CLinesSpec {
        path: full.clone(),
        from: 9,
        to: 15,
    };
    refuse(&[a.clone(), b.clone()], &[cl_non_sparse], "not sparse-only");

    let sp_b = make_shard("sp_b", 9, 15, Some(sample_str), true);
    // Missing --c-lines for sparse-only shard:
    refuse(&[a.clone(), sp_b.clone()], &[], "has no matching --c-lines");
    // Wrong range in --c-lines:
    refuse(
        &[a.clone(), sp_b.clone()],
        &[CLinesSpec {
            path: full.clone(),
            from: 9,
            to: 14,
        }],
        "matches no shard's range",
    );
    // Duplicate --c-lines for one range.
    refuse(
        &[a.clone(), sp_b.clone()],
        &[
            CLinesSpec {
                path: full.clone(),
                from: 9,
                to: 15,
            },
            CLinesSpec {
                path: full.clone(),
                from: 9,
                to: 15,
            },
        ],
        "duplicate --c-lines for range 9..15",
    );
    // Wrong H in --c-lines file:
    let full_bad_h = dir.path().join("full_bad_h.jsonl");
    let mut f_lines: Vec<Vec<u8>> = std::fs::read(&full)
        .unwrap()
        .split_inclusive(|&x| x == b'\n')
        .map(<[u8]>::to_vec)
        .collect();
    f_lines[0] = b_lines[0].clone();
    std::fs::write(&full_bad_h, f_lines.concat()).unwrap();
    refuse(
        &[a.clone(), sp_b.clone()],
        &[CLinesSpec {
            path: full_bad_h,
            from: 9,
            to: 15,
        }],
        "not byte-identical to the shards' H",
    );
    // Truncated before B (stops at height 13 instead of 15):
    let short_full = dir.path().join("short_full.jsonl");
    let full_raw: Vec<Vec<u8>> = std::fs::read(&full)
        .unwrap()
        .split_inclusive(|&x| x == b'\n')
        .take(15) // H + C(0..=13) -> lacks 14 and 15
        .map(<[u8]>::to_vec)
        .collect();
    std::fs::write(&short_full, full_raw.concat()).unwrap();
    refuse(
        &[a.clone(), sp_b.clone()],
        &[CLinesSpec {
            path: short_full,
            from: 9,
            to: 15,
        }],
        "ended before completing 9..=15",
    );

    // Extra C record past the shard range.
    let extra_c = dir.path().join("extra_c.jsonl");
    let mut extra_lines: Vec<Vec<u8>> =
        std::fs::read(&a).unwrap().split_inclusive(|&x| x == b'\n').map(<[u8]>::to_vec).collect();
    let Record::Outcome(mut extra_o) = dash_statedigest::parse_line(&extra_lines[1]).unwrap()
    else {
        unreachable!()
    };
    extra_o.id.h = 100;
    // After the nine C records and before the S records, so the extra is still a C-section record.
    extra_lines.insert(10, Record::Outcome(extra_o).to_line());
    std::fs::write(&extra_c, extra_lines.concat()).unwrap();
    std::fs::copy(sidecar_path(&a), sidecar_path(&extra_c)).unwrap();
    refuse(&[extra_c, b.clone()], &[], "expected 9 C records for 0..=8, got 10");

    // A gap inside a --c-lines range, and a duplicated height inside it.
    let gapped = dir.path().join("gapped.jsonl");
    let mut gap_lines: Vec<Vec<u8>> = std::fs::read(&full)
        .unwrap()
        .split_inclusive(|&x| x == b'\n')
        .map(<[u8]>::to_vec)
        .collect();
    gap_lines.remove(13); // C at height 12 (line 0 is H, line 13 is height 12)
    std::fs::write(&gapped, gap_lines.concat()).unwrap();
    refuse(
        &[a.clone(), sp_b.clone()],
        &[CLinesSpec {
            path: gapped,
            from: 9,
            to: 15,
        }],
        "expected C record at height 12, got 13",
    );
    let duplicated = dir.path().join("duplicated.jsonl");
    let mut dup_lines: Vec<Vec<u8>> = std::fs::read(&full)
        .unwrap()
        .split_inclusive(|&x| x == b'\n')
        .map(<[u8]>::to_vec)
        .collect();
    dup_lines.insert(11, dup_lines[10].clone()); // second copy of height 9 before height 10
    std::fs::write(&duplicated, dup_lines.concat()).unwrap();
    refuse(
        &[a.clone(), sp_b.clone()],
        &[CLinesSpec {
            path: duplicated,
            from: 9,
            to: 15,
        }],
        "expected C record at height 10, got 9",
    );

    // Heights below A that do not rise.
    let unrising = dir.path().join("unrising.jsonl");
    let mut rise_lines: Vec<Vec<u8>> = std::fs::read(&full)
        .unwrap()
        .split_inclusive(|&x| x == b'\n')
        .map(<[u8]>::to_vec)
        .collect();
    rise_lines.swap(3, 4); // heights 2 and 3, both below 9
    std::fs::write(&unrising, rise_lines.concat()).unwrap();
    refuse(
        &[a.clone(), sp_b.clone()],
        &[CLinesSpec {
            path: unrising,
            from: 9,
            to: 15,
        }],
        "non-rising C height",
    );

    // A sparse-only shard that still holds a C record.
    let sp_with_c = dir.path().join("sp_with_c.jsonl");
    let mut sp_lines: Vec<Vec<u8>> = std::fs::read(&sp_b)
        .unwrap()
        .split_inclusive(|&x| x == b'\n')
        .map(<[u8]>::to_vec)
        .collect();
    sp_lines.insert(
        1,
        std::fs::read(&full).unwrap().split_inclusive(|&x| x == b'\n').nth(10).unwrap().to_vec(),
    );
    std::fs::write(&sp_with_c, sp_lines.concat()).unwrap();
    std::fs::copy(sidecar_path(&sp_b), sidecar_path(&sp_with_c)).unwrap();
    refuse(
        &[a.clone(), sp_with_c],
        &[CLinesSpec {
            path: full.clone(),
            from: 9,
            to: 15,
        }],
        "sparse-only shard contains a C record",
    );

    // rotationinfo above the shard's `to`, and another S record outside the range.
    let ri_high = dir.path().join("ri_high.jsonl");
    let ri_bytes: Vec<u8> = std::fs::read(&r1)
        .unwrap()
        .split_inclusive(|&x| x == b'\n')
        .flat_map(|line| match dash_statedigest::parse_line(line).unwrap() {
            Record::Sparse(s) if s.rpc == b"rotationinfo" => {
                let changed =
                    Sparse::new(16, s.b, "rotationinfo", s.args().to_vec(), s.res().clone())
                        .unwrap();
                Record::Sparse(changed).to_line()
            }
            _ => line.to_vec(),
        })
        .collect();
    std::fs::write(&ri_high, ri_bytes).unwrap();
    std::fs::copy(sidecar_path(&r1), sidecar_path(&ri_high)).unwrap();
    refuse(&[r0, ri_high], &[], "rotationinfo height 16 outside 0..=15");

    let outside_s = dir.path().join("outside_s.jsonl");
    let outside_bytes: Vec<u8> = std::fs::read(&b)
        .unwrap()
        .split_inclusive(|&x| x == b'\n')
        .flat_map(|line| match dash_statedigest::parse_line(line).unwrap() {
            Record::Sparse(s) if s.rpc != b"rotationinfo" => {
                let changed = Sparse::new(
                    16,
                    s.b,
                    std::str::from_utf8(&s.rpc).unwrap(),
                    s.args().to_vec(),
                    s.res().clone(),
                )
                .unwrap();
                Record::Sparse(changed).to_line()
            }
            _ => line.to_vec(),
        })
        .collect();
    std::fs::write(&outside_s, outside_bytes).unwrap();
    std::fs::copy(sidecar_path(&b), sidecar_path(&outside_s)).unwrap();
    refuse(&[a, outside_s], &[], "outside shard range");
}

/// rich_chain plus block 16, with asset unlock 40 mined at unsampled height 14.
fn chain_with_unlock_40() -> Vec<Spec> {
    let mut specs = rich_chain();
    let mut b16 = spec(true, true);
    (b16.pool, b16.ehf) = (specs[15].pool.clone(), specs[15].ehf.clone());
    specs.push(b16);
    mine_unlocks(&mut specs, 14, vec![40]);
    specs
}

fn sample_span_4_16() -> Options {
    Options {
        sample: Some(
            crate::digest::sample::SampleSpec::parse(
                "every=12,edges=0,windows=0x1,seed=1,span=4..16",
            )
            .unwrap(),
        ),
        ..opts(false)
    }
}

#[test]
fn sampled_extract_rejects_an_unlock_dropped_at_an_unsampled_height() {
    let specs = chain_with_unlock_40();
    let digest = digest_dropping_unlock_40(&specs);
    let dense = err_text(run(&Fake::new(specs.clone()), &digest, true));
    assert!(dense.contains("block 14") && dense.contains("unlock indexes"), "{dense}");
    let sampled = format!(
        "{:#}",
        extract(&Fake::new(specs), &digest, &sample_span_4_16(), &mut Vec::new()).unwrap_err()
    );
    assert!(sampled.contains("block 14") && sampled.contains("unlock indexes"), "{sampled}");
}

#[test]
fn honest_sampled_extract_records_the_unsampled_asset_unlock() {
    let specs = chain_with_unlock_40();
    let digest = simple_digest(&specs);
    let mut out = Vec::new();
    extract(&Fake::new(specs), &digest, &sample_span_4_16(), &mut out).unwrap();
    let recs = records(&out);
    let hit: Vec<_> = sparse_of(&recs)
        .into_iter()
        .filter(|s| s.rpc == b"assetunlockstatuses" && s.h == 14)
        .collect();
    assert_eq!(hit.len(), 1, "height 14 must keep its assetunlockstatuses record");
    assert_eq!(hit[0].args()[0], Value::Arr(vec![Value::string("40")]));
}

fn digest_dropping_unlock_40(specs: &[Spec]) -> Vec<u8> {
    digest_with(specs, |h, c| {
        if (14..=16).contains(&h) {
            let mut pool = specs[h as usize].pool.clone();
            pool.remove(&40);
            set(
                c,
                "cp",
                Value::Arr(vec![
                    Value::Int(1),
                    Value::Int(2),
                    Value::Int(3),
                    Value::hash(&ranges_hash(&pool)),
                ]),
            );
        }
    })
}

#[test]
fn ranged_extract_rejects_an_unlock_dropped_before_a() {
    // 14 lies in the pre-A walk (`skip_connect`) and 15 is the warm-up.
    let specs = chain_with_unlock_40();
    let digest = digest_dropping_unlock_40(&specs);
    let ranged = Options {
        from: Some(16),
        to: Some(16),
        ..opts(false)
    };
    let msg =
        format!("{:#}", extract(&Fake::new(specs), &digest, &ranged, &mut Vec::new()).unwrap_err());
    assert!(msg.contains("block 15") && msg.contains("unlock indexes"), "{msg}");
}

#[test]
fn sparse_only_takes_asset_unlocks_from_the_node() {
    let specs = chain_with_unlock_40();
    let digest = digest_dropping_unlock_40(&specs);
    let sparse = Options {
        sparse_only: true,
        ..opts(false)
    };
    let mut out = Vec::new();
    extract(&Fake::new(specs), &digest, &sparse, &mut out).unwrap();
    let recs = records(&out);
    let hit: Vec<_> = sparse_of(&recs)
        .into_iter()
        .filter(|s| s.rpc == b"assetunlockstatuses" && s.h == 14)
        .collect();
    assert_eq!(hit.len(), 1, "height 14 must keep its assetunlockstatuses record");
    assert_eq!(hit[0].args()[0], Value::Arr(vec![Value::string("40")]));
}

#[test]
fn sparse_only_refuses_qc_rows_the_node_does_not_have() {
    let specs = chain_with_unlock_40();
    let sparse = Options {
        sparse_only: true,
        ..opts(false)
    };
    let dropped = digest_with(&specs, |h, c| {
        if h == 9 {
            set(c, "qc", Value::Arr(Vec::new()))
        }
    });
    let msg = format!(
        "{:#}",
        extract(&Fake::new(specs.clone()), &dropped, &sparse, &mut Vec::new()).unwrap_err()
    );
    assert!(msg.contains("s.qc of block 9") && msg.contains("non-null commitments"), "{msg}");
    let extra = digest_with(&specs, |h, c| {
        if h == 8 {
            let row =
                Value::Arr(vec![Value::Int(103), Value::hash(&bh(100)), Value::hash(&fill(9))]);
            set(c, "qc", Value::Arr(vec![row]))
        }
    });
    let msg =
        format!("{:#}", extract(&Fake::new(specs), &extra, &sparse, &mut Vec::new()).unwrap_err());
    assert!(msg.contains("s.qc of block 8") && msg.contains("non-null commitments"), "{msg}");
}

#[test]
fn sparse_only_checks_qc_between_dip3_and_v20() {
    let mut specs = rich_chain();
    mine_qc(&mut specs, 3, vec![(100, bh(77))]);
    let sparse = Options {
        sparse_only: true,
        ..opts(false)
    };
    extract(&Fake::new(specs.clone()), &simple_digest(&specs), &sparse, &mut Vec::new()).unwrap();
    let dropped = digest_with(&specs, |h, c| {
        if h == 3 {
            set(c, "qc", Value::Arr(Vec::new()))
        }
    });
    let msg = format!(
        "{:#}",
        extract(&Fake::new(specs), &dropped, &sparse, &mut Vec::new()).unwrap_err()
    );
    assert!(msg.contains("s.qc of block 3") && msg.contains("non-null commitments"), "{msg}");
}

#[test]
fn explicit_from_refuses_a_digest_that_starts_later() {
    let specs = rich_chain();
    let suffix = digest_of(
        std::iter::once(header(Chain::Regtest))
            .chain(
                (9..=15).map(|h| Record::Outcome(c_record(h, h, Origin::Tip, &specs[h as usize]))),
            )
            .collect(),
    );
    for sparse_only in [true, false] {
        let ranged = Options {
            from: Some(6),
            to: Some(11),
            sparse_only,
            ..opts(false)
        };
        let msg = format!(
            "{:#}",
            extract(&Fake::new(specs.clone()), &suffix, &ranged, &mut Vec::new()).unwrap_err()
        );
        assert!(msg.contains("no tip C record for block 6"), "sparse_only={sparse_only}: {msg}");
    }
}

#[test]
fn getspecialtxes_and_getblock_parse_to_the_same_specials() {
    let specs = chain_with_unlock_40();
    let fake = Fake::new(specs.clone());
    let cl = Client {
        node: &fake,
    };
    for (h, sp) in specs.iter().enumerate() {
        let hash = bh(h as i64);
        let from_block = super::checks::block_specials(&cl, &hash).unwrap();
        let from_list = super::checks::listed_specials(&cl, &hash, sp.ntx).unwrap();
        assert_eq!(from_block, from_list, "height {h}");
        let params = fake
            .log
            .borrow()
            .iter()
            .rev()
            .find(|(m, _)| m == "getspecialtxes")
            .map(|(_, p)| p.clone())
            .unwrap();
        assert_eq!(params, json!([hash.to_hex(), -1, sp.ntx, 0, 2]), "height {h}");
    }
    // count and skip trim the special transactions in block order.
    let first =
        fake.call("getspecialtxes", json!([bh(14).to_hex(), -1, 1, 0, 2])).unwrap().unwrap();
    let second =
        fake.call("getspecialtxes", json!([bh(14).to_hex(), -1, 1, 1, 2])).unwrap().unwrap();
    assert_eq!(first.as_arr().unwrap().len(), 1);
    assert_eq!(second.as_arr().unwrap().len(), 1);
    assert_ne!(first, second);
}

#[test]
fn getspecialtxes_with_more_entries_than_ntx_is_refused() {
    let mut fake = Fake::new(chain_with_unlock_40());
    fake.extra_specials = 1;
    let cl = Client {
        node: &fake,
    };
    let err = super::checks::listed_specials(&cl, &bh(14), 2).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("more than nTx"), "{msg}");
}

#[test]
fn an_ehf_row_that_appears_at_an_unsampled_height_is_rejected() {
    let mut specs = rich_chain();
    let mut b16 = spec(true, true);
    (b16.pool, b16.ehf) = (specs[15].pool.clone(), specs[15].ehf.clone());
    specs.push(b16);
    // Signal (10, 5) expires at unsampled 6 and is back at unsampled 7, gone again by 8.
    // A check that waits for the next sampled height (12) never sees it.
    let digest = digest_with(&specs, |h, c| {
        if h == 6 {
            set(c, "ehf", Value::Arr(vec![]));
        }
        if h == 7 {
            set(c, "ehf", Value::Arr(vec![Value::Arr(vec![Value::Int(10), Value::Int(5)])]));
        }
    });
    let msg = format!(
        "{:#}",
        extract(&Fake::new(specs), &digest, &sample_span_4_16(), &mut Vec::new()).unwrap_err()
    );
    assert!(msg.contains("block 7") && msg.contains("came from nowhere"), "{msg}");
}

#[test]
fn unsampled_heights_enforce_era_nullness() {
    let specs = rich_chain();
    let sample = Options {
        sample: Some(
            crate::digest::sample::SampleSpec::parse(
                "every=100,edges=0,windows=0x1,seed=1,span=0..15",
            )
            .unwrap(),
        ),
        ..opts(false)
    };
    let outside = digest_with(&specs, |h, c| {
        if h == 1 {
            set(c, "mnl", Value::hash(&fill(9)));
        }
    });
    let msg = format!(
        "{:#}",
        extract(&Fake::new(specs.clone()), &outside, &sample, &mut Vec::new()).unwrap_err()
    );
    assert!(msg.contains("s.mnl is set at height 1, outside its era"), "{msg}");
    let inside = digest_with(&specs, |h, c| {
        if h == 6 {
            set(c, "cp", Value::Null);
        }
    });
    let msg =
        format!("{:#}", extract(&Fake::new(specs), &inside, &sample, &mut Vec::new()).unwrap_err());
    assert!(msg.contains("s.cp is null at height 6, inside its era"), "{msg}");
}

#[test]
fn sidecar_from_follows_the_first_written_height() {
    use crate::digest::sample::{RangeSidecar, sidecar_path};

    let specs = vec![spec(false, false), spec(false, false), spec(false, false)];
    let digest = digest_of(vec![
        header(Chain::Regtest),
        Record::Outcome(c_record(1, 1, Origin::Tip, &specs[1])),
        Record::Outcome(c_record(2, 2, Origin::Tip, &specs[2])),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("suffix.jsonl");
    extract_to_file(&Fake::new(specs), &digest, &opts(false), None, &out).unwrap();
    let sc = RangeSidecar::read(&sidecar_path(&out)).unwrap();
    assert_eq!((sc.from, sc.to), (1, 2));

    let specs = rich_chain();
    let kept: Vec<Record> = std::iter::once(header(Chain::Regtest))
        .chain((8..=15).map(|h| Record::Outcome(c_record(h, h, Origin::Tip, &specs[h as usize]))))
        .collect();
    let digest = digest_of(kept);
    let full = dir.path().join("from8.jsonl");
    extract_to_file(&Fake::new(specs.clone()), &digest, &opts(false), None, &full).unwrap();
    let sc = RangeSidecar::read(&sidecar_path(&full)).unwrap();
    assert_eq!(sc.from, 8, "the first C height, not a rotationinfo below it");
    let min_s = records(&std::fs::read(&full).unwrap())
        .iter()
        .filter_map(|r| match r {
            Record::Sparse(s) => Some(s.h),
            _ => None,
        })
        .min();
    assert_eq!(min_s, Some(7));

    let sparse = dir.path().join("from8-sparse.jsonl");
    extract_to_file(
        &Fake::new(specs),
        &digest,
        &Options {
            sparse_only: true,
            ..opts(false)
        },
        None,
        &sparse,
    )
    .unwrap();
    let sc = RangeSidecar::read(&sidecar_path(&sparse)).unwrap();
    assert_eq!(sc.from, 7, "sparse-only records the first S height");
}

#[test]
fn join_refuses_a_coin_key_at_an_unsampled_height() {
    use crate::digest::join::{CLinesSpec, join};
    use crate::digest::sample::sidecar_path;

    let specs = rich_chain();
    let digest = simple_digest(&specs);
    let dir = tempfile::tempdir().unwrap();
    let sample = "every=100,edges=0,windows=0x1,seed=1,span=0..15";
    let shard = |name: &str, from: i64, to: i64, sample: Option<&str>, sparse_only: bool| {
        let p = dir.path().join(name);
        extract_to_file(
            &Fake::new(specs.clone()),
            &digest,
            &Options {
                from: Some(from),
                to: Some(to),
                sparse_only,
                ..opts(false)
            },
            sample,
            &p,
        )
        .unwrap();
        p
    };
    let a = shard("a.jsonl", 0, 7, Some(sample), false);
    let b = shard("b.jsonl", 8, 15, Some(sample), false);
    let out = dir.path().join("out.jsonl");

    let mut lines: Vec<Vec<u8>> =
        std::fs::read(&a).unwrap().split_inclusive(|&x| x == b'\n').map(<[u8]>::to_vec).collect();
    let Record::Outcome(src) = dash_statedigest::parse_line(&lines[1]).unwrap() else {
        unreachable!()
    };
    let muhash = src.s().get("muhash").unwrap().clone();
    let Record::Outcome(mut at3) = dash_statedigest::parse_line(&lines[4]).unwrap() else {
        unreachable!()
    };
    assert_eq!(at3.id.h, 3);
    at3.s_mut().set("muhash", muhash).unwrap();
    lines[4] = Record::Outcome(at3).to_line();
    let planted = dir.path().join("planted.jsonl");
    std::fs::write(&planted, lines.concat()).unwrap();
    std::fs::copy(sidecar_path(&a), sidecar_path(&planted)).unwrap();
    let err = join(&[planted, b.clone()], &[], &BTreeSet::new(), &out).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("height 3") && msg.contains("coin-stat keys present at an unsampled height"),
        "{msg}"
    );

    // A dense --c-lines source for a range outside the span passes; inside the span it does not.
    let wide = "every=100,edges=0,windows=0x1,seed=1,span=10..15";
    let dense = shard("dense.jsonl", 0, 15, None, false);
    let early = shard("early.jsonl", 0, 4, Some(wide), true);
    let rest = shard("rest.jsonl", 5, 15, Some(wide), false);
    join(
        &[early, rest.clone()],
        &[CLinesSpec {
            path: dense.clone(),
            from: 0,
            to: 4,
        }],
        &BTreeSet::new(),
        &out,
    )
    .unwrap();
    let late = shard("late.jsonl", 11, 13, Some(wide), true);
    let tail = shard("tail.jsonl", 14, 15, Some(wide), false);
    let head = shard("head.jsonl", 0, 10, Some(wide), false);
    let err = join(
        &[head, late, tail],
        &[CLinesSpec {
            path: dense,
            from: 11,
            to: 13,
        }],
        &BTreeSet::new(),
        &out,
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("height 11") && msg.contains("coin-stat keys present at an unsampled height"),
        "{msg}"
    );
    let _ = b;
}
