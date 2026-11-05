//! The independent checks of a digest `C` record before it becomes golden (spec §13.5).
//!
//! Two extractions agreeing shows only that they agree: a broken Core hook writes the same wrong
//! value every time. So every key the patch writes into `s` is compared with what the frozen copy
//! says through another path, and the eras come from the node's consensus parameters
//! (`getdeploymentinfo`'s buried heights), never from the digest under check.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail, ensure};
use dash_statedigest::{Hash256, Outcome, Value, decode_hex};
use dashcore::hashes::{Hash, sha256d};
use serde_json::json;

use super::{Client, amount, get, int, num, text};
use crate::digest::amount::int_from_number;

/// The buried deployments in `Consensus::BuriedDeployment` order: bit `i` of `bur` (spec §5.2).
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

/// `TRANSACTION_QUORUM_COMMITMENT`, `TRANSACTION_MNHF_SIGNAL`, `TRANSACTION_ASSET_UNLOCK`.
const TX_QUORUM_COMMITMENT: i64 = 6;
const TX_MNHF_SIGNAL: i64 = 7;
const TX_ASSET_UNLOCK: i64 = 9;

/// The first heights of the DIP0003 and V20 eras: `DeploymentActiveAt(pindex, …)` is
/// `pindex->nHeight >= height` for a buried deployment.
#[derive(Clone, Copy, Debug)]
pub(super) struct Eras {
    pub dip3: i64,
    pub v20: i64,
}

impl Eras {
    pub(super) fn from_node(cl: &Client<'_>) -> Result<Eras> {
        let info = cl.call("getdeploymentinfo", json!([]))?;
        let deployments = get(&info, "deployments")?;
        let height = |name: &str| {
            int(get(deployments, name)?, "height")
                .with_context(|| format!("getdeploymentinfo: the buried height of {name}"))
        };
        Ok(Eras {
            dip3: height("dip0003")?,
            v20: height("v20")?,
        })
    }
}

/// What the previous block's checks leave for the next block's.
struct Prev {
    h: i64,
    /// `null` when the block's BIP9 values are not checked.
    dep: Value,
    ehf: Value,
}

/// A block's special transactions, from `getblock <hash> 2`.
#[derive(Default)]
struct Specials {
    /// Non-null commitments: `[type, quorumHash, sha256d(commitment bytes)]`.
    qc: Vec<(i64, Hash256, Hash256)>,
    /// New MNHF signal bits.
    signals: Vec<i64>,
    /// Asset-unlock indexes in transaction order, as the node printed them.
    unlocks: Vec<String>,
}

/// The `k` added to the stored list's registration counter that `mnl` may differ by (§13.5).
pub(super) const COUNTER_ADDS: std::ops::RangeInclusive<u32> = 1..=16;

/// How `s.mnl` agreed with `getcanonicalmnlist`.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum MnlAgreement {
    /// The hashes are equal.
    Equal,
    /// Only the registration counter differs, by exactly one `k` of [`COUNTER_ADDS`].
    CounterAdjusted,
}

/// The offset of the registration counter in a canonical list, found by reading the head of
/// `SerializeCanonicalMNList`: block hash (32 bytes), height (`i32`), counter (`u32`), entry count
/// (compact size), then the entries.
fn counter_offset(list: &[u8]) -> Result<usize> {
    const COUNTER: usize = 32 + 4;
    let count = list.get(COUNTER + 4).ok_or_else(|| anyhow!("the canonical list is cut short"))?;
    let count_len = match count {
        0..=0xfc => 1,
        0xfd => 3,
        0xfe => 5,
        0xff => 9,
    };
    ensure!(list.len() >= COUNTER + 4 + count_len, "the canonical list is cut short");
    Ok(COUNTER)
}

/// `s.mnl` against the reply of `getcanonicalmnlist`: equal hashes, or else the hash of the reply's
/// `hex` with only its registration counter changed, by exactly one `k` of [`COUNTER_ADDS`].
pub(super) fn mnl_agreement(mnl: &Value, reply: &Value) -> Result<MnlAgreement> {
    let want = get(reply, "hash")?.as_hash().map(|h| Value::hash(&h));
    if want.as_ref() == Some(mnl) {
        return Ok(MnlAgreement::Equal);
    }
    let differs = || anyhow!("s.mnl is {mnl:?}, but getcanonicalmnlist says {want:?}");
    let mut bytes = decode_hex(text(reply, "hex")?.as_bytes())
        .ok_or_else(|| anyhow!("getcanonicalmnlist: hex is not hex"))?;
    let at = counter_offset(&bytes).map_err(|_| differs())?;
    let counter = u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"));
    let mut matching = 0;
    for k in COUNTER_ADDS {
        let Some(adjusted) = counter.checked_add(k) else {
            break;
        };
        bytes[at..at + 4].copy_from_slice(&adjusted.to_le_bytes());
        let hash = Hash256::from_wire_bytes(sha256d::Hash::hash(&bytes).to_byte_array());
        matching += usize::from(Some(&Value::hash(&hash)) == Some(mnl));
    }
    ensure!(matching == 1, differs());
    Ok(MnlAgreement::CounterAdjusted)
}

/// Runs the checks block by block, in height order.
pub(super) struct Checker<'a> {
    cl: &'a Client<'a>,
    pub eras: Eras,
    undo: Option<UndoIndex>,
    /// Every asset-unlock index mined so far: the credit pool's `indexes`.
    unlock_indexes: BTreeSet<u64>,
    /// Every MNHF signal mined so far, as `(bit, height)`: the only rows `ehf` can hold.
    mined_signals: BTreeSet<(i64, i64)>,
    prev: Option<Prev>,
    /// How many `mnl` values agreed only after the registration counter adjustment (§13.5).
    pub mnl_counter_adjusted: u64,
}

impl<'a> Checker<'a> {
    pub(super) fn new(cl: &'a Client<'a>, blocks_dir: Option<&Path>) -> Result<Checker<'a>> {
        Ok(Checker {
            cl,
            eras: Eras::from_node(cl)?,
            undo: blocks_dir.map(UndoIndex::scan).transpose()?,
            unlock_indexes: BTreeSet::new(),
            mined_signals: BTreeSet::new(),
            prev: None,
            mnl_counter_adjusted: 0,
        })
    }

    /// A block before the digest's suffix: only its asset unlocks and MNHF signals are kept (the
    /// credit pool's index set counts every unlock since genesis, and `ehf` rows can be older than
    /// the suffix).
    pub(super) fn skip(&mut self, h: i64, hash: &Hash256) -> Result<()> {
        if h >= self.eras.v20 {
            let ntx = int(&self.cl.call("getblockheader", json!([hash.to_hex()]))?, "nTx")?;
            if ntx > 1 {
                let specials = self.specials(hash)?;
                self.add_unlocks(&specials.unlocks)?;
                self.mined_signals.extend(specials.signals.iter().map(|bit| (*bit, h)));
            }
        }
        Ok(())
    }

    /// Checks the patch's keys of `c`, the digest's `C` record for block `h`, which the caller has
    /// matched to the active chain's block `hash`. `bip9` is false when the record came from a run
    /// with other BIP9 parameters than the copy's: then `dep` and the EHF heights the node reports
    /// are left unchecked. Returns the block's asset-unlock indexes.
    pub(super) fn check(
        &mut self,
        c: &Outcome,
        h: i64,
        hash: &Hash256,
        bip9: bool,
    ) -> Result<Vec<String>> {
        let s = c.s();
        let key = |k: &str| s.get(k).ok_or_else(|| anyhow!("the C record lacks s.{k}"));
        let dip3 = h >= self.eras.dip3;
        let v20 = h >= self.eras.v20;
        // Genesis is connected by ConnectBlock's early return: nothing from its body (spec §5).
        for (k, era) in [
            ("undo", h > 0),
            ("mnl", dip3 && h > 0),
            ("qc", dip3 && h > 0),
            ("cp", v20),
            ("ehf", v20),
        ] {
            match (era, key(k)?.is_null()) {
                (true, true) => bail!("s.{k} is null at height {h}, inside its era"),
                (false, false) => bail!("s.{k} is set at height {h}, outside its era"),
                _ => {}
            }
        }

        let ntx = self.check_header(c, h, hash)?;
        self.check_deployments(c, h, hash, bip9)?;

        let specials = if ntx > 1 {
            self.specials(hash)?
        } else {
            Specials::default()
        };
        if dip3 && h > 0 {
            // The list the node stored for the block, through `GetListForBlock`, not the tap.
            let list = self.cl.call("getcanonicalmnlist", json!([hash.to_hex()]))?;
            if mnl_agreement(key("mnl")?, &list)? == MnlAgreement::CounterAdjusted {
                self.mnl_counter_adjusted += 1;
            }
            let mut want = specials.qc.clone();
            want.sort_by_key(|a| (a.0, a.1.to_hex()));
            let rows = rows(key("qc")?)?;
            ensure!(rows == want, "s.qc {rows:?} is not the block's non-null commitments {want:?}");
        }
        self.add_unlocks(&specials.unlocks)?;
        self.mined_signals.extend(specials.signals.iter().map(|bit| (*bit, h)));
        if v20 {
            self.check_credit_pool(key("cp")?, h)?;
            self.check_signals(key("ehf")?, h, &specials.signals)?;
        }
        if h > 0
            && let Some(undo) = &self.undo
        {
            let want = key("undo")?.as_hash().ok_or_else(|| anyhow!("s.undo is not a hash"))?;
            let prev = key("prev")?.as_hash().ok_or_else(|| anyhow!("s.prev is not a hash"))?;
            undo.check(&want, &prev).with_context(|| format!("s.undo of block {h}"))?;
        }
        self.prev = Some(Prev {
            h,
            dep: if bip9 {
                key("dep")?.clone()
            } else {
                Value::Null
            },
            ehf: key("ehf")?.clone(),
        });
        Ok(specials.unlocks)
    }

    /// Only the deployment checks of [`Checker::check`], BIP9 included: for a copy started with the
    /// parameters of the run that recorded `c` (`super::check_bip9`).
    pub(super) fn check_bip9(&mut self, c: &Outcome, h: i64, hash: &Hash256) -> Result<()> {
        self.check_deployments(c, h, hash, true)?;
        let key =
            |k: &str| c.s().get(k).cloned().ok_or_else(|| anyhow!("the C record lacks s.{k}"));
        self.prev = Some(Prev {
            h,
            dep: key("dep")?,
            ehf: key("ehf")?,
        });
        Ok(())
    }

    /// `prev`, `ver`, `mrkl`, `time`, `bits` and `nonce` against the block's own header bytes,
    /// `ntx` against its transaction count, `work` against `getblockheader`'s `chainwork`.
    /// Returns the transaction count.
    fn check_header(&self, c: &Outcome, h: i64, hash: &Hash256) -> Result<i64> {
        let raw = self.cl.call("getblock", json!([hash.to_hex(), 0]))?;
        let raw = raw
            .as_str()
            .and_then(|t| hex_bytes(t.as_bytes()))
            .ok_or_else(|| anyhow!("getblock {hash} 0: not hex"))?;
        let header = RawHeader::parse(&raw).with_context(|| format!("getblock {hash} 0"))?;
        let prev = if h == 0 {
            ensure!(header.prev == [0; 32], "the genesis header has a previous block");
            Value::Null
        } else {
            Value::hash(&Hash256::from_wire_bytes(header.prev))
        };
        let work = get(&self.cl.call("getblockheader", json!([hash.to_hex()]))?, "chainwork")?
            .as_hash()
            .ok_or_else(|| anyhow!("getblockheader {hash}: chainwork is not a hash"))?;
        let want = [
            ("prev", prev),
            ("ver", Value::Int(header.version.into())),
            ("mrkl", Value::hash(&Hash256::from_wire_bytes(header.merkle))),
            ("time", Value::Int(header.time.into())),
            ("bits", Value::Int(header.bits.into())),
            ("nonce", Value::Int(header.nonce.into())),
            ("work", Value::hash(&work)),
            ("ntx", Value::Int(header.ntx)),
        ];
        for (k, v) in want {
            let got = c.s().get(k);
            ensure!(got == Some(&v), "s.{k} is {got:?}, but the block says {v:?}");
        }
        Ok(header.ntx)
    }

    /// `bur`, `bls` and `dep` against `getdeploymentinfo <hash>`, which answers for the block's
    /// child as the patch does (`DeploymentActiveAfter`, `State(pindex)`). A block's `since`
    /// values are checked against its own reply ([`since_next`]) and, the reply's `since` being
    /// `StateSinceHeight(pindex->pprev)`, against its child's. The BIP9 parts only when `bip9`.
    fn check_deployments(&mut self, c: &Outcome, h: i64, hash: &Hash256, bip9: bool) -> Result<()> {
        let info = self.cl.call("getdeploymentinfo", json!([hash.to_hex()]))?;
        let deployments = get(&info, "deployments")?;
        let s = c.s();
        let bur = s.get("bur").and_then(Value::as_int).ok_or_else(|| anyhow!("s.bur"))?;
        let mut want = 0i64;
        for (bit, name) in BURIED.iter().enumerate() {
            if get(deployments, name)?.get(b"active").and_then(Value::as_bool) == Some(true) {
                want |= 1 << bit;
            }
        }
        ensure!(bur == want, "s.bur is {bur}, but getdeploymentinfo gives {want}");
        let v19 = get(deployments, "v19")?.get(b"active").and_then(Value::as_bool);
        ensure!(s.get("bls") == Some(&Value::Bool(v19 == Some(false))), "s.bls is not !v19.active");
        // The parent's `since` values, if it was checked under the copy's parameters: this block
        // may come from another run (a restart with other -vbparams), the parent's did not.
        self.check_parent_since(deployments, h)?;
        if !bip9 {
            return Ok(());
        }

        let dep = s.get("dep").and_then(Value::as_arr).ok_or_else(|| anyhow!("s.dep"))?;
        // Core writes every deployment of `VersionBitsDeploymentInfo`, in that order (§5.2 pins
        // the names), while the RPC lists them in another and leaves out disabled ones
        // (`DeploymentEnabled`: start time NEVER_ACTIVE), whose state `GetStateFor` makes failed
        // since 0.
        let names = dep
            .iter()
            .map(|e| match e.as_arr() {
                Some([name, _, _]) => text_of(name),
                _ => bail!("malformed s.dep entry"),
            })
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            names == DEP_NAMES,
            "s.dep names {names:?}, but Core's BIP9 deployments are {DEP_NAMES:?}, in that order"
        );
        let unlisted: Vec<String> = bip9_deployments(deployments)?
            .map(|(name, _)| String::from_utf8_lossy(name).into_owned())
            .filter(|name| !DEP_NAMES.contains(&name.as_str()))
            .collect();
        ensure!(
            unlisted.is_empty(),
            "the node lists BIP9 deployments {unlisted:?}, which s.dep lacks"
        );
        for entry in dep {
            let Some([name, state, since]) = entry.as_arr() else {
                bail!("malformed s.dep entry")
            };
            let (name, state) = (text_of(name)?, text_of(state)?);
            if let Some(d) = bip9_deployment(deployments, name) {
                let b = get(d, "bip9")?;
                ensure!(
                    text(b, "status_next")? == state,
                    "s.dep: {name} is {state}, but getdeploymentinfo's status_next is {}",
                    text(b, "status_next")?
                );
                let want = since_next(d, h).with_context(|| format!("{name}'s since"))?;
                ensure!(
                    since.as_int() == Some(want),
                    "s.dep: {name} since {since:?}, but getdeploymentinfo gives {want}"
                );
            } else {
                ensure!(
                    state == "failed" && since.as_int() == Some(0),
                    "s.dep: {name} is {state} since {since:?}, but the node does not list it, so it is \
                     disabled (failed since 0)"
                );
            }
        }
        // Every EHF height the node reports for the child is a row of the block's ehf, with the
        // deployment's bit when the node reports one.
        let rows: Vec<(i64, i64)> = s
            .get("ehf")
            .and_then(Value::as_arr)
            .unwrap_or_default()
            .iter()
            .filter_map(|r| match r.as_arr() {
                Some([bit, at]) => Some((bit.as_int()?, at.as_int()?)),
                _ => None,
            })
            .collect();
        for (name, d) in bip9_deployments(deployments)? {
            let Some(b) = d.get(b"bip9") else {
                continue;
            };
            if let Some(e) = b.get(b"ehf_height") {
                let e = e.as_num().map(int_from_number).transpose()?;
                let bit = b.get(b"bit").and_then(Value::as_num).map(int_from_number).transpose()?;
                ensure!(
                    rows.iter().any(|(rb, at)| Some(*at) == e && bit.is_none_or(|bit| bit == *rb)),
                    "getdeploymentinfo reports {} (bit {bit:?}) ehf_height {e:?}, which s.ehf lacks",
                    String::from_utf8_lossy(name)
                );
            }
        }
        Ok(())
    }

    /// The `since` values of block `h - 1`, if it was checked under the copy's parameters (its
    /// `dep` was checked), against `getdeploymentinfo`'s reply for block `h`, whose `since` is
    /// `StateSinceHeight(pindex->pprev)`. A disabled deployment's `since` was checked as 0 with its
    /// block.
    fn check_parent_since(&self, deployments: &Value, h: i64) -> Result<()> {
        let Some(prev) = self.prev.as_ref().filter(|p| p.h == h - 1) else {
            return Ok(());
        };
        for entry in prev.dep.as_arr().unwrap_or_default() {
            if let Some([name, _, since]) = entry.as_arr() {
                let name = text_of(name)?;
                let Some(d) = bip9_deployment(deployments, name) else {
                    continue;
                };
                let want = int(get(d, "bip9")?, "since")?;
                ensure!(
                    since.as_int() == Some(want),
                    "s.dep of block {}: {name} since {since:?}, but the child's getdeploymentinfo says {want}",
                    h - 1
                );
            }
        }
        Ok(())
    }

    /// For a block `check_bip9` does not check: the `since` values of its parent, if that was
    /// checked (the last block of a run before a restart with other parameters).
    pub(super) fn check_child(&mut self, h: i64, hash: &Hash256) -> Result<()> {
        if self.prev.as_ref().is_some_and(|p| p.h == h - 1) {
            let info = self.cl.call("getdeploymentinfo", json!([hash.to_hex()]))?;
            self.check_parent_since(get(&info, "deployments")?, h)?;
        }
        self.prev = None;
        Ok(())
    }

    /// `cp` against `getcreditpoolinfo <h>` (balance, current limit, unlocked in the window) and
    /// against the hash of every unlock index mined so far, serialized as Core's `CRangesSet`.
    fn check_credit_pool(&self, cp: &Value, h: i64) -> Result<()> {
        let info = self.cl.call("getcreditpoolinfo", json!([h]))?;
        let want = [
            Value::Int(amount(&info, "balance")?),
            Value::Int(amount(&info, "currentlimit")?),
            Value::Int(amount(get(&info, "window")?, "unlocked")?),
            Value::hash(&ranges_hash(&self.unlock_indexes)),
        ];
        ensure!(
            cp.as_arr() == Some(&want[..]),
            "s.cp is {cp:?}, but the node's credit pool and unlock indexes give {want:?}"
        );
        Ok(())
    }

    /// `ehf` holds this block's new signals at this height, every row is a signal mined at its
    /// height, and nothing is neither one of the block's signals nor in the parent's `ehf` (an
    /// entry may only expire, spec §5.2).
    fn check_signals(&self, ehf: &Value, h: i64, signals: &[i64]) -> Result<()> {
        let rows = ehf.as_arr().ok_or_else(|| anyhow!("s.ehf is not an array"))?;
        for row in rows {
            let mined = match row.as_arr() {
                Some([bit, at]) => bit.as_int().zip(at.as_int()),
                _ => None,
            };
            ensure!(
                mined.is_some_and(|m| self.mined_signals.contains(&m)),
                "s.ehf entry {row:?} is no MNHF signal mined at its height"
            );
        }
        for bit in signals {
            let row = Value::Arr(vec![Value::Int(*bit), Value::Int(h)]);
            ensure!(rows.contains(&row), "s.ehf lacks the block's signal for bit {bit}");
        }
        if let Some(prev) = self.prev.as_ref().filter(|p| p.h == h - 1) {
            let before = prev.ehf.as_arr().unwrap_or_default();
            for row in rows {
                let new = matches!(row.as_arr(), Some([b, at]) if at.as_int() == Some(h)
                    && b.as_int().is_some_and(|b| signals.contains(&b)));
                ensure!(new || before.contains(row), "s.ehf entry {row:?} came from nowhere");
            }
        }
        Ok(())
    }

    fn add_unlocks(&mut self, unlocks: &[String]) -> Result<()> {
        for i in unlocks {
            let index = i.parse().with_context(|| format!("asset unlock index {i}"))?;
            ensure!(self.unlock_indexes.insert(index), "asset unlock index {i} mined twice");
        }
        Ok(())
    }

    fn specials(&self, hash: &Hash256) -> Result<Specials> {
        let block = self.cl.call("getblock", json!([hash.to_hex(), 2]))?;
        let txs =
            get(&block, "tx")?.as_arr().ok_or_else(|| anyhow!("getblock: tx is not an array"))?;
        let mut out = Specials::default();
        for tx in txs {
            match tx.get(b"type").and_then(Value::as_num).map(int_from_number).transpose()? {
                Some(TX_QUORUM_COMMITMENT) => {
                    let commitment = get(get(tx, "qcTx")?, "commitment")?;
                    if is_null_commitment(commitment)? {
                        continue;
                    }
                    let payload = hex_bytes(text(tx, "extraPayload")?.as_bytes())
                        .ok_or_else(|| anyhow!("extraPayload is not hex"))?;
                    // CFinalCommitmentTxPayload: nVersion (u16), nHeight (u32), commitment
                    ensure!(payload.len() > 6, "a commitment payload of {} bytes", payload.len());
                    let digest = sha256d::Hash::hash(&payload[6..]).to_byte_array();
                    let quorum = get(commitment, "quorumHash")?
                        .as_hash()
                        .ok_or_else(|| anyhow!("quorumHash is not a hash"))?;
                    out.qc.push((
                        int(commitment, "llmqType")?,
                        quorum,
                        Hash256::from_wire_bytes(digest),
                    ));
                }
                Some(TX_MNHF_SIGNAL) => {
                    out.signals.push(int(get(get(tx, "mnhfTx")?, "signal")?, "versionBit")?);
                }
                Some(TX_ASSET_UNLOCK) => {
                    let unlock =
                        get(tx, "assetUnlockTx").context("an asset unlock without its payload")?;
                    let index = num(unlock, "index")?;
                    ensure!(
                        !index.is_empty() && index.bytes().all(|c| c.is_ascii_digit()),
                        "asset unlock index is not a decimal integer: {index:?}"
                    );
                    out.unlocks.push(index.to_owned());
                }
                _ => {}
            }
        }
        Ok(out)
    }
}

fn text_of(v: &Value) -> Result<&str> {
    v.as_str().ok_or_else(|| anyhow!("not a string: {v:?}"))
}

/// `qc`'s rows as `(type, quorumHash, commitment hash)`.
fn rows(qc: &Value) -> Result<Vec<(i64, Hash256, Hash256)>> {
    qc.as_arr()
        .ok_or_else(|| anyhow!("s.qc is not an array"))?
        .iter()
        .map(|row| match row.as_arr() {
            Some([t, q, c]) => Some((t.as_int()?, q.as_hash()?, c.as_hash()?)),
            _ => None,
        })
        .map(|r| r.ok_or_else(|| anyhow!("malformed s.qc row")))
        .collect()
}

/// The BIP9 deployments of `VersionBitsDeploymentInfo` at v24, in `Consensus::DeploymentPos`
/// order: the names of `s.dep` (spec §5.2).
const DEP_NAMES: [&str; 3] = ["testdummy", "v24", "evo_shares"];

/// The entries of a `getdeploymentinfo` `deployments` object of type `bip9`.
fn bip9_deployments(deployments: &Value) -> Result<impl Iterator<Item = &(Vec<u8>, Value)>> {
    Ok(deployments
        .as_obj()
        .ok_or_else(|| anyhow!("deployments is not an object"))?
        .iter()
        .filter(|(_, d)| d.get(b"type").and_then(Value::as_str) == Some("bip9")))
}

/// `StateSinceHeight(pindex)` of a `bip9` entry of `getdeploymentinfo <block h>`: the height
/// from which the child's state (`status_next`) holds. The RPC gives it as `height` when that
/// state is active. Otherwise it follows from `since`, `StateSinceHeight(pindex->pprev)`: a
/// state changes only at a period's first block, so a state the child keeps holds since the
/// block's `since`, and a state the child enters holds since the child. Both walks of
/// `GetStateSinceHeightFor` then cover the same periods; a defined state is since 0 either way.
pub(super) fn since_next(d: &Value, h: i64) -> Result<i64> {
    let b = get(d, "bip9")?;
    let derived = if text(b, "status_next")? == text(b, "status")? {
        int(b, "since")?
    } else {
        h + 1
    };
    if let Some(height) = d.get(b"height") {
        let height = height.as_num().map(int_from_number).transpose()?;
        ensure!(
            height == Some(derived),
            "height {height:?}, but status, since and status_next give {derived}"
        );
    }
    Ok(derived)
}

fn bip9_deployment<'v>(deployments: &'v Value, name: &str) -> Option<&'v Value> {
    deployments
        .get(name.as_bytes())
        .filter(|d| d.get(b"type").and_then(Value::as_str) == Some("bip9"))
}

/// `CFinalCommitment::IsNull` from the RPC's JSON: no signers, no valid members, a zero
/// verification-vector hash, and the all-zero public key and signatures an unset BLS object
/// prints.
fn is_null_commitment(c: &Value) -> Result<bool> {
    let zero = |k: &str| -> Result<bool> { Ok(text(c, k)?.bytes().all(|b| b == b'0')) };
    Ok(int(c, "signersCount")? == 0
        && int(c, "validMembersCount")? == 0
        && zero("quorumVvecHash")?
        && zero("quorumPublicKey")?
        && zero("quorumSig")?
        && zero("membersSig")?)
}

/// `sha256d(serialize(CRangesSet))` of a set of indexes: a compact size, then each maximal run
/// of consecutive values as `[begin, end)`, two little-endian `u64`s (`util/ranges_set.h`).
pub(super) fn ranges_hash(indexes: &BTreeSet<u64>) -> Hash256 {
    let mut runs: Vec<(u64, u64)> = Vec::new();
    for &i in indexes {
        match runs.last_mut() {
            Some((_, end)) if *end == i => *end = i.wrapping_add(1),
            _ => runs.push((i, i.wrapping_add(1))),
        }
    }
    let mut bytes = compact_size(runs.len() as u64);
    for (begin, end) in runs {
        bytes.extend_from_slice(&begin.to_le_bytes());
        bytes.extend_from_slice(&end.to_le_bytes());
    }
    Hash256::from_wire_bytes(sha256d::Hash::hash(&bytes).to_byte_array())
}

fn compact_size(n: u64) -> Vec<u8> {
    match n {
        0..=0xfc => vec![n as u8],
        0xfd..=0xffff => [&[0xfd][..], &(n as u16).to_le_bytes()].concat(),
        0x1_0000..=0xffff_ffff => [&[0xfe][..], &(n as u32).to_le_bytes()].concat(),
        _ => [&[0xff][..], &n.to_le_bytes()].concat(),
    }
}

fn hex_bytes(text: &[u8]) -> Option<Vec<u8>> {
    hex_conservative::decode_to_vec(std::str::from_utf8(text).ok()?).ok()
}

/// The 80-byte block header and the transaction count that follows it.
struct RawHeader {
    version: i32,
    prev: [u8; 32],
    merkle: [u8; 32],
    time: u32,
    bits: u32,
    nonce: u32,
    ntx: i64,
}

impl RawHeader {
    fn parse(raw: &[u8]) -> Result<RawHeader> {
        ensure!(raw.len() > 80, "a block of {} bytes", raw.len());
        let u32_at = |at: usize| u32::from_le_bytes(raw[at..at + 4].try_into().unwrap_or_default());
        let ntx = match raw[80] {
            n @ 0..=0xfc => u64::from(n),
            0xfd if raw.len() >= 83 => u64::from(u16::from_le_bytes([raw[81], raw[82]])),
            0xfe if raw.len() >= 85 => u64::from(u32_at(81)),
            _ => bail!("an unusable transaction count"),
        };
        Ok(RawHeader {
            version: u32_at(0) as i32,
            prev: raw[4..36].try_into()?,
            merkle: raw[36..68].try_into()?,
            time: u32_at(68),
            bits: u32_at(72),
            nonce: u32_at(76),
            ntx: i64::try_from(ntx)?,
        })
    }
}

/// The undo records of a node's `rev*.dat` files, by `sha256d` of their bytes.
///
/// `WriteUndoDataForBlock` frames each record as network magic, a little-endian `u32` size, the
/// serialized `CBlockUndo`, and `sha256d(prevBlockHash ‖ CBlockUndo)`. So `undo` checks out when a
/// record has exactly those bytes and its checksum binds them to the block's parent.
pub(super) struct UndoIndex {
    files: Vec<PathBuf>,
    /// By `sha256d(bytes)`, wire order.
    records: HashMap<[u8; 32], UndoRecord>,
}

/// Where one copy of an undo record's bytes is, and every checksum stored with them (blocks
/// without spends share their bytes).
struct UndoRecord {
    file: usize,
    start: usize,
    size: usize,
    checksums: Vec<[u8; 32]>,
}

impl UndoIndex {
    pub(super) fn scan(dir: &Path) -> Result<UndoIndex> {
        let mut files: Vec<PathBuf> = fs::read_dir(dir)
            .with_context(|| format!("reading {}", dir.display()))?
            .map(|e| e.map(|e| e.path()))
            .collect::<Result<_, _>>()?;
        files.retain(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rev") && n.ends_with(".dat"))
        });
        files.sort();
        ensure!(!files.is_empty(), "no rev*.dat files in {}", dir.display());
        let mut records: HashMap<[u8; 32], UndoRecord> = HashMap::new();
        let mut magic: Option<[u8; 4]> = None;
        for (f, path) in files.iter().enumerate() {
            let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            let mut at = 0;
            // The files are preallocated with zeros after the last record.
            while at + 8 <= bytes.len() && bytes[at..at + 4] != [0; 4] {
                let m: [u8; 4] = bytes[at..at + 4].try_into()?;
                ensure!(
                    *magic.get_or_insert(m) == m,
                    "{}: a record with other magic",
                    path.display()
                );
                let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into()?) as usize;
                let start = at + 8;
                let end = start + size;
                ensure!(end + 32 <= bytes.len(), "{}: a truncated record at {at}", path.display());
                let digest = sha256d::Hash::hash(&bytes[start..end]).to_byte_array();
                let checksum: [u8; 32] = bytes[end..end + 32].try_into()?;
                records
                    .entry(digest)
                    .or_insert(UndoRecord {
                        file: f,
                        start,
                        size,
                        checksums: Vec::new(),
                    })
                    .checksums
                    .push(checksum);
                at = end + 32;
            }
        }
        Ok(UndoIndex {
            files,
            records,
        })
    }

    /// Whether a record has `sha256d(bytes) == undo` and a checksum binding it to `prev`.
    pub(super) fn check(&self, undo: &Hash256, prev: &Hash256) -> Result<()> {
        let Some(record) = self.records.get(&undo.to_wire_bytes()) else {
            bail!("no undo record in the rev files hashes to {undo}");
        };
        let path = &self.files[record.file];
        let mut framed = prev.to_wire_bytes().to_vec();
        framed.resize(32 + record.size, 0);
        let mut file =
            fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
        file.seek(SeekFrom::Start(record.start as u64))?;
        file.read_exact(&mut framed[32..])
            .with_context(|| format!("reading {}", path.display()))?;
        let checksum = sha256d::Hash::hash(&framed).to_byte_array();
        ensure!(
            record.checksums.contains(&checksum),
            "the undo record hashing to {undo} is not stored for a block whose parent is {prev}"
        );
        Ok(())
    }
}
