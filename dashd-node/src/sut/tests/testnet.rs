//! CH7b-2 C16 (PLAN §5.3, T-TN): testnet connects through its first non-null commitments.
//!
//! `CH7B2_TESTNET_WINDOW` names a directory whose `blocks/` holds a testnet datadir's block files
//! (a reflink copy of O8's stopped node's). The node connects genesis to [`H_END`] under
//! `-assumevalid=0`, so every block runs its script checks and with them the CbTx's root checks
//! (`fCheckCbTxMerkleRoots`, v24.0.0-rc.4 `validation.cpp:2669-2671`): step 8's MN-list root at
//! exactly the heights whose coinbase is a CbTx (`evo/specialtxman.cpp:984-996`), step 9's quorum
//! root at exactly those whose CbTx is version 2 or more (`:1003-1011`; none below DIP0008's
//! 78,800), and none skipped. The first non-null commitment, an LLMQ_50_60 one mined at 7,522
//! (DIP0003 at 7,000), connects with its members, and two of its DKG intervals past it. A CbTx's
//! roots commit to the MN list and the mined quorums Core built, so each connect is a
//! differential against the chain Core accepted. Past the first rotated cycle (base 769,824), the
//! heights from the first block that mines a rotated commitment are step 9 over rotated rows.
//!
//! Without the variable the test says it skipped. `CH7B2_TESTNET_TO=<height>` runs it further:
//! the one-off run to 905,099, the last block before V20's CbTx ChainLocks, whose step 10 needs
//! CH7b-3's verify.
//!
//! CH7a-5b C4, the state-hash mode: with `CH7A5B_TESTNET_GOLDEN=<slice dir>` and
//! `CH7A5B_OWNER_REPORT=<dir>` (both or neither), the node writes a state-hash log into the owner
//! directory from genesis, `muhash` at every block, and after the connect to [`H_END`] that log
//! must equal SW-V24's slice of its rc.5 testnet recording over 0..7,570 on every key
//! ([`linear_clean`]). The evo keys are null below DIP0003 (7,000); from there `mnl` and `qc` are
//! set, with the first non-null commitment's row at 7,522, and `cp` and `ehf` stay null (no V20).
//! - The slice is checked before the connect (DEC-1061): its sha256, compressed and as its
//!   decompressed stream, `provenance.json`'s Core describe and commit (the rc.5 recording's,
//!   not CH3a D146's), the full golden's sha256 (`FULL-GOLDEN.sha256` and provenance's
//!   `full_golden_sha256`, OD-CP-53), `H.chain` = testnet, and its last `C` at [`H_END`]. Each
//!   golden `C`'s block and header keys are the located block's (CH3a D146).
//! - Run2 pending (DEC-1082): the slice is from SW-V24's first run; its second run's
//!   determinism check has not confirmed it yet.
//! - A failure message carries heights and key names only; the values go to the owner report,
//!   `owner-report.txt` (mode 0600) in `CH7A5B_OWNER_REPORT`. The run goes through the
//!   owner-only runner of CH7a-5b's plan (§5), never a shared console.
//!
//! The span is 0..7,570 only: the compare refuses `CH7B2_TESTNET_TO`. Without the two variables
//! the test says it ran without the compare.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::PathBuf;

use dash_consensus::Block;
use dash_consensus::primitives::{decode_block, decode_header};
use dash_primitives::QuorumHash;
use dash_statedigest::{Chain, Outcome, Record, Value};
use dashd_chainstate::chain::OnV;
use dashd_chainstate::connect::MuHashAt;
use dashd_chainstate::evo::gates::counts_under;

use super::state_hash::{DigestRun, EVO_KEYS, linear_clean_noting, projected, read_records};
use super::*;

/// The first non-null testnet commitment's block: LLMQ_50_60 (`dkgInterval` 24), at 7,522.
const FIRST_QC: (u32, &str) =
    (7_522, "000000001c500ea39146e911f9f7630a389af7c603ab48d89b58b5d0b424b43d");
/// [`FIRST_QC`] and two of its type's DKG intervals.
const H_END: (u32, &str) =
    (7_570, "00000000b5f7ec21d990e84ab593a3a5e42185237ebf27bdd32a08769edaa34c");
/// A height by which testnet's first rotated cycle, based at 769,824, has mined its commitments.
const ROTATED_BY: u32 = 769_824 + 288;
/// The indexed (rotated) commitment versions, legacy and basic BLS (`llmq/commitment.h:48-51`).
const INDEXED: [u16; 2] = [2, 4];

/// CH7a-5b C4's slice (DEC-1061): SW-V24's cut of its rc.5 testnet recording, `H` and the `C`s
/// of 0..[`H_END`]. Run2 pending (DEC-1082).
const SLICE: &str = "golden-0-7570.jsonl.zst";
/// The slice's sha256: the file's, and its decompressed stream's (`SHA256SUMS`).
const SLICE_SHA256: (&str, &str) = (
    "ccb8f059877fe23141eb307f011c00df7b6bf0cac95e54d520a8b9fdb79f9102",
    "a11b49056ecaac74819a957961ddafb7b1854ed8c9008371ba7ad8ae6288b226",
);
/// The sha256 of the full golden the slice was cut from (`FULL-GOLDEN.sha256`).
const FULL_GOLDEN_SHA256: &str = "2597ed0d0619daafa0c658f3cc8aa40fcada181e8cb1c34c905a5f012c9862a1";
/// The recording's Core (`provenance.json`'s `core`): describe and commit.
const SLICE_CORE: (&str, &str) =
    ("v24.0.0-rc.5-16-g8a8e56406e13", "8a8e56406e135a667b032b8249fcd75c17337b2c");
/// Testnet's DIP0003 height: below it every evo key is null.
const DIP3: i64 = 7_000;

/// C4's compare: the slice's directory and the owner report's.
struct Compare {
    slice: PathBuf,
    owner: PathBuf,
}

impl Compare {
    /// Both variables, or neither: no compare.
    fn from_env() -> Option<Compare> {
        let slice = std::env::var_os("CH7A5B_TESTNET_GOLDEN").map(PathBuf::from);
        let owner = std::env::var_os("CH7A5B_OWNER_REPORT").map(PathBuf::from);
        match (slice, owner) {
            (Some(slice), Some(owner)) => Some(Compare {
                slice,
                owner,
            }),
            (None, None) => None,
            _ => panic!("CH7A5B_TESTNET_GOLDEN and CH7A5B_OWNER_REPORT go together"),
        }
    }

    /// Appends `what` to the owner report (mode 0600).
    fn note(&self, what: &str) {
        use std::io::Write as _;
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts.open(self.owner.join("owner-report.txt")).expect("the owner report");
        writeln!(f, "{what}").expect("the owner report");
    }

    /// Fails with `msg`, which carries no value, after noting `detail` in the owner report.
    fn fail(&self, msg: &str, detail: &str) -> ! {
        self.note(&format!("{msg}: {detail}"));
        panic!("{msg} (details in the owner report)");
    }

    /// The slice's records, once its pins hold: its sha256s, the full golden's, its Core and
    /// network, and its `H`.
    fn pinned_slice(&self) -> Vec<Record> {
        let path = self.slice.join(SLICE);
        let zst = std::fs::read(&path).unwrap_or_else(|e| self.fail("no slice", &e.to_string()));
        let plain = zstd::decode_all(zst.as_slice())
            .unwrap_or_else(|e| self.fail("the slice does not decompress", &e.to_string()));
        for (what, bytes, want) in
            [("file", &zst, SLICE_SHA256.0), ("decompressed stream", &plain, SLICE_SHA256.1)]
        {
            let got = sha256_hex(bytes);
            if got != want {
                self.fail(&format!("the slice's {what} sha256"), &format!("{got}, want {want}"));
            }
        }
        let provenance = std::fs::read(self.slice.join("provenance.json"))
            .unwrap_or_else(|e| self.fail("no provenance.json", &e.to_string()));
        let provenance: serde_json::Value = serde_json::from_slice(&provenance)
            .unwrap_or_else(|e| self.fail("provenance.json does not parse", &e.to_string()));
        for (key, want) in [("describe", SLICE_CORE.0), ("commit", SLICE_CORE.1)] {
            let got = provenance["core"][key].as_str();
            if got != Some(want) {
                self.fail(&format!("provenance's core.{key}"), &format!("{got:?}, want {want}"));
            }
        }
        let full = std::fs::read_to_string(self.slice.join("FULL-GOLDEN.sha256"))
            .unwrap_or_else(|e| self.fail("no FULL-GOLDEN.sha256", &e.to_string()));
        for (what, got) in [
            ("FULL-GOLDEN.sha256", full.split_whitespace().next()),
            ("provenance's full_golden_sha256", provenance["full_golden_sha256"].as_str()),
        ] {
            if got != Some(FULL_GOLDEN_SHA256) {
                self.fail(what, &format!("{got:?}, want {FULL_GOLDEN_SHA256}"));
            }
        }
        let network = provenance["network"].as_str();
        if network != Some("testnet") {
            self.fail("provenance's network", &format!("{network:?}"));
        }
        let records = read_records(&path);
        match records.first() {
            Some(Record::Header(h)) if h.chain == Chain::Test => {}
            other => self.fail("the slice's H is not testnet's", &format!("{other:?}")),
        }
        records
    }

    /// The slice's `C`s are the located chain to [`H_END`] (CH3a D146): one per height, its
    /// block the located one, its header keys that block's header.
    fn golden_is_the_located_chain(
        &self,
        golden: &[(usize, Outcome)],
        chain: &[(BlockHash, Located)],
    ) {
        let want_n = H_END.0 as usize + 1;
        if golden.len() != want_n {
            self.fail("the slice's C count", &format!("{}, want {want_n}", golden.len()));
        }
        for (h, ((_, o), (hash, located))) in golden.iter().zip(chain).enumerate() {
            if o.id.h != i64::try_from(h).expect("a height") || o.id.b != display(*hash) {
                self.fail(
                    &format!("{h}: the slice's C is not the located block"),
                    &format!("{:?}", o.id),
                );
            }
            let block = located.read();
            let header = block.header();
            let prev = h.checked_sub(1).map_or(Value::Null, |p| Value::hash(&display(chain[p].0)));
            let want = [
                ("prev", prev),
                ("ver", Value::Int(header.version().into())),
                ("mrkl", Value::hash(&display(header.merkle_root()))),
                ("time", Value::Int(header.time().into())),
                ("bits", Value::Int(header.bits().into())),
                ("nonce", Value::Int(header.nonce().into())),
            ];
            for (key, want) in want {
                let got = o.s().get(key);
                if got != Some(&want) {
                    self.fail(
                        &format!("{h}: the slice's {key} is not the located block's"),
                        &format!("{got:?}, want {want:?}"),
                    );
                }
            }
        }
        let last = &golden[golden.len() - 1].1.id;
        if last.b.to_string() != H_END.1 {
            self.fail("the slice's last C is not H_END", &format!("{last:?}"));
        }
    }
}

/// The compare's node UI: the node's log lines on stderr, which the owner-only runner captures
/// (an `AbortNode` among them).
fn owner_ui() -> Arc<Ui> {
    let logger = Arc::new(Logger::new());
    logger.push_back_callback(|line| eprintln!("{}", line.trim_end_matches('\n')));
    assert!(logger.start_logging());
    Arc::new(Ui::with_stderr(logger, Box::new(|_| {})))
}

/// `bytes`' sha256 as `sha256sum` prints it.
fn sha256_hex(bytes: &[u8]) -> String {
    dash_statedigest::Hash256::from_display_bytes(dash_sha256::sha256(bytes)).to_hex()
}

/// A hash's display text as a record's hash.
fn display(hash: impl std::fmt::Display) -> dash_statedigest::Hash256 {
    let text = format!("{hash:0>64}");
    dash_statedigest::Hash256::from_hex(text.as_bytes()).expect("a 256-bit hash")
}

/// The run's evo keys by era (C4): all null below DIP0003; from it `mnl` and `qc` set, `cp` and
/// `ehf` null (no V20 in the span); the first non-null commitment's row in 7,522's `qc`.
fn evo_keys_by_era(log: &[(usize, Outcome)]) {
    let null = |o: &Outcome, key: &str| o.s().get(key).is_some_and(Value::is_null);
    for (_, o) in log {
        let h = o.id.h;
        if h < DIP3 {
            for key in EVO_KEYS {
                assert!(null(o, key), "{h}: {key} is set below DIP0003");
            }
        } else {
            for key in ["mnl", "qc"] {
                assert!(o.s().get(key).is_some_and(|v| !v.is_null()), "{h}: {key} is null");
            }
            for key in ["cp", "ehf"] {
                assert!(null(o, key), "{h}: {key} is set before V20");
            }
        }
    }
    let first_qc = log.iter().find(|(_, o)| o.id.h == i64::from(FIRST_QC.0)).expect("7,522's C");
    let rows = first_qc.1.s().get("qc");
    assert!(matches!(rows, Some(Value::Arr(rows)) if !rows.is_empty()), "7522: qc has no row");
}

/// Where a located block's bytes are.
struct Located {
    file: PathBuf,
    offset: u64,
    size: usize,
}

impl Located {
    fn read(&self) -> Arc<Block> {
        let mut f = File::open(&self.file).unwrap();
        f.seek(SeekFrom::Start(self.offset)).unwrap();
        let mut raw = vec![0; self.size];
        f.read_exact(&mut raw).unwrap();
        let block = decode_block(&mut raw.as_slice()).expect("a block");
        Arc::new(block.into_value())
    }
}

/// The chain from genesis to the first block met at height `to`, located in `blocks`'s
/// `blk?????.dat` files, read in file order: each record's header is hashed and linked to its
/// parent, so the heights follow from genesis.
fn locate(blocks: &Path, magic: [u8; 4], genesis: BlockHash, to: u32) -> Vec<(BlockHash, Located)> {
    assert!(!blocks.join("xor.dat").exists(), "an obfuscated copy");
    let mut files: Vec<PathBuf> = std::fs::read_dir(blocks)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_str().unwrap();
            name.starts_with("blk") && name.ends_with(".dat")
        })
        .collect();
    files.sort();
    let mut height = HashMap::from([(genesis, 0_u32)]);
    let mut parent: HashMap<BlockHash, BlockHash> = HashMap::new();
    let mut at: HashMap<BlockHash, Located> = HashMap::new();
    let mut waiting: HashMap<BlockHash, Vec<BlockHash>> = HashMap::new();
    let mut target = None;
    'files: for file in files {
        let bytes = std::fs::read(&file).unwrap();
        let mut i = 0;
        while i + 8 <= bytes.len() && bytes[i..i + 4] != [0; 4] {
            assert_eq!(bytes[i..i + 4], magic, "{}@{i}: a record", file.display());
            let size = u32::from_le_bytes(bytes[i + 4..i + 8].try_into().unwrap()) as usize;
            let offset = i + 8;
            i = offset + size;
            let Some(raw) = bytes.get(offset..offset + 80) else {
                break;
            };
            let header = decode_header(&mut &raw[..]).expect("a header").into_value();
            let (hash, prev) = (header.hash(), header.prev_hash());
            let file = file.clone();
            at.insert(
                hash,
                Located {
                    file,
                    offset: offset as u64,
                    size,
                },
            );
            parent.insert(hash, prev);
            let Some(&h) = height.get(&prev) else {
                waiting.entry(prev).or_default().push(hash);
                continue;
            };
            // `hash` and every block waiting on it, at their heights.
            let mut linked = vec![(hash, h + 1)];
            while let Some((hash, h)) = linked.pop() {
                height.insert(hash, h);
                if h == to {
                    target = Some(hash);
                    break 'files;
                }
                let children = waiting.remove(&hash).unwrap_or_default();
                linked.extend(children.into_iter().map(|c| (c, h + 1)));
            }
        }
    }
    let mut chain = vec![target.unwrap_or_else(|| panic!("no block at {to}"))];
    while chain.len() <= to as usize {
        chain.push(parent[chain.last().unwrap()]);
    }
    assert_eq!(chain.last(), Some(&genesis), "linked to genesis");
    chain.reverse();
    // Genesis's own record is in the files too.
    chain.into_iter().map(|hash| (hash, at.remove(&hash).expect("located"))).collect()
}

/// The coinbase's CbTx version: `None` for a coinbase that is no CbTx (`nVersion` 3, type 5).
fn cbtx_version(block: &Block) -> Option<u16> {
    let cb = block.txs()[0].to_mutable();
    let special = cb.version >= 3 && cb.tx_type == TxType::new(5);
    special.then(|| u16::from_le_bytes(cb.extra_payload[..2].try_into().unwrap()))
}

/// The LLMQ type, quorum hash and commitment version of each of `block`'s non-null commitments
/// (type 6): its payload's version and height, then the commitment's version, type and quorum
/// hash (`llmq/commitment.h:95-110`, `:158-161`).
fn non_null_commitments(block: &Block) -> Vec<(dash_params::LlmqType, QuorumHash, u16)> {
    let commitment = |tx: &Arc<Tx>| {
        let tx = tx.to_mutable();
        let p = &tx.extra_payload;
        let null = p.len() < 192 || p[p.len() - 192..].iter().all(|&b| b == 0);
        (tx.version >= 3 && tx.tx_type == TxType::new(6) && !null).then(|| {
            let version = u16::from_le_bytes(p[6..8].try_into().unwrap());
            let hash: [u8; 32] = p[9..41].try_into().unwrap();
            (dash_params::LlmqType::new(p[8]), QuorumHash::from(hash), version)
        })
    };
    block.txs().iter().filter_map(commitment).collect()
}

/// Where `got` and `want` part, for an assertion's message: a run to 905,099 records some 900,000
/// heights.
fn parting(got: &[u32], want: &[u32]) -> String {
    let at = got.iter().zip(want).position(|(g, w)| g != w).unwrap_or(got.len().min(want.len()));
    let (g, w) = (got.get(at), want.get(at));
    format!("{} heights, want {}; at #{at} {g:?}, want {w:?}", got.len(), want.len())
}

/// PLAN §5.3's T-TN.
#[test]
fn testnet_connects_through_its_first_non_null_commitments() {
    let compare = Compare::from_env();
    let Some(window) = std::env::var_os("CH7B2_TESTNET_WINDOW").map(PathBuf::from) else {
        assert!(
            compare.is_none(),
            "the state-hash compare (CH7A5B_TESTNET_GOLDEN, CH7A5B_OWNER_REPORT) needs \
             CH7B2_TESTNET_WINDOW"
        );
        eprintln!(
            "\n*** SKIPPED testnet_connects_through_its_first_non_null_commitments: \
             CH7B2_TESTNET_WINDOW is not set. Set it to a directory whose blocks/ holds a \
             testnet datadir's block files (PLAN §5.3). ***\n"
        );
        return;
    };
    let to = std::env::var("CH7B2_TESTNET_TO").map_or(H_END.0, |t| t.parse().expect("a height"));
    if compare.is_some() {
        assert_eq!(to, H_END.0, "the state-hash compare spans 0..7,570 only");
    }
    // The slice's pins before the connect.
    let golden = compare.as_ref().map(Compare::pinned_slice);
    let probe = tempfile::tempdir().unwrap();
    let flags = ["-testnet", "-printtoconsole=0"];
    let p = NodeSut::new(probe.path(), &flags, MOCKTIME, ui()).unwrap().params.chain.clone();
    let genesis = dashd_chainstate::chain::genesis_block(&p).hash();
    let chain = locate(&window.join("blocks"), p.message_start, genesis, to);
    let hash = |h: u32| chain[h as usize].0;
    for (h, want) in [FIRST_QC, H_END].into_iter().filter(|&(h, _)| h <= to) {
        assert_eq!(hash(h).to_string(), want, "the located block at {h}");
    }
    if let (Some(compare), Some(golden)) = (&compare, &golden) {
        compare.golden_is_the_located_chain(&projected("golden", golden), &chain);
    }

    // A day past the last block, so none is too far ahead of the clock.
    let last = chain.last().unwrap().1.read();
    let mocktime = i64::from(last.header().time()) + 86_400;
    let dir = tempfile::tempdir().unwrap();
    let argv = ["-testnet", "-printtoconsole=0", "-assumevalid=0"];
    let (mut sut, state_log) = match &compare {
        Some(compare) => {
            let (run, sink) = DigestRun::on(&compare.owner, MuHashAt::Every, Chain::Test);
            let sut = NodeSut::new_with_digest(dir.path(), &argv, mocktime, owner_ui(), sink);
            (sut.expect("accepted"), Some(run))
        }
        None => (NodeSut::new(dir.path(), &argv, mocktime, ui()).expect("accepted"), None),
    };
    imported(&mut sut);
    let (mut s8, mut s9, mut first, mut first_rotated) = (Vec::new(), Vec::new(), None, None);
    for (h, (hash, located)) in (0_u32..).zip(&chain).skip(1) {
        let b = located.read();
        assert_eq!(b.hash(), *hash, "{h}: the located bytes");
        if let Some(version) = cbtx_version(&b) {
            s8.push(h);
            if version >= 2 {
                s9.push(h);
            }
        }
        let commitments = non_null_commitments(&b);
        if first.is_none() {
            first = commitments.first().map(|&(llmq_type, quorum, _)| (h, (llmq_type, quorum)));
        }
        if first_rotated.is_none() && commitments.iter().any(|c| INDEXED.contains(&c.2)) {
            first_rotated = Some(h);
        }
        let r = sut.chain().unwrap().process_block(b, true, None, true).recv();
        let r = r.expect("V replies");
        let connected = r.accepted && r.state.is_valid() && view_tip(&sut) == *hash;
        assert!(connected, "{h}: accepted {}, {}, tip {}", r.accepted, r.state, view_tip(&sut));
    }
    assert_eq!(view_tip(&sut), hash(to));
    match (&compare, &state_log) {
        (Some(compare), Some(run)) => {
            let slice = compare.slice.join(SLICE);
            let end = i64::from(H_END.0);
            let note = |r: &dash_statedigest::compare::Report| compare.note(&format!("{r:#?}"));
            let clean = linear_clean_noting(run, &slice, 0, end, note);
            evo_keys_by_era(&projected("rdashd", &run.records()));
            eprintln!("T-TN state-hash compare: {} heights equal on every key", clean.pairs);
        }
        _ => eprintln!(
            "T-TN ran without the state-hash compare: CH7A5B_TESTNET_GOLDEN and \
             CH7A5B_OWNER_REPORT are not set (CH7a-5b C4)"
        ),
    }

    let roots = counts_under(dir.path()).expect("the node's cells").root_checks();
    assert!(roots.mn_list == s8, "S8: {}", parting(&roots.mn_list, &s8));
    assert!(roots.quorums == s9, "S9: {}", parting(&roots.quorums, &s9));
    let unchecked = &roots.unchecked;
    assert!(
        unchecked.is_empty(),
        "{} judged without the flag, from {:?}",
        unchecked.len(),
        unchecked.first()
    );
    // Step 9 over rotated rows: each CbTx's quorum root from the first rotated commitment's block.
    if to >= ROTATED_BY {
        assert!(first_rotated.is_some(), "a rotated commitment by {ROTATED_BY}");
    }
    let rotated = |s: &[u32]| -> Vec<u32> {
        s.iter().copied().filter(|&h| first_rotated.is_some_and(|r| h >= r)).collect()
    };
    let (s9_rotated, checked_rotated) = (rotated(&s9), rotated(&roots.quorums));
    assert!(
        checked_rotated == s9_rotated,
        "S9 rotated: {}",
        parting(&checked_rotated, &s9_rotated)
    );
    eprintln!(
        "T-TN to {to} ({}): S8 {} heights from {:?}, S9 {} from {:?}, rotated {} from {:?}",
        hash(to),
        s8.len(),
        s8.first(),
        s9.len(),
        s9.first(),
        s9_rotated.len(),
        first_rotated
    );

    // The first non-null commitment, through route B at the tip: mined at its height, with its
    // members.
    let (mined_at, (llmq_type, quorum_hash)) = first.expect("a non-null commitment");
    assert_eq!(mined_at, FIRST_QC.0);
    let quorum = sut.chain().unwrap().on_v(move |o: &OnV<'_>, q: &ChainQuery<'_>| {
        let quorum = q.bind(o).quorum(llmq_type, &quorum_hash).map_err(|e| e.to_string());
        quorum.map(|got| got.map(|q| (q.mined_block, q.members.len())))
    });
    let (mined, members) = quorum.recv().expect("served").expect("no fault").expect("mined");
    assert_eq!(mined, hash(FIRST_QC.0));
    assert!(members > 0, "a non-null commitment's quorum has members");
    eprintln!(
        "T-TN: the first non-null commitment, {llmq_type:?} at {mined_at}: {members} members"
    );
}
