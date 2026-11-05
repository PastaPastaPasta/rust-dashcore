//! A mainnet window of blocks for the build's benches (EV2b D29; EV2 BG `crit:dml_build_w1`):
//! the headers and special transactions as mined, every other transaction reduced to what the
//! build reads of it. The build skips the coinbase and reads an ordinary transaction only in
//! step 4, which looks up each input's prevout as a collateral. So an ordinary transaction keeps
//! its input count and the prevouts that spend a masternode's collateral; its other prevouts
//! are synthetic and never match one. The window builds the mainnet lists at a fraction of the
//! blocks' size; its generator (`examples/dml_window_fixture.rs`) checks that every list equals
//! the real blocks' one and Core's.
//!
//! A `dmlwin01` window starts at the default list before DIP3 and names no quorum. A `dmlwin02`
//! one (EV2b D32, w2) starts at a supplied list and carries the members of the quorums its
//! commitments name, as their proTxHashes: the build reads nothing else of a member (Core's
//! `HandleQuorumCommitment` reads `proTxHash` too), so each member is a placeholder masternode
//! with that proTxHash.
//!
//! The format, little-endian, with Core's CompactSize for counts: the magic; the parent's
//! height (i32) and the block count (u32); for `dmlwin02` the count of the headers before the
//! parent's and those headers (back to the oldest quorum base block a commitment names); the
//! parent's header; for `dmlwin02` the start list (its length and its bytes as evodb stores
//! it), the count of quorums and each one's llmqType (u8), quorum hash, member count and
//! members' proTxHashes; then per block its header, the count of its transactions after the
//! coinbase, and each as `0`, its length and its bytes, or as `1`, its input count, the count
//! of kept inputs and each kept input's position and prevout (txid, then index as u32).
//!
//! The rules are mainnet's with V24 inactive (`v24: false`), which the format does not record:
//! a window past V24's activation needs it added (and a new magic).

use std::sync::Arc;

use dash_consensus::primitives::{Block, decode_block, read_compact_size, write_compact_size};
use dash_consensus::{BlockId, ChainAncestry};
use dash_evo::llmq::QuorumMembers;
use dash_primitives::{BlockHash, OutPoint};

use super::replay::{CORE_NET_INFO, Headers, Members, mainnet, payee_rules};
use super::{dmn, list_disk_bytes};
use crate::codec::core_disk;
use crate::netinfo::NetInfoRules;
use crate::reference::{RefMnList, build_new_list_from_block_ref};
use crate::{
    DmlBuildError, DmlParams, DmlRules, DmlTrace, Dmn, MnList, MnType, NoCoins, NoTrace, PrevBlock,
    ProTxHash, build_new_list_from_block,
};

const MAGIC: &[u8; 8] = b"dmlwin01";
const MAGIC_FROM_LIST: &[u8; 8] = b"dmlwin02";

/// A transaction of a window block after the coinbase.
#[derive(Debug)]
pub enum WindowTx {
    /// As mined.
    Raw(Vec<u8>),
    /// An ordinary transaction's shape: its input count, and the inputs (position, prevout)
    /// that keep their prevout.
    Shape {
        inputs: u32,
        kept: Vec<(u32, OutPoint)>,
    },
}

/// A window block: its header and its transactions after the coinbase.
#[derive(Debug)]
pub struct WindowBlock {
    pub header: [u8; 80],
    pub txs: Vec<WindowTx>,
}

/// Where a `dmlwin02` window starts.
#[derive(Debug, Default)]
pub struct WindowStart {
    /// The headers before the parent's, consecutive.
    pub prefix: Vec<[u8; 80]>,
    /// The list after the parent block, as evodb stores it.
    pub list: Vec<u8>,
    /// Each quorum a commitment names: llmqType, quorum hash and members' proTxHashes in
    /// Core's order.
    pub quorums: Vec<(u8, [u8; 32], Vec<ProTxHash>)>,
}

/// The window's file bytes: `blocks` after `parent`, at `parent_height`, from the default list
/// (`dmlwin01`) or from `start` (`dmlwin02`).
pub fn encode(
    parent_height: i32,
    parent: &[u8; 80],
    start: Option<&WindowStart>,
    blocks: &[WindowBlock],
) -> Vec<u8> {
    let magic = if start.is_some() {
        MAGIC_FROM_LIST
    } else {
        MAGIC
    };
    let mut out = magic.to_vec();
    out.extend(parent_height.to_le_bytes());
    out.extend((blocks.len() as u32).to_le_bytes());
    if let Some(start) = start {
        write_compact_size(start.prefix.len() as u64, &mut out);
        out.extend(start.prefix.iter().flatten());
    }
    out.extend(parent);
    if let Some(start) = start {
        write_compact_size(start.list.len() as u64, &mut out);
        out.extend(&start.list);
        write_compact_size(start.quorums.len() as u64, &mut out);
        for (llmq_type, quorum_hash, members) in &start.quorums {
            out.push(*llmq_type);
            out.extend(quorum_hash);
            write_compact_size(members.len() as u64, &mut out);
            for member in members {
                out.extend(member.as_bytes());
            }
        }
    }
    for block in blocks {
        out.extend(block.header);
        write_compact_size(block.txs.len() as u64, &mut out);
        for tx in &block.txs {
            match tx {
                WindowTx::Raw(bytes) => {
                    out.push(0);
                    write_compact_size(bytes.len() as u64, &mut out);
                    out.extend(bytes);
                }
                WindowTx::Shape {
                    inputs,
                    kept,
                } => {
                    out.push(1);
                    write_compact_size(u64::from(*inputs), &mut out);
                    write_compact_size(kept.len() as u64, &mut out);
                    for (at, prevout) in kept {
                        write_compact_size(u64::from(*at), &mut out);
                        out.extend(prevout.hash.as_bytes());
                        out.extend(prevout.index.to_le_bytes());
                    }
                }
            }
        }
    }
    out
}

/// A window read back as mainnet blocks, with what the build needs around them.
pub struct Window {
    parent_height: i32,
    /// The headers before the parent's, the parent's, then each block's.
    headers: Headers,
    /// The parent header's index in `headers`.
    parent_at: usize,
    start: MnList,
    /// `start` as evodb stores it, which the twin reads itself.
    start_disk: Vec<u8>,
    quorums: Vec<(u8, [u8; 32], QuorumMembers<Arc<Dmn>>)>,
    blocks: Vec<Block>,
    params: DmlParams,
    net_info: NetInfoRules,
}

impl Window {
    /// The window of `bytes`; `None` when they are not one. Each block gets a minimal coinbase,
    /// and each shape a transaction with its kept prevouts at their positions, synthetic
    /// prevouts elsewhere and one empty output.
    pub fn decode(bytes: &[u8]) -> Option<Window> {
        let mut r = Reader(bytes);
        let from_list = match r.take(8)? {
            m if m == MAGIC => false,
            m if m == MAGIC_FROM_LIST => true,
            _ => return None,
        };
        let parent_height = i32::from_le_bytes(r.array()?);
        let count = u32::from_le_bytes(r.array()?);
        let mut headers = Vec::new();
        if from_list {
            for _ in 0..r.compact()? {
                headers.push(r.array::<80>()?);
            }
        }
        let parent_at = headers.len();
        headers.push(r.array::<80>()?);
        let (start, start_disk, quorums) = if from_list {
            let len = usize::try_from(r.compact()?).ok()?;
            let disk = r.take(len)?;
            let mut list = disk;
            let start = core_disk::decode_list_s3(&mut list).ok()?;
            if !list.is_empty() {
                return None;
            }
            let placeholder = dmn(0, MnType::Regular);
            let mut quorums = Vec::new();
            for _ in 0..r.compact()? {
                let llmq_type = r.take(1)?[0];
                let quorum_hash = r.array()?;
                let members = (0..r.compact()?)
                    .map(|_| {
                        Some(Arc::new(Dmn {
                            pro_tx_hash: ProTxHash::from(r.array::<32>()?),
                            ..Dmn::clone(&placeholder)
                        }))
                    })
                    .collect::<Option<Vec<_>>>()?;
                quorums.push((llmq_type, quorum_hash, QuorumMembers::from(members)));
            }
            (start, disk.to_vec(), quorums)
        } else {
            let start = MnList::default_pre_dip3();
            let disk = list_disk_bytes(&start);
            (start, disk, Vec::new())
        };
        let mut blocks = Vec::with_capacity(count as usize);
        let mut synthetic = 0u64;
        for _ in 0..count {
            let header: [u8; 80] = r.array()?;
            let txs = r.compact()?;
            let mut raw = header.to_vec();
            write_compact_size(txs.checked_add(1)?, &mut raw);
            push_coinbase(&mut raw);
            for _ in 0..txs {
                match r.take(1)?[0] {
                    0 => {
                        let len = usize::try_from(r.compact()?).ok()?;
                        raw.extend(r.take(len)?);
                    }
                    1 => push_shape(&mut r, &mut synthetic, &mut raw)?,
                    _ => return None,
                }
            }
            blocks.push(decode_block(&mut &raw[..]).ok()?.into_value());
            headers.push(header);
        }
        if !r.0.is_empty() {
            return None;
        }
        let params = mainnet();
        let first_height = parent_height.checked_sub(i32::try_from(parent_at).ok()?)?;
        let chain = Headers::new(first_height, &headers)?;
        // Each header (the prefix's, the parent's, each block's) names the one before it.
        let linked = (1..headers.len())
            .all(|k| headers[k][4..36] == *chain.header(BlockId(k as u32 - 1)).hash.as_bytes());
        if !linked {
            return None;
        }
        Some(Window {
            parent_height,
            headers: chain,
            parent_at,
            start,
            start_disk,
            quorums,
            blocks,
            params: DmlParams::new(params),
            net_info: NetInfoRules::new(params, params, CORE_NET_INFO),
        })
    }

    /// The list the window starts at: the default list before DIP3 (`dmlwin01`) or the
    /// supplied one (`dmlwin02`).
    pub fn start(&self) -> &MnList {
        &self.start
    }

    /// The reference twin at the start list, read from its evodb bytes (not copied from
    /// [`Window::start`], so the twin's collision buckets are its own).
    pub fn twin_start(&self) -> RefMnList {
        RefMnList::decode_s3(&mut &self.start_disk[..]).expect("the start list decoded once")
    }

    /// The blocks, in height order.
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// Block `i`'s height.
    pub fn height(&self, i: usize) -> i32 {
        self.parent_height + 1 + i as i32
    }

    /// The hash of header `i` from the parent's: the parent's for 0, block `i - 1`'s after it.
    fn hash_at(&self, i: usize) -> BlockHash {
        self.headers.header(self.id(i)).hash
    }

    /// The id of header `i` from the parent's.
    fn id(&self, i: usize) -> BlockId {
        BlockId((self.parent_at + i) as u32)
    }

    /// Block `i`'s predecessor.
    fn prev(&self, i: usize) -> PrevBlock<'_> {
        PrevBlock {
            id: self.id(i),
            hash: self.hash_at(i),
            height: self.parent_height + i as i32,
            chain: &self.headers,
        }
    }

    /// The rules of block `i` (mainnet's, V24 inactive: module docs).
    fn rules(&self, i: usize) -> DmlRules<'_> {
        let buried = payee_rules(mainnet(), self.parent_height + i as i32);
        DmlRules {
            params: &self.params,
            net_info: &self.net_info,
            v19: buried.v19,
            mn_rr: buried.mn_rr,
            v24: false,
        }
    }

    /// The window's quorums: a commitment naming another one stops the build with an error.
    fn members(&self) -> Members<'_> {
        Members::from_members(&self.headers, self.quorums.clone())
    }

    /// Every block through `build_new_list_from_block` from `start`, each list's block hash
    /// set as `ProcessBlock` sets it; `each` sees each block's list.
    pub fn build(
        &self,
        start: MnList,
        trace: &mut dyn DmlTrace,
        mut each: impl FnMut(usize, &MnList),
    ) -> Result<MnList, DmlBuildError> {
        let members = self.members();
        let mut list = start;
        for (i, block) in self.blocks.iter().enumerate() {
            let built = build_new_list_from_block(
                block,
                self.prev(i),
                &list,
                &NoCoins,
                &members,
                &self.rules(i),
                &mut Vec::new(),
                trace,
            )?;
            list = built.list.with_block_hash(self.hash_at(i + 1));
            each(i, &list);
        }
        Ok(list)
    }

    /// [`Window::build`] through the reference twin.
    pub fn build_ref(&self, start: RefMnList) -> Result<RefMnList, DmlBuildError> {
        let members = self.members();
        let mut list = start;
        for (i, block) in self.blocks.iter().enumerate() {
            let built = build_new_list_from_block_ref(
                block,
                self.prev(i),
                &list,
                &NoCoins,
                &members,
                &self.rules(i),
                &mut Vec::new(),
                &mut NoTrace,
            )?;
            list = built.list.with_block_hash(self.hash_at(i + 1));
        }
        Ok(list)
    }
}

/// One empty output of 0, then lock time 0.
fn push_tail(raw: &mut Vec<u8>) {
    raw.push(1);
    raw.extend([0; 9]);
    raw.extend([0; 4]);
}

/// A version-1 coinbase: the null prevout, a two-byte scriptSig, one empty output of 0.
fn push_coinbase(raw: &mut Vec<u8>) {
    raw.extend(1i32.to_le_bytes());
    raw.push(1); // one input
    raw.extend([0; 32]);
    raw.extend(u32::MAX.to_le_bytes()); // the null prevout
    raw.extend([2, 1, 1]); // the scriptSig
    raw.extend(u32::MAX.to_le_bytes()); // the sequence
    push_tail(raw);
}

/// Reads a shape and appends the transaction it stands for.
fn push_shape(r: &mut Reader<'_>, synthetic: &mut u64, raw: &mut Vec<u8>) -> Option<()> {
    let inputs = r.compact()?;
    let mut kept = Vec::new();
    for _ in 0..r.compact()? {
        kept.push((r.compact()?, r.take(36)?));
    }
    raw.extend(1i32.to_le_bytes());
    write_compact_size(inputs, raw);
    let mut kept = kept.into_iter().peekable();
    for at in 0..inputs {
        match kept.next_if(|(k, _)| *k == at) {
            Some((_, prevout)) => raw.extend(prevout),
            None => {
                *synthetic += 1;
                for lane in 0..4 {
                    raw.extend(splitmix(*synthetic * 4 + lane).to_le_bytes());
                }
                raw.extend(0u32.to_le_bytes());
            }
        }
        raw.push(0); // an empty scriptSig
        raw.extend(u32::MAX.to_le_bytes()); // a final sequence
    }
    kept.next().is_none().then(|| push_tail(raw))
}

fn splitmix(n: u64) -> u64 {
    let mut z = n.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(head)
    }

    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }

    /// A CompactSize, as [`write_compact_size`] writes it (range-checked).
    fn compact(&mut self) -> Option<u64> {
        read_compact_size(&mut self.0, true).ok()
    }
}
