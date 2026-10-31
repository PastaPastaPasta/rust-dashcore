//! LQ1b's L2 rotated replay (LQ1 §6.3.2 items 2 to 4): every LLMQ_60_75 cycle of mainnet or
//! testnet history, from DIP0024 activation, through the node's model and through Core's context,
//! from what a node reads (the work blocks' lists and coinbases) with the rows chained from cycle to
//! cycle. Per cycle it requires:
//! - the members of all 32 quorum indexes equal on both sides, with the full trace;
//! - the rotated `members_uncached` over a copy of the rows equal to the facade;
//! - the cycle's snapshot row byte-equal to the one Core stores (the per-cycle digest that stands
//!   in for `quorum rotationinfo`, which is not a pure read on the live oracles);
//! - every non-null mined commitment of the cycle verifying, signatures included, with Core's
//!   members (the signer-set certificate, §6.3.2 item 2).
//!
//! Ignored: it reads a pure-read collection (`LQ1B_L2_DIR=<dir>/<net>`, written by the collector
//! recorded in `test-vectors/lq1-rot/PROVENANCE.md`) and writes each cycle's Core row and the
//! capture cycles' members to `LQ1B_L2_OUT`, which the capture generator packages.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, Read, Write};
use std::sync::Arc;

use dash_consensus::BlockId;
use dash_dml::codec::core_disk::decode_list_s3;
use dash_dml::{Dmn, MnList};
use dash_evo::bls_ctx_after;
use dash_evo::llmq::QuorumSnapshot;
use dash_evo::llmq::reference::{ModelQuorumManager, ModelRows, ModelViews, RuntimeRows};
use dash_evo::llmq::{LlmqCtx, members_uncached};
use dash_oracle_sys::{
    Chain, Checkpoints, CoinsView, Context, DEPLOYMENT_V24, LlmqFixture, Member,
    MnList as CoreMnList, Oracle, Pin, QuorumMembers as CoreQuorum, ThresholdState, TraceEvent,
    Verdict,
};
use dash_params::{ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::{BlockHash, LlmqType};
use dash_types::Numeric;
use node_l1::llmq::{
    HeightDeployments, HeldChain, MapSource, MinedIndex, coinbase_facts, compare_members,
    core_event, header_hash, rows_digest, seed_mined, snapshot_bytes,
};

const ROTATED: u8 = 5;
const INTERVAL: u32 = 288;
const QUORUMS: u32 = 32;
const WORK_DIFF_DEPTH: u32 = 8;

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn read_gz(path: &std::path::Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(std::fs::File::open(path).expect("a collected list"))
        .read_to_end(&mut bytes)
        .expect("gzip");
    bytes
}

fn split_headers(bytes: &[u8]) -> Vec<[u8; 80]> {
    bytes.chunks(80).map(|h| h.try_into().expect("80 bytes")).collect()
}

fn params_for(net: &str) -> ChainParams {
    let network = match net {
        "mainnet" => NetworkKind::Mainnet,
        "testnet" => NetworkKind::Testnet,
        other => panic!("{other}"),
    };
    ChainParamsBuilder::new(network, Release::V24_0).build().expect("parameters")
}

/// (quorum index, commitment, mined height)
type Mined = (usize, Vec<u8>, i32);

fn mined_of(q: &serde_json::Value) -> Mined {
    (
        q["index"].as_u64().expect("an index") as usize,
        unhex(q["commitment"].as_str().expect("a commitment")),
        q["mined_height"].as_i64().expect("a height") as i32,
    )
}

/// A hash in RPC (reversed) hex.
fn rpc_hex(hash: &[u8; 32]) -> String {
    let mut h = *hash;
    h.reverse();
    hex(&h)
}

fn rpc_hexes(members: &[Member]) -> Vec<String> {
    members.iter().map(|m| rpc_hex(&m.pro_tx_hash)).collect()
}

fn dmns_of(members: &[Member]) -> Vec<Vec<u8>> {
    members.iter().map(|m| m.dmn.clone()).collect()
}

struct Cycle {
    cycle: u32,
    work: u32,
    coinbase: Vec<u8>,
    mined: Vec<Mined>,
}

fn cycles(dir: &std::path::Path) -> Vec<Cycle> {
    let file = std::fs::File::open(dir.join("cycles.jsonl")).expect("cycles.jsonl");
    let mut out: Vec<Cycle> = std::io::BufReader::new(file)
        .lines()
        .map(|line| {
            let v: serde_json::Value = serde_json::from_str(&line.expect("a line")).expect("json");
            Cycle {
                cycle: v["cycle"].as_u64().expect("a cycle") as u32,
                work: v["work"].as_u64().expect("a height") as u32,
                coinbase: unhex(v["coinbase"].as_str().expect("a coinbase")),
                mined: v["mined"].as_array().expect("mined").iter().map(mined_of).collect(),
            }
        })
        .collect();
    out.sort_by_key(|c| c.cycle);
    out.dedup_by_key(|c| c.cycle);
    out
}

#[test]
#[ignore = "needs the LQ1b L2 collection (LQ1B_L2_DIR, LQ1B_L2_OUT)"]
fn rotated_history_matches_core() {
    let dir = std::path::PathBuf::from(std::env::var("LQ1B_L2_DIR").expect("LQ1B_L2_DIR"));
    let out_dir = std::path::PathBuf::from(std::env::var("LQ1B_L2_OUT").expect("LQ1B_L2_OUT"));
    let meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("meta.json")).expect("meta")).expect("json");
    let net = meta["net"].as_str().expect("a network").to_owned();
    let chain_name = meta["chain"].as_str().expect("a chain").to_owned();
    let first = meta["first"].as_u64().expect("a height") as u32;
    let first_cycle = meta["first_cycle"].as_u64().expect("a height") as u32;
    let captures: Vec<u32> = meta["captures"]
        .as_array()
        .expect("captures")
        .iter()
        .map(|c| c.as_u64().expect("a height") as u32)
        .collect();
    let headers = split_headers(&std::fs::read(dir.join("headers.bin")).expect("headers"));
    let hashes: Vec<BlockHash> =
        headers.iter().map(|h| header_hash(h).expect("a header")).collect();
    for (i, h) in headers.iter().enumerate().skip(1) {
        assert_eq!(
            &h[4..36],
            hashes[i - 1].as_bytes(),
            "the headers link at {}",
            first as usize + i
        );
    }
    let chain = HeldChain::new(first, hashes.iter().copied());

    let params = params_for(&net);
    let core_chain = Chain::new(&chain_name, &[]);
    let oracle = Oracle::new(Pin::V24_0);
    let held = Held {
        net: &net,
        oracle: &oracle,
        core_chain: &core_chain,
        params: &params,
        chain: &chain,
        headers: &headers,
        first,
    };

    let mut source = MapSource::default();
    let mut lists: HashMap<u32, Vec<u8>> = HashMap::new();
    let mut coinbases: HashMap<u32, Vec<u8>> = HashMap::new();
    let mut rows = ModelRows::new();
    let mut core_rows: BTreeMap<u32, Vec<u8>> = BTreeMap::new();
    let mut manager = ModelQuorumManager::<Arc<Dmn>>::new(&params);
    let mut index = None;
    let mut rejected = Vec::new();
    let mut out = std::fs::File::create(out_dir.join(format!("{net}-replay.jsonl"))).expect("out");
    let (mut quorums, mut verified, mut cycles_done, mut nonempty) = (0, 0, 0, 0);
    let mut no_row = Vec::new();
    for rec in cycles(&dir) {
        let list = read_gz(&dir.join("lists").join(format!("{}.bin.gz", rec.work)));
        let decoded: MnList = decode_list_s3(&mut &list[..]).expect("an evodb list");
        source.lists.insert(rec.work, Arc::new(decoded));
        source.coinbases.insert(rec.work, coinbase_facts(&rec.coinbase, true).expect("a coinbase"));
        lists.insert(rec.work, list);
        coinbases.insert(rec.work, rec.coinbase.clone());
        let oldest = rec.work.saturating_sub(3 * INTERVAL);
        source.lists.retain(|h, _| *h >= oldest);
        source.coinbases.retain(|h, _| *h >= oldest);
        lists.retain(|h, _| *h >= oldest);
        coinbases.retain(|h, _| *h >= oldest);
        if rec.cycle < first_cycle {
            continue;
        }
        let cycle = rec.cycle;
        let window = cycle - 3 * INTERVAL - WORK_DIFF_DEPTH;
        let works: Vec<u32> = (0..4).map(|k| cycle - k * INTERVAL - WORK_DIFF_DEPTH).collect();
        let fixture = LlmqFixture {
            chain: chain_name.clone(),
            args: vec![],
            headers: (window..cycle + QUORUMS).map(|h| (h as i32, held.header(h))).collect(),
            lists: works
                .iter()
                .filter_map(|w| Some((held.hash(*w), lists.get(w)?.clone())))
                .collect(),
            coinbases: works
                .iter()
                .filter_map(|w| Some((held.hash(*w), coinbases.get(w)?.clone(), true)))
                .collect(),
            snapshots: (1..4)
                .filter_map(|k| {
                    let prev = cycle - k * INTERVAL;
                    Some((ROTATED, held.hash(prev), core_rows.get(&prev)?.clone()))
                })
                .collect(),
            mined: vec![],
        };
        let checked = check_cycle(
            &held,
            &source,
            &mut rows,
            &mut manager,
            &mut index,
            &fixture,
            cycle,
            &rec.mined,
        );
        quorums += QUORUMS as usize;
        nonempty += checked.members.iter().filter(|m| !m.is_empty()).count();
        verified += rec.mined.len() - checked.rejected.len();
        rejected.extend(checked.rejected.iter().map(|qi| (cycle, *qi)));
        let mut line = serde_json::json!({
            "cycle": cycle,
            "row": checked.row.as_deref().map(hex),
            "mined": rec.mined.len(),
            "rejected": checked.rejected,
        });
        if captures.contains(&cycle) {
            line["members"] =
                checked.members.iter().map(|m| rpc_hexes(m)).collect::<Vec<_>>().into();
        }
        writeln!(out, "{line}").expect("write");
        match checked.row {
            Some(row) => {
                core_rows.insert(cycle, row);
            }
            None => no_row.push(cycle),
        }
        core_rows.retain(|c, _| *c + 3 * INTERVAL >= cycle);
        cycles_done += 1;
        if cycles_done % 50 == 0 {
            eprintln!(
                "{net}: {cycles_done} cycles to {cycle}, {quorums} quorums, {verified} verified"
            );
        }
    }
    let digest = hex(&rows_digest(&rows));
    let summary = format!(
        "{net}: {cycles_done} cycles from {first_cycle}, {quorums} quorums ({nonempty} non-empty) \
         equal, {} rows equal (digest {digest}), no row (28e) at {no_row:?}, {verified} commitments \
         verified, {} rejected: {rejected:?}",
        rows.len(),
        rejected.len()
    );
    eprintln!("{summary}");
    std::fs::write(out_dir.join(format!("{net}-summary.txt")), summary + "\n").expect("summary");
    assert!(cycles_done > 0 && verified > 0);
    assert!(rejected.is_empty(), "mined commitments that do not verify: {rejected:?}");
}

/// What every checked cycle shares: the network, Core, the parameters and the held headers.
struct Held<'a> {
    net: &'a str,
    oracle: &'a Oracle,
    core_chain: &'a Chain,
    params: &'a ChainParams,
    chain: &'a HeldChain,
    headers: &'a [[u8; 80]],
    first: u32,
}

impl<'a> Held<'a> {
    fn deployments(&self) -> HeightDeployments<'a> {
        HeightDeployments {
            params: self.params,
        }
    }

    fn header(&self, height: u32) -> [u8; 80] {
        self.headers[(height - self.first) as usize]
    }

    fn hash(&self, height: u32) -> [u8; 32] {
        *self.chain.hash(height).expect("held").as_bytes()
    }
}

/// A checked cycle: the members Core computed per index, and the row it stored (none where it
/// returned early, 28e: fewer valid masternodes than a quarter, as on testnet at 838,368).
struct Checked {
    members: Vec<Vec<Member>>,
    row: Option<Vec<u8>>,
    /// The indexes whose mined commitment does not verify with Core's members.
    rejected: Vec<usize>,
}

/// One rotated cycle through Core's context (loaded with `fixture`) and through the node's model
/// (`source`, `rows`, `manager`): per index, the same members and trace, and the rotated
/// `members_uncached` over a copy of the rows equal to the facade; the same row stored for the
/// cycle; and every mined commitment (index, bytes, mined height) verifying, signatures
/// included, with Core's members.
#[allow(clippy::too_many_arguments)]
fn check_cycle(
    held: &Held<'_>,
    source: &MapSource,
    rows: &mut ModelRows,
    manager: &mut ModelQuorumManager<Arc<Dmn>>,
    index: &mut Option<MinedIndex>,
    fixture: &LlmqFixture,
    cycle: u32,
    mined: &[Mined],
) -> Checked {
    let net = held.net;
    let deployments = held.deployments();
    let mut core = held.oracle.llmq_context(held.core_chain).expect("a context");
    core.load(fixture).expect("the cycle loads");
    let bls = bls_ctx_after(&deployments, Some(BlockId(cycle)));
    let ctx = LlmqCtx::new(held.params, held.chain, &deployments, bls).expect("in domain");
    let reader = index.get_or_insert_with(|| {
        seed_mined(&[], held.params, &ctx, held.chain, source, &mut core)
            .expect("no divergence")
            .expect("no commitments")
    });
    let views = ModelViews {
        ctx: &ctx,
        src: source,
        reader,
        tip: BlockId(cycle + QUORUMS),
    };
    let ty = LlmqType::from_base(ROTATED);
    let key = (ROTATED, held.chain.hash(cycle).expect("held"));
    let mut members = Vec::new();
    let mut core_row = None;
    let mut uncached_row = None;
    for i in 0..QUORUMS {
        let base = cycle + i;
        let mut cold = rows.clone();
        let uncached =
            members_uncached(&ctx, source, &mut RuntimeRows::new(&mut cold), ty, BlockId(base))
                .expect("held");
        if i == 0 {
            // The first index computes the cycle's row; later ones find it in the copy.
            uncached_row = cold.get(&key).map(snapshot_bytes);
        }
        let ours = manager
            .members(&views, &mut RuntimeRows::new(rows), ty, BlockId(base), false)
            .expect("held");
        // Both read the same held lists, so equal members are the same entries.
        assert!(
            uncached.len() == ours.len()
                && uncached.iter().zip(ours.iter()).all(|(a, b)| Arc::ptr_eq(a, b)),
            "{net} members_uncached ≡ the facade at {base}"
        );
        let our_events: Vec<TraceEvent> = manager.take_events().iter().map(core_event).collect();
        let (theirs, events) =
            core.members(ROTATED, &held.hash(base), bls.is_legacy(), false).expect("members");
        assert_eq!(our_events, events, "{net} trace at {base}");
        assert_eq!(compare_members(&ours, &theirs), Ok(()), "{net} members at {base}");
        for e in events {
            if let TraceEvent::Row {
                snapshot,
                ..
            } = e
            {
                assert!(core_row.replace(snapshot).is_none(), "one row per cycle");
            }
        }
        members.push(theirs);
    }
    drop(core);
    let row = core_row;
    assert_eq!(rows.get(&key).map(snapshot_bytes), row, "{net} row at {cycle}");
    assert_eq!(uncached_row, row, "{net} members_uncached row at {cycle}");

    // The signer-set certificate: the mined commitments that do not verify with Core's members.
    let rejected = mined
        .iter()
        .filter(|(qi, qc, mined_height)| {
            let dmns = dmns_of(&members[*qi]);
            !verify(held, cycle + *qi as u32, *mined_height as u32, qc, dmns).is_accept()
        })
        .map(|(qi, ..)| *qi)
        .collect();
    Checked {
        members,
        row,
        rejected,
    }
}

/// `CFinalCommitment::Verify()` of `qc`, mined at `mined`, for the quorum based at `base` with
/// `members`, signatures included, in the scheme Core verified it with (legacy until V19 is
/// active at the mined block).
fn verify(held: &Held<'_>, base: u32, mined: u32, qc: &[u8], members: Vec<Vec<u8>>) -> Verdict {
    let deployments = held.deployments();
    let context = Context::new(
        (base - WORK_DIFF_DEPTH) as i32,
        (base - WORK_DIFF_DEPTH..=mined).map(|h| held.header(h)).collect(),
        bls_ctx_after(&deployments, Some(BlockId(mined - 1))).is_legacy(),
        vec![(DEPLOYMENT_V24, ThresholdState::Defined)],
        0,
        Checkpoints::Enabled {
            best_header_height: mined as i32,
        },
        CoinsView::Unchecked(vec![]),
        CoreMnList::Absent,
        vec![CoreQuorum {
            llmq_type: ROTATED,
            quorum_hash: held.hash(base),
            members,
        }],
    );
    held.oracle.verify_commitment(held.core_chain, qc, &context, true).expect("a verdict")
}

// ---- the rotated captures (test-vectors/lq1-rot, shared vectors) ----

fn read_compact(r: &mut &[u8]) -> usize {
    let (n, width) = match r[0] {
        0xfd => (u64::from(u16::from_le_bytes([r[1], r[2]])), 3),
        0xfe => (u64::from(u32::from_le_bytes(r[1..5].try_into().expect("4 bytes"))), 5),
        0xff => (u64::from_le_bytes(r[1..9].try_into().expect("8 bytes")), 9),
        n => (u64::from(n), 1),
    };
    *r = &r[width..];
    n as usize
}

/// A CompactSize bit count and its bits (LSB first), consumed from `r`.
fn read_bitset(r: &mut &[u8]) -> Vec<bool> {
    let bits = read_compact(r);
    let set = (0..bits).map(|i| r[i / 8] & (1 << (i % 8)) != 0).collect();
    *r = &r[bits.div_ceil(8)..];
    set
}

/// A row as Core serializes it, decoded (the inverse of `snapshot_bytes`, which re-encodes it).
fn snapshot_of(bytes: &[u8]) -> QuorumSnapshot {
    let r = &mut &bytes[..];
    let skip_list_mode = i32::from_le_bytes(r[..4].try_into().expect("4 bytes"));
    *r = &r[4..];
    let active_quorum_members = read_bitset(r);
    let skips = read_compact(r);
    let skip_list = (0..skips)
        .map(|i| i32::from_le_bytes(r[4 * i..4 * i + 4].try_into().expect("4 bytes")))
        .collect();
    assert_eq!(r.len(), 4 * skips, "a canonical row");
    let snapshot = QuorumSnapshot {
        active_quorum_members,
        skip_list_mode,
        skip_list,
    };
    assert_eq!(snapshot_bytes(&snapshot), bytes, "a canonical row");
    snapshot
}

/// LQ1 §6.4's rotated captures, one LLMQ_60_75 cycle per era and network (provenance in
/// `test-vectors/lq1-rot/PROVENANCE.md`): from the lists, coinbases and previous rows a node
/// holds, the node's model and Core's context give every index the vector's members, store the
/// vector's row, and each mined commitment verifies with those members while a signer swapped
/// with a non-signer does not.
#[test]
fn rotated_captures_match_core() {
    let dir = format!("{}/../test-vectors/lq1-rot", env!("CARGO_MANIFEST_DIR"));
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("the captures")
        .map(|e| e.expect("an entry").path())
        .filter(|p| p.to_string_lossy().ends_with(".json.gz"))
        .collect();
    paths.sort();
    let oracle = Oracle::new(Pin::V24_0);
    let (mut eras, mut rows_used, mut verified, mut with_commitments) = (Vec::new(), 0, 0, 0);
    let mut swaps = 0;
    for path in &paths {
        let v: serde_json::Value = serde_json::from_slice(&read_gz(path)).expect("json");
        let net = v["network"].as_str().expect("a network");
        let chain_name = v["chain"].as_str().expect("a chain");
        let cycle = v["cycle"].as_u64().expect("a cycle") as u32;
        let first = v["first_height"].as_u64().expect("a height") as u32;
        let headers = split_headers(&unhex(v["headers"].as_str().expect("headers")));
        let chain =
            HeldChain::new(first, headers.iter().map(|h| header_hash(h).expect("a header")));
        let params = params_for(net);
        let core_chain = Chain::new(chain_name, &[]);
        let held = Held {
            net,
            oracle: &oracle,
            core_chain: &core_chain,
            params: &params,
            chain: &chain,
            headers: &headers,
            first,
        };
        let dmns: Vec<Vec<u8>> = v["dmns"]
            .as_array()
            .expect("records")
            .iter()
            .map(|d| unhex(d.as_str().expect("hex")))
            .collect();
        let mut source = MapSource::default();
        let mut fixture = LlmqFixture {
            chain: chain_name.into(),
            args: vec![],
            headers: headers.iter().zip(first..).map(|(h, height)| (height as i32, *h)).collect(),
            lists: vec![],
            coinbases: vec![],
            snapshots: vec![],
            mined: vec![],
        };
        for list in v["lists"].as_array().expect("lists") {
            let height = list["height"].as_u64().expect("a height") as u32;
            let entries = list["entries"].as_array().expect("entries");
            let mut bytes = unhex(list["head"].as_str().expect("a head"));
            dash_consensus::primitives::write_compact_size(entries.len() as u64, &mut bytes);
            for e in entries {
                bytes.extend(&dmns[e.as_u64().expect("an index") as usize]);
            }
            let list = decode_list_s3(&mut &bytes[..]).expect("an evodb list");
            source.lists.insert(height, Arc::new(list));
            fixture.lists.push((held.hash(height), bytes));
        }
        for cb in v["coinbases"].as_array().expect("coinbases") {
            let height = cb["height"].as_u64().expect("a height") as u32;
            let tx = unhex(cb["tx"].as_str().expect("a coinbase"));
            source.coinbases.insert(height, coinbase_facts(&tx, true).expect("a coinbase"));
            fixture.coinbases.push((held.hash(height), tx, true));
        }
        let mut rows = ModelRows::new();
        for row in v["rows"].as_array().expect("rows") {
            let at = row["cycle"].as_u64().expect("a cycle") as u32;
            let bytes = unhex(row["snapshot"].as_str().expect("a row"));
            rows.insert((ROTATED, chain.hash(at).expect("held")), snapshot_of(&bytes));
            fixture.snapshots.push((ROTATED, held.hash(at), bytes));
            rows_used += 1;
        }
        let quorums = v["quorums"].as_array().expect("quorums");
        let mined: Vec<Mined> =
            quorums.iter().filter(|q| q.get("commitment").is_some()).map(mined_of).collect();
        // None in the first rotated cycles (nothing mined before DIP0024's quorums height) and
        // in testnet's 28e cycle.
        with_commitments += usize::from(!mined.is_empty());
        let mut manager = ModelQuorumManager::<Arc<Dmn>>::new(&params);
        let checked = check_cycle(
            &held,
            &source,
            &mut rows,
            &mut manager,
            &mut None,
            &fixture,
            cycle,
            &mined,
        );
        assert_eq!(
            checked.row.as_deref().map(hex).as_deref(),
            v["row"].as_str(),
            "{net} {cycle} row"
        );
        let rejected: Vec<usize> = quorums
            .iter()
            .filter(|q| q["verifies"] == false)
            .map(|q| q["index"].as_u64().expect("an index") as usize)
            .collect();
        assert_eq!(checked.rejected, rejected, "{net} {cycle}: the commitments that verify");
        for (q, members) in quorums.iter().zip(&checked.members) {
            let want: Vec<&str> = q["members"]
                .as_array()
                .expect("members")
                .iter()
                .map(|m| m.as_str().expect("hex"))
                .collect();
            assert_eq!(rpc_hexes(members), want, "{net} {cycle} index {}", q["index"]);
        }
        verified += mined.len() - rejected.len();
        // Discrimination: a signer swapped with a non-signer of the same quorum fails Core's
        // signature check (the first verified commitment that has a non-signer).
        let swap =
            mined.iter().filter(|(qi, ..)| !rejected.contains(qi)).find_map(|(qi, qc, h)| {
                let signers = commitment_signers(qc);
                let signer = signers.iter().position(|s| *s)?;
                let non_signer =
                    signers.iter().take(checked.members[*qi].len()).position(|s| !*s)?;
                Some((*qi, qc, *h, signer, non_signer))
            });
        if let Some((qi, qc, mined_height, signer, non_signer)) = swap {
            let mut swapped = dmns_of(&checked.members[qi]);
            swapped.swap(signer, non_signer);
            match verify(&held, cycle + qi as u32, mined_height as u32, qc, swapped) {
                Verdict::Reject(r) => assert!(
                    r.debug_message.ends_with("invalid aggregated members signature"),
                    "{net} {cycle}: {r:?}"
                ),
                Verdict::Accept => panic!("{net} {cycle}: a swapped signer verifies"),
            }
            swaps += 1;
        }
        eras.push((net.to_owned(), cycle));
    }
    // LQ1 §6.3.2 item 4's eras on both networks (within the mainnet oracle's height bound), and
    // testnet's early return (28e).
    assert_eq!(eras.len(), 12, "{eras:?}");
    assert!(rows_used >= 3 * 9, "{rows_used}");
    assert_eq!(with_commitments, 9, "captures with mined commitments");
    // Two captures mine only commitments that every member signed: nothing to swap with.
    assert_eq!(swaps, with_commitments - 2, "swapped-signer checks");
    // Every mined commitment of the captures (none of which Core rejects).
    assert_eq!(verified, 228, "commitments verified");
}

/// The signers bitset of a `CFinalCommitment` (network serialization).
fn commitment_signers(qc: &[u8]) -> Vec<bool> {
    let version = u16::from_le_bytes([qc[0], qc[1]]);
    // nVersion, llmqType, quorumHash, then quorumIndex in the indexed versions (2 and 4).
    let index_len = if version.is_multiple_of(2) {
        2
    } else {
        0
    };
    read_bitset(&mut &qc[2 + 1 + 32 + index_len..])
}
