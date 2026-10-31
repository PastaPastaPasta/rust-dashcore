//! Step 5 over LQ1's rotated captures (`test-vectors/lq1-rot`, provenance in its
//! `PROVENANCE.md`): one LLMQ_60_75 cycle per era on mainnet and testnet, with the work-block lists
//! projected to the SML (`CDeterministicMN::to_sml_entry`), the coinbases and the previous cycles'
//! rows, and Core's row for the cycle. Over the engine's views every index gets Core's members,
//! every mined commitment verifies, and a list with a signer's and a non-signer's operator keys
//! swapped does not. LQ1's row for the cycle is Core's, and replaces a wrong candidate (D20). The
//! two activation cycles read the rows of cycles before rotation, which Core never stores: its
//! absence, so they compute too.

use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;

use dash_chain::{AncestryError, AnchoredAncestry, BlockRef};
use dash_consensus::BlockId;
use dash_consensus::ctx::HeaderCtx;
use dash_consensus::primitives::{decode_header, decode_tx_exact};
use dash_dml::Dmn;
use dash_dml::codec::core_disk::decode_list_s3;
use dash_evo::cbtx::CoinbaseFacts;
use dash_evo::llmq::{MnListView, QuorumSnapshot};
use dash_evo::sml::{SmlEntry, SmlList};
use dash_num::{Arith256, CompactTarget};
use dash_p2p_core::QuorumSnapshot as WireSnapshot;
use dash_params::{ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::{BlockHash, Commitment, MerkleRoot};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use serde_json::Value;

use super::{hex, u32_of};
use crate::convert::snapshot;
use crate::view::HeldRow;
use crate::{LlmqConfig, MasternodeListEngine, MnChain, Mode, QuorumState, RequestKey};

/// A capture's headers, height as id, from its first height.
struct CaptureChain {
    first: u32,
    headers: Vec<HeaderCtx>,
    ids: HashMap<BlockHash, u32>,
}

impl CaptureChain {
    fn new(first: u32, raw: &[u8]) -> Self {
        let headers: Vec<HeaderCtx> = raw
            .chunks(80)
            .zip(first..)
            .map(|(h, height)| {
                let header = decode_header(&mut &h[..]).expect("a header").into_value();
                HeaderCtx {
                    height,
                    hash: header.hash(),
                    time: header.raw().time,
                    bits: CompactTarget::new(header.raw().bits),
                    version: header.raw().version,
                    mtp: 0,
                    chain_work: Arith256::ZERO,
                }
            })
            .collect();
        let ids = headers.iter().map(|h| (h.hash, h.height)).collect();
        CaptureChain {
            first,
            headers,
            ids,
        }
    }

    fn tip(&self) -> u32 {
        self.first + u32::try_from(self.headers.len()).expect("small") - 1
    }

    fn hash(&self, height: u32) -> BlockHash {
        self.headers[(height - self.first) as usize].hash
    }

    fn held(&self, height: u32) -> Result<usize, AncestryError> {
        if height < self.first {
            return Err(AncestryError::BelowFloor {
                height,
            });
        }
        assert!(height <= self.tip());
        Ok((height - self.first) as usize)
    }
}

impl AnchoredAncestry for CaptureChain {
    fn floor(&self) -> u32 {
        self.first
    }

    fn header(&self, id: BlockId) -> Result<HeaderCtx, AncestryError> {
        Ok(self.headers[self.held(id.0)?])
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, AncestryError> {
        if height > id.0 {
            return Ok(None);
        }
        self.held(height).map(|_| Some(BlockId(height)))
    }

    fn prev(&self, id: BlockId) -> Result<Option<BlockId>, AncestryError> {
        match id.0.checked_sub(1) {
            None => Ok(None),
            Some(h) => self.held(h).map(|_| Some(BlockId(h))),
        }
    }
}

impl MnChain for CaptureChain {
    fn ancestry(&self) -> &dyn AnchoredAncestry {
        self
    }

    fn lookup(&self, hash: &BlockHash) -> Result<Option<BlockId>, AncestryError> {
        Ok(self.ids.get(hash).map(|h| BlockId(*h)))
    }

    fn active_tip(&self) -> Result<BlockRef, AncestryError> {
        let tip = self.tip();
        Ok(BlockRef {
            id: BlockId(tip),
            hash: self.hash(tip),
            height: tip,
        })
    }

    fn merkle_root(&self, _: BlockId) -> Result<Option<MerkleRoot>, AncestryError> {
        unreachable!("step 5 reads no merkle root")
    }
}

/// `CDeterministicMN::to_sml_entry()` (`evo/deterministicmns.cpp` v24:43-50): the state's version,
/// and its operator key written under that version's scheme.
fn sml_entry(dmn: &Dmn) -> SmlEntry {
    let cold = &dmn.cold;
    SmlEntry {
        version: u16::try_from(cold.version).expect("a u16 version"),
        pro_reg_tx_hash: dmn.pro_tx_hash,
        confirmed_hash: BlockHash::from(*cold.confirmed_hash.as_bytes()),
        net_info: (*cold.net_info).clone(),
        operator_key: cold.pub_key_operator.clone().to_bytes_as(cold.key_scheme()),
        voting_key_id: *cold.key_id_voting.as_bytes(),
        is_valid: dmn.is_valid(),
        mn_type: dmn.mn_type,
        platform_http_port: cold.platform_http_port,
        platform_node_id: cold.platform_node_id,
    }
}

fn row(hex_row: &Value) -> QuorumSnapshot {
    let bytes = hex(hex_row.as_str().expect("a row"));
    let mut r = &bytes[..];
    let wire = WireSnapshot::decode(&mut r).expect("Core's row");
    assert!(r.is_empty());
    snapshot(&wire)
}

fn read_gz(path: &std::path::Path) -> Value {
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(std::fs::File::open(path).expect("a capture"))
        .read_to_end(&mut bytes)
        .expect("gzip");
    serde_json::from_slice(&bytes).expect("json")
}

/// Two operator keys swapped in every list (a rotated member may come from an earlier quarter's).
type Swap = ([u8; 48], [u8; 48]);

/// A capture's lists, coinbases and rows held by an engine, as the trust chain would hold them.
fn held(
    v: &Value,
    chain: &CaptureChain,
    cfg: &LlmqConfig,
    swap: Option<Swap>,
) -> MasternodeListEngine {
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let dmns: Vec<Vec<u8>> = v["dmns"]
        .as_array()
        .expect("records")
        .iter()
        .map(|d| hex(d.as_str().expect("hex")))
        .collect();
    let coinbases: HashMap<u32, CoinbaseFacts> = v["coinbases"]
        .as_array()
        .expect("coinbases")
        .iter()
        .map(|cb| {
            let tx = decode_tx_exact(&hex(cb["tx"].as_str().expect("a coinbase")))
                .expect("a coinbase")
                .into_value();
            (u32_of(&cb["height"]), CoinbaseFacts::First(Arc::new(tx)))
        })
        .collect();
    for list in v["lists"].as_array().expect("lists") {
        let height = u32_of(&list["height"]);
        let entries = list["entries"].as_array().expect("entries");
        let mut bytes = hex(list["head"].as_str().expect("a head"));
        dash_consensus::primitives::write_compact_size(entries.len() as u64, &mut bytes);
        for e in entries {
            bytes.extend(&dmns[usize::try_from(e.as_u64().expect("an index")).expect("small")]);
        }
        let list = decode_list_s3(&mut &bytes[..]).expect("an evodb list");
        let mut sml = Vec::new();
        MnListView::for_each(&list, false, &mut |d: &Arc<Dmn>| sml.push(sml_entry(d)));
        if let Some((a, b)) = swap {
            for e in &mut sml {
                if e.operator_key == a {
                    e.operator_key = b;
                } else if e.operator_key == b {
                    e.operator_key = a;
                }
            }
        }
        let coinbase =
            coinbases.get(&height).cloned().unwrap_or(CoinbaseFacts::BlockDataUnavailable);
        engine.hold(
            BlockId(height),
            height,
            chain.hash(height),
            SmlList::from_entries(sml),
            coinbase,
        );
    }
    let rotated = cfg.params().consensus.llmq_type_dip0024_instant_send;
    for r in v["rows"].as_array().expect("rows") {
        engine.hold_row(rotated, chain.hash(u32_of(&r["cycle"])), row(&r["snapshot"]));
    }
    engine
}

#[test]
fn rotated_captures_verify_over_the_sml_views() {
    let dir = format!("{}/../test-vectors/lq1-rot", env!("CARGO_MANIFEST_DIR"));
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("the captures")
        .map(|e| e.expect("an entry").path())
        .filter(|p| p.to_string_lossy().ends_with(".json.gz"))
        .collect();
    paths.sort();
    let (mut eras, mut verified, mut swaps, mut activation, mut no_row) = (0, 0, 0, 0, 0);
    for path in &paths {
        let v = read_gz(path);
        let net = v["network"].as_str().expect("a network");
        let network = match net {
            "mainnet" => NetworkKind::Mainnet,
            "testnet" => NetworkKind::Testnet,
            other => panic!("{other}"),
        };
        let params = ChainParamsBuilder::new(network, Release::V24_0).build().expect("params");
        let cfg = LlmqConfig::new(Arc::new(params)).expect("in the domain");
        let cycle = u32_of(&v["cycle"]);
        let chain = CaptureChain::new(
            u32_of(&v["first_height"]),
            &hex(v["headers"].as_str().expect("headers")),
        );
        let mut engine = held(&v, &chain, &cfg, None);
        let mut swapped_once = false;
        let rotated = cfg.params().consensus.llmq_type_dip0024_instant_send;
        for q in v["quorums"].as_array().expect("quorums") {
            let index = u32_of(&q["index"]);
            let base = BlockId(cycle + index);
            let members = engine
                .members(&chain, &cfg, rotated, base, false)
                .unwrap_or_else(|e| panic!("{net} {cycle} index {index}: {e:?}"));
            if v["rows"].as_array().expect("rows").is_empty() {
                activation += 1;
            }
            let ours: Vec<String> = members
                .iter()
                .map(|m| {
                    let mut h = *m.pro_reg_tx_hash.as_bytes();
                    h.reverse();
                    h.iter().map(|b| format!("{b:02x}")).collect()
                })
                .collect();
            let core: Vec<&str> = q["members"]
                .as_array()
                .expect("members")
                .iter()
                .map(|m| m.as_str().expect("hex"))
                .collect();
            assert_eq!(ours, core, "{net} {cycle} index {index}");
            let Some(qc) = q.get("commitment") else {
                continue;
            };
            assert_eq!(q["verifies"], true, "Core accepts every captured commitment");
            let qc = Commitment::decode(&mut &hex(qc.as_str().expect("hex"))[..]).expect("Core's");
            let mined = u32_of(&q["mined_height"]);
            let state = engine.step5(qc.clone(), mined, &chain, &cfg);
            assert!(matches!(state, QuorumState::Verified(_)), "{net} {cycle} {index}: {state:?}");
            verified += 1;
            // Discrimination, once per capture: a signer's and a non-signer's operator keys
            // swapped fail the members signature.
            if !swapped_once {
                let signed = |i: usize| qc.signers.get(i as u64) == Some(true);
                let s = (0..members.len()).find(|i| signed(*i));
                let n = (0..members.len()).find(|i| !signed(*i));
                if let (Some(s), Some(n)) = (s, n) {
                    let swap = (members[s].operator_key, members[n].operator_key);
                    let swapped = held(&v, &chain, &cfg, Some(swap));
                    assert_eq!(
                        swapped.step5(qc, mined, &chain, &cfg),
                        QuorumState::Unverified,
                        "{net} {cycle} {index}: a swapped signer verifies"
                    );
                    swapped_once = true;
                    swaps += 1;
                }
            }
        }
        // A wrong candidate for the cycle's own row, then the row LQ1 computes: Core's. Where Core
        // stores none (its early return, 28e), LQ1 confirms none and the candidate stays.
        let core = (!v["row"].is_null()).then(|| row(&v["row"]));
        let mut wrong = core.clone().unwrap_or_default();
        wrong.active_quorum_members.push(true);
        engine.hold_candidate(rotated, chain.hash(cycle), wrong.clone());
        engine.confirm_held_rows(&chain, &cfg).expect("no stop");
        let expected = match core {
            Some(core) => HeldRow::Core(core),
            None => HeldRow::Candidates(vec![wrong]),
        };
        assert_eq!(engine.held_row(rotated, chain.hash(cycle)), Some(&expected), "{net} {cycle}");
        no_row += usize::from(v["row"].is_null());
        eras += 1;
    }
    assert_eq!(eras, 12);
    assert_eq!(verified, 228, "every mined commitment of the captures");
    // Two captures mine only commitments every member signed: nothing to swap.
    assert_eq!(swaps, 9 - 2, "swapped-signer checks");
    assert_eq!(activation, 2 * 32, "the two DIP0024 activation cycles");
    assert_eq!(no_row, 1, "Core's early return");
}

/// D20 (validator r1 B, Sol r2 minor 1): `BlsUnsupported` under a candidate row is no verdict. In
/// the pre-V19 era `mainnet-1738656` (the legacy scheme), cycle H − C holds one wrong candidate
/// row, and a masternode that signs only among its members gets an operator key outside the
/// prime-order subgroup, a legacy-decoded `an exotic key` operand of BS12's Core-backed corpus
/// (base-sdk `pkgs/pkc/corpus/bls_oracle/edge.json5`; Core: false, the legacy flag's verdict
/// dash-pkc cannot give). Step 5 asks for the cycle's `qrinfo` again rather than ending
/// `Unverified`; a later honest candidate, Core's row, verifies.
#[test]
fn an_unsupported_key_under_a_wrong_row_is_no_verdict() {
    const EXOTIC: &str = "99d56efc828f1263dc540de1fb59a7d550e13c7e9aa82d81cd6a2420412255f7ceed3ab7dbc5fa186c9470566de12a54";
    let path =
        format!("{}/../test-vectors/lq1-rot/mainnet-1738656.json.gz", env!("CARGO_MANIFEST_DIR"));
    let v = read_gz(path.as_ref());
    let params =
        ChainParamsBuilder::new(NetworkKind::Mainnet, Release::V24_0).build().expect("params");
    let cfg = LlmqConfig::new(Arc::new(params)).expect("in the domain");
    let v19 = cfg.params().consensus.v19_height.to_u32();
    let rotated = cfg.params().consensus.llmq_type_dip0024_instant_send;
    let cycle = u32_of(&v["cycle"]);
    let chain = CaptureChain::new(
        u32_of(&v["first_height"]),
        &hex(v["headers"].as_str().expect("headers")),
    );
    let h_c = v["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .max_by_key(|r| u32_of(&r["cycle"]))
        .expect("a row");
    let (h_c_cycle, core_row) = (chain.hash(u32_of(&h_c["cycle"])), row(&h_c["snapshot"]));
    let mut wrong = core_row.clone();
    for bit in &mut wrong.active_quorum_members {
        *bit = !*bit;
    }

    let honest = held(&v, &chain, &cfg, None);
    let mut engine = held(&v, &chain, &cfg, None);
    engine.hold_candidates(rotated, h_c_cycle, vec![wrong]);
    // The first commitment with a signer only the wrong row makes a member.
    let (qc, mined, pro) = v["quorums"]
        .as_array()
        .expect("quorums")
        .iter()
        .find_map(|q| {
            let qc =
                Commitment::decode(&mut &hex(q.get("commitment")?.as_str()?)[..]).expect("Core's");
            let base = BlockId(cycle + u32_of(&q["index"]));
            let cores = honest.members(&chain, &cfg, rotated, base, false).expect("Core's members");
            let under_wrong = engine.members(&chain, &cfg, rotated, base, true).ok()?;
            let pro = under_wrong.iter().enumerate().find_map(|(i, m)| {
                (qc.signers.get(i as u64) == Some(true)
                    && !cores.iter().any(|c| c.pro_reg_tx_hash == m.pro_reg_tx_hash))
                .then_some(m.pro_reg_tx_hash)
            })?;
            Some((qc, u32_of(&q["mined_height"]), pro))
        })
        .expect("a signer only the wrong row makes a member");
    assert!(mined < v19, "the legacy scheme");
    engine.set_operator_key(&pro, hex(EXOTIC).try_into().expect("48 bytes"));
    let state = engine.step5(qc.clone(), mined, &chain, &cfg);
    assert!(matches!(state, QuorumState::Pending(RequestKey::QrInfo(_))), "{state:?}");

    engine.hold_candidate(rotated, h_c_cycle, core_row);
    let state = engine.step5(qc, mined, &chain, &cfg);
    assert!(matches!(state, QuorumState::Verified(_)), "{state:?}");
}
