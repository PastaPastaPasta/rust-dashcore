//! Reactive multi-peer block download scripts; honest compact blocks only.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, OnceLock};

use bitcoin_hashes::sha256;
use dash_consensus::primitives::core_serialize;
use dash_consensus::{Block, Header, decode_block};
use dash_num::{Arith256, CompactTarget};
use dash_oracle_sys::{ConnType, NetAddr, Oracle, Pin, SendMode, Service, TICK_MAINTENANCE};
use dash_p2p::siphash::SipHasher;
use dash_primitives::{BlockHash, BlockHeader};
use dash_types::Numeric;
use dashd_p2p::sim::{PeerVersionMsg, SIM_START};

use crate::p2p_addr::node_address_relay;
use crate::p2p_node::{
    DownloadView, PeerSpec, Session, Step, StepResolver, StepSource, compare_session,
};
use crate::{Compared, Outcome, divergence};

pub const FIXTURE_SHA256: [u8; 32] = [
    0x7d, 0x64, 0x2f, 0x2a, 0x7d, 0x37, 0x6a, 0x77, 0x0f, 0xe7, 0x69, 0xb6, 0x00, 0xa5, 0x0b, 0xc4,
    0xc4, 0x55, 0x46, 0x64, 0xfc, 0xfa, 0xe6, 0xb3, 0xef, 0x56, 0x3c, 0x26, 0x70, 0x79, 0x39, 0x97,
];

pub struct Fixture {
    pub deep: Vec<Arc<Block>>,
    pub prefix: Vec<Arc<Block>>,
    pub workload: Vec<Arc<Block>>,
    pub side: Vec<Arc<Block>>,
}

/// Loads the digest-pinned Core fixture once.
pub fn fixture() -> Result<&'static Fixture, &'static str> {
    static FIXTURE: OnceLock<Result<Fixture, &'static str>> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let bytes = include_bytes!("../fixtures/p3_multi_blocks.bin");
            if sha256::Hash::hash(bytes).to_byte_array() != FIXTURE_SHA256 {
                return Err("fixture digest");
            }
            let mut reader = Reader(bytes);
            if reader.take(8)? != b"P3MB01\0\0" {
                return Err("fixture magic");
            }
            let counts = [reader.u32()?, reader.u32()?, reader.u32()?, reader.u32()?];
            if counts != [1100, 100, 24, 3] {
                return Err("fixture counts");
            }
            let mut sections = Vec::new();
            for count in counts {
                let mut section = Vec::new();
                for _ in 0..count {
                    let n = reader.u32()? as usize;
                    let mut bytes = reader.take(n)?;
                    let block = decode_block(&mut bytes).map_err(|_| "fixture block")?.into_value();
                    if !bytes.is_empty() {
                        return Err("fixture trailing block bytes");
                    }
                    section.push(Arc::new(block));
                }
                sections.push(section);
            }
            if !reader.0.is_empty() {
                return Err("fixture trailing bytes");
            }
            let mut sections = sections.into_iter();
            Ok(Fixture {
                deep: sections.next().ok_or("deep")?,
                prefix: sections.next().ok_or("prefix")?,
                workload: sections.next().ok_or("workload")?,
                side: sections.next().ok_or("side")?,
            })
        })
        .as_ref()
        .map_err(|e| *e)
}

impl Fixture {
    pub fn chain(&self, profile: u8) -> Vec<Arc<Block>> {
        if profile == 0 {
            self.deep.clone()
        } else {
            self.prefix.iter().chain(&self.workload).chain(&self.side).cloned().collect()
        }
    }
}

struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], &'static str> {
        let (head, tail) = self.0.split_at_checked(n).ok_or("short input")?;
        self.0 = tail;
        Ok(head)
    }
    fn byte(&mut self) -> Result<u8, &'static str> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, &'static str> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().map_err(|_| "u16")?))
    }
    fn u32(&mut self) -> Result<u32, &'static str> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().map_err(|_| "u32")?))
    }
    fn u64(&mut self) -> Result<u64, &'static str> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().map_err(|_| "u64")?))
    }
}

fn compact(n: usize, out: &mut Vec<u8>) {
    if n < 253 {
        out.push(n as u8);
    } else {
        out.push(253);
        out.extend((n as u16).to_le_bytes());
    }
}

pub fn headers(blocks: &[Arc<Block>]) -> Vec<u8> {
    let mut out = Vec::new();
    compact(blocks.len(), &mut out);
    for block in blocks {
        out.extend(core_serialize(block.header()));
        out.push(0);
    }
    out
}

pub fn deliver(peer: usize, command: &'static str, payload: Vec<u8>) -> Step {
    Step::Deliver {
        peer,
        command,
        payload,
        subs: Vec::new(),
    }
}

/// Encodes the block's own transactions at their true prefilled positions.
pub fn honest_cmpctblock(
    block: &Block,
    nonce: u64,
    prefilled: &[usize],
) -> Result<Vec<u8>, &'static str> {
    if prefilled.windows(2).any(|w| w[0] >= w[1])
        || prefilled.iter().any(|&i| i >= block.txs().len())
    {
        return Err("prefilled indexes");
    }
    let mut out = core_serialize(block.header());
    out.extend(nonce.to_le_bytes());
    let key = sha256::Hash::hash(&out).to_byte_array();
    let k0 = u64::from_le_bytes(key[..8].try_into().map_err(|_| "key")?);
    let k1 = u64::from_le_bytes(key[8..16].try_into().map_err(|_| "key")?);
    compact(block.txs().len() - prefilled.len(), &mut out);
    for (index, tx) in block.txs().iter().enumerate() {
        if !prefilled.contains(&index) {
            let short = SipHasher::new(k0, k1).write(tx.txid().as_bytes()).finish();
            out.extend(&short.to_le_bytes()[..6]);
        }
    }
    compact(prefilled.len(), &mut out);
    let mut next = 0;
    for &index in prefilled {
        compact(index - next, &mut out);
        out.extend(core_serialize(&*block.txs()[index]));
        next = index + 1;
    }
    Ok(out)
}

/// The three full-block invalid variants; compact blocks never use these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BadVariant {
    TrailingPair,
    MerkleRoot,
    DuplicateTransaction,
}

pub fn bad_block(block: &Block, variant: BadVariant) -> Result<Arc<Block>, &'static str> {
    let mut txs = block.txs().to_vec();
    if txs.len() < 3 {
        return Err("not enough transactions");
    }
    match variant {
        BadVariant::TrailingPair => {
            if txs.len() % 2 != 1 {
                return Err("not an odd transaction count");
            }
            txs.push(Arc::clone(txs.last().ok_or("last transaction")?));
        }
        BadVariant::MerkleRoot => {
            let n = txs.len();
            txs.swap(n - 2, n - 1);
        }
        BadVariant::DuplicateTransaction => {
            txs.push(Arc::clone(&txs[1]));
        }
    }
    let mut header = *block.header();
    if variant == BadVariant::DuplicateTransaction {
        let root = Block::new(header, txs.clone()).merkle().0;
        let old = *header.raw();
        let target = CompactTarget::new(old.bits()).expand().value;
        let mut nonce = 0;
        loop {
            header = Header::new(BlockHeader::new(
                old.version(),
                old.prev_hash(),
                root,
                old.time(),
                old.bits(),
                nonce,
            ));
            if Arith256::from_lendian(*header.hash().as_bytes()) <= target {
                break;
            }
            nonce = nonce.checked_add(1).ok_or("nonce exhausted")?;
        }
    }
    Ok(Arc::new(Block::new(header, txs)))
}

type InvalidBlocks = Vec<[Option<Arc<Block>>; 3]>;
type Serving = (u16, u16, Option<(u8, bool)>);

fn bad_variants() -> Result<&'static InvalidBlocks, &'static str> {
    static BAD: OnceLock<Result<InvalidBlocks, &'static str>> = OnceLock::new();
    BAD.get_or_init(|| {
        Ok(fixture()?
            .workload
            .iter()
            .map(|b| {
                [
                    bad_block(b, BadVariant::TrailingPair).ok(),
                    bad_block(b, BadVariant::MerkleRoot).ok(),
                    bad_block(b, BadVariant::DuplicateTransaction).ok(),
                ]
            })
            .collect())
    })
    .as_ref()
    .map_err(|e| *e)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptHeader {
    pub seed: u64,
    pub profile: u8,
    pub peers: Vec<[u8; 2]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Choice {
    pub mode: u8,
    pub index: u16,
}
impl Choice {
    pub const fn absolute(index: u16) -> Self {
        Self {
            mode: 2,
            index,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    Pass {
        class: u8,
        peer: u8,
        delta: u16,
    },
    Headers {
        peer: u8,
        from: Choice,
        count: u16,
    },
    NextHeader {
        peer: u8,
    },
    Block {
        peer: u8,
        which: Choice,
    },
    BadBlock {
        peer: u8,
        which: Choice,
        variant: BadVariant,
    },
    Serve {
        rounds: u16,
        peers: u16,
        withhold: Option<(u8, bool)>,
    },
    CmpctBlock {
        peer: u8,
        which: Choice,
        nonce: u64,
        prefilled: u64,
    },
    BlockTxn {
        peer: u8,
        source: u8,
        index: u16,
    },
    GetBlockTxn {
        peer: u8,
        depth: u8,
        indexes: u64,
    },
    SendCmpct {
        peer: u8,
        high: bool,
        version: u8,
    },
    Inv {
        peer: u8,
        index: u16,
    },
    Pong {
        peer: u8,
        wrong: bool,
    },
    Tick,
}

/// Decodes at most 48 reactive operations.
/// Biased aliases favor short clock advances, contiguous headers and the IBD window.
pub fn decode(bytes: &[u8]) -> Option<(ScriptHeader, Vec<Op>)> {
    let mut r = Reader(bytes);
    let seed = r.u64().ok()?;
    let profile = r.byte().ok()? % 8;
    let n = 2 + r.byte().ok()? % 9;
    let peers =
        (0..n).map(|_| Some([r.byte().ok()?, r.byte().ok()?])).collect::<Option<Vec<_>>>()?;
    let header = ScriptHeader {
        seed,
        profile,
        peers,
    };
    let mut ops = Vec::new();
    while ops.len() < 48 && !r.0.is_empty() {
        let parsed = (|| {
            let b = r.byte()?;
            let arg = b >> 4;
            let choice = |r: &mut Reader<'_>| -> Result<Choice, &'static str> {
                Ok(Choice {
                    mode: (arg & 7) % 5,
                    index: r.u16()?,
                })
            };
            Ok::<_, &'static str>(match b & 15 {
                0 | 1 => Op::Pass {
                    class: if arg < 5 {
                        arg
                    } else {
                        0
                    },
                    peer: r.byte()? % n,
                    delta: r.u16()?,
                },
                2 => {
                    let peer = r.byte()? % n;
                    let from = Choice {
                        mode: if arg < 3 {
                            arg
                        } else {
                            0
                        },
                        index: r.u16()?,
                    };
                    let v = r.u16()?;
                    let count = if (1..=2000).contains(&v) {
                        v
                    } else {
                        match v % 7 {
                            0 => 1,
                            1 => 2 + v / 7 % 7,
                            2 => 16,
                            3 => 17,
                            4 => 18,
                            _ => 1023 + v % 3,
                        }
                    };
                    Op::Headers {
                        peer,
                        from,
                        count,
                    }
                }
                3 => Op::NextHeader {
                    peer: r.byte()? % n,
                },
                4 => Op::Block {
                    peer: r.byte()? % n,
                    which: choice(&mut r)?,
                },
                5 => Op::BadBlock {
                    peer: r.byte()? % n,
                    which: choice(&mut r)?,
                    variant: match r.byte()? % 3 {
                        0 => BadVariant::TrailingPair,
                        1 => BadVariant::MerkleRoot,
                        _ => BadVariant::DuplicateTransaction,
                    },
                },
                6 => {
                    let peer = r.byte()?;
                    let rounds = if profile == 0 {
                        128 + u16::from(r.byte()?)
                    } else {
                        1 + u16::from(r.byte()? % 255)
                    };
                    let mask = r.u16()? & ((1 << n) - 1);
                    let window = profile == 0 && arg >= 4;
                    Op::Serve {
                        rounds,
                        peers: if window {
                            (1 << n) - 1
                        } else {
                            mask
                        },
                        withhold: if window {
                            Some((peer % n, false))
                        } else {
                            (arg & 1 != 0).then_some((peer % n, arg & 2 != 0))
                        },
                    }
                }
                7 | 8 => Op::CmpctBlock {
                    peer: r.byte()? % n,
                    which: choice(&mut r)?,
                    nonce: r.u64()?,
                    prefilled: {
                        let omit_coinbase = r.byte()? % 8 == 0;
                        let v = r.u64()?;
                        if !omit_coinbase {
                            v | 1
                        } else {
                            v & !1
                        }
                    },
                },
                9 | 10 => Op::BlockTxn {
                    peer: r.byte()? % n,
                    source: arg % 4,
                    index: r.u16()?,
                },
                11 => Op::GetBlockTxn {
                    peer: r.byte()? % n,
                    depth: arg % 13,
                    indexes: r.u64()?,
                },
                12 => Op::SendCmpct {
                    peer: r.byte()? % n,
                    high: arg & 1 != 0,
                    version: 1 + (arg >> 1 & 1),
                },
                13 => Op::Inv {
                    peer: r.byte()? % n,
                    index: r.u16()?,
                },
                14 => Op::Pong {
                    peer: r.byte()? % n,
                    wrong: arg & 1 != 0,
                },
                _ => Op::Tick,
            })
        })();
        match parsed {
            Ok(op) => ops.push(op),
            Err(_) => break,
        }
    }
    (!ops.is_empty()).then_some((header, ops))
}

/// Encodes the exact operations accepted by the decoder.
pub fn encode(header: &ScriptHeader, ops: &[Op]) -> Result<Vec<u8>, &'static str> {
    if !(2..=10).contains(&header.peers.len())
        || header.profile > 7
        || ops.is_empty()
        || ops.len() > 48
    {
        return Err("script header");
    }
    let mut out = header.seed.to_le_bytes().to_vec();
    out.extend([header.profile, header.peers.len() as u8 - 2]);
    for peer in &header.peers {
        out.extend(peer);
    }
    for op in ops {
        let valid = match op {
            Op::Pass {
                class,
                peer,
                ..
            } => *class < 5 && (*peer as usize) < header.peers.len(),
            Op::Headers {
                peer,
                from,
                count,
            } => {
                (*peer as usize) < header.peers.len() && from.mode < 3 && (1..=2000).contains(count)
            }
            Op::NextHeader {
                peer,
            }
            | Op::Inv {
                peer,
                ..
            }
            | Op::Pong {
                peer,
                ..
            } => (*peer as usize) < header.peers.len(),
            Op::Block {
                peer,
                which,
            }
            | Op::BadBlock {
                peer,
                which,
                ..
            }
            | Op::CmpctBlock {
                peer,
                which,
                ..
            } => (*peer as usize) < header.peers.len() && which.mode < 5,
            Op::Serve {
                peers,
                withhold,
                ..
            } => {
                *peers < 1 << header.peers.len()
                    && withhold.is_none_or(|(peer, _)| (peer as usize) < header.peers.len())
            }
            Op::BlockTxn {
                peer,
                source,
                ..
            } => (*peer as usize) < header.peers.len() && *source < 4,
            Op::GetBlockTxn {
                peer,
                depth,
                ..
            } => (*peer as usize) < header.peers.len() && *depth <= 12,
            Op::SendCmpct {
                peer,
                version,
                ..
            } => (*peer as usize) < header.peers.len() && (1..=2).contains(version),
            Op::Tick => true,
        };
        if !valid {
            return Err("unrepresentable operation");
        }
        match op {
            Op::Pass {
                class,
                peer,
                delta,
            } => {
                out.extend([class << 4, *peer]);
                out.extend(delta.to_le_bytes());
            }
            Op::Headers {
                peer,
                from,
                count,
            } => {
                if !(1..=2000).contains(count) || from.mode > 2 {
                    return Err("header count");
                }
                out.extend([2 | from.mode << 4, *peer]);
                out.extend(from.index.to_le_bytes());
                out.extend(count.to_le_bytes());
            }
            Op::NextHeader {
                peer,
            } => out.extend([3, *peer]),
            Op::Block {
                peer,
                which,
            } => {
                out.extend([4 | (which.mode << 4), *peer]);
                out.extend(which.index.to_le_bytes());
            }
            Op::BadBlock {
                peer,
                which,
                variant,
            } => {
                out.extend([5 | (which.mode << 4), *peer]);
                out.extend(which.index.to_le_bytes());
                out.push(*variant as u8);
            }
            Op::Serve {
                rounds,
                peers,
                withhold,
            } => {
                let base = if header.profile == 0 {
                    128
                } else {
                    1
                };
                let r = rounds
                    .checked_sub(base)
                    .filter(|r| {
                        *r <= if header.profile == 0 {
                            255
                        } else {
                            254
                        }
                    })
                    .ok_or("serve rounds")?;
                let (peer, arg) =
                    withhold.map_or((0, 0), |(peer, all)| (peer, 1 | (u8::from(all) << 1)));
                out.extend([6 | arg << 4, peer, r as u8]);
                out.extend(peers.to_le_bytes());
            }
            Op::CmpctBlock {
                peer,
                which,
                nonce,
                prefilled,
            } => {
                out.extend([7 | which.mode << 4, *peer]);
                out.extend(which.index.to_le_bytes());
                out.extend(nonce.to_le_bytes());
                out.push(if prefilled & 1 == 0 {
                    0
                } else {
                    1
                });
                out.extend(prefilled.to_le_bytes());
            }
            Op::BlockTxn {
                peer,
                source,
                index,
            } => {
                out.extend([9 | source << 4, *peer]);
                out.extend(index.to_le_bytes());
            }
            Op::GetBlockTxn {
                peer,
                depth,
                indexes,
            } => {
                out.extend([11 | depth << 4, *peer]);
                out.extend(indexes.to_le_bytes());
            }
            Op::SendCmpct {
                peer,
                high,
                version,
            } => out.extend([12 | (u8::from(*high) | (version - 1) << 1) << 4, *peer]),
            Op::Inv {
                peer,
                index,
            } => {
                out.extend([13, *peer]);
                out.extend(index.to_le_bytes());
            }
            Op::Pong {
                peer,
                wrong,
            } => out.extend([14 | u8::from(*wrong) << 4, *peer]),
            Op::Tick => out.push(15),
        }
    }
    Ok(out)
}

#[derive(Clone, Debug)]
pub struct Script {
    pub header: ScriptHeader,
    pub ops: Vec<Op>,
}

impl StepSource for Script {
    fn start(&self) -> Box<dyn StepResolver> {
        let blocks = fixture().map(|f| f.chain(self.header.profile)).unwrap_or_default();
        Box::new(Resolver {
            script: self.clone(),
            blocks,
            pending: VecDeque::new(),
            op: 0,
            preamble: false,
            now: SIM_START,
            delivered: HashSet::new(),
            announced: HashSet::new(),
            last: HashMap::new(),
            answer: None,
            prefilled: HashMap::new(),
            serving: None,
            steps: 0,
            blocks_sent: 0,
        })
    }
    fn bad_connects(&self) -> Vec<BlockHash> {
        bad_variants()
            .map(|variants| {
                variants.iter().filter_map(|v| v[2].as_ref().map(|b| b.hash())).collect()
            })
            .unwrap_or_default()
    }
}

impl Script {
    pub fn session(&self) -> Result<Session, &'static str> {
        let f = fixture()?;
        let peers = self
            .header
            .peers
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let kind = p[0] % 4;
                let conn = if self.header.profile == 2 {
                    if kind < 3 {
                        ConnType::Inbound
                    } else {
                        ConnType::OutboundFullRelay
                    }
                } else {
                    match kind {
                        0 | 1 => ConnType::Inbound,
                        2 => ConnType::OutboundFullRelay,
                        _ => ConnType::BlockRelay,
                    }
                };
                PeerSpec {
                    conn,
                    addr: Service::new(NetAddr::ipv4([1, 2, 3, i as u8]), 19899),
                    send_mode: SendMode::Eager,
                }
            })
            .collect();
        Ok(Session {
            seed: self.header.seed,
            our_services: 1 | 4 | 1024,
            mocktime: SIM_START,
            check_block_index: false,
            peers,
            steps: Vec::new(),
            blocks: if self.header.profile >= 2 {
                f.prefix.iter().map(|b| core_serialize(&**b)).collect()
            } else {
                Vec::new()
            },
            max_tip_age: None,
            addr_relay: Some(node_address_relay),
            block_download: true,
            min_chain_work: (self.header.profile == 2).then_some([0xff; 32]),
            ops: Some(Arc::new(self.clone())),
        })
    }
}

struct Resolver {
    script: Script,
    blocks: Vec<Arc<Block>>,
    pending: VecDeque<Step>,
    op: usize,
    preamble: bool,
    now: i64,
    delivered: HashSet<(usize, BlockHash)>,
    announced: HashSet<BlockHash>,
    last: HashMap<(usize, String), Vec<u8>>,
    answer: Option<Vec<u8>>,
    prefilled: HashMap<BlockHash, Vec<usize>>,
    serving: Option<Serving>,
    steps: usize,
    blocks_sent: usize,
}

impl Resolver {
    fn preamble(&mut self) {
        for round in 0..5 {
            for (peer, [c, a]) in self.script.header.peers.iter().copied().enumerate() {
                let message = match round {
                    0 => {
                        let services = 1
                            | 4
                            | 1024
                            | if c & 64 != 0 {
                                2048
                            } else {
                                0
                            };
                        let services = if c & 128 != 0 {
                            services & !1
                        } else {
                            services
                        };
                        Some((
                            "version",
                            PeerVersionMsg {
                                version: 70242,
                                services,
                                ..PeerVersionMsg::v24(SIM_START)
                            }
                            .payload(),
                        ))
                    }
                    1 => (a >> 1 & 7 != 0).then_some(("verack", Vec::new())),
                    2 => match c >> 2 & 3 {
                        0 => None,
                        2 => Some(("sendheaders2", Vec::new())),
                        _ => Some(("sendheaders", Vec::new())),
                    },
                    3 => {
                        let compact = c >> 4 & 3;
                        (compact != 0).then(|| ("sendcmpct", sendcmpct(compact > 1, 1)))
                    }
                    _ => {
                        let n = if self.script.header.profile == 0 {
                            1100
                        } else if self.script.header.profile == 1 {
                            0
                        } else {
                            100
                        };
                        (a & 1 != 0 && n != 0).then(|| {
                            self.announced.extend(self.blocks[..n].iter().map(|b| b.hash()));
                            ("headers", headers(&self.blocks[..n]))
                        })
                    }
                };
                if let Some((command, payload)) = message {
                    self.pending.push_back(deliver(peer, command, payload));
                }
            }
            self.pending.push_back(Step::Pass(self.now));
        }
    }

    fn index(&self, hash: &[u8; 32]) -> Option<usize> {
        self.blocks.iter().position(|b| b.hash().as_bytes() == hash)
    }

    fn select(&self, view: Option<&DownloadView>, peer: usize, which: Choice) -> Option<usize> {
        let v = view;
        match which.mode % 5 {
            0 => {
                let q = &v?.peers.get(peer)?.as_ref()?.blocks_in_flight;
                self.index(&q.get(which.index as usize % q.len().max(1))?.0.0)
            }
            1 => {
                let q = &v?.globals.blocks_in_flight;
                let other: Vec<_> = q.iter().filter(|(_, p)| *p != peer as i64).collect();
                self.index(&other.get(which.index as usize % other.len().max(1))?.0)
            }
            2 => (!self.blocks.is_empty()).then_some(which.index as usize % self.blocks.len()),
            3 => {
                let tip = self.index(&v?.tip.0).map_or(0, |i| i + 1);
                Some((tip + which.index as usize % 2).min(self.blocks.len() - 1))
            }
            _ => self
                .blocks
                .iter()
                .enumerate()
                .find(|(_, b)| {
                    self.announced.contains(&b.hash())
                        && !self.delivered.iter().any(|(_, h)| *h == b.hash())
                        && v.is_some_and(|v| {
                            let mut height = 1;
                            let mut prev = b.header().raw().prev_hash();
                            while let Some(parent) = self.blocks.iter().find(|p| p.hash() == prev) {
                                height += 1;
                                prev = parent.header().raw().prev_hash();
                            }
                            height > v.tip.1
                                && !v
                                    .globals
                                    .blocks_in_flight
                                    .iter()
                                    .any(|(h, _)| h == b.hash().as_bytes())
                        })
                })
                .map(|(i, _)| i),
        }
    }

    fn queue_block(&mut self, peer: usize, index: usize) {
        let block = &self.blocks[index];
        self.delivered.insert((peer, block.hash()));
        self.pending.push_back(deliver(peer, "block", core_serialize(&**block)));
        self.blocks_sent += 1;
    }

    fn pong(&mut self, peer: usize, wrong: bool) -> Option<Step> {
        let key = (peer, "ping".to_owned());
        let mut payload = if wrong {
            self.last.get(&key)?.clone()
        } else {
            self.last.remove(&key)?
        };
        if wrong {
            *payload.first_mut()? ^= 1;
        }
        Some(deliver(peer, "pong", payload))
    }

    fn serve(&mut self, view: Option<&DownloadView>, peers: u16, withhold: Option<(u8, bool)>) {
        let Some(view) = view else {
            self.pending.push_back(Step::Pass(self.now));
            return;
        };
        let mut batch = Vec::new();
        for (peer, state) in view.peers.iter().enumerate() {
            if peers & (1 << peer) == 0 {
                continue;
            }
            if let Some(state) = state {
                for (front, (block, _)) in state.blocks_in_flight.iter().enumerate() {
                    if withhold.is_some_and(|(p, all)| p as usize == peer && (all || front == 0)) {
                        continue;
                    }
                    if let Some(index) = self.index(&block.0)
                        && self.delivered.insert((peer, self.blocks[index].hash()))
                    {
                        batch.push(deliver(peer, "block", core_serialize(&*self.blocks[index])));
                        self.blocks_sent += 1;
                    }
                }
            }
            if let Some(pong) = self.pong(peer, false) {
                batch.push(pong);
            }
        }
        self.pending.push_back(Step::Batch(batch));
        self.pending.push_back(Step::Pass(self.now));
    }

    fn resolve(&mut self, view: Option<&DownloadView>, op: Op) {
        match op {
            Op::Pass {
                class,
                peer,
                delta,
            } => {
                let peer = peer as usize;
                let edge = match class % 5 {
                    0 => self.now,
                    1 => self.now + 1 + i64::from(delta % 3),
                    2 => view
                        .and_then(|v| {
                            v.peers
                                .iter()
                                .flatten()
                                .filter(|p| p.stalling_since_us != 0)
                                .map(|p| {
                                    p.stalling_since_us / 1000000
                                        + v.globals.block_stalling_timeout_s
                                        + i64::from(delta & 1)
                                })
                                .min()
                        })
                        .unwrap_or(self.now + 1),
                    3 => view
                        .and_then(|v| {
                            let state = v.peers.get(peer)?.as_ref()?;
                            (state.downloading_since_us != 0).then_some(
                                state.downloading_since_us / 1000000
                                    + 150
                                    + 75 * i64::from(v.globals.peers_downloading_from - 1)
                                    + i64::from(delta & 1),
                            )
                        })
                        .unwrap_or(self.now + 1),
                    _ => self.now + i64::from(delta),
                };
                self.now = self.now.max(edge);
                self.pending.push_back(Step::Pass(self.now));
            }
            Op::Headers {
                peer,
                from,
                count,
            } => {
                let start = match from.mode % 3 {
                    0 => view
                        .and_then(|v| v.best_known.get(peer as usize))
                        .and_then(|b| self.index(&b.0))
                        .map_or(0, |i| i + 1),
                    1 => from.index as usize,
                    _ => view.and_then(|v| self.index(&v.tip.0)).map_or(1, |i| i + 2),
                };
                let start = start.min(self.blocks.len());
                let end = (start + count as usize).min(self.blocks.len());
                for b in &self.blocks[start..end] {
                    self.announced.insert(b.hash());
                }
                self.pending.push_back(deliver(
                    peer as usize,
                    "headers",
                    headers(&self.blocks[start..end]),
                ));
            }
            Op::NextHeader {
                peer,
            } => {
                let start = view
                    .and_then(|v| self.index(&v.tip.0))
                    .map_or(0, |i| i + 1)
                    .min(self.blocks.len() - 1);
                self.announced.insert(self.blocks[start].hash());
                self.pending.push_back(deliver(
                    peer as usize,
                    "headers",
                    headers(&self.blocks[start..start + 1]),
                ));
            }
            Op::Block {
                peer,
                which,
            } => {
                if let Some(index) = self.select(view, peer as usize, which) {
                    self.queue_block(peer as usize, index);
                }
            }
            Op::BadBlock {
                peer,
                which,
                variant,
            } => {
                let start =
                    self.select(view, peer as usize, which).unwrap_or(100).saturating_sub(100);
                if let Ok(variants) = bad_variants() {
                    for v in variants.iter().cycle().skip(start % 24).take(24) {
                        if let Some(block) = &v[variant as usize] {
                            self.pending.push_back(deliver(
                                peer as usize,
                                "block",
                                core_serialize(&**block),
                            ));
                            self.blocks_sent += 1;
                            break;
                        }
                    }
                }
            }
            Op::Serve {
                rounds,
                peers,
                withhold,
            } => {
                self.serving = Some((rounds, peers, withhold));
            }
            Op::CmpctBlock {
                peer,
                which,
                nonce,
                prefilled,
            } => {
                let index = match which.mode {
                    0 | 1 => self.select(
                        view,
                        peer as usize,
                        Choice {
                            mode: 3,
                            index: u16::from(which.mode),
                        },
                    ),
                    3 => {
                        self.last.get(&(peer as usize, "getdata".to_owned())).and_then(|payload| {
                            payload.get(1..)?.as_chunks::<36>().0.iter().rev().find_map(|inv| {
                                (inv[..4] == [4, 0, 0, 0])
                                    .then(|| self.index(&inv[4..].try_into().ok()?))
                                    .flatten()
                            })
                        })
                    }
                    _ => self.select(view, peer as usize, which),
                };
                if let Some(index) = index {
                    let block = &self.blocks[index];
                    let prefilled: Vec<_> =
                        (0..block.txs().len()).filter(|i| prefilled & (1 << i) != 0).collect();
                    if let Ok(payload) = honest_cmpctblock(block, nonce, &prefilled) {
                        self.prefilled.insert(block.hash(), prefilled);
                        self.delivered.insert((peer as usize, block.hash()));
                        self.pending.push_back(deliver(peer as usize, "cmpctblock", payload));
                        self.blocks_sent += 1;
                    }
                }
            }
            Op::BlockTxn {
                peer,
                source,
                index,
            } => {
                let source_peer = if source == 1 {
                    (peer as usize + 1) % self.script.header.peers.len()
                } else {
                    peer as usize
                };
                let payload = if source == 2 {
                    self.answer.clone()
                } else if source == 3 {
                    self.blocks.get(index as usize % self.blocks.len()).map(|b| {
                        let prefilled =
                            self.prefilled.get(&b.hash()).map_or(&[][..], Vec::as_slice);
                        blocktxn(
                            b,
                            &(0..b.txs().len())
                                .filter(|i| !prefilled.contains(i))
                                .collect::<Vec<_>>(),
                        )
                    })
                } else {
                    self.last
                        .get(&(source_peer, "getblocktxn".to_owned()))
                        .and_then(|p| self.answer_request(p))
                };
                if let Some(payload) = payload {
                    self.answer = Some(payload.clone());
                    self.pending.push_back(deliver(peer as usize, "blocktxn", payload));
                    self.blocks_sent += 1;
                }
            }
            Op::GetBlockTxn {
                peer,
                depth,
                indexes,
            } => {
                if let Some(view) = view {
                    let mut hash = view.tip.0;
                    let mut block = self.index(&hash).map(|i| &self.blocks[i]);
                    for _ in 0..depth {
                        let Some(parent) = block else {
                            break;
                        };
                        hash = *parent.header().raw().prev_hash().as_bytes();
                        block = self.index(&hash).map(|i| &self.blocks[i]);
                    }
                    let count = block.map_or(1, |b| b.txs().len());
                    let indexes: Vec<_> = (0..count).filter(|i| indexes & (1 << i) != 0).collect();
                    self.pending.push_back(deliver(
                        peer as usize,
                        "getblocktxn",
                        getblocktxn(hash, &indexes),
                    ));
                }
            }
            Op::SendCmpct {
                peer,
                high,
                version,
            } => self.pending.push_back(deliver(
                peer as usize,
                "sendcmpct",
                sendcmpct(high, version),
            )),
            Op::Inv {
                peer,
                index,
            } => {
                let block = &self.blocks[index as usize % self.blocks.len()];
                let mut payload = vec![1, 2, 0, 0, 0];
                payload.extend(block.hash().as_bytes());
                self.pending.push_back(deliver(peer as usize, "inv", payload));
            }
            Op::Pong {
                peer,
                wrong,
            } => {
                if let Some(step) = self.pong(peer as usize, wrong) {
                    self.pending.push_back(step);
                }
            }
            Op::Tick => self.pending.push_back(Step::Tick(self.now, TICK_MAINTENANCE)),
        }
    }

    fn answer_request(&self, payload: &[u8]) -> Option<Vec<u8>> {
        let (hash, indexes) = request_indexes(payload)?;
        let block = &self.blocks[self.index(&hash)?];
        indexes.iter().all(|i| *i < block.txs().len()).then(|| blocktxn(block, &indexes))
    }
}

impl StepResolver for Resolver {
    fn next(&mut self, previous: Option<&DownloadView>) -> Option<Step> {
        if let Some(view) = previous {
            for (peer, messages) in view.received.iter().enumerate() {
                for (command, payload) in messages {
                    self.last.insert((peer, command.clone()), payload.clone());
                }
            }
        }
        if !self.preamble {
            self.preamble = true;
            self.preamble();
        }
        loop {
            if self.steps >= 1200 || self.blocks_sent >= 1400 {
                return None;
            }
            if let Some(step) = self.pending.pop_front() {
                self.steps += 1;
                return Some(step);
            }
            if let Some((rounds, peers, withhold)) = self.serving.take()
                && rounds != 0
            {
                self.serve(previous, peers, withhold);
                self.serving = Some((rounds - 1, peers, withhold));
                continue;
            }
            let op = self.script.ops.get(self.op)?.clone();
            self.op += 1;
            self.resolve(previous, op);
        }
    }
}

pub fn sendcmpct(high: bool, version: u8) -> Vec<u8> {
    let mut out = vec![u8::from(high)];
    out.extend(u64::from(version).to_le_bytes());
    out
}

pub fn getblocktxn(hash: [u8; 32], indexes: &[usize]) -> Vec<u8> {
    let mut out = hash.to_vec();
    compact(indexes.len(), &mut out);
    let mut next = 0;
    for &index in indexes {
        compact(index - next, &mut out);
        next = index + 1;
    }
    out
}

pub fn request_indexes(payload: &[u8]) -> Option<([u8; 32], Vec<usize>)> {
    let mut r = Reader(payload);
    let hash = r.take(32).ok()?.try_into().ok()?;
    let n = r.byte().ok()?;
    if n >= 253 {
        return None;
    }
    let mut next = 0;
    let mut indexes = Vec::new();
    for _ in 0..n {
        next += r.byte().ok()? as usize;
        indexes.push(next);
        next += 1;
    }
    Some((hash, indexes))
}

pub fn blocktxn(block: &Block, indexes: &[usize]) -> Vec<u8> {
    let mut out = block.hash().as_bytes().to_vec();
    compact(indexes.len(), &mut out);
    for &index in indexes {
        out.extend(core_serialize(&*block.txs()[index]));
    }
    out
}

pub fn compare_p2p_multi_blocks(bytes: &[u8], pin: Pin) -> Compared {
    let Some((header, ops)) = decode(bytes) else {
        return Ok(Outcome::OutOfDomain);
    };
    let session = Script {
        header,
        ops,
    }
    .session()
    .map_err(divergence)?;
    compare_session(&Oracle::new(pin), &session)
}
