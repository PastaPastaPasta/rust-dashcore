//! LQ2's L1 differentials over Core-produced commitments (`designs/LQ2-notes.md`):
//! - D20: `commitment_hash` is the double SHA-256 of the bytes Core mined, for every commitment
//!   of the rotated captures (`test-vectors/lq1-rot`, provenance in its `PROVENANCE.md`);
//! - D11: the mined-signature twin. Every captured commitment, with the members Core gave its
//!   quorum, through `verify_mined_signatures` (the seam `process_block`'s `fBLSChecks` calls) and
//!   `verify_commitment(…, Mined)`, against Core's mined path; and the same commitments as blocks
//!   through `process_block`, against the per-commitment verdicts;
//! - D17: the L3 QcTx BLS-encoding operators on QcTx payloads Core produced, mined non-null
//!   commitments and the null ones Core's miner builds, through `check_special_tx_inner` (type 6's
//!   arm, `check_llmq_commitment`, EV1-5b-1) against Core's `CheckSpecialTx`; and EV1 D19's first
//!   reject for every pair of the operators' steps (`6c` in `contrib/node/ev1-pairs.toml`).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::io::Read;
use std::sync::Arc;

use dash_consensus::ctx::CheckpointView;
use dash_consensus::hash::sha256d;
use dash_consensus::primitives::{
    Block, Tx, core_serialize, decode_block, decode_tx, write_commitment,
};
use dash_consensus::{BlockId, ChainAncestry, ConsensusCtx, HeaderCtx};
use dash_dml::blockprocessor::{
    BlockLookup, QcBlockAt, QcBlockError, QcCtx, QcMode, commitment_hash, process_block,
};
use dash_dml::codec::core_disk::decode_dmn;
use dash_dml::netinfo::NetInfoRules;
use dash_dml::specialtx::qc::{CommitmentVerifier, NodeCommitmentVerifier};
use dash_dml::specialtx::sig::SerialVerifier;
use dash_dml::specialtx::{QuorumSeams, SpecialTxCtx, SpecialTxRules, check_special_tx_inner};
use dash_dml::store::MinedCommitmentRecord;
use dash_dml::testkit::replay::{CORE_NET_INFO, Coins, mainnet};
use dash_dml::{Dmn, MemEvoStore, MnList, StoreError};
use dash_evo::bls_ctx_after;
use dash_evo::ctx::EvoCtx;
use dash_evo::effect::{Effect, EffectSink};
use dash_evo::llmq::{
    BlsCheckRunner, CommitmentError, CommitmentReject, LlmqChain, LlmqCommitment, LlmqCtx,
    MemberError, PrecomputeMode, QuorumMemberSource, QuorumMembers, QuorumPrecompute,
    SerialBlsChecks, SigPath, verify_commitment, verify_mined_signatures,
};
use dash_oracle_sys::{
    Chain, Checkpoints, CoinsView, Context, DEPLOYMENT_V24, LlmqFixture, MinedVerdict,
    MnList as CoreMnList, Oracle, Pin, QuorumMembers as CoreQuorum, ThresholdState, Verdict,
};
use dash_params::LlmqType;
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, Network, Release};
use dash_pkc::bls::BlsSchemeKind;
use dash_primitives::{Transaction, TxType};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashd_chainstate::checkqueue::bls::{
    BlsCheckQueue, DEFAULT_BLSCHECK_THREADS, bls_check_threads,
};
use node_l1::llmq::{HeightDeployments, HeldChain, ViewError, header_hash};

/// LLMQ_60_75, the rotated type of the captures.
const ROTATED: u8 = 5;
/// The quorum base depth the captures' `Verify` contexts start at (`WORK_DIFF_DEPTH`).
const WORK_DIFF_DEPTH: u32 = 8;

fn unhex(s: &str) -> Vec<u8> {
    let nibble = |c: u8| match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => panic!("hex"),
    };
    s.as_bytes().chunks(2).map(|p| nibble(p[0]) << 4 | nibble(p[1])).collect()
}

fn read_gz(path: &std::path::Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(std::fs::File::open(path).expect("a capture"))
        .read_to_end(&mut bytes)
        .expect("gzip");
    bytes
}

fn split_headers(bytes: &[u8]) -> Vec<[u8; 80]> {
    bytes.chunks(80).map(|h| h.try_into().expect("80 bytes")).collect()
}

// ---- the chain, the block index, the members and the precompute of a held window ----

/// A held chain as the node's views see it: its reads with the store's error, and as complete
/// ancestry (every read is inside the window).
struct StoreChain<'a>(&'a HeldChain);

fn not_held(e: ViewError) -> StoreError {
    StoreError::read(format!("{e:?}"))
}

impl LlmqChain for StoreChain<'_> {
    type Error = StoreError;

    fn header(&self, id: BlockId) -> Result<HeaderCtx, StoreError> {
        LlmqChain::header(self.0, id).map_err(not_held)
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, StoreError> {
        LlmqChain::ancestor(self.0, id, height).map_err(not_held)
    }

    fn prev(&self, id: BlockId) -> Result<Option<BlockId>, StoreError> {
        LlmqChain::prev(self.0, id).map_err(not_held)
    }
}

impl ChainAncestry for StoreChain<'_> {
    fn header(&self, id: BlockId) -> HeaderCtx {
        LlmqChain::header(self.0, id).expect("held")
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        LlmqChain::ancestor(self.0, id, height).expect("held")
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        LlmqChain::prev(self.0, id).expect("held")
    }
}

/// `LookupBlockIndex` over the held headers.
struct HeldIndex(HashMap<[u8; 32], u32>);

impl HeldIndex {
    fn of(chain: &HeldChain, first: u32) -> Self {
        HeldIndex(
            (first..=chain.tip()).map(|h| (*chain.hash(h).expect("held").as_bytes(), h)).collect(),
        )
    }
}

impl BlockLookup for HeldIndex {
    fn lookup(&self, hash: &dash_primitives::BlockHash) -> Option<(BlockId, u32)> {
        self.0.get(hash.as_bytes()).map(|&h| (BlockId(h), h))
    }
}

/// `GetAllQuorumMembers` as Core answered it: the members of each captured quorum, by (type,
/// base height). A quorum the source does not hold is a harness fault.
#[derive(Clone, Default)]
struct Given(HashMap<(u8, u32), Vec<Arc<Dmn>>>);

impl QuorumMemberSource for Given {
    type Entry = Arc<Dmn>;
    type Error = StoreError;

    fn members(
        &self,
        llmq_type: LlmqType,
        base: BlockId,
        _: &mut dyn EffectSink,
    ) -> Result<QuorumMembers<Arc<Dmn>>, MemberError<StoreError>> {
        let members = self.0.get(&(llmq_type.to_base(), base.0));
        Ok(members
            .unwrap_or_else(|| panic!("no members for {llmq_type:?} at {}", base.0))
            .iter()
            .cloned()
            .collect())
    }
}

/// The captures' members are given, so there is nothing to precompute.
struct NoPrecompute;

impl QuorumPrecompute for NoPrecompute {
    type Error = StoreError;

    fn precompute(
        &self,
        _: BlockId,
        _: PrecomputeMode,
        _: &mut dyn EffectSink,
    ) -> Result<(), MemberError<StoreError>> {
        Ok(())
    }
}

// ---- the rotated captures ----

/// One captured quorum: its index, base height, members (Core's disk records and decoded, in
/// member order) and its mined commitment with the height that mined it.
struct Quorum {
    index: usize,
    base: u32,
    records: Vec<Vec<u8>>,
    members: Vec<Arc<Dmn>>,
    mined: Option<(Vec<u8>, u32)>,
}

/// One rotated capture (`test-vectors/lq1-rot`): what Core's context loads, and its quorums.
struct Capture {
    name: String,
    chain_name: String,
    params: ChainSetup,
    first: u32,
    headers: Vec<[u8; 80]>,
    chain: HeldChain,
    fixture: LlmqFixture,
    quorums: Vec<Quorum>,
}

/// The captures with mined commitments (the others mine none: the first rotated cycles, and
/// testnet's 28e cycle).
const MINED: [&str; 9] = [
    "mainnet-1738656.json.gz",
    "mainnet-1899072.json.gz",
    "mainnet-1987776.json.gz",
    "mainnet-1988064.json.gz",
    "mainnet-2089728.json.gz",
    "testnet-1067040.json.gz",
    "testnet-1572480.json.gz",
    "testnet-850176.json.gz",
    "testnet-905184.json.gz",
];

fn captures() -> Vec<Capture> {
    captures_named(&MINED)
}

fn captures_named(names: &[&str]) -> Vec<Capture> {
    let dir = format!("{}/../test-vectors/lq1-rot", env!("CARGO_MANIFEST_DIR"));
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("the captures")
        .map(|e| e.expect("an entry").path())
        .filter(|p| p.to_string_lossy().ends_with(".json.gz"))
        .collect();
    paths.sort();
    let named = |p: &std::path::PathBuf| {
        names.contains(&&*p.file_name().expect("a name").to_string_lossy())
    };
    let read: Vec<Capture> = paths.iter().filter(|p| named(p)).map(|p| Capture::read(p)).collect();
    assert_eq!(read.len(), names.len(), "every named capture");
    read
}

impl Capture {
    fn read(path: &std::path::Path) -> Self {
        let v: serde_json::Value = serde_json::from_slice(&read_gz(path)).expect("json");
        let network = match v["network"].as_str().expect("a network") {
            "mainnet" => Network::Main,
            "testnet" => Network::Testnet3,
            other => panic!("{other}"),
        };
        let params = ChainParamsBuilder::new(network, Release::V24_0).build().expect("parameters");
        let chain_name = v["chain"].as_str().expect("a chain").to_owned();
        let first = v["first_height"].as_u64().expect("a height") as u32;
        let headers = split_headers(&unhex(v["headers"].as_str().expect("headers")));
        let chain =
            HeldChain::new(first, headers.iter().map(|h| header_hash(h).expect("a header")));
        for h in first + 1..=chain.tip() {
            let header = &headers[(h - first) as usize];
            assert_eq!(&header[4..36], chain.hash(h - 1).expect("held").as_bytes(), "linked");
        }
        let hash = |height: u32| *chain.hash(height).expect("held").as_bytes();
        let dmns: Vec<Vec<u8>> = v["dmns"]
            .as_array()
            .expect("records")
            .iter()
            .map(|d| unhex(d.as_str().expect("hex")))
            .collect();
        // A record starts with its proTxHash (`CDeterministicMN::SERIALIZE_METHODS`); the members'
        // records are decoded below and checked against it.
        let by_pro_tx: HashMap<[u8; 32], usize> = dmns
            .iter()
            .enumerate()
            .map(|(i, d)| (d[..32].try_into().expect("a record"), i))
            .collect();
        let mut fixture = LlmqFixture {
            chain: chain_name.clone(),
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
            fixture.lists.push((hash(height), bytes));
        }
        for cb in v["coinbases"].as_array().expect("coinbases") {
            let height = cb["height"].as_u64().expect("a height") as u32;
            fixture.coinbases.push((hash(height), unhex(cb["tx"].as_str().expect("a tx")), true));
        }
        for row in v["rows"].as_array().expect("rows") {
            let at = row["cycle"].as_u64().expect("a cycle") as u32;
            fixture.snapshots.push((
                ROTATED,
                hash(at),
                unhex(row["snapshot"].as_str().expect("a row")),
            ));
        }
        let quorums = v["quorums"]
            .as_array()
            .expect("quorums")
            .iter()
            .map(|q| {
                let records: Vec<Vec<u8>> = q["members"]
                    .as_array()
                    .expect("members")
                    .iter()
                    .map(|m| {
                        // RPC (reversed) hex.
                        let mut pro_tx: [u8; 32] =
                            unhex(m.as_str().expect("hex")).try_into().expect("32 bytes");
                        pro_tx.reverse();
                        dmns[by_pro_tx[&pro_tx]].clone()
                    })
                    .collect();
                let members = records
                    .iter()
                    .map(|r| {
                        let dmn = decode_dmn(&mut &r[..]).expect("a Core disk record");
                        assert_eq!(dmn.pro_tx_hash.as_bytes(), &r[..32], "the record's proTxHash");
                        Arc::new(dmn)
                    })
                    .collect();
                Quorum {
                    index: q["index"].as_u64().expect("an index") as usize,
                    base: q["base_height"].as_u64().expect("a height") as u32,
                    records,
                    members,
                    mined: q.get("commitment").map(|c| {
                        (
                            unhex(c.as_str().expect("a commitment")),
                            q["mined_height"].as_u64().expect("a height") as u32,
                        )
                    }),
                }
            })
            .collect();
        Capture {
            name: path.file_name().expect("a name").to_string_lossy().into_owned(),
            chain_name,
            params,
            first,
            headers,
            chain,
            fixture,
            quorums,
        }
    }

    fn deployments(&self) -> HeightDeployments<'_> {
        HeightDeployments {
            params: &self.params,
        }
    }

    /// The quorums with a mined commitment.
    fn mined(&self) -> impl Iterator<Item = (&Quorum, &[u8], u32)> {
        self.quorums.iter().filter_map(|q| q.mined.as_ref().map(|(qc, h)| (q, &qc[..], *h)))
    }

    /// The captured members of every quorum.
    fn given(&self) -> Given {
        Given(self.quorums.iter().map(|q| ((ROTATED, q.base), q.members.clone())).collect())
    }

    /// Core's `Verify` context for the quorum based at `base` with `members`, the commitment
    /// checked on top of `tip`, in the scheme of the block after `tip`.
    fn context(&self, base: u32, tip: u32, members: Vec<Vec<u8>>) -> Context {
        let deployments = self.deployments();
        let from = base - WORK_DIFF_DEPTH;
        Context::new(
            from as i32,
            self.headers[(from - self.first) as usize..=(tip - self.first) as usize].to_vec(),
            bls_ctx_after(&deployments, Some(BlockId(tip))).is_legacy(),
            vec![(DEPLOYMENT_V24, ThresholdState::Defined)],
            0,
            Checkpoints::Enabled {
                best_header_height: tip as i32,
            },
            CoinsView::Unchecked(vec![]),
            CoreMnList::Absent,
            vec![CoreQuorum {
                llmq_type: ROTATED,
                quorum_hash: *self.chain.hash(base).expect("held").as_bytes(),
                members,
            }],
        )
    }
}

/// The signers bitset of a commitment, as bools.
fn signers(qc: &LlmqCommitment) -> Vec<bool> {
    (0..qc.signers.num_bits).map(|i| qc.signers.get(i) == Some(true)).collect()
}

/// `members` with the first signer swapped with the first non-signer, if the quorum has one.
fn swapped<T: Clone>(qc: &LlmqCommitment, members: &[T]) -> Option<Vec<T>> {
    let signers = signers(qc);
    let signer = signers.iter().take(members.len()).position(|s| *s)?;
    let non_signer = signers.iter().take(members.len()).position(|s| !*s)?;
    let mut out = members.to_vec();
    out.swap(signer, non_signer);
    Some(out)
}

/// LQ2 D20: every commitment Core mined in the captures, and every null commitment Core's miner
/// built on the regtest chain (one per version), decodes, dash-consensus's commitment writer
/// gives back the bytes Core mined, and `commitment_hash` is their double SHA-256 (Core's
/// `::SerializeHash(qc)`, `llmq/blockprocessor.cpp` v24:452, and the quorum root's leaves,
/// `evo/cbtx.cpp` v24:65-156). Between them they hold every version.
#[test]
fn captured_commitments_hash_as_received() {
    let mut versions: BTreeMap<(bool, u16), usize> = BTreeMap::new();
    let mut bitset_sizes = BTreeSet::new();
    let captured = captures().into_iter().flat_map(|capture| {
        capture
            .mined()
            .map(|(q, bytes, _)| (format!("{} index {}", capture.name, q.index), bytes.to_vec()))
            .collect::<Vec<_>>()
    });
    let nulls = NULL_TXS.iter().map(|(mined, tx)| {
        (format!("regtest null at {mined}"), null_payload(&unhex(tx))[6..].to_vec())
    });
    for (what, bytes) in captured.chain(nulls) {
        let qc = LlmqCommitment::decode(&mut &bytes[..]).expect("a commitment");
        let mut written = Vec::new();
        write_commitment(&qc, &mut written);
        assert_eq!(written, bytes, "{what}: the commitment writes as Core mined it");
        assert_eq!(commitment_hash(&qc), sha256d(&bytes), "{what}");
        *versions.entry((qc.is_null(), qc.version)).or_default() += 1;
        bitset_sizes.extend([qc.signers.num_bits, qc.valid_members.num_bits]);
    }
    // The legacy-scheme mainnet cycle (version 2), the basic-scheme ones (version 4) and the
    // regtest nulls (versions 1 to 4).
    assert_eq!(
        versions,
        BTreeMap::from([
            ((false, 2), 32),
            ((false, 4), 196),
            ((true, 1), 1),
            ((true, 2), 1),
            ((true, 3), 1),
            ((true, 4), 1)
        ]),
        "commitments per version"
    );
    // Every bitset Core mined has padding bits (LLMQ_60_75's 60 members, LLMQ_TEST's 3 and
    // LLMQ_TEST_DIP0024's 4); `longer_bitset_on_a_byte_boundary` writes 400- and 401-bit ones.
    assert_eq!(bitset_sizes, BTreeSet::from([3, 4, 60]), "bitset sizes");
}

/// Core's mined-path verdict as ours names it: accepted, or rejected by `Verify`'s structure
/// (false) or by the queued signature checks (true).
fn core_mined(verdict: MinedVerdict) -> Result<(), bool> {
    match verdict {
        MinedVerdict::Accepted => Ok(()),
        MinedVerdict::VerifyFailed => Err(false),
        MinedVerdict::SignaturesFailed => Err(true),
    }
}

/// Our mined-path verdict: `Verify`'s structure (false) or the signature checks (true).
fn ours_mined(r: Result<(), CommitmentError<StoreError>>, what: &str) -> Result<(), bool> {
    match r {
        Ok(()) => Ok(()),
        Err(CommitmentError::Rejected(r)) => Err(matches!(
            r,
            CommitmentReject::MemberSignature
                | CommitmentReject::MembersSignature
                | CommitmentReject::QuorumSignature
        )),
        Err(e) => panic!("{what}: no verdict: {e:?}"),
    }
}

/// LQ2 D11, the per-commitment twin ("legacy individual", and the contract BS11b's batch must
/// meet): every captured commitment, with the members Core gave its quorum and the scheme of
/// the block that mined it (V19 after the mined block's parent), through
/// `verify_mined_signatures` (the check `process_block` queues under `fBLSChecks`) and
/// `verify_commitment(…, Some(SigPath::Mined))`, against Core's mined path
/// (`oracle_llmq_verify_mined`: `CheckLLMQCommitment`'s `Verify(checkSigs = false)`, then
/// `ProcessBlock`'s queued `BlsCheck`s, `llmq/blockprocessor.cpp` v24:267-284), where Core
/// computes the members from the capture's lists. All accept. Then, for every commitment with a
/// non-signer, a signer swapped with a non-signer rejects on our mined path and in Core's
/// `Verify(checkSigs = true)` with those members (Core's mined path computes its own members,
/// so the swap goes through the context's; `VerifySignatureAsync`, `llmq/commitment.cpp`
/// v24:35-110, checks a several-signer quorum as one secure aggregate on both paths).
#[test]
fn mined_signatures_twin_matches_core() {
    let oracle = Oracle::new(Pin::V24_0);
    let (mut legacy, mut basic, mut swaps) = (0, 0, 0);
    for capture in captures() {
        let deployments = capture.deployments();
        let core_chain = Chain::new(&capture.chain_name, &[]);
        let mined: Vec<(&Quorum, &[u8], u32)> = capture.mined().collect();
        let what =
            |q: &Quorum, mined: u32| format!("{} index {} mined at {mined}", capture.name, q.index);
        let is_legacy =
            |mined: u32| bls_ctx_after(&deployments, Some(BlockId(mined - 1))).is_legacy();
        // Ours, on threads (the BLS work dominates; Core's calls stay on this thread).
        let given = capture.given();
        let ours: Vec<(Result<(), bool>, Option<bool>)> = std::thread::scope(|scope| {
            let threads: Vec<_> = mined
                .chunks(mined.len().div_ceil(8))
                .map(|chunk| {
                    let (capture, given) = (&capture, &given);
                    scope.spawn(move || {
                        chunk
                            .iter()
                            .map(|&(q, bytes, mined)| ours_twin(capture, given, q, bytes, mined))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            threads.into_iter().flat_map(|t| t.join().expect("a thread")).collect()
        });
        // Core's mined verdicts: no other oracle call runs while its context is live.
        let core_verdicts: Vec<MinedVerdict> = {
            let mut core = oracle.llmq_context(&core_chain).expect("a context");
            core.load(&capture.fixture).expect("the capture loads");
            mined
                .iter()
                .map(|&(q, bytes, h)| {
                    let r = core.verify_mined(bytes, is_legacy(h));
                    r.unwrap_or_else(|e| panic!("{}: {e}", what(q, h))).0
                })
                .collect()
        };
        for (&(q, bytes, h), ((ours, swap), verdict)) in
            mined.iter().zip(ours.into_iter().zip(core_verdicts))
        {
            let what = what(q, h);
            assert_eq!(ours, core_mined(verdict), "{what}");
            assert_eq!(verdict, MinedVerdict::Accepted, "{what}");
            if is_legacy(h) {
                legacy += 1;
            } else {
                basic += 1;
            }
            let Some(rejected) = swap else {
                continue;
            };
            assert!(rejected, "{what}: a swapped signer rejects on our mined path");
            let qc = LlmqCommitment::decode(&mut &bytes[..]).expect("a commitment");
            let records = swapped(&qc, &q.records).expect("a non-signer");
            match oracle
                .verify_commitment(&core_chain, bytes, &capture.context(q.base, h, records), true)
                .expect(&what)
            {
                Verdict::Reject(r) => assert!(
                    r.debug_message.ends_with("invalid aggregated members signature"),
                    "{what}: {r:?}"
                ),
                Verdict::Accept => panic!("{what}: Core accepts a swapped signer"),
            }
            swaps += 1;
        }
    }
    eprintln!("mined path: {legacy} legacy, {basic} basic; {swaps} swapped-signer rejections");
    assert_eq!((legacy, basic), (32, 196), "every captured commitment, legacy and basic");
    assert!(swaps > 0, "{swaps}");
}

/// Our side of the mined twin for one captured commitment, in the scheme of the block that mined
/// it: the verdict of `verify_mined_signatures` (asserted equal to `verify_commitment(…, Mined)`'s),
/// and, when the quorum has a non-signer, whether a signer swapped with one rejects on both
/// paths with `MembersSignature`.
fn ours_twin(
    capture: &Capture,
    given: &Given,
    q: &Quorum,
    bytes: &[u8],
    mined: u32,
) -> (Result<(), bool>, Option<bool>) {
    let what = format!("{} index {} mined at {mined}", capture.name, q.index);
    let deployments = capture.deployments();
    let store_chain = StoreChain(&capture.chain);
    let qc = LlmqCommitment::decode(&mut &bytes[..]).expect("a commitment");
    let bls = bls_ctx_after(&deployments, Some(BlockId(mined - 1)));
    let ctx = LlmqCtx::new(&capture.params, &store_chain, &deployments, bls).expect("in domain");
    let base = BlockId(q.base);
    let both = |members: &Given| {
        let seam = verify_mined_signatures(&ctx, members, &qc, base, &mut Vec::new());
        let full =
            verify_commitment(&ctx, members, &qc, base, Some(SigPath::Mined), &mut Vec::new());
        assert_eq!(seam, full, "{what}: the seam and Verify");
        seam
    };
    let verdict = ours_mined(both(given), &what);
    let swap = swapped(&qc, &q.members).map(|members| {
        let mut swapped_given = given.clone();
        swapped_given.0.insert((ROTATED, q.base), members);
        both(&swapped_given) == Err(CommitmentError::Rejected(CommitmentReject::MembersSignature))
    });
    (verdict, swap)
}

/// A block of a coinbase and one version-3 type-6 transaction per payload.
fn block(payloads: &[Vec<u8>]) -> Block {
    let tx = |version, tx_type, extra_payload| {
        core_serialize(&Transaction {
            version,
            tx_type,
            inputs: vec![],
            outputs: vec![],
            lock_time: 0,
            extra_payload,
        })
    };
    let mut raw = vec![0; 80];
    dash_consensus::primitives::write_compact_size(1 + payloads.len() as u64, &mut raw);
    raw.extend(tx(1, TxType::Spend, vec![]));
    for p in payloads {
        raw.extend(tx(3, TxType::QuorumCommitment, p.clone()));
    }
    decode_block(&mut &raw[..]).expect("a block").into_value()
}

/// `CFinalCommitmentTxPayload` {`CURRENT_VERSION` 1, `nHeight`, the commitment}
/// (`llmq/commitment.h` v24, `CFinalCommitmentTxPayload::SERIALIZE_METHODS`).
fn qc_payload(height: u32, commitment: &[u8]) -> Vec<u8> {
    let mut p = 1u16.to_le_bytes().to_vec();
    p.extend(height.to_le_bytes());
    p.extend(commitment);
    p
}

/// What `process_block` returned for a block, and the effects it emitted.
type Connected = (Result<Vec<MinedCommitmentRecord>, QcBlockError>, Vec<Effect>);

/// A capture's chain as `process_block` reads it: its deployments, the blocks it holds and an
/// empty mined index.
struct MinedSetup<'c> {
    capture: &'c Capture,
    deployments: HeightDeployments<'c>,
    index: HeldIndex,
    store: MemEvoStore,
}

impl<'c> MinedSetup<'c> {
    fn new(capture: &'c Capture) -> Self {
        Self {
            capture,
            deployments: capture.deployments(),
            index: HeldIndex::of(&capture.chain, capture.first),
            store: MemEvoStore::new(),
        }
    }

    /// `process_block` of `block`, mined at `mined`, in crash replay (`tip: None`: no count
    /// checks, no `bad-qc-block`, `llmq/blockprocessor.cpp` v24:229-232, :347-349) with
    /// `fBLSChecks`, the members of `members` and the signature checks run by `bls_checks`.
    fn connect(
        &self,
        block: &Block,
        mined: u32,
        members: &Given,
        bls_checks: &dyn BlsCheckRunner,
    ) -> Connected {
        let store_chain = StoreChain(&self.capture.chain);
        let view = self.store.view(&store_chain);
        let bls = bls_ctx_after(&self.deployments, Some(BlockId(mined - 1)));
        let llmq = LlmqCtx::new(&self.capture.params, &store_chain, &self.deployments, bls)
            .expect("in domain");
        let ctx = QcCtx {
            llmq,
            index: &self.index,
            mined: &view,
            members,
            precompute: &NoPrecompute,
            bls_checks,
        };
        let mut effects: Vec<Effect> = Vec::new();
        let at = QcBlockAt {
            id: BlockId(mined),
            height: mined,
        };
        let mode = QcMode {
            just_check: false,
            bls_checks: true,
        };
        let r = process_block(&ctx, block, at, None, mode, &mut effects);
        (r, effects)
    }
}

/// LQ2 D11 and D5, the block-level twin: each capture's commitments, grouped by the height that
/// mined them, as blocks through `process_block` in crash replay (`tip: None`: no count checks,
/// no `bad-qc-block`, `llmq/blockprocessor.cpp` v24:229-232, :347-349) with `fBLSChecks`, the
/// captured members and an empty mined index. Every block is accepted with one record per
/// commitment, in the indexed family (rotation at the base, v24:421) at the quorum's index and
/// base height, as each commitment's own mined path accepts it. A signer swapped with a
/// non-signer in one quorum fails the block with `bad-qc-invalid` (v24:280-284) and, on the
/// per-commitment path, exactly that commitment. Every block gives the same on Core's BLS check
/// threads ([`BlsCheckQueue`]) as with the checks run serially.
#[test]
fn process_block_twin_over_captured_mining_blocks() {
    let pool = BlsCheckQueue::new(3).expect("the BLS check threads");
    let (mut blocks, mut records_seen, mut swaps) = (0, 0, 0);
    for capture in captures() {
        let mut groups: BTreeMap<u32, Vec<(&Quorum, &[u8])>> = BTreeMap::new();
        for (q, bytes, mined) in capture.mined() {
            groups.entry(mined).or_default().push((q, bytes));
        }
        if groups.is_empty() {
            continue;
        }
        let setup = MinedSetup::new(&capture);
        let store_chain = StoreChain(&capture.chain);
        let given = capture.given();
        let mut swapped_once = false;
        for (&mined, group) in &groups {
            let what = format!("{} block {mined}", capture.name);
            let bls = bls_ctx_after(&setup.deployments, Some(BlockId(mined - 1)));
            let llmq = LlmqCtx::new(&capture.params, &store_chain, &setup.deployments, bls)
                .expect("in domain");
            let block =
                block(&group.iter().map(|(_, bytes)| qc_payload(mined, bytes)).collect::<Vec<_>>());
            let qcs: Vec<LlmqCommitment> = group
                .iter()
                .map(|(_, b)| LlmqCommitment::decode(&mut &b[..]).expect("a commitment"))
                .collect();
            // The block on Core's BLS check threads gives what the serial checks give.
            let run = |members: &Given| {
                let serial = setup.connect(&block, mined, members, &SerialBlsChecks);
                assert_eq!(setup.connect(&block, mined, members, &pool), serial, "{what}: pool");
                serial
            };
            let per_commitment = |members: &Given| -> Vec<bool> {
                group
                    .iter()
                    .zip(&qcs)
                    .map(|((q, _), qc)| {
                        let base = BlockId(q.base);
                        let seam =
                            verify_mined_signatures(&llmq, members, qc, base, &mut Vec::new());
                        let full = verify_commitment(
                            &llmq,
                            members,
                            qc,
                            base,
                            Some(SigPath::Mined),
                            &mut Vec::new(),
                        );
                        assert_eq!(seam.is_ok(), full.is_ok(), "{what} index {}", q.index);
                        full.is_ok()
                    })
                    .collect()
            };

            let (r, effects) = run(&given);
            let records = r.unwrap_or_else(|e| panic!("{what}: {e}"));
            assert!(per_commitment(&given).iter().all(|ok| *ok), "{what}: per commitment");
            assert_eq!(records.len(), group.len(), "{what}: one record per commitment");
            for ((record, (q, _)), qc) in records.iter().zip(group).zip(&qcs) {
                assert_eq!(record.llmq_type.to_base(), ROTATED, "{what}");
                assert_eq!(record.quorum_hash, qc.quorum_hash, "{what}");
                assert_eq!(record.rotation_index, Some(q.index as i16), "{what}");
                assert_eq!(record.base_height, q.base, "{what}");
                assert_eq!(*record.commitment, *qc, "{what}");
            }
            // One `EraseMineableCommitment` per written commitment (v24:450-452, LQ2 D7).
            let erased: Vec<[u8; 32]> = effects
                .iter()
                .filter_map(|e| match e {
                    Effect::EraseMineableCommitment {
                        commitment_hash,
                        ..
                    } => Some(*commitment_hash),
                    _ => None,
                })
                .collect();
            let hashes: Vec<[u8; 32]> = group.iter().map(|(_, b)| sha256d(b)).collect();
            assert_eq!(erased, hashes, "{what}: the pool erasures");
            blocks += 1;
            records_seen += records.len();

            if swapped_once {
                continue;
            }
            let Some((at, members)) = group
                .iter()
                .zip(&qcs)
                .enumerate()
                .find_map(|(i, ((q, _), qc))| Some((i, swapped(qc, &q.members)?)))
            else {
                continue;
            };
            let mut bad = given.clone();
            bad.0.insert((ROTATED, group[at].0.base), members);
            let (r, _) = run(&bad);
            assert_eq!(
                r.as_ref().err().and_then(|e| e.reject_reason()),
                Some("bad-qc-invalid"),
                "{what}: a signer swapped at index {}: {r:?}",
                group[at].0.index
            );
            let verdicts = per_commitment(&bad);
            let rejected: Vec<usize> =
                verdicts.iter().enumerate().filter(|(_, ok)| !**ok).map(|(i, _)| i).collect();
            assert_eq!(rejected, vec![at], "{what}: exactly the swapped commitment rejects");
            swapped_once = true;
            swaps += 1;
        }
    }
    eprintln!("process_block: {blocks} blocks, {records_seen} records, {swaps} swapped blocks");
    assert_eq!(records_seen, 228, "every captured commitment");
    assert!(swaps > 0, "{swaps}");
}

/// The height of the qc-block-mainnet block (the `dash-dml` `qc_block` bench's): it mines the 32
/// LLMQ_60_75 commitments of the cycle at 2,089,728 (capture `mainnet-2089728`).
const QC_BLOCK_MAINNET: u32 = 2_089_770;

/// DEC-325's evidence (LQ2 D11), run by hand under `perf stat -e instructions:u`: the
/// qc-block-mainnet block's signature work `LQ2_PERF_ITERS` times on the side `LQ2_PERF_SIDE`:
/// - `core`: Core's mined path for each of its commitments (`oracle_llmq_verify_mined`:
///   `Verify(checkSigs = false)`, then `ProcessBlock`'s queued `BlsCheck`s);
/// - `serial`: `process_block` with the checks run on the caller ([`SerialBlsChecks`]);
/// - `pool`: `process_block` on Core's BLS check threads at `-parbls`'s default (from the
///   threads this process may use; the node passes `get_nprocs()`, as Core's `GetNumCores()`).
///
/// A tenth of the difference between 11 iterations and 1 is one block, without the setup and
/// the first pass (Core's member calculation, cached after it).
#[test]
#[ignore = "DEC-325 evidence: run by hand under perf stat"]
fn qc_block_mainnet_instructions() {
    let side = std::env::var("LQ2_PERF_SIDE").expect("LQ2_PERF_SIDE");
    let iters: usize = std::env::var("LQ2_PERF_ITERS").map_or(1, |n| n.parse().expect("a count"));
    let capture = captures_named(&["mainnet-2089728.json.gz"]).pop().expect("the capture");
    let group: Vec<&[u8]> = capture
        .mined()
        .filter(|&(_, _, mined)| mined == QC_BLOCK_MAINNET)
        .map(|(_, bytes, _)| bytes)
        .collect();
    assert_eq!(group.len(), 32, "the cycle's commitments");
    let mined_after = Some(BlockId(QC_BLOCK_MAINNET - 1));
    match &*side {
        "core" => {
            let oracle = Oracle::new(Pin::V24_0);
            let mut core =
                oracle.llmq_context(&Chain::new(&capture.chain_name, &[])).expect("a context");
            core.load(&capture.fixture).expect("the capture loads");
            let legacy = bls_ctx_after(&capture.deployments(), mined_after).is_legacy();
            for _ in 0..iters {
                for bytes in &group {
                    let (verdict, _) = core.verify_mined(bytes, legacy).expect("a verdict");
                    assert_eq!(verdict, MinedVerdict::Accepted);
                }
            }
        }
        "serial" | "pool" => {
            let setup = MinedSetup::new(&capture);
            let given = capture.given();
            let payloads: Vec<Vec<u8>> =
                group.iter().map(|bytes| qc_payload(QC_BLOCK_MAINNET, bytes)).collect();
            let block = block(&payloads);
            let pool;
            let bls_checks: &dyn BlsCheckRunner = if side == "pool" {
                let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
                let threads = bls_check_threads(DEFAULT_BLSCHECK_THREADS, cores);
                pool = BlsCheckQueue::new(threads).expect("the BLS check threads");
                &pool
            } else {
                &SerialBlsChecks
            };
            for _ in 0..iters {
                let (r, _) = setup.connect(&block, QC_BLOCK_MAINNET, &given, bls_checks);
                assert_eq!(r.expect("the block connects").len(), 32);
            }
        }
        other => panic!("LQ2_PERF_SIDE={other}: core, serial or pool"),
    }
}

// ---- LQ2 D17: the L3 QcTx BLS-encoding operators ----

/// The captures the operators sample: mainnet legacy (v2), mainnet basic and testnet basic (v4).
const SAMPLED: [&str; 3] =
    ["mainnet-1738656.json.gz", "mainnet-2089728.json.gz", "testnet-1572480.json.gz"];

/// The base field's modulus p, big-endian: an x coordinate no scheme encodes.
const P: &str = "1a0111ea397fe69a4b1ba7b6434bacd764774b84f38512bf6730d2a0f6b0f6241eabfffeb153ffffb9feffffffffaaab";

/// The regtest chain the null commitments come from (`feature_protx_version.py`, node 0, under
/// `/work/conformance/regtest-v24/feature_protx_version-node0`): its consensus arguments, from
/// its `debug.log`.
const NULL_ARGS: [&str; 5] = [
    "-dip3params=2:2",
    "-testactivationheight=v20@100",
    "-testactivationheight=mn_rr@100",
    "-testactivationheight=v19@200",
    "-vbparams=v24:0:999999999999:350:10:8:6:5:0",
];

/// The height of `NULL_HEADERS`' first header.
const NULL_FIRST: u32 = 0;

/// A commitment's BLS field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Field {
    QuorumPublicKey,
    QuorumSig,
    MembersSig,
}

/// An L3 QcTx operator (LQ2 D17).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Op {
    /// The commitment as Core produced it.
    AsProduced,
    /// The field's object serialized under the other scheme.
    OtherScheme(Field),
    /// The infinity point (`0xc0` then zeros, the same in both schemes).
    Infinity(Field),
    /// x = p with the compression flag: canonical in neither scheme.
    NonCanonical(Field),
    /// All zeros (Core's invalid object).
    Zeros(Field),
    /// The version flipped between the legacy and the basic scheme (1 ↔ 3, 2 ↔ 4).
    FlipVersion,
    /// The payload's `nVersion` (Core's miner writes 1).
    PayloadVersion(u16),
    /// The payload's `nHeight`, the mined height plus this.
    PayloadHeight(i8),
    /// The commitment's `llmqType`.
    LlmqType(u8),
    /// A `quorumHash` that names no block.
    UnknownQuorumHash,
    /// The bitset one bit longer, the new bit clear.
    LongerBitset(Bitset),
}

/// A commitment's bitset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Bitset {
    Signers,
    ValidMembers,
}

const FIELDS: [Field; 3] = [Field::QuorumPublicKey, Field::QuorumSig, Field::MembersSig];

fn ops() -> Vec<Op> {
    let mut ops = vec![Op::AsProduced];
    for f in FIELDS {
        ops.extend([Op::OtherScheme(f), Op::Infinity(f), Op::NonCanonical(f), Op::Zeros(f)]);
    }
    ops.push(Op::FlipVersion);
    // The checks before the commitment's own (LQ2 Opus r1 Minor 1): `CheckLLMQCommitment`'s
    // type, version, height and quorum-hash reasons, and `VerifySizes` (v24:250-297). 100 is
    // LLMQ_TEST, configured on regtest only; 0xff on no network.
    ops.extend([
        Op::PayloadVersion(0),
        Op::PayloadVersion(2),
        Op::PayloadHeight(-1),
        Op::PayloadHeight(1),
        Op::LlmqType(100),
        Op::LlmqType(0xff),
        Op::UnknownQuorumHash,
        Op::LongerBitset(Bitset::Signers),
        Op::LongerBitset(Bitset::ValidMembers),
    ]);
    ops
}

/// The offset of the signers bitset in a commitment's serialization: version, type, quorum
/// hash and, for versions 2 and 4, the index.
fn signers_offset(qc: &LlmqCommitment) -> usize {
    2 + 1 + 32 + 2 * usize::from(qc.quorum_index.is_some())
}

/// The serialized size of a bitset of `n` bits: its CompactSize length, then the bytes.
fn bitset_len(n: u64) -> usize {
    let mut size = Vec::new();
    dash_consensus::primitives::write_compact_size(n, &mut size);
    size.len() + n.div_ceil(8) as usize
}

/// The byte range of `field` in the commitment's serialization.
fn field_range(qc: &LlmqCommitment, field: Field) -> std::ops::Range<usize> {
    let pk = signers_offset(qc)
        + bitset_len(qc.signers.num_bits)
        + bitset_len(qc.valid_members.num_bits);
    let quorum_sig = pk + 48 + 32;
    match field {
        Field::QuorumPublicKey => pk..pk + 48,
        Field::QuorumSig => quorum_sig..quorum_sig + 96,
        Field::MembersSig => quorum_sig + 96..quorum_sig + 192,
    }
}

/// `bytes`, a commitment Core produced, with `op` applied.
fn apply(bytes: &[u8], op: Op) -> Vec<u8> {
    let qc = LlmqCommitment::decode(&mut &bytes[..]).expect("a commitment");
    assert_eq!(field_range(&qc, Field::MembersSig).end, bytes.len(), "the layout");
    // The version's scheme: legacy iff version 1 or 2 (`llmq/commitment.h` v24:110-113).
    let scheme = BlsSchemeKind::from_legacy(matches!(qc.version, 1 | 2));
    let mut out = bytes.to_vec();
    let mut set = |field: Field, value: Vec<u8>| {
        let range = field_range(&qc, field);
        assert_eq!(range.len(), value.len());
        out[range].copy_from_slice(&value);
    };
    match op {
        // The payload operators leave the commitment alone: `op_payload` rewrites the payload.
        Op::AsProduced | Op::PayloadVersion(_) | Op::PayloadHeight(_) => {}
        Op::OtherScheme(f) => {
            let value = match f {
                Field::QuorumPublicKey => qc.quorum_public_key.to_bytes(scheme.opposite()).to_vec(),
                Field::QuorumSig => qc.quorum_sig.to_bytes(scheme.opposite()).to_vec(),
                Field::MembersSig => qc.members_sig.to_bytes(scheme.opposite()).to_vec(),
            };
            set(f, value);
        }
        Op::Infinity(f) => {
            let mut value = vec![0; field_range(&qc, f).len()];
            value[0] = 0xc0;
            set(f, value);
        }
        Op::NonCanonical(f) => {
            let mut value = vec![0; field_range(&qc, f).len()];
            value[..48].copy_from_slice(&unhex(P));
            value[0] |= 0x80;
            set(f, value);
        }
        Op::Zeros(f) => set(f, vec![0; field_range(&qc, f).len()]),
        Op::FlipVersion => {
            let flipped = match qc.version {
                1 => 3u16,
                3 => 1,
                2 => 4,
                4 => 2,
                v => panic!("version {v}"),
            };
            out[..2].copy_from_slice(&flipped.to_le_bytes());
        }
        Op::LlmqType(t) => out[2] = t,
        Op::UnknownQuorumHash => out[3..35].copy_from_slice(&[0x5a; 32]),
        Op::LongerBitset(which) => {
            let signers = signers_offset(&qc);
            let (start, bits) = match which {
                Bitset::Signers => (signers, &qc.signers),
                Bitset::ValidMembers => {
                    (signers + bitset_len(qc.signers.num_bits), &qc.valid_members)
                }
            };
            let mut longer = Vec::new();
            dash_consensus::primitives::write_compact_size(bits.num_bits + 1, &mut longer);
            longer.extend(&bits.data);
            longer.resize(bitset_len(bits.num_bits + 1), 0);
            out.splice(start..start + bitset_len(bits.num_bits), longer);
        }
    }
    out
}

/// The type-6 payload of `commitment`, mined at `mined`, with `op` applied.
fn op_payload(mined: u32, commitment: &[u8], op: Op) -> Vec<u8> {
    let mut payload = qc_payload(mined, &apply(commitment, op));
    match op {
        Op::PayloadVersion(v) => payload[..2].copy_from_slice(&v.to_le_bytes()),
        Op::PayloadHeight(d) => {
            payload[2..6].copy_from_slice(&mined.wrapping_add_signed(d.into()).to_le_bytes())
        }
        _ => {}
    }
    payload
}

/// A version-3 type-6 transaction with no inputs or outputs carrying `payload`, as Core's
/// miner builds it (`llmq/blockprocessor.cpp` v24 `GetMineableCommitmentsTx`).
fn qc_tx(payload: &[u8]) -> Vec<u8> {
    let mut tx = vec![3, 0, 6, 0, 0, 0, 0, 0, 0, 0];
    dash_consensus::primitives::write_compact_size(payload.len() as u64, &mut tx);
    tx.extend(payload);
    tx
}

/// A verdict as both sides name it: accepted, or the reject reason.
fn named(verdict: Verdict) -> String {
    match verdict {
        Verdict::Accept => "accept".into(),
        Verdict::Reject(r) => r.reason,
    }
}

struct NoCheckpoints;

impl CheckpointView for NoCheckpoints {
    fn highest_checkpoint_in_index(&self) -> Option<u32> {
        None
    }
}

/// `CheckSpecialTxInner` of the type-6 `tx` checked on top of `input`'s `mined - 1`, type 6's
/// seams bound to `llmq`, `index` and `verifier`: accept, or the reject reason.
fn dispatched(
    input: &QcInput<'_>,
    tx: &Tx,
    llmq: &LlmqCtx<'_, StoreError>,
    index: &HeldIndex,
    verifier: &dyn CommitmentVerifier,
    what: &str,
) -> String {
    let deployments = HeightDeployments {
        params: input.params,
    };
    let chain = StoreChain(input.chain);
    let prev = Some(BlockId(input.mined - 1));
    let cons = ConsensusCtx {
        params: input.params,
        height: input.mined,
        prev,
        chain: &chain,
        deployments: &deployments,
        checkpoints: &NoCheckpoints,
        bls: bls_ctx_after(&deployments, prev),
        median_time_past: 0,
        header_check_time: 0,
    };
    let (list, coins) = (MnList::scratch(), Coins::new(&[], None));
    let mut effects = Vec::new();
    let mut cx = SpecialTxCtx {
        evo: EvoCtx {
            cons: &cons,
            check_sigs: true,
        },
        rules: SpecialTxRules {
            v24: false,
            evo_shares: false,
        },
        list: &list,
        coins: &coins,
        net_info: NetInfoRules::new(input.params, mainnet(), CORE_NET_INFO),
        verifier: &SerialVerifier,
        quorum: Some(QuorumSeams {
            llmq: *llmq,
            index,
            commitments: verifier,
            effects: &mut effects,
        }),
    };
    match check_special_tx_inner(tx, tx.payload(), &mut cx) {
        Ok(Ok(())) => "accept".to_owned(),
        Ok(Err(e)) => e.reject_reason().to_owned(),
        Err(e) => panic!("{what}: no verdict: {e:?}"),
    }
}

/// Whether `op` runs on `input`: an unknown `quorumHash` only where Core's context starts at
/// genesis (the regtest nulls), since a context above it cannot say which blocks the node knows;
/// a type change only to another type.
fn applies(op: Op, input: &QcInput<'_>) -> bool {
    match op {
        Op::UnknownQuorumHash => input.context.first_height == 0,
        Op::LlmqType(t) => input.commitment[2] != t,
        _ => true,
    }
}

/// `apply`'s `LongerBitset(which)` on `bytes`: the result decodes to the same commitment with
/// that bitset one bit longer, the new bit clear, and writes as built.
fn check_longer_bitset(bytes: &[u8], which: Bitset) {
    let mut want = LlmqCommitment::decode(&mut &bytes[..]).expect("a commitment");
    let longer = apply(bytes, Op::LongerBitset(which));
    let got = LlmqCommitment::decode(&mut &longer[..]).expect("still a commitment");
    let bits = match which {
        Bitset::Signers => &mut want.signers,
        Bitset::ValidMembers => &mut want.valid_members,
    };
    bits.num_bits += 1;
    bits.data.resize(bits.num_bits.div_ceil(8) as usize, 0);
    assert_eq!(got, want, "{which:?}");
    let mut written = Vec::new();
    write_commitment(&got, &mut written);
    assert_eq!(written, longer, "{which:?}");
}

/// `LongerBitset` on bitsets of a whole number of bytes (400 bits, LLMQ_400_60's size), where the
/// longer one takes another byte: the sampled commitments' bitsets have 60, 3 or 4 bits.
#[test]
fn longer_bitset_on_a_byte_boundary() {
    let capture = &captures_named(&SAMPLED[1..2])[0];
    let (_, bytes, _) = capture.mined().next().expect("a commitment");
    let mut qc = LlmqCommitment::decode(&mut &bytes[..]).expect("a commitment");
    for bits in [&mut qc.signers, &mut qc.valid_members] {
        bits.num_bits = 400;
        bits.data = (0..50u8).map(|i| i | 0x81).collect();
    }
    let mut bytes = Vec::new();
    write_commitment(&qc, &mut bytes);
    for which in [Bitset::Signers, Bitset::ValidMembers] {
        check_longer_bitset(&bytes, which);
    }
}

/// One operator input: a commitment Core produced and where it is checked.
struct QcInput<'a> {
    what: String,
    null: bool,
    commitment: Vec<u8>,
    /// The height of the block that mined it: the payload's `nHeight`, checked on top of the
    /// block before.
    mined: u32,
    params: &'a ChainSetup,
    chain: &'a HeldChain,
    first: u32,
    core_chain: &'a Chain,
    /// Core's context on top of `mined - 1`.
    context: Context,
    members: Given,
}

/// The operators' inputs (LQ2 D17): three commitments of each sampled capture (mainnet legacy,
/// mainnet basic, testnet basic) and the null commitments Core's miner built on a regtest chain,
/// one per version, with the chains and parameters they are checked on.
struct QcCorpus {
    captures: Vec<Capture>,
    chains: Vec<Chain>,
    null_params: ChainSetup,
    null_core_chain: Chain,
    null_headers: Vec<[u8; 80]>,
    null_chain: HeldChain,
}

impl QcCorpus {
    fn new() -> Self {
        let captures = captures_named(&SAMPLED);
        let chains: Vec<Chain> = captures.iter().map(|c| Chain::new(&c.chain_name, &[])).collect();
        // The regtest nulls.
        let null_params = NULL_ARGS
            .iter()
            .fold(ChainParamsBuilder::new(Network::Regtest, Release::V24_0), |b, arg| {
                let (name, value) = arg.split_once('=').expect("-name=value");
                b.arg(ChainOption::from_name(name).expect("a chain option"), value)
            })
            .build()
            .expect("the regtest chain's parameters");
        let null_core_chain = Chain::new("regtest", &NULL_ARGS);
        let null_headers = split_headers(&unhex(NULL_HEADERS));
        let null_chain = HeldChain::new(
            NULL_FIRST,
            null_headers.iter().map(|h| header_hash(h).expect("a header")),
        );
        for h in NULL_FIRST + 1..=null_chain.tip() {
            let header = &null_headers[(h - NULL_FIRST) as usize];
            assert_eq!(&header[4..36], null_chain.hash(h - 1).expect("held").as_bytes(), "linked");
        }
        QcCorpus {
            captures,
            chains,
            null_params,
            null_core_chain,
            null_headers,
            null_chain,
        }
    }

    fn inputs(&self) -> Vec<QcInput<'_>> {
        let mut inputs = Vec::new();
        // Three commitments of each sampled capture: mainnet legacy, mainnet basic, testnet basic.
        for (capture, core_chain) in self.captures.iter().zip(&self.chains) {
            for (q, bytes, mined) in capture.mined().take(3) {
                inputs.push(QcInput {
                    what: format!("{} index {}", capture.name, q.index),
                    null: false,
                    commitment: bytes.to_vec(),
                    mined,
                    params: &capture.params,
                    chain: &capture.chain,
                    first: capture.first,
                    core_chain,
                    context: capture.context(q.base, mined - 1, q.records.clone()),
                    members: capture.given(),
                });
            }
        }
        assert_eq!(inputs.len(), 9, "the sampled non-null commitments");

        let null_deployments = HeightDeployments {
            params: &self.null_params,
        };
        for (mined, tx) in NULL_TXS {
            let tx = unhex(tx);
            let payload = null_payload(&tx);
            assert_eq!(qc_tx(payload), tx, "the builder rebuilds Core's transaction");
            assert_eq!(payload[2..6], mined.to_le_bytes(), "the payload height");
            let commitment = payload[6..].to_vec();
            let qc = LlmqCommitment::decode(&mut &commitment[..]).expect("a commitment");
            assert!(qc.is_null(), "a null commitment at {mined}");
            let tip = mined - 1;
            let context = Context::new(
                NULL_FIRST as i32,
                self.null_headers[..=(tip - NULL_FIRST) as usize].to_vec(),
                bls_ctx_after(&null_deployments, Some(BlockId(tip))).is_legacy(),
                vec![(DEPLOYMENT_V24, ThresholdState::Defined)],
                0,
                Checkpoints::Enabled {
                    best_header_height: tip as i32,
                },
                CoinsView::Unchecked(vec![]),
                CoreMnList::Absent,
                vec![],
            );
            inputs.push(QcInput {
                what: format!(
                    "regtest null v{} type {} at {mined}",
                    qc.version,
                    qc.llmq_type.to_base()
                ),
                null: true,
                commitment,
                mined,
                params: &self.null_params,
                chain: &self.null_chain,
                first: NULL_FIRST,
                core_chain: &self.null_core_chain,
                context,
                members: Given::default(),
            });
        }
        let null_versions: Vec<u16> = inputs
            .iter()
            .filter(|i| i.null)
            .map(|i| u16::from_le_bytes([i.commitment[0], i.commitment[1]]))
            .collect();
        assert_eq!(null_versions, [1, 2, 3, 4], "a Core-built null of each version");
        inputs
    }
}

/// LQ2 D17, the L3 QcTx BLS-encoding operators (A1 §4.7), null and non-null: each of
/// `quorumPublicKey`, `quorumSig` and `membersSig` re-encoded under the other scheme, replaced
/// by the infinity point, by a non-canonical encoding and by zeros, and the version flipped
/// between the legacy and the basic scheme; and, before the commitment's own checks, the payload
/// `nVersion` (0, 2) and `nHeight` (mined ∓ 1), an unconfigured `llmqType` (LLMQ_TEST off
/// regtest, 0xff), an unknown `quorumHash`, and a signers or valid-members bitset one bit longer
/// (`VerifySizes`). Inputs: captured mainnet legacy (v2), mainnet basic
/// (v4) and testnet basic (v4) commitments, and the null commitments Core's miner built
/// (`CFinalCommitment(params, quorumHash)`, `llmq/blockprocessor.cpp` v24
/// `GetMineableCommitments`) on a regtest chain, one per version (v1 to v4). Each mutated
/// commitment, in a type-6 transaction at its mined height, gets the same verdict from
/// `check_llmq_commitment` (with `NodeCommitmentVerifier`) as from Core's `CheckSpecialTx`
/// (`CheckLLMQCommitment`, `llmq/commitment.cpp` v24:240-310): accept, or the same reason
/// (`bad-qc-payload` where the payload does not decode, v24:242-246). Every reason of
/// `CheckLLMQCommitment` is reached; the off-chain `quorumHash` branch (v24:286-290) is not, as
/// the context holds one chain whose last header is the tip (LQ2 D17).
#[test]
fn qctx_bls_operators_match_core() {
    let oracle = Oracle::new(Pin::V24_0);
    let corpus = QcCorpus::new();
    let inputs = corpus.inputs();

    let mut table: BTreeMap<(Op, bool, String), usize> = BTreeMap::new();
    for input in &inputs {
        let deployments = HeightDeployments {
            params: input.params,
        };
        let store_chain = StoreChain(input.chain);
        let index = HeldIndex::of(input.chain, input.first);
        let prev = BlockId(input.mined - 1);
        let llmq = LlmqCtx::new(
            input.params,
            &store_chain,
            &deployments,
            bls_ctx_after(&deployments, Some(prev)),
        )
        .expect("in domain");
        let verifier = NodeCommitmentVerifier {
            llmq,
            members: &input.members,
        };
        for op in ops() {
            if !applies(op, input) {
                continue;
            }
            if let Op::LongerBitset(which) = op {
                check_longer_bitset(&input.commitment, which);
            }
            let what = format!("{}: {op:?}", input.what);
            let tx_bytes = qc_tx(&op_payload(input.mined, &input.commitment, op));
            let tx: Tx = decode_tx(&mut &tx_bytes[..]).expect("a transaction").into_value();
            let ours = dispatched(input, &tx, &llmq, &index, &verifier, &what);
            let core = named(
                oracle
                    .check_special_tx(input.core_chain, &tx_bytes, &input.context, true)
                    .expect(&what),
            );
            assert_eq!(ours, core, "{what}");
            if op == Op::AsProduced {
                assert_eq!(core, "accept", "{what}");
            }
            *table.entry((op, input.null, core)).or_default() += 1;
        }
    }
    let mut got = String::new();
    for ((op, null, reason), n) in &table {
        let kind = if *null {
            "null"
        } else {
            "non-null"
        };
        writeln!(got, "{op:?}\t{kind}\t{reason}\t{n}").expect("a string");
    }
    eprint!("{got}");
    assert_eq!(got, QCTX_TABLE, "Core's verdicts per operator");
    for op in ops() {
        for null in [false, true] {
            if !inputs.iter().any(|i| i.null == null && applies(op, i)) {
                continue;
            }
            assert!(
                table.keys().any(|(o, n, _)| *o == op && *n == null),
                "{op:?} on a {} input",
                if null {
                    "null"
                } else {
                    "non-null"
                }
            );
        }
    }
    let verdicts: usize = table.values().sum();
    let applied = inputs.iter().map(|i| ops().into_iter().filter(|&op| applies(op, i)).count());
    assert_eq!(verdicts, applied.sum::<usize>(), "one verdict per input and operator");
    assert_eq!(verdicts, 288, "the table's size (LQ2 D17)");
    let reasons: BTreeSet<&str> = table.keys().map(|(_, _, r)| r.as_str()).collect();
    assert_eq!(
        reasons,
        BTreeSet::from([
            "accept",
            "bad-qc-commitment-type",
            "bad-qc-height",
            "bad-qc-invalid",
            "bad-qc-invalid-null",
            "bad-qc-payload",
            "bad-qc-quorum-hash",
            "bad-qc-version",
        ]),
        "every CheckLLMQCommitment reason has a Core vector"
    );
}

/// `6c`'s step (EV1 §3.11) of each reason of `CheckLLMQCommitment`; `bad-qc-quorum-hash` is 5a
/// here, as each context holds one chain (5b is `elsewhere` in `ev1-pairs.toml`).
fn qc_step(reason: &str) -> Option<&'static str> {
    Some(match reason {
        "bad-qc-payload" => "6c:1",
        "bad-qc-commitment-type" => "6c:2",
        "bad-qc-version" => "6c:3",
        "bad-qc-height" => "6c:4",
        "bad-qc-quorum-hash" => "6c:5a",
        "bad-qc-invalid-null" => "6c:6",
        "bad-qc-invalid" => "6c:7",
        _ => return None,
    })
}

/// Whether `bytes` decode as a commitment.
fn decodes(bytes: &[u8]) -> bool {
    LlmqCommitment::decode(&mut &bytes[..]).is_ok()
}

/// The type-6 payload of `commitment`, mined at `mined`, with `a` and `b` applied: the
/// commitment operator that keeps it decodable first, `None` when neither does.
fn pair_payload(mined: u32, commitment: &[u8], a: Op, b: Op) -> Option<Vec<u8>> {
    let after_a = apply(commitment, a);
    let both = if decodes(&after_a) {
        apply(&after_a, b)
    } else {
        let after_b = apply(commitment, b);
        if !decodes(&after_b) {
            return None;
        }
        apply(&after_b, a)
    };
    let mut payload = qc_payload(mined, &both);
    for op in [a, b] {
        match op {
            Op::PayloadVersion(v) => payload[..2].copy_from_slice(&v.to_le_bytes()),
            Op::PayloadHeight(d) => {
                payload[2..6].copy_from_slice(&mined.wrapping_add_signed(d.into()).to_le_bytes())
            }
            _ => {}
        }
    }
    Some(payload)
}

/// `6c` of `contrib/node/ev1-pairs.toml`, in its restricted shape (D29): the steps, those this
/// target leaves to other tests, and the pairs listed infeasible or outside the world.
struct QcPairTable {
    steps: BTreeSet<String>,
    excluded: BTreeSet<String>,
    unreached: BTreeSet<(String, String)>,
}

fn qc_pair_table() -> QcPairTable {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../contrib/node/ev1-pairs.toml");
    let text = std::fs::read_to_string(path).expect("ev1-pairs.toml");
    let mut table = QcPairTable {
        steps: BTreeSet::new(),
        excluded: BTreeSet::new(),
        unreached: BTreeSet::new(),
    };
    let (mut kind, mut array) = (String::new(), None::<String>);
    for line in text.lines() {
        if let Some(k) = line.strip_prefix("[[").and_then(|l| l.strip_suffix("]]")) {
            kind = k.to_owned();
        } else if let Some(key) = line.strip_suffix(" = [") {
            array = Some(key.to_owned());
        } else if line == "]" {
            array = None;
        } else if let Some(item) = line.strip_prefix("  \"6c:").and_then(|l| l.strip_suffix("\","))
        {
            let mut words = format!("6c:{item}").split(' ').map(str::to_owned).collect::<Vec<_>>();
            let first = words.remove(0);
            match (kind.as_str(), array.as_deref()) {
                ("table", Some("steps")) => {
                    table.steps.insert(first);
                }
                ("unreachable" | "elsewhere", Some("steps")) => {
                    table.excluded.insert(first);
                }
                ("infeasible" | "world", Some("pairs")) => {
                    let second = words.first().expect("a pair").clone();
                    table.unreached.insert(ordered(first, second));
                }
                _ => {}
            }
        }
    }
    table
}

fn ordered(a: String, b: String) -> (String, String) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// EV1 D19's first reject for type 6 (EV1-5b-1, DEC-830 Q2): on each input, every two operators
/// whose own verdicts are different steps of `6c`, applied together, get the same verdict from
/// `check_special_tx_inner` as from Core's `CheckSpecialTx`; and every pair of `6c`'s driven
/// steps is reached as Core's first reject (one of the two), unless `ev1-pairs.toml` lists it
/// infeasible or outside the world, and none it lists is reached.
#[test]
fn qctx_pairs_first_reject_d19() {
    let oracle = Oracle::new(Pin::V24_0);
    let corpus = QcCorpus::new();
    let mut reached: BTreeSet<(String, String)> = BTreeSet::new();
    let mut compared = 0;
    for input in &corpus.inputs() {
        let deployments = HeightDeployments {
            params: input.params,
        };
        let store_chain = StoreChain(input.chain);
        let index = HeldIndex::of(input.chain, input.first);
        let prev = BlockId(input.mined - 1);
        let llmq = LlmqCtx::new(
            input.params,
            &store_chain,
            &deployments,
            bls_ctx_after(&deployments, Some(prev)),
        )
        .expect("in domain");
        let verifier = NodeCommitmentVerifier {
            llmq,
            members: &input.members,
        };
        let core = |payload: &[u8], what: &str| {
            let tx_bytes = qc_tx(payload);
            let tx: Tx = decode_tx(&mut &tx_bytes[..]).expect("a transaction").into_value();
            let core = named(
                oracle
                    .check_special_tx(input.core_chain, &tx_bytes, &input.context, true)
                    .expect(what),
            );
            (core, dispatched(input, &tx, &llmq, &index, &verifier, what))
        };
        let singles: Vec<(Op, &str)> = ops()
            .into_iter()
            .filter(|&op| applies(op, input))
            .filter_map(|op| {
                let what = format!("{}: {op:?}", input.what);
                let (core, _) = core(&op_payload(input.mined, &input.commitment, op), &what);
                Some((op, qc_step(&core)?))
            })
            .collect();
        for (i, &(a, step_a)) in singles.iter().enumerate() {
            for &(b, step_b) in &singles[i + 1..] {
                if step_a == step_b {
                    continue;
                }
                let Some(payload) = pair_payload(input.mined, &input.commitment, a, b) else {
                    continue;
                };
                let what = format!("{}: {a:?} + {b:?}", input.what);
                let (core, ours) = core(&payload, &what);
                assert_eq!(ours, core, "{what}");
                compared += 1;
                if qc_step(&core).is_some_and(|s| s == step_a || s == step_b) {
                    reached.insert(ordered(step_a.to_owned(), step_b.to_owned()));
                }
            }
        }
    }
    let file = qc_pair_table();
    let driven: Vec<&String> = file.steps.iter().filter(|s| !file.excluded.contains(*s)).collect();
    let mut problems = Vec::new();
    for (i, a) in driven.iter().enumerate() {
        for b in &driven[i + 1..] {
            let pair = ordered((*a).clone(), (*b).clone());
            match (file.unreached.contains(&pair), reached.contains(&pair)) {
                (false, false) => problems.push(format!("{a} {b}: not reached")),
                (true, true) => problems.push(format!("{a} {b}: listed, but reached")),
                _ => {}
            }
        }
    }
    eprintln!("6c: {compared} pairs compared, {} step pairs reached", reached.len());
    assert!(problems.is_empty(), "{problems:#?}");
    assert_eq!(driven.len(), 7, "6c's driven steps");
}

/// The verdicts of `qctx_bls_operators_match_core`, Core's and ours, per operator, input kind and
/// reason: an operator that stops running, or stops reaching its reason, fails the test.
const QCTX_TABLE: &str = "\
AsProduced\tnon-null\taccept\t9\n\
AsProduced\tnull\taccept\t4\n\
OtherScheme(QuorumPublicKey)\tnon-null\taccept\t3\n\
OtherScheme(QuorumPublicKey)\tnon-null\tbad-qc-invalid\t6\n\
OtherScheme(QuorumPublicKey)\tnull\taccept\t4\n\
OtherScheme(QuorumSig)\tnon-null\taccept\t1\n\
OtherScheme(QuorumSig)\tnon-null\tbad-qc-invalid\t8\n\
OtherScheme(QuorumSig)\tnull\taccept\t4\n\
OtherScheme(MembersSig)\tnon-null\tbad-qc-invalid\t9\n\
OtherScheme(MembersSig)\tnull\taccept\t4\n\
Infinity(QuorumPublicKey)\tnon-null\tbad-qc-payload\t9\n\
Infinity(QuorumPublicKey)\tnull\tbad-qc-payload\t4\n\
Infinity(QuorumSig)\tnon-null\tbad-qc-payload\t9\n\
Infinity(QuorumSig)\tnull\tbad-qc-payload\t4\n\
Infinity(MembersSig)\tnon-null\tbad-qc-payload\t9\n\
Infinity(MembersSig)\tnull\tbad-qc-payload\t4\n\
NonCanonical(QuorumPublicKey)\tnon-null\tbad-qc-payload\t9\n\
NonCanonical(QuorumPublicKey)\tnull\tbad-qc-payload\t4\n\
NonCanonical(QuorumSig)\tnon-null\tbad-qc-payload\t9\n\
NonCanonical(QuorumSig)\tnull\tbad-qc-payload\t4\n\
NonCanonical(MembersSig)\tnon-null\tbad-qc-payload\t9\n\
NonCanonical(MembersSig)\tnull\tbad-qc-payload\t4\n\
Zeros(QuorumPublicKey)\tnon-null\tbad-qc-invalid\t9\n\
Zeros(QuorumPublicKey)\tnull\taccept\t4\n\
Zeros(QuorumSig)\tnon-null\tbad-qc-invalid\t9\n\
Zeros(QuorumSig)\tnull\taccept\t4\n\
Zeros(MembersSig)\tnon-null\tbad-qc-invalid\t9\n\
Zeros(MembersSig)\tnull\taccept\t4\n\
FlipVersion\tnon-null\tbad-qc-invalid\t9\n\
FlipVersion\tnull\taccept\t4\n\
PayloadVersion(0)\tnon-null\tbad-qc-version\t9\n\
PayloadVersion(0)\tnull\tbad-qc-version\t4\n\
PayloadVersion(2)\tnon-null\tbad-qc-version\t9\n\
PayloadVersion(2)\tnull\tbad-qc-version\t4\n\
PayloadHeight(-1)\tnon-null\tbad-qc-height\t9\n\
PayloadHeight(-1)\tnull\tbad-qc-height\t4\n\
PayloadHeight(1)\tnon-null\tbad-qc-height\t9\n\
PayloadHeight(1)\tnull\tbad-qc-height\t4\n\
LlmqType(100)\tnon-null\tbad-qc-commitment-type\t9\n\
LlmqType(100)\tnull\tbad-qc-invalid-null\t2\n\
LlmqType(255)\tnon-null\tbad-qc-commitment-type\t9\n\
LlmqType(255)\tnull\tbad-qc-commitment-type\t4\n\
UnknownQuorumHash\tnull\tbad-qc-quorum-hash\t4\n\
LongerBitset(Signers)\tnon-null\tbad-qc-invalid\t9\n\
LongerBitset(Signers)\tnull\tbad-qc-invalid-null\t4\n\
LongerBitset(ValidMembers)\tnon-null\tbad-qc-invalid\t9\n\
LongerBitset(ValidMembers)\tnull\tbad-qc-invalid-null\t4\n\
";

/// The headers of `feature_protx_version-node0` at heights 0 to 203 (its `blk00000.dat`, linked
/// to its `debug.log` `UpdateTip` hashes): from genesis, so Core's context knows every block the
/// node would, and an unknown `quorumHash` reaches `CheckLLMQCommitment` (the shim refuses it on
/// a context starting above genesis, `oracle.cpp` `QuorumBaseInContext`).
const NULL_HEADERS: &str = concat!(
    "010000000000000000000000000000000000000000000000000000000000000000000000c762a6567f3cc092",
    "f0684bb62b7e00a84890b990f07cc71a6bb58d64b98e02e0b9968054ffff7f20ffba1000000000202e3df23e",
    "ec5cd6a86edd509539028e2c3a3dc05315eb28f2baa43218ca08000012819fab17e84f1e1348112732d3f194",
    "a7bbd9243e6220730dd555e6818d0136ba968054ffff7f20020000000000002047c8841adefcbfb3236572c8",
    "d9908812585598d12ff591670da497ee16ef13204a5ab1ac25cf44e4f45439e7c658276502a638bfe6c1d42f",
    "99cd451fa20a7c09bb968054ffff7f200000000000000020807ce669e5372b0f96142f9e582baf361ce38d9b",
    "8f3597cd4879bbac2e8ed62ff5e2f69628aa31e91612a24a054596886fe124fa85d187e06663dcc4ecabd8ba",
    "bb968054ffff7f20040000000000002042a5bfc6758c8b8d922faef63d7a07e95d99528d95c2a8a551e42346",
    "7c9cdc1c4cc365af0b3f859116e095c77dcb07a3c184d3c77aef3cc4d0cc4ce76ebfb06abc968054ffff7f20",
    "0000000000000020f9481815e86acbccfbfbddfe25bc3247882cec192643eda1fe3232f0af7f7e254923484b",
    "3d1782603cfd13bf27006ba85054719bbee1ec4572a4d215ed84c0f8bc968054ffff7f200300000000000020",
    "4c654abfacda3e5fea3bb184bd94ffcc5c96b86108171950000ca7efee1c2b24e92421ea86e9c0411b00d468",
    "5dfe8f9c9e89403017f51559b474a841f92adb73bc968054ffff7f20030000000000002006d2bbe4b8f6f96a",
    "38721a2694e1722f106d73df9f42f73fde795135112ff14f0b5b6eea6691a8436eeb3047dbdcc5e6aa7a6365",
    "316387ed2664854b43ce5ee7bc968054ffff7f200100000000000020269c45753282d3ea5bd8b0528eeb804a",
    "3e517bb93d45ea7d03538d8d3f77002a827c858b162cc6b72086a3e05cfc7dc2870f3247f86d2032fdf93768",
    "ba424403bd968054ffff7f200000000000000020a0d5bfd28e7cc6daf9da9660ed08d71ce4bb799858f83612",
    "41b1c536dafb1520767c84cc2acfe468d13457f3e4094b7756b0891ea51b363f0fd29fc742323722bd968054",
    "ffff7f200000000000200020b31cfdc19747b49ae8de0b7122319cfedbbbe7cc38bf9cf2ea824a227cc72237",
    "a2fbff8728a374cf5a05c6417ee77da553556b94147a082c80bcf4afebf1c7a5bd968054ffff7f2001000000",
    "002000208c74bf157017b38f1b9971a36a95c14f1b559f81e3e515bb334632a56ee3bf47a79692c492ddc451",
    "1c51326524318feaea1c3672be2fd929f1311dc0cce2f652bd968054ffff7f2001000000002000201f8570fb",
    "4d14f35acd08f55456328e67f56a71238658498d535d6536e2ff486b20c11acb34e6a71f137c062b9d9e53ec",
    "fa6fef014b743f2cb083709f6f6f122cbd968054ffff7f2000000000002000209dffd1a50e361a65a9b386cb",
    "7edb93f07354afef3a66d2964a4a30c4daa39f59ae612726431f965c3f9aa518c3df7fdc6d4628d7ac857ad1",
    "ac0c258b71bdf009bd968054ffff7f2000000000002000205a55453b8170cd4e934d308367cf5c34b9e29be3",
    "1d9f5ebf1bd7ca84c473825d25f14f68e4c2d732a0995b14698ab456a9a48fce921a7f32b4984480f0bb2215",
    "be968054ffff7f200000000000200020bbcd98e54b2b1667bb76998aaf51086a9fd459e0f657f6945474d909",
    "f47fe9612d05929cab8d8ad8e4d7c88594a87fc338e3e633643e77b46ac192c70954a16ebe968054ffff7f20",
    "01000000002000208f85055e524ece1e881cf55ee56ceac265f8ca4e98d7ba47ee895e70ea1e8d0aeaa8a844",
    "a7b88c6b52bb0fae93ee4efedf137704d52b8736d774f41e84b9f533be968054ffff7f200000000000200020",
    "a252a906feea8e16a61f68178586809c435b7fa8af62c121483313cd80b02b19e9a26f494d232f066119fc44",
    "bcc7f120303a70797bb4065c167ccf0fd1e2f5efbe968054ffff7f200100000000200020a71866be71b0e277",
    "2a52bcc53280e27a0be76c7dacbaa0ff625f51ff679e7c262842b8c6f7e5868edcd12c6440ad3cb5a080a943",
    "7e6f8f9c86b66c26bb29ed80be968054ffff7f200300000000200020486e01e269705532ae2505f7009dadc3",
    "ecea25bfbc72db5cd861c3514722b60d319cae2cb0fd05a70f938527a4eef395eadc089a11dd109dd860d9dc",
    "fb532157be968054ffff7f200000000000200020d9e12e31909f3aa6b2a7f5c6ffa4c216bdb903edb78d3152",
    "463b58e97e79221253286decbfce38cf9fe96d76d7178034bf031c114f2c005fd7e9b07dc2fecba7bf968054",
    "ffff7f200100000000200020635d3dc0880f469956359d91c37c7a47157302571ca5d391fedfb0ee70136a0d",
    "5f4fae6d2e3e512adc78524310428020999de6c944c3de18783afad187071530bf968054ffff7f2000000000",
    "002000202b23ab1c10847093f4d54ce3e2ad5e0fd402d5106bc2bbface54f3ba8652d814adb36e600d559965",
    "27f0e7960a2e4e71b0b53a1b6c21cfd31f35960b60bf545abf968054ffff7f200000000000200020bf95f771",
    "96fdbc0451042dd0fb632c18578853a768f0e66e089b9766a5dd0825fd74c6263d8142cbd75edb80d90c3fef",
    "f4edba714fd7cd1e3181e8d604271298bf968054ffff7f200000000000200020ca2ce71498cfa120e534370a",
    "04e775c39c0b05c5c91749e0ecb77aab7d8b095dee8146c8bcb4922e22ddadef83b74f97d67f10c672ac41b7",
    "efd1e0f695642254bf968054ffff7f200000000000200020123e8507126290820860e23039a3627863a4d176",
    "c4769f84a0d352bb737b5606ca67b3551832416eb30b358d923836c1d8f8d333aa64c4dfd14a888d2a0ce49c",
    "bf968054ffff7f200200000000200020c0167342b05537d4673e062e84b0e7edae17fb1c14dd2ac3cac4f01e",
    "0158282080a24a6369f131dfa2dfc1cde6799beadc9ccd9af4d7aeea77ed50b47537153ac0968054ffff7f20",
    "010000000020002031a47696a3c490e23ee931185005bf139e142d40899c0693ce8444e53c7b892c86848989",
    "629f406f0ed31830afb683be664d23aa8b6c2d5931437bb221e434afc0968054ffff7f200000000000200020",
    "37f024d1ec671df74b7bc4b4328c6281165d490c4ac53702630a535514d5e35be2e265f400da096b0e673414",
    "864846fde77d1deb452367149bd3131e624543efc0968054ffff7f200000000000200020b066d3b5516e83c1",
    "181ed878264f28884b2fb4bdf475ee873f2a5ae27a80ef5af6feac516e12e345215c51fc1ef9ef8f8e74d953",
    "96227bbcd368ad898453b231c0968054ffff7f2000000000002000207bf7d6cab5537c88f55b2b2571d10025",
    "d96c89fbd35fa156af27898c460b9962b3166c27446ab9f225514c9a2eb470f02d56872e30924cacd645b718",
    "bf3f0e03c0968054ffff7f2001000000002000208320e2caf4bb6a38cbfe3aff87dac40d542769536d61234c",
    "98a977201965807cc2b014eaf9ce3e9b960569121edccdc89a36a6068290e0716d5a747b1184ae43c0968054",
    "ffff7f2000000000002000201049968341548c655f58767b318b8af34042134a43acfe3e160614130f4a6610",
    "f08beebc13b752fa3a8c53ee556afe4ef44e979885c5192c6abb55491bb8188ac1968054ffff7f2000000000",
    "002000200ee454cc23413a1d7c1de4636ce8a65706eceddaf9173f219c80d825e6917d08311dcd8e64ef5557",
    "33fa384546e91f7aabdaaaed22f8c4989989775d9aad9878c1968054ffff7f200000000000200020aac521a3",
    "75427ae8caf3df722214d44d7cd3ad455ab01655c955159bda2d147156008e0fbbb1163da1ec7cb589022232",
    "70306b7f168664e19512480ae49709f1c1968054ffff7f200000000000200020b7c32a96138e3036282abc85",
    "bc241a5fedf87545732ccd9f16b6bb6183d7bd2625718236d50ed22fd601072bed2d6f329a20a24f0163333c",
    "05f60fd42c01e5f0c1968054ffff7f2001000000002000204072e94a178e37acda9c659cbf1b48449449208a",
    "78412507541bb3f3c5b16a03af5bb81e2b88ff501fce8b156b500b827464ca4290bf5ed8b259336b80e90ae7",
    "c1968054ffff7f20010000000020002034147ee0f5d2ec66df043b25b3884fb729f2baf46642474578aa1981",
    "0f6c434aa52743b0d723fb448780f0ede71bda8e359d57bb003ba547dbddd3c2e629c97ec1968054ffff7f20",
    "0000000000200020b14e5a5e180af656b2a7ee97ea3b25b3683886d099b104e65dcfb9d22e30905715ebb105",
    "51564f3d6fc5c2be53fc042bb3e0b8dc4436134beeeed9d47f2f7df7c2968054ffff7f200000000000200020",
    "333f0eb4ff94e40c6c8d8862a4ddffae3592a547019344928a48a3bcc4898261d3137053b343fcffab15e9d2",
    "68a0b2815c74f0596be6e09977460902924f7c24c2968054ffff7f200000000000200020c1ed1ce952890362",
    "429b0fa45ebc6b6852d35dc0496faed3f3ffeacf6784582d74a603758b9b5b0e8187491a06f45b0d362838a1",
    "10bfca2b4a5cbebd7e052116c2968054ffff7f20000000000020002091159bb37ef99f881c164ca9a55d80bf",
    "4bb4f250e76c9b2926096518527a270f4fce82871a71bf8980637ec4bdac782a7a15e171bdf602299388fe03",
    "56c4e3f5c2968054ffff7f200200000000200020a609be2c154357db57783f48f4ad44dc6560e2f04d256843",
    "dbc5c580783e17698265f97a207d69b1347f558a144c2f4a40badd391b1fe8ee8fc69aa9987ec040c2968054",
    "ffff7f200100000000200020f0fc6ee0cafcc43220d0eacc6b4d3377464cd20f969bb976e11a24e116abb137",
    "80a97ce92115a7fc1f5d2d73b1ebb44ca63c96de4afb8e7e742a97a626e56a08c2968054ffff7f2000000000",
    "002000207e308f9e1ead7d12df1ca092606fc672bbb010e8263120bd5c18f83e62c9a8704c1fe4f265297704",
    "0720ec09545f1166adaf5e42572ec3847557a9e00c2b80d4c3968054ffff7f200000000000200020528f2c55",
    "80e1b6e3fd69926c917d1b11828bad3e510822454ef99e0ac29ce529465b5eb07f4cd8530b3979bc59f72ead",
    "27889f0416c3a819ec32a61c923b812bc3968054ffff7f2003000000002000201ada5499d08e3e95f5da227c",
    "b37a3efb606ef790bff53843cf8a8aa003318a15042879dd31b34cc3392087b4f150e807a330325bda72cf10",
    "2a67360a76839e0dc3968054ffff7f200100000000200020893a16f34e9e3d0f74bac64b9c5e6ac2e0cbe40b",
    "3b7b40722f8e4f64322a166286a7974c130a485da86d4987a967c41555efdf74790ad66170ae4c4bdaeef13d",
    "c3968054ffff7f20010000000020002016bb36c67ebf53e9f5d05e78140e2c4bce69287b978437feb1e44ae6",
    "4a3ac363e088dfff880832a7c1407307a7f08e8f0fb20f040f62242f52b158b454fbf655c3968054ffff7f20",
    "020000000020002018658387bfdeb7fc6af63237d5a43652f7bded33706a09b0a35953f22ce1ce4656ea0144",
    "7bdcf7db5214883b0261e438d9ce90faf9639d46b812b43cdcaa8c9ec3968054ffff7f200000000000200020",
    "6bbb311e1f3e906ea1b8e3b8b6ddfbebc80a83f31b23178e063809a2f3486748d49379166d5d381ceb028d25",
    "62b8b674207f0d628ed7477d9598d41a766e942ec4968054ffff7f20010000000020002091513c86b57ba53b",
    "043f531c84f191a1e6a8a7d3cdd44950f16598a1b5710f6ba3ebd881391a4de7dcff62c3d8ce1b8e51dcd07e",
    "9e8d3ebad93e295ec8806cf6c4968054ffff7f200200000000200020bca99cd9ed7b498a1419c6c1a3cb009d",
    "e343ab20764a6def87e4b83f59f50d0e1db5b36c4281d9ee420caf5ff9c3ab22ef95a639c3930d9cb0ea25df",
    "f29d0dd5c4968054ffff7f200000000000200020a38459219e524055f80d50f986106f353846e19a5b7218e2",
    "b3ca1c1b5a5c50530b83eb20c7c1a09a82e6ed2eb64747398057dc0e7ad724d9b75bf824a607da84c4968054",
    "ffff7f200200000000200020c03478b7cf6c4cdc41ed927d289c6134d5c2e818d2fd25b6c8207524fdd2016b",
    "18dca0d42871b415a72a6521b9dbc6ae235e985b9f1a70b1a5a6478e4ed60832c4968054ffff7f2001000000",
    "00200020b997dd34ca0b8645e9abafb539a16fe6fb9de958d9f43a275db4d45d2cd8666213772e0823096411",
    "f06a0fb3bc17acc4c1c8bc8d6728b74092eb3b5769933657c4968054ffff7f200000000000200020eb4d3df2",
    "8ed9804d1e2203169c7e58e247ea3b4fc6e3bcc9543f55643cd93f0e1bbc74eaa394c2ef34b9d1df675922b5",
    "0f29eb70c59f9b9d236ea74e986a64c6c5968054ffff7f200000000000200020e8e76ab018434829ecd62fce",
    "e603bc6a71945d191cd3ea533c4ffcdabeea8503b3cb197a4055493d012690aa5c7fcb7a4d71bed913b38bef",
    "7f4b52e502d58083c5968054ffff7f200000000000200020431ff28b20bb090b0f50b25f77d8a0bcbbea2462",
    "8fdb8f68616c06b10a00a8245794cb1cc39187cecd537381d8232626959ca441aa8a92b1180eb2ef848a2f73",
    "c5968054ffff7f200000000000200020b7964652256ca3bdeef3c0d7109e726f9bd22e2e3f43ce4f702be8dc",
    "d1821c1916f223807e50151db191a3bada2170f38d0c1630b9a6eb10584987075782785dc5968054ffff7f20",
    "01000000002000204e87d1eb6d31836e9267e4d8aac2176fd1e61acc4444640521d210da762afe41fdcb50e5",
    "66e658fbf5bcab36e3ddca111ede1b698fe37e8d7fc542f8a090abbec5968054ffff7f200000000000200020",
    "5b28e23fff8b62ec05fb8aa99c8fc779a46dc0320cde62afb779d569ec9a3768b56878db50b6936454e686b8",
    "b75be27e80915c52c8887611a2038e485803e1b4c5968054ffff7f2001000000002000208715ca2b01b65087",
    "09aa9fd52eccab116ba912b7f43105be87f19c4c4f5abc3a6cd212e7f44560079b96ea90665d24116448ba90",
    "ae19e0dea6dda82ac40558dcc6968054ffff7f20000000000020002051de6568857383567b5f297d481d1f68",
    "94d34d3e87b3ec1c2cfdf1691f1f74292c31dd72fd32c9eb0b67305313856518d6d3c51bb4fe293b5876f976",
    "021f7c2dc6968054ffff7f2000000000002000203d58a23493dd29cdef405ec178e42e0d9b63b21001ba2edf",
    "3c62c26c46f00c2aede6d6d4d09b276c9bce5d0ff6770a93d91c6d422ec0d361b1e6b78dab9f277ec6968054",
    "ffff7f2000000000002000206e30785318d93d0d7a9da39bc359c99f0bbe7b5623dea5f71f735350520fd751",
    "a8c09f05f28f400adc11b6d0a71d373a4d748126476298eb1ab59371f2bd7110c6968054ffff7f2000000000",
    "002000209d9d39edc1e21e51dd21e3dd82ad1745124e7e43eb51141d0d4370f198ee141260ff99ee703954b0",
    "4d4b0f57607718f1f3433e43efeb00c803397d02f51c1972c6968054ffff7f2000000000002000202306aca2",
    "2f465beb293c6440bbcaf484abacf4e84cb336bb231e0e9652deca7794d33a8d0dbcaf3d8c6b814d477ed084",
    "031df97ca22ce744a27e1ee9a3d3be27c6968054ffff7f200000000000200020f28714cc821bf6a7322a128a",
    "7ad864903fd11d10bb4de5e9cfef91a25a4d78781e8040d57802c96a8f594a4e2a6affe271bac57dbc7f061f",
    "7ba9db1efccf09fac7968054ffff7f20010000000020002063f5a585a60f380732cfba184d7fd701965b4963",
    "922d787d746278bafee0220a0385b37d452ccbc0867079529753ce4216d4fd0bf0e79d30677b77db86ea5518",
    "c7968054ffff7f2002000000002000202cfddc2ae47aa20dd0fc87de0ac9cf37f020465baa740aaa2108d380",
    "f744a12eacc3ca2eadb6d0b6611bbbaf2bca9ad456fba9ea2ae392cebb2c34ce1c7d34f8c7968054ffff7f20",
    "0200000000200020259edde64e26d429d594af5ca9ea3b2660497ab870034b75dd5b2c523d5af73ed5e7ca61",
    "ad7b44dcba02f6ce6d8e1b18c6de0a86bfe7b863c44c20b6b9fad907c7968054ffff7f200000000000200020",
    "56ef30f9a3eec37bcf157e5e771d2d583748e95565eadf9fa34d95618bfb58722f914c0f8d8ca1f6ccbb3914",
    "3b7a643083e5eb5d750ffa09cafeae34eab444abc7968054ffff7f200200000000200020a399223a7b14e4ad",
    "3ea39f6f671a64af8d7d370f87b9e1264da8af9aa47a60052d8057229e99825da64d0743f1b12b44a79bce76",
    "644aec73b6ff8fcce37d3495c7968054ffff7f200000000000200020baf94b7c226d71f80fe9347a84afa490",
    "471bbc07a243e5381b2bf152c57dd601c5dfaddc1803336f4808e7b965d09a3ed0cfd5785ea442c456e0823e",
    "0d9234f0c8968054ffff7f20000000000020002086d1b0e63463f33d1ab3276520f5f2a062f2b68706111ca1",
    "a0fd32611f34cb535fe7c59a5ed086e583d718263757ebeda010c2c42572115c89e260a58b9eeaf9c8968054",
    "ffff7f20010000000020002081cefff01a4ec461ff517ccd1458420041973243a4f7fc797566c8b0a63dea5b",
    "643007ba022140f8b43ec3cdf7271a6cdbe8faf85000104a0bb036130a4234a7c8968054ffff7f2000000000",
    "002000204173a34d8396704c0c46730c5c240fbd52c7705fa63025cce8291c99d7df100c1121a9a693727497",
    "ac86f50340ecd633713df20d00e0c04334d5e1a7f98196fbc8968054ffff7f2002000000002000206ce8bfa6",
    "b3e40dc19a5f5d7d69a6a52f29271c87d18cdcb6e27bd4c9cead2c79276f00d9256bc3512b18a494aa914b82",
    "5943c86b4176192fcd78a341a95a2b25c8968054ffff7f20030000000020002084309af3b76b904f68bfba90",
    "d4f851b1919d294ab2a06dafeefe1686c1b0c875b6658251ecd0346ab967361a2f45e9bf6d55d98fc6a157e8",
    "2a8b07bccb7fe2d4c8968054ffff7f200000000000200020d6a83c53c66a7d61753f4f5f035d38ff1114270f",
    "9ceeaa559a95a9bf3e9a53681accd61a585277788dde47cb9db415249bd673e495f2e28d3d76e86a2357414a",
    "c9968054ffff7f20010000000020002019b792a439950d06df09cae685e3b47db7fa026d400f80b94f728199",
    "84da303bfdb876793a196aa690e750f8c4ea4bdaf47081442342442bfbe4e5445c920ed5c9968054ffff7f20",
    "0100000000200020554cfc1aa1a4aa3cfb8f4e3c61ab8228f84b9035b8e488bca3305063836476781b8a0eaa",
    "e256d7a8200ee5ea35f9ab4bc99fe9e50aafdfdb80a38c2380c2b127c9968054ffff7f200000000000200020",
    "0003bec541d47b8d506be789887e57949d39c18bda66f1f448082b8fd9b86f7aea8b359577e0c98a9c3a0c01",
    "32f59b2e9025794d030b5abeb94a66f65b8d10cdc9968054ffff7f200000000000200020b6432c825192d309",
    "67db9f846e4ea0b07c78f2c6e5c8518df685768ba06b74254cd3db9533a6296d3e404608fc7c631fc779e091",
    "77f1ac1876ecc90dd653b783c9968054ffff7f200100000000200020c5eb16a6b78138ccc9b27322ea68c2bf",
    "0105f76815ffe099a895c98057c24069efbec68cec23d7781e45576253d22b371b352f8c6766ee6a6f6e3fa7",
    "02f36be4c9968054ffff7f20000000000020002034b1d7f07e5bb50ad3823cd78221de20c52018e62c9a9269",
    "61cdcad44de2555250ad78e98bcb297d9a508a452d5cd750670e273a9513516b4ecd670d5e514200ca968054",
    "ffff7f200100000000200020869b00e98a893f06c0b136effdd3022f4872ac5d1e44c9b3ff37fb6a25521c46",
    "358e43d6c98bcb7f41b1ae2b25b689458a38a122bbfea74dd6bdd799f2756b49ca968054ffff7f2000000000",
    "002000201d787c17f57411806d2d09a94fa7624da1d8c6cde69a6ef95130a684882aa76215e13677f0819890",
    "f71a46a51c45a8e2e620b4b83c58b851522765cd6d7d2280ca968054ffff7f2000000000002000204b47ec4f",
    "b6d63e743c413d0561c1527ea2314232c600a3bb33775ca6a1ff992a0e0832284d977927064c1546e0915fe7",
    "db9dbfd3cf8994d28e32d6757c9b35bbca968054ffff7f2002000000002000203b8be15ade2793919b14ff0f",
    "514f229512350a6e2441da843f45b942f475cc56576772848b3462047f89b0bdc0afc1b40e1aa082dbcc521d",
    "0f62e1b24f046867ca968054ffff7f200000000000200020a391a4dd95888d2f1f94ece820529f81e4dc2705",
    "8906ef56b214e6d9e4945a6c79d36d2d396e79b4531044c6fb38f598e2373950a50f0e768c5b5005455e12a5",
    "ca968054ffff7f200100000000200020e0867d1512693f9aaed5cc11042f2ffafc5f745a1f4114c6faa529f0",
    "d5b2573518484e25e202a1ee7f2174ae91ff3267fd9830a3984a9d4b174fb84eb185c3ebcb968054ffff7f20",
    "0000000000200020b4266506c877d110c1407e2ce9dd2fdcc39b97af26427e701dd3d0a9be4a724283f7698b",
    "34901cb5b55eae0b86f956f6f8d88675792abc3c86e53a03fd8094a1cb968054ffff7f200100000000200020",
    "bd701fe62d2cc9cd71376b37a706672df61fb4b66898e91ca8c3262c4765e235bfa5cd940c0773da762d1e81",
    "b5667f8b3e1332a81709357629f7aa210f8f926bcb968054ffff7f2000000000002000207fa4d38cb267ad58",
    "ae79ccdfc56694043b01d728122800dc908579290130e519222c8af721c8a5d78fd5e131de3cf91ec023c8bc",
    "0bea2ede56502a471980a8c6cb968054ffff7f2000000000002000200a44079712bbbf5e8c6725e260238348",
    "8a6b1920ad7919109b10516ddc37621f3bf8472965b2b658f62c8660390df94991716b117fa54ba094d63415",
    "05d39467cb968054ffff7f200100000000200020dbb250b8517e536a71aafad3052fc2b8eb65f6f0b1879461",
    "c678c77a1878ab3e89c80dad77d73c0678723a961623024cff76828021859da30a490f9905f0796ecb968054",
    "ffff7f200100000000200020ef1d4d93090d52bb47f19870267a83580f12b61552f5a32447d0a68b9b4ec34c",
    "8b499305e5f971a0e5db0b6377e0dcbbe18776210ea91c7a972f7bdfea90bc3dcc968054ffff7f2000000000",
    "002000203ec668f265a81ba80ee7188b4d406cda0241444ffca80085baced74afcc6f77e6feb956f9d28dac1",
    "dc3708f5be535b9b526ba0293bc14de373811328e6aebe7ecc968054ffff7f2000000000002000202dded7d1",
    "e2c05decaeac08fdda13ac81184867ecfb405a91efd7bca67faff4051f5a8e8f213e87053da787faad22a788",
    "d3aa5653aa4640ab08757fe2058a1d47cc968054ffff7f2001000000002000205d7c0276b796cdd455a77c56",
    "d555adce13c538d0ed2f4f0c978e39c832339f747a41b3d43f33ba28a68d34fd9204456224ca9c6801def5f7",
    "f7746c19160d9c66cc968054ffff7f20000000000020002006d45dd85bb75a91eb424a9537c2a8746301fa31",
    "cc9717234fbc3793fb37f45530673511f5faec8da05d1455ca42361e555f113ef3a496e0e746c8fdf729d7c7",
    "cc968054ffff7f200000000000200020a091e2546602eeb8578306c4ab81ebd967ac64f290a099bcc7dc207a",
    "12b8f73c746c9e06cacc8b029ecd4f054d8b87f3244cb22c12bb7810a7500a638de031cfcc968054ffff7f20",
    "0300000000200020a7299b4d0e9368dab92a022c02a986bbd3f307804e611e106727601fafe4af144a960ebe",
    "3dbd9e4cc98659435752876f551813135260df3969922accca7dc09fcd968054ffff7f200000000000200020",
    "c66669832bf024ce26d3288130bfb5cea18505b974e48af35e93dbaff427274e6def017322be2f71c60af8cb",
    "713eca67491bba7c647220ac337240d9e1b2db28cd968054ffff7f20010000000020002019bcabe8fc4ac20e",
    "e6bbdf8d1038adaba44ad39444b385a06b5d7178ba853b6f19e10c423cb1dd0a42aafb9687649655cb344f7f",
    "f9c34d581f8736c2dca1aa7acd968054ffff7f200000000000200020d02cb616dfae5a7faf8610a44c32905c",
    "c0c4829c7338258f9e523b5f61d9c97aba8921ca5ef26da0062407a9f6ba26c3f9ebf70cd76b3ffc6b27276e",
    "2f5bc698cd968054ffff7f2001000000002000205b6eee49d23e622cad6e6eda4e0831ab9acda9f7ef325363",
    "517b11e38e78aa6d1a7544ca160ef0c5f9a056a8a9f07e5c49d053b444e2fc2c37f5744e8e15fdf7cd968054",
    "ffff7f20040000000020002066c6823d2db77303982d74cf1014d32a5420d44e38bd5a02ba233ab124c0a338",
    "2688a7e4918118d6b57597916741541344bfdb3363a5a5cd5e5c7d0014d32362cd968054ffff7f2001000000",
    "00200020b2c77b7a601c45e455d83db2719638cd61f077d68b00572767536d5717098a1e2f09c61e4dad8548",
    "015e43d1cfb6dcc0befb76de14bea3caade93d1bd631b531ce968054ffff7f20000000000020002039d6cbe9",
    "1e6192bb86dea863ea26a935d6393e289e13f19d47da23a4100cb15422e3dd28f35bc943544bc19a280eb02c",
    "49a4a1ba058407458efcc6f1d801de9ace968054ffff7f2004000000002000203d9726905b77e8af02539b22",
    "3635c6c580f6c8c8983aecc2e45d683474ab01732aa390d62edada32daa83082d9bc48324b06e290558dfe38",
    "f847e9405c29b96ece968054ffff7f200100000000200020a66ebc59034511bfd6a5fbaebe8f5c13d70db55c",
    "e1b8bb62a181d2232bb1be743423f1d243b8ed48564e9e3783781fd1b3c4e206645674528fb30d2a20f559b8",
    "ce968054ffff7f200200000000200020d1411e34753d1377f6fd1e82a6e23712c40d9923e3974c1ed8574171",
    "96e0753ffdce5600034f148f95027f6e745cbcb1d73d5e707b1397856eb813dd38cd8506ce968054ffff7f20",
    "0000000000200020462399ffabdd8cbd8a41f2c48506265cf604636fa028838db53c500047873413c6005a37",
    "5b25d320248e8f0d55c30f972aef0dcea4c78f92fb8aa4067f5fbef3ce968054ffff7f200000000000200020",
    "57ba8f4204321b29d6254c4a7c580113ae33d984fe45c2bf31ebe4908a2eb54eed02de72402c53d1020ebd0b",
    "d93048b93fcc5b14e89490f77a47e8674398716ccf968054ffff7f200400000000200020a3857a1782fc5e43",
    "8e6b914bd23cf466b8a681a22e93d689e96e699ef63d343d4a42cd5b4888611e6bc8b9b60395add2c41d3be8",
    "81d6a086d0038dd207ca36fdcf968054ffff7f200000000000200020c70ffb2fe7a0c33ab65f654444f1d0ca",
    "9934e37cca39928675f1e13000dd5139428550304cd9c63dd5d40fe543af9ba1ef4e9fe0638423cc7d67718f",
    "7c16ed71cf968054ffff7f20000000000020002087279ff4d569fd4e0ddb31b51609af614414d2b7a0ce4bc7",
    "2ee14c13516c05039baf53eb123afb4f19bc7cfc381225f42f7458fb1ca0da8f12d9d898e9665fadcf968054",
    "ffff7f20010000000020002072bb907b01ad2556640aa58df6a0b458d0c0f3c83fe68d1186e0b92d0f3f7d04",
    "1ca6d7e25c9a56b141748fc731cae2ac99d74fae054ccdce3a15e44dd9ff1ed8cf968054ffff7f2002000000",
    "00200020d9e94227624597a948422739031b28891d455b25fe4bfc840ddff84364f6f61ac9e29e8ceda7cb51",
    "fbe688b241eb8f27dfd7ea4e7dc1897e4b12588af070b31fcf968054ffff7f200100000000200020ecd1fb1e",
    "c88bda8a155ea54e74761cb3f9c413968c930e4dd072d632872e79076381c9bd28f497929070ca95dda62e99",
    "43f18e89e4a90fa4ad649f2173365e1dd0968054ffff7f20040000000020002004de744cbcf785e165fdfa4a",
    "5584deef66440adf58baf56bb81a9fb6aab04a1a76c2b84c77eb45bf07f808adccc7dd34c3ac1f84a4d2d697",
    "371137ff79ea52f8d0968054ffff7f200100000000200020eb1c175e32b7fde2580d42beed13e415268eb865",
    "9e6b9917a4b6a848cf86a247c030d9e6d8d713a25c07b43660f4349e99c43f5adff6f047d9842add332f498d",
    "d0968054ffff7f200300000000200020da2f4f94927775a85dd1b689a23d2231aa9be83fda50a623e3eacacc",
    "95f66c5f998335d207baa8417221952b924e66dfe3d6dd28760c4100c9232eb7b0c32fd6d0968054ffff7f20",
    "010000000020002047b6786567b5d8622fb740640abb0f4231ee619b614e08d004c5dfcd2346f769707ddc50",
    "fd5a55f0467cefc31983f1e4aa30730cd82c5cd1a935a62c80a14fe6d0968054ffff7f200000000000200020",
    "042d7ab0d2f36683782706313e0ddc74df219d685d838991be6ad953b568ea202fe64eb621d997f73c9d16e5",
    "61670cd36350535ce5fb4cba6bc1cc80acb9a134d0968054ffff7f20000000000020002087725e3e801ae740",
    "8c4628853f64e60133429f27b877896d85d746732d5a6061151e76530839fadb908bf8510e3e980d8fd8baf8",
    "c9b049562d38b3d1ca6949a0d1968054ffff7f20020000000020002062eae9eac4dd8588f8e8d04ce71a9379",
    "3e65934f7b54848fc4d7db267efe2b090e6bc1c9b57d821b6b06008ccc83a541b7ecc5ade47ebfcf4377fae0",
    "2ffc4612d1968054ffff7f200000000000200020071f3f6816dffee99e27709fc572c475e1f9184cb4317e3d",
    "9d173ba915cd1b3b84274365b1e8e685f28db64a08b1b4dc56fe5a99254f76ddbf7be241e6a049d738998054",
    "ffff7f200000000000200020567ce8cdbbe5c6b185d41a403a49af90cc03d33f306d65704ef584e2f9b79d26",
    "5292cfb573c0704d38214c4d755df900b667de4310f601b49c9a9d69356eb3b7919b8054ffff7f2001000000",
    "002000208c3147069030fcab834d5680ee84e6269a563f0c56a9caa4c2257a0c95385d7a1e5bbe0d9e20e48c",
    "846c22c10510ab8671e9c7afc1d46626851403a9d5f35431139e8054ffff7f2001000000002000200de80d59",
    "21baeab3bdf8eeb3cd9ffa09a0a4874827dfb4d4727f9d2228d092700076d716b6fa5b19b835b0890c46a9b1",
    "d7b611705c56d5f716468b499d9b66686ca08054ffff7f200000000000200020a4a8e5c0ef22244924d32eda",
    "c54baaacd7996ac0f5806f7cfe7eaf83efa54d0b943df969ec3fe54f3ec7aafb7ecdab0c7a2c69f4ec07c168",
    "ac39cae188aba50ee5a28054ffff7f200200000000200020909ee3320ece17bb8799c9920565c36679743a02",
    "f74c896bed47ef65b8777c526d6b9c8b2dc5c8095d5dc5f651f69d608345099fbadc8951a792789681536fad",
    "3ea58054ffff7f2000000000002000205f78d0032d9cc1754c41fa80c9bdcab103ce422734f68e48237710db",
    "7c42410977f7b6e957160080d9fda99f0c5c66c618615a85e93d6bbccc65057268e562fb61a58054ffff7f20",
    "0700000000200020a2db74f5246520fb856d4a31c120934943bb14254be8c6fffaaa5f1c22d3e340c50782ad",
    "4778273bd15809248783c4be2c7c0c6af6f7b5a04de1ac929dfbe33e61a58054ffff7f200000000000200020",
    "16eafc230022a45a5884607b912db041621f6360800b152a1c3f9a7ca43d0333c2781444af89a38e31f58b1e",
    "27b5cea0840d102637b466676bf46bc256d673b261a58054ffff7f200000000000200020addb61bcca58af58",
    "bb00448c62da4539e536a81750a0221ff38ff3058ef22d7f1028614bdfe8bd6a0b859c0432503ca7d7c8abc6",
    "1938f45c60ca6aed2f3edebe61a58054ffff7f200300000000200020e60d5f0225ba58451ba65a288452ae43",
    "4bcf066b15d5974764c462679c8f9c4218101778530a239658e85685c177a22e329c5b349268f7c4529781c8",
    "b4c7099461a58054ffff7f20000000000020002037eb564404d369e9b1199bd2f5b484c1613160e84f0ac130",
    "80ce25cbf76e85321886b4bc2414c5da77bb21e2be018fb3a0f553717eea0b82c469113e7054313f61a58054",
    "ffff7f200100000000200020b9370d337af19f99eed36c4e9f6e89fa590e317172382401e73bcc4b9da1e817",
    "91ff181ce9dce4ae5f1377fc762c6ce81b7bec54c9c8ece7b40f6284710cb9f262a58054ffff7f2001000000",
    "002000202ea6f86524edbc807d024bdb82cdc7ae15e38945c22038ebbd816fff17cd6b3db08d74720df83707",
    "e185b4d05d42ca71a40a2fbf0578fd4328fe7840ee92478462a58054ffff7f2000000000002000305073d98e",
    "fc495f883c65b4ed2509839b517df11d757a14a5e2760bc3b253a95c6d9841835fff6b197a1c474e0f6bf477",
    "cae03c40444d093b72965c61e471d86562a58054ffff7f20030000000020003019e28df33dcd69e96ef48001",
    "66464d6cd3eca9b5b01657c47c8212bcc877a617044b48eec7084fe3afeeff21da0d057b4981cbdf1fcb889c",
    "b9b722225103f59162a58054ffff7f2001000000002000308afb28d6cc44a75c72bbd4018359592c7f32f311",
    "75c1f0915d3c6d2352868a3ab96a627c889ed5f0282cbad8a319a648152bd35553038480869b7939a28cc577",
    "62a58054ffff7f200100000000200030aecb1768ada87d41b4fe0e2a5cfd6a8c658526556687d74090643f8d",
    "cd8aa6281e751e9a65c5658fb027b493443c3ada092d02131277405859a78b4d0511425e63a58054ffff7f20",
    "0100000000200030e52133048fae6c9c82288f6bf4eadefc96d71347e09d3a06833cf733e54f4121d4da09a9",
    "a97ee803f5216b0c7117489ada3c7dbf50e875abb095b55275f42abe63a58054ffff7f200100000000200030",
    "963a87966e2ecaa68086a517c0561d02546346625db6faed7c31d3daaaee220f0f9ac610f2c9844a01fc7d9d",
    "581bcd51fe288298523d7cf39f66c55eed8239ae64a58054ffff7f200000000000200030603de9709588171e",
    "c0ba4c63ee07fbe3bc9f2dacddd7d640858a1c619ffa0658e15cb51b03b31893190720106fa4fd5b855043d3",
    "1fc06417d2f6ec0d9c199f5264a58054ffff7f200000000000200030f4eb4984fe3eb43144bce79b0cd47c08",
    "24acd2c39fd4a742e43496d4ae862873443d0435111a4da1530c5f9a2bc4c0cf1b786e8d24f8f4f6d72e1f61",
    "13e6531e65a58054ffff7f200100000000200030f7ffe8032a62621c3cd97faf4b816374ba3f9099ba7b9353",
    "ef2178abe01d7946686c7b105b2adae4413e81e7a33788073c82c161892758bbb06bec2beb05038c65a58054",
    "ffff7f2000000000002000304cfc49a215183d0710b0be638ed41884eed29a220a02ec0d9f604209339b0652",
    "d49d929f39e4d348c6bb63178478e25a2614ed1d54f1f3503545ab5b3e84602966a58054ffff7f2000000000",
    "002000307ef3f90cb4dce7eba0def309fa286d2214376d1e603281443d2f495c35a6fe10bcbf6f8929bfd422",
    "4aff7f2564c3380b3c5e89225a9ca625d1bc131626dbd5a266a58054ffff7f200100000000200030d2de9d70",
    "c05685d6425fe216b317e28a34ef3d232205499c4c8a6f76fe1a822a71d039acf1314253e78715bb30c50a7c",
    "c24d19b4f225f1cdd722117853b1953667a58054ffff7f200200000000200030d602d6dfae7e2ef1289096e9",
    "ae89789cbd8c5c4be2119e834cb816f726f4ae0c629adfe7fab1a1f738da45c1a13332556da61103bba9fd1c",
    "5a0fe84917a25ed667a58054ffff7f2004000000002000301ccede40736935302fca393a65b69640671484b0",
    "fd587b6fc765bf16d8869d2219ab2b9345a0955f608551f01b382a3cd904233d663a1f69b1ad578fbaaf2b41",
    "67a58054ffff7f200000000000200030085c1efa64c841f51da8fcbf42d43695069887ca25988b4cdae49743",
    "cb1f310b60d5c13691184dbd1f0d94856f674be0d86b0dbd0def281459932257047c019867a58054ffff7f20",
    "0100000000200030523f74f2a7c6f7b4cd21fa71a9d9c507210dee0a31ce9ab6c5bdedae0a60830d4371f687",
    "c612e07beab3d8f9addd2bd34014285126c04465f46351ece2bfb67667a58054ffff7f200100000000200030",
    "cebab9d9ce5b3bc4e060995066679a82e33ee2ce7d1609b5318163bd3ffe0633bf2b8e256f807d50189d459b",
    "465f7addd1b75a0683df8f1658e486b6ec502e2267a58054ffff7f200400000000200030031a002a5e85ddcc",
    "7abb2dc024a356cebcdd9b1af755c4a8ae1a018cd4bcd64e035113c22ba907f4a84cdeaa84548e53163b5ad7",
    "3d9358a98476345a43d10dc568a58054ffff7f20010000000020003042269893db5299a738f6f081287a3e03",
    "cbf70cca496110f4a4bf3d694dc1937ea73d8ff69e08fa3ca67c90babc28f7930bc1f65444dfc1b914d50e04",
    "f1ac717868a58054ffff7f2001000000002000302f4ebb4e259ae47ee0ca103caa19c24bac69e93a2cd63a71",
    "398ba708d3335324d2ae3fd701277c38f3309396943ac48b4bac0a1f458e97cb476b8f1540b92d7868a58054",
    "ffff7f2005000000002000301202fbacb580d7ef3c739ed63995bbcffeae91362c98dec6a3a2d1fd333de106",
    "5d0613ae805eae8e28a3588aebea75e44ed0d4f57f31636a93ca7d26a53295e47aa58054ffff7f2004000000",
    "00200030ef1c4ef84d514465ed5a1369d6f24726da4c0470835f3beb342b819bfd095944d17cf5c36362e07e",
    "55f0f38989a80d1c6c31f1f7879e9b899c17266355c628737aa58054ffff7f2002000000002000304c17547c",
    "0b7d3b0d23c545012ee1a814b901fadd0e61bc3200413a8a1e44bd25663974324ea9b6f6872ff356039bf389",
    "e0be7b9ed161e5abf5db62ec412c6c897aa58054ffff7f20010000000020003070a9f7702e6b4031571b0e66",
    "a85460edea97ef0b88ebe5a68539e13673a1742eb58663318934013445574fb534c2b4b7602dddee1fd20afe",
    "6a49ff4c901606027aa58054ffff7f20040000000020003002c6ab07dd29d41147a6f98204793330ad32dbc6",
    "ca78024ffb140ce6e43b1133519d7672090588e69ed52b96159f98afc1a7f1183042fa776fd5c95abb9a1c38",
    "7aa58054ffff7f200d0000000020003032e5113f9b1490a45d3a2f7dee117488753cac470f53e9871a46180f",
    "22e6dd083ffb66d3d913dec31c72c1ef9aef96c60678b51a51f502d5a51377ffa6f0df327aa58054ffff7f20",
    "0000000000200030abc8d305f59e950c5e12a30d609ddd8b951bb9e83209f135f7e627cce52a7e42192601e7",
    "c7038a5a10f60baf16259e3d2e7d49a40f9905b5c6652013de60bcee7ba58054ffff7f200500000000200030",
    "d2d6efce8ff42be3754502f6f655da8a561984a3ca485e0514f122a9106a7835c42992644afabfeceef17bcb",
    "a77c356ecf6c832448c117d24f9dc6d0ce6800857ba58054ffff7f2000000000002000303ac499748d8447e9",
    "2fe4f4fa84e748eee1cd32e30079eca7dea15ebc7e5d68130789e9a201d402aa5776f465897b15b32a58db68",
    "7fb47c5462548433e98467427ba58054ffff7f2001000000002000306853d61d5a44ebadd38fbe932c862efe",
    "e32c89b31d51136260fb5d8431606855c89d9a251007049e92f6fa22000767369964d112e59be17245973040",
    "e373c3f17ba58054ffff7f200500000000200030432d41bd28759e430676b37902e69b7ad99991538c6fd2fe",
    "3ce7af47d290505d1d992733e0774832bbec65a8731b3029880c82fe6dc2d759028d7a8c3c8069ee84a58054",
    "ffff7f2000000000002000304bce6e824af0b3c030aea10484d45f286378020a47c4e20b72dc84092f5d4141",
    "a7be473e43529cc6be8536e0060d90debb00354be2b3cdade233780b5099b07684a58054ffff7f2000000000",
    "002000304dbd34e2f6e9c1d03b040fc5b0a8d1ea5ee63583c2d2b32a5b4c696afb467b390b16912cf8052995",
    "b1b390aa2cd6cdd1eb83664bc91da58572a44543dc0f8ef984a58054ffff7f200300000000200030935c03d1",
    "7a06a817a04d218aee17a26ff5b3a3a907fd5a55e9d4fc1d01c51a5afc545c2724775059efda817e22148047",
    "421e37aac7acb6fd187df336f1e6c3e984a58054ffff7f20010000000020003044322090e0e011c558acec21",
    "a4533368f93d1f64920bda00f1a25bd0822a42366c9d27929bbfe8720d2a7f0a3ba7ab0f330e47d1dfffc589",
    "9274d7d3f51bc35184a58054ffff7f200000000000200030d570fdc47e93fd62753668a922a23e1775318ac1",
    "c9de74c5066c1c6570e92f6c98fc718eac2c981b638b2309e0b71bce591d766667bb099e0a156005c22dec78",
    "84a58054ffff7f2000000000002000306cc889258a481793b9ee1c7eb2861b18c19dfc387a8cafe5fd0bfd37",
    "5e94b73205613a59035fbee5b2d0daccbbc95859026768a49efc7f49abc1ebe3fde7f17085a58054ffff7f20",
    "0000000000200030237d9cd76f3ba1d3eb912aa30c4304a10cb28e525280fa43c08b507b6fe781055418fbed",
    "7b253b4d70646d887c7ed81899a0f7c231d04e9b84edfe0a22a06bd685a58054ffff7f200000000000200030",
    "2beba9ff6ce0ca6f7414aec16920e11dad64b950b7ccd59ab53352b5c5998066acbadf6b8596f340be4e00a7",
    "2c9cde6efdd6de4670240f2a2b99fa88a15a2e7c85a58054ffff7f200000000000200030a8d5ae925979c2a0",
    "c354bb277bdd2046645ac1ab42d2ea11e45d21fe128725700a277331422c748a49e304c80eb7c0b76bc48aa4",
    "77c951caefdb8f19c6d5086b85a58054ffff7f200000000000200030df0d056147f31ff87c43dc9abb62ee46",
    "36c91fb2bc088ef2b893c49da2b5cf5c50f23d455a5183e3706e114b1f9a9e5c63c64c2555e153b644ea3016",
    "f26347bf8ea58054ffff7f2000000000002000307c406297d0e67f40a903957f426fc88b37e5ec10299066a4",
    "4d4efeb8ded4c71a171147fd32e08f09cc741312f26740c0d1b95280b3932c30237da63c6109140d8ea58054",
    "ffff7f200100000000200030292cf68441e64efb37a43352fd86251fd408d21d8e6fa3ef2b0b1c20ab44ba65",
    "0f4ca3456ad1ecf649f1546f94a630eb1d25fa3caadb7e55ea33bf02fb85a7608ea58054ffff7f2000000000",
    "00200030eb4513580344a90d9e8747514c879f3f7c40a93a64d579a9dd08be1b9448a86923648008ebc441d4",
    "c582c22e90edc43ae13c5cdb2088f698734987e359a01b868ea58054ffff7f200000000000200030977ca240",
    "e2265ee735c4f6a48eeb24957471e136693b233b80c9839ad8a93652f2f67f4179efa77fe93ebcd18e3bd818",
    "02f6b56d8821dbcec435213f94010d3e8ea58054ffff7f20030000000020003043c16eaa8df29f7e76be924e",
    "f76a010728267f8a36f497f7f3f96bdaa260b924300e0fd79eeef8578119dd996b118ab2cc99df1ddaac71bc",
    "20e64276687988ae8ea58054ffff7f200000000000200030f2d059e6f0e820374feb83559d1a106a9c58b5f9",
    "4f7331f18edb3bfdce249b0e35d0548020e51f4611c57e057c20f975d508f04c12ea419f3e3a32192898b63b",
    "8fa58054ffff7f20010000000020003008d339b78e36a0810f3ba2d263d1087ded00ba0eb792aeff089f328f",
    "7e5be56ba7e66e51a8ba705fd6631810d769b295a1959b5ceeeb96ceac553589a00b33518fa58054ffff7f20",
    "0000000000200030b002ec77bf26cb0e0e95456f91d78ee84235d78a0915511693b6cc0b730b1d320660ce25",
    "22d7c4eb1bf43f83fe041a3fcd89778dbba99b1bc6380b5153f89e208fa58054ffff7f200100000000200030",
    "47c18d4c8bd87818f1db0d342e8fae585e131cdb8a3e727d0032a4e38950f44366983bca50627f1f5992dd22",
    "260d1414dfe188aeeef3d6f5a8ecab05c014caa58fa58054ffff7f200100000000200030429f707161f8272b",
    "f59042f31c096488cc9590d9653e6863192d8551f4cf877401e237cf2fb219a5f77dcf3e223783830617e870",
    "fdf885af410073fae131cd2093a58054ffff7f200000000000200030a3034103ac505fd4ba26bcf5c00e4202",
    "97bf97ba74c568c48237cd4ac0250839a5fc937779fa18407fff2bba932c9356035cf89f1bafcdbd44242830",
    "aed3265f93a58054ffff7f200000000000200030e3d22b76a130475885810c2068a055ac7d071d8360a1a5f0",
    "295eec627720cb12960a6c5e294eb745d856b72f16588732c2a5bdcbca87407e2f071ca0d3f9202d93a58054",
    "ffff7f200600000000200030e90a92300b92769e7c0eccc41b1f3183496b036bc5bcfc17fa1b0b5004a00a4d",
    "bb73d1bf482013253f7da65e3a7ab727376411e3095674cef586b2a10fab2bc493a58054ffff7f2000000000",
    "0020003020ea73522a531cc3f9a636aaefcd5cc4bd358fd8b2e761eafb4639907cfe890391e6c57b91b4a1b4",
    "2bfb37255b75091caf57b15a3889ba23547ad8f7097dbcda93a58054ffff7f2002000000002000308f565a37",
    "197f0f6df68c5e66f754fd414db3aea39dbf9c5546edef243f0feb58004c625059e90a199ba8ca2e6ce0fd29",
    "2f51573adf2a12c4e8c8579bfdef8d5194a58054ffff7f2001000000002000309d3c1aa790ade6eaba13882a",
    "adc86bc028301407108d4b6ad67dcbf35a7aa00f102c0612dc6a80443a067f979b7fc7387beb995bca1679ba",
    "eb2357d12194e230f6a78054ffff7f200100000000200030947a049e759acb5e4c9cb55443be870e7af2d5d9",
    "28e8526158696580b46330223bb4f7438a1db57969aa5eedb2131406a1b2ca6faaf4ae43724f33906800be72",
    "4faa8054ffff7f2000000000002000303e9d55e4bdc666c48d0958f7fe5d635b9b15d59cd98eb39b23e992eb",
    "bed5a2362f84704dd3596c61eaea64935d93c7c2779ced3888e341116c0e9e076ed400c6c8ac8054ffff7f20",
    "0000000000200030199d0b8e1b33b4df6f4954f1290219891c225c72a157c1ace223ae7c3fb8ef386cec670a",
    "865bb2a7b0e528243240493b576fdddbe9d3028252259314cb09cb7621af8054ffff7f2000000000",
);

/// The payload of a `NULL_TXS` transaction: after its 10 bytes and the payload's CompactSize (0xfd
/// and its length), {version 1, nHeight, the commitment}.
fn null_payload(tx: &[u8]) -> &[u8] {
    &tx[10 + 3..]
}

/// `(mined height, the type-6 transaction as Core mined it)`.
const NULL_TXS: [(u32, &str); 4] = [
    (
        178,
        concat!(
            "03000600000000000000fd3d010100b200000001006432e5113f9b1490a45d3a2f7dee117488753cac470f53",
            "e9871a46180f22e6dd0803000300000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "00000000000000000000000000000000000000000000",
        ),
    ),
    (
        180,
        concat!(
            "03000600000000000000fd3f010100b4000000020067abc8d305f59e950c5e12a30d609ddd8b951bb9e83209",
            "f135f7e627cce52a7e4201000400040000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "000000000000000000000000000000000000000000000000",
        ),
    ),
    (
        202,
        concat!(
            "03000600000000000000fd3d010100ca00000003006447c18d4c8bd87818f1db0d342e8fae585e131cdb8a3e",
            "727d0032a4e38950f44303000300000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "00000000000000000000000000000000000000000000",
        ),
    ),
    (
        204,
        concat!(
            "03000600000000000000fd3f010100cc000000040067429f707161f8272bf59042f31c096488cc9590d9653e",
            "6863192d8551f4cf877401000400040000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "000000000000000000000000000000000000000000000000",
        ),
    ),
];
