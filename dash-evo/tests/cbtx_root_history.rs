//! SB7 acceptance: the CbTx `merkleRootMNList` equals [`SmlList::merkle_root`] at every block of
//! a chain's history (PLAN SB7, FR-EVO-07).
//!
//! The ignored test reads per-block dumps of Core's simplified masternode list, taken from a Dash
//! Core v24 node over its RPC (`getblock` for the CbTx, `protx diff` for Core's
//! `CSimplifiedMNListEntry` fields; the SB7 report says where the dump tool lives). The dump tool
//! refetches the full list when the per-block root differs. Each dump file is gzip JSONL, one
//! record per block, and its first record holds the full list.
//!
//! An acceptance run names the network (`mainnet` or `testnet`) and the exact range, and takes one
//! or more dump files in height order, comma-separated, and the coinbase proofs of the range:
//!
//! ```text
//! SB7_HISTORY=a.jsonl.gz,b.jsonl.gz SB7_HISTORY_PROOFS=testnet.proof.gz \
//!   SB7_HISTORY_NETWORK=testnet SB7_HISTORY_START=7000 SB7_HISTORY_END=1569775 \
//!   cargo test -p dash-evo --release --test cbtx_root_history -- --ignored --nocapture
//! ```
//!
//! It fails unless the heights run from `SB7_HISTORY_START` to exactly `SB7_HISTORY_END` with no gap
//! (across files too: each file must continue the list the previous one ended with), every record
//! carries a CbTx, and every read, gzip and JSON record is whole. A mainnet end above
//! [`MAINNET_END_CAP`] is refused.
//!
//! The run is bound to the network's chain, record contents included. The proofs file is one gzip
//! member of text, one `<height> <merkleblock hex> <coinbase hex>` line for each height of the run
//! and nothing else, read to its end: Core's `gettxoutproof` for the block's coinbase, and the
//! coinbase.
//! For each record:
//!
//! - its block hash is the X11 hash of the proof's header, which links to the previous record's;
//! - the partial merkle tree proves the coinbase is the block's first transaction
//!   ([`verify_cbtx_proof`]), and the record's CbTx version and `merkleRootMNList` are that
//!   coinbase's;
//! - at every anchor height its hash is the anchor's. The anchors are Core's checkpoints for the
//!   network (dash-params) and the tracked [`ANCHOR_MANIFEST`].
//!
//! Every file must hold an anchor height, and the end must be one, so the hash links bind every
//! record to it.
//!
//! Every block's cached root is compared with the CbTx, and every `SB7_FULL_EVERY` blocks
//! (default 10,000) with [`merkle_root_mn_list`] over the whole list as well.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Lines, Read};
use std::net::IpAddr;

use dash_consensus::hash::x11;
use dash_consensus::primitives::{BlockHash, MerkleRoot, decode_tx_exact};
use dash_evo::cbtx::{PartialMerkleTree, verify_cbtx_proof};
use dash_evo::sml::{SmlEntry, SmlList, merkle_root_mn_list};
use dash_evo::types::{ProTxHash, RawMnType};
use dash_num::Hash256;
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::{
    AddrV1, CoinbaseCommitment, NetInfo, NetInfoV1, PlatformNodeId, ServiceV1, SpecialPayload,
    TxType,
};
use dash_types::Numeric;
use flate2::bufread::GzDecoder;
use flate2::read::MultiGzDecoder;
use serde_json::Value;

/// The highest mainnet height a run may cover.
const MAINNET_END_CAP: u64 = 2_090_000;

/// Anchor hashes beyond Core's checkpoints: `<network> <height> <hash>` lines, with their source.
const ANCHOR_MANIFEST: &str = include_str!("data/sb7_anchor_hashes.txt");

/// A block hash in internal byte order, from its display hex.
fn internal_hash(display: &str) -> Result<[u8; 32], String> {
    if display.len() != 64 || !display.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("block hash {display:?}"));
    }
    let mut hash: [u8; 32] = unhex(display).try_into().unwrap();
    hash.reverse();
    Ok(hash)
}

/// Core's checkpoints for `network` and its manifest entries, by height.
fn anchors(network: Network) -> BTreeMap<u64, [u8; 32]> {
    let params =
        ChainParamsBuilder::new(network, Release::V24_0).build().expect("Core's parameters");
    let mut anchors = BTreeMap::new();
    for (height, hash) in params.checkpoints {
        let mut internal = hash.to_bendian();
        internal.reverse();
        anchors.insert(u64::from(height.to_u32()), internal);
    }
    let name = match network {
        Network::Main => "mainnet",
        Network::Testnet3 => "testnet",
        other => panic!("no anchors for {other:?}"),
    };
    for line in ANCHOR_MANIFEST.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        let [net, height, hash] = line.split_whitespace().collect::<Vec<_>>()[..] else {
            panic!("manifest line {line:?}");
        };
        assert!(["mainnet", "testnet"].contains(&net), "manifest line {line:?}");
        if net != name {
            continue;
        }
        let hash = internal_hash(hash).unwrap();
        let old = anchors.insert(height.parse().unwrap(), hash);
        assert!(old.is_none_or(|old| old == hash), "manifest {line:?} contradicts an anchor");
    }
    anchors
}

/// A gzip stream of exactly one member, as the proof fetcher writes: any byte after the member's
/// trailer, even a whole empty member, is a read error.
struct OneMember<R: BufRead>(GzDecoder<R>);

impl<R: BufRead> Read for OneMember<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.0.read(buf)?;
        if read == 0 && !buf.is_empty() && !self.0.get_mut().fill_buf()?.is_empty() {
            return Err(std::io::Error::other("data after the gzip member"));
        }
        Ok(read)
    }
}

/// The proofs file's lines, read from its one gzip member.
fn proofs_reader(gz: impl BufRead + 'static) -> Box<dyn BufRead> {
    Box::new(BufReader::new(OneMember(GzDecoder::new(gz))))
}

/// The network's chain a strict run is bound to: its anchors and the coinbase proofs of the range.
struct Chain {
    anchors: BTreeMap<u64, [u8; 32]>,
    proofs: Lines<Box<dyn BufRead>>,
    /// The previous record's hash.
    prev: Option<[u8; 32]>,
}

/// What a proof line establishes about its block.
struct Proven {
    anchor: bool,
    cbtx: CoinbaseCommitment,
}

/// A compact size from the front of `bytes`.
fn compact_size(bytes: &mut &[u8]) -> Option<u64> {
    let (&first, rest) = bytes.split_first()?;
    let width = match first {
        0xfd => 2,
        0xfe => 4,
        0xff => 8,
        small => {
            *bytes = rest;
            return Some(u64::from(small));
        }
    };
    let (value, rest) = rest.split_at_checked(width)?;
    *bytes = rest;
    let mut le = [0; 8];
    le[..width].copy_from_slice(value);
    Some(u64::from_le_bytes(le))
}

/// A serialized `CMerkleBlock`: the header and the partial merkle tree, with nothing left over.
fn merkle_block(bytes: &[u8]) -> Option<([u8; 80], PartialMerkleTree)> {
    let (header, mut rest) = bytes.split_at_checked(80)?;
    let (transactions, tail) = rest.split_at_checked(4)?;
    rest = tail;
    let hashes = (0..compact_size(&mut rest)?)
        .map(|_| {
            let (hash, tail) = rest.split_at_checked(32)?;
            rest = tail;
            hash.try_into().ok()
        })
        .collect::<Option<_>>()?;
    let flags_len = compact_size(&mut rest)?;
    (rest.len() as u64 == flags_len).then(|| {
        let tree = PartialMerkleTree {
            transactions: u32::from_le_bytes(transactions.try_into().unwrap()),
            hashes,
            flags: rest.to_vec(),
        };
        (header.try_into().unwrap(), tree)
    })
}

impl Chain {
    fn new(anchors: BTreeMap<u64, [u8; 32]>, proofs: Box<dyn BufRead>) -> Self {
        Self {
            anchors,
            proofs: proofs.lines(),
            prev: None,
        }
    }

    /// Checks the record at `height` with block hash `hash` against its proof, the previous record
    /// and an anchor, and returns the block's CbTx.
    fn block(&mut self, height: u64, hash: [u8; 32]) -> Result<Proven, String> {
        let Some(line) = self.proofs.next() else {
            return Err(format!("proofs: none for {height}"));
        };
        let line = line.map_err(|err| format!("proofs: read error ({err})"))?;
        let at = line.split(' ').next().and_then(|at| at.parse::<u64>().ok());
        let at = at.ok_or_else(|| format!("proofs: bad line {line:?}"))?;
        if at != height {
            return Err(format!("proofs: {at} where {height} was due"));
        }
        let bad = || format!("proofs: bad line for {height}");
        let [_, block, coinbase] = line.split(' ').collect::<Vec<_>>()[..] else {
            return Err(bad());
        };
        let hex = |text: &str| {
            (text.len().is_multiple_of(2) && text.bytes().all(|b| b.is_ascii_hexdigit()))
                .then(|| unhex(text))
        };
        let (header, tree) = hex(block).as_deref().and_then(merkle_block).ok_or_else(bad)?;
        let coinbase = hex(coinbase).ok_or_else(bad)?;
        if x11(&header) != hash {
            return Err(format!("the block hash at {height} is not its header's"));
        }
        if self.prev.is_some_and(|prev| header[4..36] != prev) {
            return Err(format!("the header at {height} does not link to the block before"));
        }
        self.prev = Some(hash);
        let anchor = match self.anchors.get(&height) {
            Some(anchor) if *anchor != hash => {
                return Err(format!("the block hash at {height} is not the network's anchor"));
            }
            anchor => anchor.is_some(),
        };
        let tx =
            decode_tx_exact(&coinbase).map_err(|err| format!("coinbase at {height}: {err:?}"))?;
        let tx = tx.into_value();
        let root = MerkleRoot::from_lendian(header[36..68].try_into().unwrap());
        verify_cbtx_proof(&tree, &root, &tx.txid())
            .map_err(|err| format!("the coinbase proof at {height}: {err}"))?;
        let cbtx = match tx.payload() {
            Some(Ok(decoded))
                if tx.is_coinbase()
                    && tx.tx_type() == TxType::CoinbaseCommitment
                    && decoded.is_complete() =>
            {
                match &decoded.payload {
                    SpecialPayload::CoinbaseCommitment(cbtx) => cbtx.clone(),
                    _ => return Err(format!("the coinbase at {height} has no CbTx")),
                }
            }
            _ => return Err(format!("the coinbase at {height} has no CbTx")),
        };
        Ok(Proven {
            anchor,
            cbtx,
        })
    }

    /// Reads the proofs to their end after the run's last block: any line left, or any read or
    /// gzip error (a damaged trailer, an incomplete member), fails.
    fn finish(&mut self, end: u64) -> Result<(), String> {
        match self.proofs.next() {
            None => Ok(()),
            Some(Ok(line)) => Err(format!(
                "proofs: a line after the end {end}: {:?}",
                line.get(..40).unwrap_or(&line)
            )),
            Some(Err(err)) => Err(format!("proofs: read error after the end {end} ({err})")),
        }
    }
}

fn unhex(hex: &str) -> Vec<u8> {
    (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect()
}

fn pro_tx(display: &Value) -> ProTxHash {
    ProTxHash::from(Hash256::from_hex(display.as_str().unwrap()).unwrap())
}

/// A legacy `CService` from Core's `ToStringAddrPort()`: `a.b.c.d:port` or `[v6]:port`; the empty
/// string is the unset service, `[::]:0`.
fn service(text: &str) -> ServiceV1 {
    if text.is_empty() {
        return ServiceV1 {
            addr: AddrV1([0; 16]),
            port: 0,
        };
    }
    let (host, port) = text.rsplit_once(':').unwrap();
    let ip: IpAddr = host.trim_start_matches('[').trim_end_matches(']').parse().unwrap();
    let addr = match ip {
        IpAddr::V4(v4) => v4.to_ipv6_mapped(),
        IpAddr::V6(v6) => v6,
    }
    .octets();
    ServiceV1 {
        addr: AddrV1(addr),
        port: port.parse().unwrap(),
    }
}

/// One `up` row: `[proTxHash, version, type, confirmedHash, service, pubKeyOperator,
/// votingKeyId, isValid, platformHTTPPort, platformNodeID]`.
fn entry(row: &Value) -> SmlEntry {
    let version = u16::try_from(row[1].as_u64().unwrap()).unwrap();
    // The dumps hold the legacy service only; no chain they cover has an ExtAddr entry.
    assert!(version < 3, "ExtAddr entry {row}");
    let mn_type = match row[2].as_str().unwrap() {
        "Regular" => RawMnType::REGULAR,
        "Evo" => RawMnType::EVO,
        other => panic!("type {other}"),
    };
    SmlEntry {
        version,
        pro_reg_tx_hash: pro_tx(&row[0]),
        confirmed_hash: BlockHash::from_hex(row[3].as_str().unwrap()).unwrap(),
        net_info: NetInfo::Legacy(NetInfoV1(service(row[4].as_str().unwrap()))),
        operator_key: unhex(row[5].as_str().unwrap()).try_into().unwrap(),
        voting_key_id: unhex(row[6].as_str().unwrap()).try_into().unwrap(),
        is_valid: row[7].as_bool().unwrap(),
        mn_type,
        platform_http_port: u16::try_from(row[8].as_u64().unwrap()).unwrap(),
        platform_node_id: PlatformNodeId::from_hex(row[9].as_str().unwrap()).unwrap(),
    }
}

/// How a run treats its input.
#[derive(Clone, Copy, Debug)]
enum Mode {
    /// Acceptance: exactly `start..=end`, whole records, a CbTx on each.
    Strict {
        start: u64,
        end: u64,
    },
    /// The prefix the files hold, stopping at the first unreadable record.
    Advisory,
}

#[derive(Debug, Default)]
struct Summary {
    first: Option<u64>,
    last: u64,
    blocks: u64,
    checked: u64,
    full_checks: u64,
    max_len: usize,
    mismatches: u64,
    /// Anchor heights whose hash matched.
    anchored: u64,
    /// The first mismatches: (height, entries).
    first_mismatches: Vec<(u64, usize)>,
    /// Why an advisory run stopped early.
    stopped: Option<String>,
}

/// Replays `files` (name, reader) in order against their CbTx roots, and in strict mode against
/// `chain`. Errors name the file and the last good height.
fn check(
    files: Vec<(String, Box<dyn BufRead>)>,
    mut chain: Option<Chain>,
    mode: Mode,
    mainnet: bool,
    full_every: u64,
) -> Result<Summary, String> {
    if let Mode::Strict {
        start,
        end,
    } = mode
    {
        if start > end {
            return Err(format!("start {start} is above end {end}"));
        }
        if mainnet && end > MAINNET_END_CAP {
            return Err(format!("mainnet end {end} is above {MAINNET_END_CAP}"));
        }
        let Some(chain) = &chain else {
            return Err("a strict run needs the network's coinbase proofs".into());
        };
        if !chain.anchors.contains_key(&end) {
            let below = chain.anchors.range(..end).next_back().map(|(h, _)| *h);
            let above = chain.anchors.range(end..).next().map(|(h, _)| *h);
            return Err(format!(
                "the end {end} is not an anchor height (nearest: {below:?}, {above:?})"
            ));
        }
    }
    if full_every == 0 {
        return Err("SB7_FULL_EVERY must be positive".into());
    }
    let strict = matches!(mode, Mode::Strict { .. });
    let mut list = SmlList::new();
    let mut sum = Summary::default();
    'files: for (index, (name, reader)) in files.into_iter().enumerate() {
        let mut first_in_file = true;
        let mut anchored = false;
        let name = &name;
        for line in reader.lines() {
            let after = match sum.first {
                Some(_) => format!("after height {}", sum.last),
                None => "before any record".into(),
            };
            let failure = move |what: String| format!("{name}: {what} {after}");
            let line = match line {
                Ok(line) => line,
                Err(err) if strict => return Err(failure(format!("read error ({err})"))),
                Err(err) => {
                    sum.stopped = Some(failure(format!("read error ({err})")));
                    break 'files;
                }
            };
            let block = match serde_json::from_str::<Value>(&line) {
                Ok(block) => block,
                Err(err) if strict => return Err(failure(format!("bad record ({err})"))),
                Err(err) => {
                    sum.stopped = Some(failure(format!("bad record ({err})")));
                    break 'files;
                }
            };
            let height =
                block["h"].as_u64().ok_or_else(|| failure("a record without a height".into()))?;
            let expected = match (sum.first, mode) {
                (Some(_), _) => Some(sum.last + 1),
                (
                    None,
                    Mode::Strict {
                        start,
                        ..
                    },
                ) => Some(start),
                (None, Mode::Advisory) => None,
            };
            match expected {
                Some(expected) if height != expected => {
                    return Err(failure(format!("height {height} where {expected} was due")));
                }
                _ => {}
            }
            if let Mode::Strict {
                end,
                ..
            } = mode
                && height > end
            {
                return Err(failure(format!("height {height} beyond the end {end}")));
            }
            if mainnet && height > MAINNET_END_CAP {
                return Err(failure(format!("mainnet height {height} above {MAINNET_END_CAP}")));
            }
            let mut proven = None;
            if strict {
                let chain = chain.as_mut().expect("checked above");
                let block = block["hash"]
                    .as_str()
                    .ok_or_else(|| "a record without a block hash".to_owned())
                    .and_then(internal_hash)
                    .and_then(|hash| chain.block(height, hash))
                    .map_err(&failure)?;
                if block.anchor {
                    anchored = true;
                    sum.anchored += 1;
                }
                proven = Some(block.cbtx);
            }
            sum.first.get_or_insert(height);
            sum.last = height;
            sum.blocks += 1;
            let removed = block["rm"].as_array().unwrap();
            let up = block["up"].as_array().unwrap();
            // A file's first record holds the full list at its height, and `rm` that block's own
            // deletions.
            let full = first_in_file.then(|| SmlList::from_entries(up.iter().map(entry)));
            if let Some(full) = &full {
                if full.len() != up.len() {
                    return Err(failure(format!("the full list at {height} repeats an entry")));
                }
                if let Some(both) = removed.iter().find(|r| full.get(&pro_tx(r)).is_some()) {
                    return Err(failure(format!("{both} both removed and listed at {height}")));
                }
            }
            // The first file starts from an empty list: its first deletions have nothing to remove.
            if !(first_in_file && index == 0) {
                for removed in removed {
                    if list.remove(&pro_tx(removed)).is_none() {
                        return Err(failure(format!(
                            "{removed} removed at {height} but not listed"
                        )));
                    }
                }
            }
            // A later file must continue the list: every entry carried over and not removed is in
            // its full list. Entries the block itself added or changed can't be told apart from
            // carried ones; the CbTx root covers their contents.
            if let Some(full) = &full
                && let Some(lost) =
                    list.entries().iter().find(|e| full.get(&e.pro_reg_tx_hash).is_none())
            {
                return Err(failure(format!(
                    "{} carried over but missing from the full list at {height}",
                    lost.pro_reg_tx_hash
                )));
            }
            for row in up {
                list.upsert(entry(row));
            }
            first_in_file = false;
            sum.max_len = sum.max_len.max(list.len());
            let root =
                list.merkle_root().map_err(|err| failure(format!("root at {height}: {err:?}")))?;
            if height % full_every == 0 {
                assert_eq!(
                    merkle_root_mn_list(list.entries()).unwrap(),
                    root,
                    "full vs cached at {height}"
                );
                sum.full_checks += 1;
            }
            let Some(cbtx) = block["cb"].as_array() else {
                if strict {
                    return Err(failure(format!("no CbTx at {height}")));
                }
                continue;
            };
            let committed = MerkleRoot::from_hex(cbtx[1].as_str().unwrap()).unwrap();
            if let Some(proven) = proven
                && (cbtx[0].as_u64() != Some(u64::from(proven.version))
                    || committed != proven.merkle_root_mn_list)
            {
                return Err(failure(format!("the record's CbTx at {height} is not the block's")));
            }
            sum.checked += 1;
            if root != committed {
                sum.mismatches += 1;
                if sum.first_mismatches.len() < 20 {
                    sum.first_mismatches.push((height, list.len()));
                }
            }
        }
        if strict && !anchored {
            return Err(format!("{name}: the file holds no anchor height"));
        }
    }
    if let Mode::Strict {
        start,
        end,
    } = mode
    {
        if sum.first.is_none() {
            return Err(format!("the dumps hold no record, not {start}..={end}"));
        }
        if sum.last != end {
            return Err(format!("the dumps end at {}, not at {end}", sum.last));
        }
        if sum.blocks != end - start + 1 || sum.checked != sum.blocks {
            return Err(format!(
                "{} blocks and {} roots compared over {start}..={end}",
                sum.blocks, sum.checked
            ));
        }
        chain.as_mut().expect("checked above").finish(end)?;
    }
    Ok(sum)
}

#[test]
#[ignore = "needs history dumps (SB7_HISTORY)"]
#[expect(
    clippy::disallowed_methods,
    reason = "a test harness, not consensus code, reads its input paths"
)]
fn cbtx_mn_list_root_over_history() {
    let var = |name: &str| std::env::var(name).ok();
    let paths = var("SB7_HISTORY").expect("SB7_HISTORY");
    let full_every: u64 = var("SB7_FULL_EVERY").map_or(10_000, |n| n.parse().unwrap());
    let network = var("SB7_HISTORY_NETWORK");
    let advisory = var("SB7_HISTORY_ADVISORY").is_some_and(|v| v == "1");
    let mode = if advisory {
        Mode::Advisory
    } else {
        let height = |name: &str| var(name).unwrap_or_else(|| panic!("{name}")).parse().unwrap();
        assert!(network.is_some(), "SB7_HISTORY_NETWORK");
        Mode::Strict {
            start: height("SB7_HISTORY_START"),
            end: height("SB7_HISTORY_END"),
        }
    };
    let kind = network.as_deref().and_then(|network| match network {
        "mainnet" => Some(Network::Main),
        "testnet" => Some(Network::Testnet3),
        "regtest" if advisory => None,
        other => panic!("network {other}"),
    });
    let open = |path: &str| {
        let file = std::fs::File::open(path).unwrap_or_else(|err| panic!("{path}: {err}"));
        Box::new(BufReader::new(MultiGzDecoder::new(file))) as Box<dyn BufRead>
    };
    let chain = (!advisory).then(|| {
        let proofs = var("SB7_HISTORY_PROOFS").expect("SB7_HISTORY_PROOFS");
        let file = std::fs::File::open(&proofs).unwrap_or_else(|err| panic!("{proofs}: {err}"));
        Chain::new(anchors(kind.unwrap()), proofs_reader(BufReader::new(file)))
    });
    let files = paths.split(',').map(|path| (path.to_owned(), open(path))).collect();
    let label = if advisory {
        "ADVISORY (not acceptance)"
    } else {
        "ACCEPTANCE"
    };
    let sum = check(files, chain, mode, kind == Some(Network::Main), full_every)
        .unwrap_or_else(|err| panic!("{label}: {err}"));
    if let Some(stopped) = &sum.stopped {
        println!("{label}: stopped early: {stopped}");
    }
    let bound = if advisory {
        "not bound to the chain".to_owned()
    } else {
        format!("headers linked, {} anchors matched, coinbases proven", sum.anchored)
    };
    println!(
        "{label} {}: heights {}..={}, {} blocks, {bound}, {} CbTx roots compared, {} full \
         recomputes, up to {} entries, {} mismatches",
        network.as_deref().unwrap_or("?"),
        sum.first.unwrap_or(0),
        sum.last,
        sum.blocks,
        sum.checked,
        sum.full_checks,
        sum.max_len,
        sum.mismatches
    );
    assert!(sum.checked > 0);
    assert_eq!(sum.mismatches, 0, "first mismatches (height, entries): {:?}", sum.first_mismatches);
    assert!(!advisory, "ADVISORY: a prefix check, never acceptance");
}

mod strictness {
    use std::io::{Cursor, Write};
    use std::ops::RangeInclusive;

    use flate2::Compression;
    use flate2::write::GzEncoder;
    use serde_json::json;

    use super::*;

    fn row(byte: u8) -> Value {
        json!([
            format!("{byte:02x}").repeat(32),
            2,
            "Regular",
            "11".repeat(32),
            "1.2.3.4:9999",
            "8a".repeat(48),
            "22".repeat(20),
            true,
            0,
            "00".repeat(20)
        ])
    }

    /// One record over the list `rows`, with the CbTx root of that list.
    fn root(rows: &[Value]) -> MerkleRoot {
        SmlList::from_entries(rows.iter().map(entry)).merkle_root().unwrap()
    }

    /// One record over the list `rows`, with the CbTx root of that list.
    fn record(height: u64, rows: &[Value], up: &[Value], rm: &[u8]) -> String {
        let root = root(rows);
        assert_eq!(MerkleRoot::from_hex(&root.to_string()).unwrap(), root);
        let rm: Vec<_> = rm.iter().map(|byte| format!("{byte:02x}").repeat(32)).collect();
        let hash = display(block_hash(height));
        json!({"h": height, "hash": hash, "cb": [1, root.to_string()], "up": up, "rm": rm})
            .to_string()
    }

    /// The list of the synthetic chain at `height`: entry 1 to 10, both at 11 and 12, then entry 2.
    fn list_at(height: u64) -> Vec<Value> {
        match height {
            ..=10 => vec![row(1)],
            11 | 12 => vec![row(1), row(2)],
            _ => vec![row(2)],
        }
    }

    /// A coinbase whose version 1 CbTx commits to `root` at `height`.
    fn coinbase(height: u64, root: &MerkleRoot) -> Vec<u8> {
        // Version 3, type 5 (CbTx); one null input; one empty output; lock time 0.
        let mut tx = vec![3, 0, 5, 0, 1];
        tx.extend([0; 32]);
        tx.extend([0xff; 4]);
        tx.extend([2, 1, height as u8]);
        tx.extend([0xff; 4]);
        tx.push(1);
        tx.extend([0; 9]);
        tx.extend([0; 4]);
        let mut payload = vec![1, 0];
        payload.extend((height as i32).to_le_bytes());
        payload.extend(root.to_lendian());
        tx.push(payload.len() as u8);
        tx.extend(payload);
        tx
    }

    fn txid(coinbase: &[u8]) -> [u8; 32] {
        decode_tx_exact(coinbase).unwrap().into_value().txid().to_lendian()
    }

    /// The headers of a synthetic chain, heights `0..=HEIGHTS`: each links to the one before and
    /// carries its coinbase as its only transaction.
    const HEIGHTS: u64 = 20;

    fn headers() -> Vec<[u8; 80]> {
        let mut headers = vec![[0; 80]; HEIGHTS as usize + 1];
        for (height, header) in headers.iter_mut().enumerate() {
            let height = height as u64;
            header[36..68].copy_from_slice(&txid(&coinbase(height, &root(&list_at(height)))));
            header[68..76].copy_from_slice(&height.to_le_bytes());
        }
        relink(&mut headers, 1);
        headers
    }

    /// Points each header from `from` on at the one before.
    fn relink(headers: &mut [[u8; 80]], from: usize) {
        for height in from..headers.len() {
            let prev = x11(&headers[height - 1]);
            headers[height][4..36].copy_from_slice(&prev);
        }
    }

    fn block_hash(height: u64) -> [u8; 32] {
        x11(&headers()[height as usize])
    }

    fn display(mut hash: [u8; 32]) -> String {
        hash.reverse();
        hash.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The proof lines of `heights` over `headers`, each block's coinbase committing to `list_at`.
    fn proof_lines(headers: &[[u8; 80]], heights: RangeInclusive<u64>) -> Vec<String> {
        heights
            .map(|height| {
                let coinbase = coinbase(height, &root(&list_at(height)));
                // A one-transaction block: the tree is the coinbase's txid, matched.
                let header = &headers[height as usize][..];
                let block = [header, &[1, 0, 0, 0, 1], &txid(&coinbase), &[1, 1]].concat();
                format!("{height} {} {}", hex(&block), hex(&coinbase))
            })
            .collect()
    }

    /// The synthetic chain with `anchors` heights anchored to their own hashes.
    fn chain_with(anchors: &[u64], proofs: Vec<String>) -> Chain {
        let anchors = anchors.iter().map(|&height| (height, block_hash(height))).collect();
        Chain::new(anchors, proofs_reader(Cursor::new(gz(&proofs))))
    }

    /// Strict and advisory runs over the synthetic chain, every height an anchor, with the proofs
    /// of the run's range.
    fn run(dumps: Vec<Vec<u8>>, mode: Mode) -> Result<Summary, String> {
        let heights = match mode {
            Mode::Strict {
                start,
                end,
            } => start..=end,
            Mode::Advisory => 0..=HEIGHTS,
        };
        let chain =
            chain_with(&(0..=HEIGHTS).collect::<Vec<_>>(), proof_lines(&headers(), heights));
        check(files(dumps), Some(chain), mode, false, 1)
    }

    fn gz(lines: &[String]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        for line in lines {
            writeln!(encoder, "{line}").unwrap();
        }
        encoder.finish().unwrap()
    }

    fn files(dumps: Vec<Vec<u8>>) -> Vec<(String, Box<dyn BufRead>)> {
        dumps
            .into_iter()
            .enumerate()
            .map(|(i, bytes)| {
                let reader = BufReader::new(MultiGzDecoder::new(Cursor::new(bytes)));
                (format!("dump{i}"), Box::new(reader) as Box<dyn BufRead>)
            })
            .collect()
    }

    /// Heights 10..=13: entry 1 from 10, entry 2 from 11, entry 1 removed at 13.
    fn history() -> Vec<String> {
        let both = [row(1), row(2)];
        vec![
            record(10, &both[..1], &both[..1], &[]),
            record(11, &both, &both[1..], &[]),
            record(12, &both, &[], &[]),
            record(13, &both[1..], &[], &[1]),
        ]
    }

    fn strict(start: u64, end: u64) -> Mode {
        Mode::Strict {
            start,
            end,
        }
    }

    #[test]
    fn a_complete_dump_passes() {
        let sum = run(vec![gz(&history())], strict(10, 13)).unwrap();
        assert_eq!((sum.first, sum.last, sum.checked, sum.mismatches), (Some(10), 13, 4, 0));
    }

    #[test]
    fn stitched_files_pass_when_continuous() {
        let h = history();
        // The second file opens with its full list at 12.
        let full_at_12 = record(12, &[row(1), row(2)], &[row(1), row(2)], &[]);
        let dumps = vec![gz(&h[..2]), gz(&[full_at_12, h[3].clone()])];
        let sum = run(dumps, strict(10, 13)).unwrap();
        assert_eq!((sum.last, sum.checked, sum.mismatches), (13, 4, 0));
    }

    #[test]
    fn a_file_that_does_not_continue_the_list_fails() {
        let h = history();
        // Its "full list" at 12 lacks entry 1, which the first file still holds.
        let partial_at_12 = record(12, &[row(2)], &[row(2)], &[]);
        let err = run(vec![gz(&h[..2]), gz(&[partial_at_12])], strict(10, 12));
        let err = err.unwrap_err();
        assert!(err.contains("carried over but missing"), "{err}");
    }

    #[test]
    fn a_full_list_with_a_repeat_fails() {
        let h = history();
        // Entry 1 twice and entry 3 make as many rows as the carried {1, 2}, without entry 2.
        let repeat_at_12 = record(12, &[row(1), row(3)], &[row(1), row(1), row(3)], &[]);
        let err = run(vec![gz(&h[..2]), gz(&[repeat_at_12])], strict(10, 12));
        let err = err.unwrap_err();
        assert!(err.contains("repeats an entry"), "{err}");
    }

    #[test]
    fn a_full_list_that_also_removes_an_entry_fails() {
        let both_at_10 = record(10, &[row(1)], &[row(1)], &[1]);
        let err = run(vec![gz(&[both_at_10])], strict(10, 10)).unwrap_err();
        assert!(err.contains("both removed and listed at 10"), "{err}");
    }

    #[test]
    fn a_start_block_that_deletes_passes() {
        // The dump starts at a block that deleted entry 9, which the empty list never held.
        let mut h = history();
        h[0] = record(10, &[row(1)], &[row(1)], &[9]);
        let sum = run(vec![gz(&h)], strict(10, 13)).unwrap();
        assert_eq!((sum.last, sum.mismatches), (13, 0));
    }

    #[test]
    fn a_later_file_may_open_on_a_deletion() {
        let h = history();
        // The second file opens with its full list at 13, the block that deleted entry 1.
        let full_at_13 = record(13, &[row(2)], &[row(2)], &[1]);
        let sum = run(vec![gz(&h[..3]), gz(&[full_at_13])], strict(10, 13));
        assert_eq!(sum.unwrap().mismatches, 0);
    }

    #[test]
    fn a_truncated_record_fails() {
        let mut h = history();
        let half = h[3].len() / 2;
        h[3].truncate(half);
        let err = run(vec![gz(&h)], strict(10, 13)).unwrap_err();
        assert!(err.contains("bad record") && err.contains("after height 12"), "{err}");
        // Advisory checks the prefix and says where it stopped.
        let sum = run(vec![gz(&h)], Mode::Advisory).unwrap();
        assert_eq!(sum.last, 12);
        assert!(sum.stopped.is_some());
    }

    #[test]
    fn a_truncated_gzip_stream_fails() {
        let mut bytes = gz(&history());
        bytes.truncate(bytes.len() - 12);
        let err = run(vec![bytes], strict(10, 13)).unwrap_err();
        assert!(err.contains("read error"), "{err}");
    }

    #[test]
    fn a_height_gap_fails() {
        let mut h = history();
        h.remove(2);
        let err = run(vec![gz(&h)], strict(10, 13)).unwrap_err();
        assert!(err.contains("height 13 where 12 was due"), "{err}");
        let err = run(vec![gz(&history())], strict(9, 13)).unwrap_err();
        assert!(err.contains("height 10 where 9 was due"), "{err}");
    }

    #[test]
    fn a_repeated_height_fails() {
        let mut h = history();
        h.insert(2, h[1].clone());
        let err = run(vec![gz(&h)], strict(10, 13)).unwrap_err();
        assert!(err.contains("height 11 where 12 was due"), "{err}");
    }

    #[test]
    fn a_height_beyond_the_end_fails() {
        let err = run(vec![gz(&history())], strict(10, 12)).unwrap_err();
        assert!(err.contains("height 13 beyond the end 12"), "{err}");
    }

    #[test]
    fn a_short_end_fails() {
        let err = run(vec![gz(&history()[..3])], strict(10, 13)).unwrap_err();
        assert!(err.contains("end at 12, not at 13"), "{err}");
        let err = run(vec![], strict(10, 13)).unwrap_err();
        assert!(err.contains("no record, not 10..=13"), "{err}");
    }

    #[test]
    fn a_record_without_a_cbtx_fails() {
        let mut h = history();
        h[1] = h[1].replace("\"cb\":[1,", "\"cb\":null,\"x\":[1,");
        let err = run(vec![gz(&h)], strict(10, 13)).unwrap_err();
        assert!(err.contains("no CbTx at 11"), "{err}");
    }

    #[test]
    fn mainnet_above_the_cap_is_refused() {
        let chain = chain_with(&[], proof_lines(&headers(), 10..=13));
        let err = check(files(vec![]), Some(chain), strict(10, MAINNET_END_CAP + 1), true, 1);
        let err = err.unwrap_err();
        assert!(err.contains("mainnet end 2090001 is above 2090000"), "{err}");
    }

    #[test]
    fn a_strict_run_needs_the_proofs() {
        let err = check(files(vec![gz(&history())]), None, strict(10, 13), false, 1).unwrap_err();
        assert!(err.contains("needs the network's coinbase proofs"), "{err}");
    }

    #[test]
    fn an_end_that_is_not_an_anchor_fails() {
        let chain = chain_with(&[10, 12], proof_lines(&headers(), 10..=13));
        let err = check(files(vec![gz(&history())]), Some(chain), strict(10, 13), false, 1);
        let err = err.unwrap_err();
        assert!(
            err.contains("the end 13 is not an anchor height (nearest: Some(12), None)"),
            "{err}"
        );
    }

    #[test]
    fn a_file_without_an_anchor_fails() {
        let h = history();
        let full_at_12 = record(12, &[row(1), row(2)], &[row(1), row(2)], &[]);
        let dumps = vec![gz(&h[..2]), gz(&[full_at_12, h[3].clone()])];
        // Only the second file reaches an anchor.
        let chain = chain_with(&[13], proof_lines(&headers(), 10..=13));
        let err = check(files(dumps), Some(chain), strict(10, 13), false, 1).unwrap_err();
        assert!(err.contains("dump0: the file holds no anchor height"), "{err}");
    }

    /// `history()` with the block hashes of `other` from height 10 on.
    fn history_on(other: &[[u8; 80]]) -> Vec<String> {
        let mut h = history();
        for (record, height) in h.iter_mut().zip(10..) {
            *record = record
                .replace(&display(block_hash(height)), &display(x11(&other[height as usize])));
        }
        h
    }

    #[test]
    fn a_block_off_the_anchored_chain_fails() {
        // A consistent chain of its own, whose block at 13 is not the anchor's.
        let mut other = headers();
        other[13][0] = 1;
        let chain = chain_with(&[13], proof_lines(&other, 10..=13));
        let err =
            check(files(vec![gz(&history_on(&other))]), Some(chain), strict(10, 13), false, 1);
        let err = err.unwrap_err();
        assert!(err.contains("the block hash at 13 is not the network's anchor"), "{err}");
    }

    #[test]
    fn a_fork_below_the_anchor_fails() {
        // A consistent fork from 11: only the anchor at 13 tells it from the chain.
        let mut other = headers();
        other[11][0] = 1;
        relink(&mut other, 12);
        let chain = chain_with(&[13], proof_lines(&other, 10..=13));
        let err =
            check(files(vec![gz(&history_on(&other))]), Some(chain), strict(10, 13), false, 1);
        let err = err.unwrap_err();
        assert!(err.contains("the block hash at 13 is not the network's anchor"), "{err}");
    }

    #[test]
    fn a_block_hash_that_is_not_its_headers_fails() {
        for at in [10, 12] {
            let mut h = history();
            h[at - 10] =
                h[at - 10].replace(&display(block_hash(at as u64)), &display(block_hash(15)));
            let err = run(vec![gz(&h)], strict(10, 13)).unwrap_err();
            assert!(err.contains(&format!("the block hash at {at} is not its header's")), "{err}");
        }
    }

    #[test]
    fn a_header_that_does_not_link_fails() {
        // Header 12 points elsewhere; the record carries that header's hash.
        let mut other = headers();
        other[12][4] ^= 1;
        let chain = chain_with(&[13], proof_lines(&other, 10..=13));
        let err =
            check(files(vec![gz(&history_on(&other))]), Some(chain), strict(10, 13), false, 1);
        let err = err.unwrap_err();
        assert!(err.contains("the header at 12 does not link to the block before"), "{err}");
    }

    #[test]
    fn a_link_broken_between_files_fails() {
        // The second file opens on a header that does not point at the first file's last block.
        let mut other = headers();
        other[12][4] ^= 1;
        let h = history_on(&other);
        let full_at_12 = record(12, &[row(1), row(2)], &[row(1), row(2)], &[])
            .replace(&display(block_hash(12)), &display(x11(&other[12])));
        let dumps = vec![gz(&h[..2]), gz(&[full_at_12, h[3].clone()])];
        let chain = chain_with(&[11, 13], proof_lines(&other, 10..=13));
        let err = check(files(dumps), Some(chain), strict(10, 13), false, 1).unwrap_err();
        assert!(err.contains("dump1: the header at 12 does not link"), "{err}");
    }

    #[test]
    fn a_missing_or_misplaced_proof_fails() {
        let all = proof_lines(&headers(), 10..=13);
        let chain = chain_with(&[13], all[..3].to_vec());
        let err = check(files(vec![gz(&history())]), Some(chain), strict(10, 13), false, 1);
        assert!(err.unwrap_err().contains("proofs: none for 13"));
        let mut gap = all.clone();
        gap.remove(2);
        let chain = chain_with(&[13], gap);
        let err = check(files(vec![gz(&history())]), Some(chain), strict(10, 13), false, 1);
        assert!(err.unwrap_err().contains("proofs: 13 where 12 was due"));
    }

    #[test]
    fn a_record_cbtx_that_is_not_the_blocks_fails() {
        // Another root, and another version, than the block's coinbase commits to.
        let other_root = root(&[row(3)]).to_string();
        let both = [row(1), row(2)];
        for edit in [
            record(12, &both, &[], &[]).replace(&root(&both).to_string(), &other_root),
            record(12, &both, &[], &[]).replace("\"cb\":[1,", "\"cb\":[2,"),
        ] {
            let mut h = history();
            h[2] = edit;
            let err = run(vec![gz(&h)], strict(10, 13)).unwrap_err();
            assert!(err.contains("the record's CbTx at 12 is not the block's"), "{err}");
        }
    }

    #[test]
    fn a_coinbase_that_is_not_the_blocks_fails() {
        // The proof's coinbase commits to another list, so it is not the header's transaction.
        let mut proofs = proof_lines(&headers(), 10..=13);
        let fake = coinbase(12, &root(&[row(3)]));
        let (keep, _) = proofs[2].rsplit_once(' ').unwrap();
        proofs[2] = format!("{keep} {}", hex(&fake));
        let chain = chain_with(&[13], proofs);
        let err = check(files(vec![gz(&history())]), Some(chain), strict(10, 13), false, 1);
        let err = err.unwrap_err();
        assert!(err.contains("the coinbase proof at 12:"), "{err}");
    }

    /// A strict run over `history()` with `proofs` as the proofs file's bytes.
    fn run_with_proofs(proofs: Vec<u8>) -> Result<Summary, String> {
        let anchors = BTreeMap::from([(13, block_hash(13))]);
        check(
            files(vec![gz(&history())]),
            Some(Chain::new(anchors, proofs_reader(Cursor::new(proofs)))),
            strict(10, 13),
            false,
            1,
        )
    }

    #[test]
    fn range_sized_proofs_pass() {
        let sum = run_with_proofs(gz(&proof_lines(&headers(), 10..=13))).unwrap();
        assert_eq!((sum.last, sum.anchored, sum.mismatches), (13, 1, 0));
    }

    #[test]
    fn proofs_below_the_start_fail() {
        let err = run_with_proofs(gz(&proof_lines(&headers(), 9..=13))).unwrap_err();
        assert!(err.contains("proofs: 9 where 10 was due"), "{err}");
    }

    #[test]
    fn a_line_after_the_end_proof_fails() {
        let lines = proof_lines(&headers(), 10..=13);
        let duplicate_end = [lines.clone(), vec![lines[3].clone()]].concat();
        let garbage = [lines.clone(), vec!["garbled trailing proof".into()]].concat();
        let higher = proof_lines(&headers(), 10..=14);
        for proofs in [duplicate_end, garbage, higher] {
            let err = run_with_proofs(gz(&proofs)).unwrap_err();
            assert!(err.contains("proofs: a line after the end 13"), "{err}");
        }
        // A partial last line, without its newline.
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        for line in &lines {
            writeln!(encoder, "{line}").unwrap();
        }
        write!(encoder, "14 abc").unwrap();
        let err = run_with_proofs(encoder.finish().unwrap()).unwrap_err();
        assert!(err.contains("proofs: a line after the end 13: \"14 abc\""), "{err}");
    }

    #[test]
    fn a_damaged_gzip_stream_after_the_end_proof_fails() {
        let whole = gz(&proof_lines(&headers(), 10..=13));
        let len = whole.len();
        let flip = |at: usize| {
            let mut bytes = whole.clone();
            bytes[at] ^= 1;
            bytes
        };
        let missing_trailer = whole[..len - 8].to_vec();
        let bad_crc = flip(len - 8);
        let bad_size = flip(len - 4);
        let second = gz(&["14 abc".to_owned()]);
        let incomplete_member = [&whole[..], &second[..second.len() / 2]].concat();
        // A valid second member, even an empty one that adds no line.
        let empty_member = [whole.clone(), gz(&[])].concat();
        let second_member = [whole.clone(), second].concat();
        for proofs in
            [missing_trailer, bad_crc, bad_size, incomplete_member, empty_member, second_member]
        {
            let err = run_with_proofs(proofs).unwrap_err();
            assert!(err.contains("proofs: read error"), "{err}");
        }
    }

    #[test]
    fn the_anchors_are_cores_checkpoints_and_the_manifest() {
        let mainnet = anchors(Network::Main);
        let testnet = anchors(Network::Testnet3);
        assert!(!mainnet.contains_key(&1_569_775) && !testnet.contains_key(&2_090_000));
        // Real headers (`getblockheader <hash> false` on the reference nodes) hash to the anchors,
        // which pins the byte order of checkpoints and manifest: mainnet 1,969,000 and testnet
        // 1,069,875 are checkpoints, the other two manifest entries.
        let headers = [
            (
                &mainnet,
                1_969_000,
                concat!(
                    "00000020f0e1205f781f75dae936541c8df6b9eae00e085ad71bb9391800000000000000c7c4b9f0",
                    "3d4f1305f2fdad4204cbc7c69f8c9fd38b3f92cd86ca150ad7113f23da1a4e65657d2519d07e95af"
                ),
            ),
            (
                &mainnet,
                2_090_000,
                concat!(
                    "00000020a7c6490a461c34577415dfcc6fed7ab48b98d1eebf467a930100000000000000e93c9697",
                    "9f1c7bc0e4342cbf9c84cd1753bd9de411156dee8f30d2df669ceca808e17066ccd422199f61ab2a"
                ),
            ),
            (
                &testnet,
                1_069_875,
                concat!(
                    "00000020f5ee919316c55ba8ddcddd975bec896608bb6de0b2cc048c49945e9912000000d2763fe3",
                    "cabd75b38f763e5e8a79ac5102609f45dadef60ec49d5ebcf534f4d31920a06638f1001eb07d0200"
                ),
            ),
            (
                &testnet,
                1_569_775,
                concat!(
                    "000000205a9de4638a12d4021c08108254c9cb26d9b6728fa2d1f607793c20755500000001daf7ad",
                    "1ba5b7c8f31e98cb4e9b01ecf635402a96e9cb798d41c357d69cf167bd2dca6ab9bc001e6fa30900"
                ),
            ),
        ];
        for (anchors, height, header) in headers {
            assert_eq!(anchors.get(&height), Some(&x11(&unhex(header))), "{height}");
        }
        // PLAN §7: the manifest holds no mainnet height above the cap.
        for line in ANCHOR_MANIFEST.lines().filter(|l| l.starts_with("mainnet ")) {
            let height: u64 = line.split_whitespace().nth(1).unwrap().parse().unwrap();
            assert!(height <= MAINNET_END_CAP, "{line}");
        }
    }
}
