//! CH5-5 §6.4's workloads, built test-side: regtest headers (X11 PoW) and the coins fan-out
//! transactions.

use dash_consensus::header::HeaderParams;
use dash_consensus::pow::check_proof_of_work;
use dash_consensus::primitives::Header;
use dash_num::CompactTarget;
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::BlockHeader;
use dash_types::codec::BaseCodec as _;
use serde_json::json;

use crate::common::mine::hex;
use crate::fixture::{Submission, internal, text};
use crate::live::Source;

/// Regtest's `powLimit` bits; regtest never retargets.
const REGTEST_BITS: u32 = 0x207f_ffff;

/// Regtest's PoW parameters, built once.
fn pow() -> &'static dash_consensus::pow::PowParams {
    static POW: std::sync::OnceLock<dash_consensus::pow::PowParams> = std::sync::OnceLock::new();
    POW.get_or_init(|| {
        let params =
            ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("regtest");
        *HeaderParams::from_chain_params(&params).expect("regtest").pow()
    })
}

/// A chain position headers are built on: the tip hash and the times of its last 11 blocks
/// (fewer near genesis), for the median-time-past rule.
#[derive(Clone)]
pub struct Parent {
    pub hash: String,
    pub times: Vec<u32>,
}

impl Parent {
    /// `hash` with its last 11 times read from the source.
    pub fn of(s: &Source, hash: &str) -> Parent {
        let mut times = Vec::new();
        let mut at = Some(hash.to_owned());
        while let Some(h) = at.filter(|_| times.len() < 11) {
            let header = s.call("getblockheader", json!([h]));
            times.push(header["time"].as_u64().expect("time") as u32);
            at = header["previousblockhash"].as_str().map(str::to_owned);
        }
        times.reverse();
        Parent {
            hash: hash.to_owned(),
            times,
        }
    }

    /// The median time past (`GetMedianTimePast`) of this parent.
    fn mtp(&self) -> u32 {
        let mut t = self.times.clone();
        t.sort_unstable();
        t[t.len() / 2]
    }

    /// The next header on this parent at the least time the MTP rule allows, its `merkle_root`
    /// made distinct by `salt` (headers-only: no body is ever sent); the submission, and the
    /// parent it makes.
    pub fn next(&self, salt: u64) -> (Submission, Parent) {
        let time = self.mtp() + 1;
        let mut raw = Vec::with_capacity(80);
        raw.extend(0x2000_0000_u32.to_le_bytes());
        raw.extend(internal(&self.hash));
        let mut merkle = [0u8; 32];
        merkle[..8].copy_from_slice(&salt.to_le_bytes());
        merkle[8..12].copy_from_slice(b"ch55");
        raw.extend(merkle);
        raw.extend(time.to_le_bytes());
        raw.extend(REGTEST_BITS.to_le_bytes());
        raw.extend(0u32.to_le_bytes());
        let pow = pow();
        let header = (0u32..)
            .map(|nonce| {
                raw[76..80].copy_from_slice(&nonce.to_le_bytes());
                Header::new(BlockHeader::decode(&mut raw.as_slice()).expect("a header"))
            })
            .find(|h| {
                check_proof_of_work(h.hash().as_bytes(), CompactTarget::new(REGTEST_BITS), pow)
            })
            .expect("a nonce");
        let hash = header.hash().to_string();
        let mut times = self.times.clone();
        times.push(time);
        if times.len() > 11 {
            times.remove(0);
        }
        let submission = Submission {
            method: "submitheader",
            hex: hex(&raw),
            hash: hash.clone(),
            result: "null".into(),
        };
        (
            submission,
            Parent {
                hash,
                times,
            },
        )
    }
}

/// Submits `sub` to the source, asserting the answer, and logs it.
pub fn submit(s: &mut Source, sub: Submission) {
    assert_eq!(sub.to(&s.node), sub.result, "{} of {}", sub.method, sub.hash);
    s.log.push(sub);
}

/// §6.4's headers workload: `branches` × `each` headers-only headers on the tip, as branches
/// from it. Each header takes the least time the MTP rule allows, which grows 1 s per 6
/// headers; under the fixed mocktime a single chain of 50 000 would pass `MAX_FUTURE_BLOCK_TIME`
/// (2 h), so the 50 000 are 10 branches of 5 000 (a mechanics note).
pub fn headers(s: &mut Source, branches: u64, each: u64) {
    let tip = Parent::of(s, &s.tip());
    for b in 0..branches {
        let mut at = tip.clone();
        for i in 0..each {
            let (sub, next) = at.next(b << 32 | i);
            let hash = sub.hash.clone();
            submit(s, sub);
            if i == 0 {
                let known = s.call("getblockheader", json!([hash]));
                assert_eq!(known["previousblockhash"], tip.hash.as_str(), "the header's hash");
            }
            at = next;
        }
    }
}

/// The follow-up write: one header on the tip.
pub fn follow_up(s: &mut Source) {
    let (sub, _) = Parent::of(s, &s.tip()).next(u64::MAX);
    submit(s, sub);
}

/// Outputs per fan-out transaction, and each one's value in duffs.
pub const OUTPUTS: usize = 9_500;
const OUTPUT_VALUE: u64 = 1_000;

/// The `raw(51)` coinbase output of `hash`'s block: its txid (display), index and value (duffs).
pub fn coinbase(s: &Source, hash: &str) -> (String, u32, u64) {
    let block = s.call("getblock", json!([hash, 2]));
    let tx = &block["tx"][0];
    let (n, out) = tx["vout"]
        .as_array()
        .expect("vout")
        .iter()
        .enumerate()
        .find(|(_, o)| o["scriptPubKey"]["hex"] == "51")
        .expect("a raw(51) output");
    let value = (out["value"].as_f64().expect("value") * 1e8).round() as u64;
    (text(&tx["txid"]), n as u32, value)
}

/// A fan-out transaction (§6.4): version 1, type 0; one input spending `(txid, n)` with an
/// empty `scriptSig`; 9 500 outputs of 1 000 duffs to `raw(51)`; lock time 0. 95 053 bytes.
pub fn fan_out(txid: &str, n: u32) -> String {
    let mut tx = Vec::with_capacity(95_053);
    tx.extend(1u16.to_le_bytes());
    tx.extend(0u16.to_le_bytes());
    tx.push(1);
    tx.extend(internal(txid));
    tx.extend(n.to_le_bytes());
    tx.push(0);
    tx.extend(u32::MAX.to_le_bytes());
    tx.push(0xfd);
    tx.extend((OUTPUTS as u16).to_le_bytes());
    for _ in 0..OUTPUTS {
        tx.extend(OUTPUT_VALUE.to_le_bytes());
        tx.extend([1, 0x51]);
    }
    tx.extend(0u32.to_le_bytes());
    assert_eq!(tx.len(), 95_053);
    hex(&tx)
}

/// §6.4's coins workload, after 160 `raw(51)` blocks: three blocks of 20 fan-out transactions,
/// transaction j of block k spending the coinbase of height 20k + j + 1, by `generateblock`.
pub fn coins(s: &mut Source) {
    fan_outs(s, 1..=60);
}

/// Fan-out transactions spending the coinbases at `heights`, in order, 20 a block (the last
/// block takes the rest), each block by `generateblock` and logged.
pub fn fan_outs(s: &mut Source, heights: std::ops::RangeInclusive<u64>) {
    let heights: Vec<u64> = heights.collect();
    for chunk in heights.chunks(20) {
        let txs: Vec<String> = chunk
            .iter()
            .map(|&height| {
                let hash = text(&s.call("getblockhash", json!([height])));
                let (txid, n, value) = coinbase(s, &hash);
                assert!(value >= OUTPUTS as u64 * OUTPUT_VALUE, "coinbase {height}: {value}");
                fan_out(&txid, n)
            })
            .collect();
        let made = s.call("generateblock", json!(["raw(51)", txs]));
        let hash = text(&made["hash"]);
        let size = s.call("getblock", json!([hash, 1]))["size"].as_u64().expect("size");
        assert!(size < 2_000_000, "the block of {chunk:?}: {size} bytes");
        let sub = Submission::block(&s.node, &hash);
        s.log.push(sub);
    }
}
