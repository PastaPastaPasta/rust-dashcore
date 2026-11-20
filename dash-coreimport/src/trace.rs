//! The UTXO churn trace: every coin created and spent by the best chain, in Core's connect order.
//!
//! Format `DSTRACE1` (all integers little-endian):
//! - header: `"DSTRACE1" ‖ network magic[4] ‖ u32 first height ‖ u32 last height ‖ anchor[32]`;
//! - per block: `u32 height ‖ u32 event count ‖ event*`, where an event is
//!   `0x00 ‖ txid[32] ‖ u32 vout` (spend) or `0x01 ‖ txid[32] ‖ u32 vout ‖ u16 len ‖ coin` (create);
//! - trailer: `u32 0xffffffff ‖ u64 spends ‖ u64 creates`.
//!
//! A coin is Core's `Coin` serialization ([`crate::coin`]).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::blockfiles::{BlockReader, HeaderIndex, RevCursors};
use crate::coin::{ScriptKind, coin_bytes, is_unspendable, read_block_undo};
use crate::error::{Error, Result};
use crate::tx::{Block, read_block_prefix};
use crate::{Hash256, hash_to_hex};

pub const MAGIC: &[u8; 8] = b"DSTRACE1";
const END: u32 = u32::MAX;

/// An outpoint as a fixed 36-byte key: `txid ‖ u32 LE vout`.
pub type OutKey = [u8; 36];

pub fn out_key(txid: &Hash256, vout: u32) -> OutKey {
    let mut k = [0u8; 36];
    k[..32].copy_from_slice(txid);
    k[32..].copy_from_slice(&vout.to_le_bytes());
    k
}

/// One trace event. A create's coin bytes live in the block's coin buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Spend(OutKey),
    Create {
        key: OutKey,
        coin_start: u32,
        coin_len: u16,
    },
}

/// One block of a trace.
#[derive(Debug, Default)]
pub struct TraceBlock {
    pub height: u32,
    pub events: Vec<Event>,
    pub coins: Vec<u8>,
}

impl TraceBlock {
    pub fn coin(&self, start: u32, len: u16) -> &[u8] {
        &self.coins[start as usize..start as usize + len as usize]
    }

    fn clear(&mut self, height: u32) {
        self.height = height;
        self.events.clear();
        self.coins.clear();
    }

    /// Appends a create event; returns where its coin is stored.
    fn push_create(&mut self, key: OutKey, coin: &[u8]) -> (u32, u16) {
        let at = (self.coins.len() as u32, coin.len() as u16);
        self.events.push(Event::Create {
            key,
            coin_start: at.0,
            coin_len: at.1,
        });
        self.coins.extend_from_slice(coin);
        at
    }
}

/// The header of a trace file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceHeader {
    pub network_magic: [u8; 4],
    pub first_height: u32,
    pub last_height: u32,
    pub anchor: Hash256,
}

pub struct TraceWriter<W: Write> {
    out: W,
    pub spends: u64,
    pub creates: u64,
}

impl<W: Write> TraceWriter<W> {
    pub fn new(mut out: W, header: &TraceHeader) -> std::io::Result<Self> {
        out.write_all(MAGIC)?;
        out.write_all(&header.network_magic)?;
        out.write_all(&header.first_height.to_le_bytes())?;
        out.write_all(&header.last_height.to_le_bytes())?;
        out.write_all(&header.anchor)?;
        Ok(Self {
            out,
            spends: 0,
            creates: 0,
        })
    }

    pub fn write_block(&mut self, block: &TraceBlock) -> std::io::Result<()> {
        self.out.write_all(&block.height.to_le_bytes())?;
        self.out.write_all(&(block.events.len() as u32).to_le_bytes())?;
        for e in &block.events {
            match e {
                Event::Spend(key) => {
                    self.out.write_all(&[0])?;
                    self.out.write_all(key)?;
                    self.spends += 1;
                }
                Event::Create {
                    key,
                    coin_start,
                    coin_len,
                } => {
                    self.out.write_all(&[1])?;
                    self.out.write_all(key)?;
                    self.out.write_all(&coin_len.to_le_bytes())?;
                    self.out.write_all(block.coin(*coin_start, *coin_len))?;
                    self.creates += 1;
                }
            }
        }
        Ok(())
    }

    pub fn finish(mut self) -> std::io::Result<W> {
        self.out.write_all(&END.to_le_bytes())?;
        self.out.write_all(&self.spends.to_le_bytes())?;
        self.out.write_all(&self.creates.to_le_bytes())?;
        self.out.flush()?;
        Ok(self.out)
    }
}

/// Streams a trace file block by block.
pub struct TraceReader<R: Read> {
    input: R,
    pub header: TraceHeader,
    done: bool,
    spends: u64,
    creates: u64,
}

fn read_exact<const N: usize>(r: &mut impl Read) -> Result<[u8; N]> {
    let mut buf = [0u8; N];
    r.read_exact(&mut buf).map_err(|e| Error::malformed("trace", e.to_string()))?;
    Ok(buf)
}

impl TraceReader<BufReader<File>> {
    pub fn open(path: &Path) -> Result<Self> {
        let f = File::open(path).map_err(|e| Error::io(path, e))?;
        Self::new(BufReader::with_capacity(4 << 20, f))
    }
}

impl<R: Read> TraceReader<R> {
    pub fn new(mut input: R) -> Result<Self> {
        if &read_exact::<8>(&mut input)? != MAGIC {
            return Err(Error::malformed("trace", "bad magic"));
        }
        let header = TraceHeader {
            network_magic: read_exact(&mut input)?,
            first_height: u32::from_le_bytes(read_exact(&mut input)?),
            last_height: u32::from_le_bytes(read_exact(&mut input)?),
            anchor: read_exact(&mut input)?,
        };
        Ok(Self {
            input,
            header,
            done: false,
            spends: 0,
            creates: 0,
        })
    }

    /// Reads the next block into `block`; `false` at the end (after the trailer checked out).
    pub fn next_block(&mut self, block: &mut TraceBlock) -> Result<bool> {
        if self.done {
            return Ok(false);
        }
        let height = u32::from_le_bytes(read_exact(&mut self.input)?);
        if height == END {
            let spends = u64::from_le_bytes(read_exact(&mut self.input)?);
            let creates = u64::from_le_bytes(read_exact(&mut self.input)?);
            if (spends, creates) != (self.spends, self.creates) {
                return Err(Error::malformed("trace", "trailer totals do not match the events"));
            }
            self.done = true;
            return Ok(false);
        }
        block.clear(height);
        let n = u32::from_le_bytes(read_exact(&mut self.input)?);
        for _ in 0..n {
            let tag = read_exact::<1>(&mut self.input)?[0];
            let key: OutKey = read_exact(&mut self.input)?;
            match tag {
                0 => {
                    block.events.push(Event::Spend(key));
                    self.spends += 1;
                }
                1 => {
                    let len = u16::from_le_bytes(read_exact(&mut self.input)?);
                    let start = block.coins.len();
                    block.coins.resize(start + len as usize, 0);
                    self.input
                        .read_exact(&mut block.coins[start..])
                        .map_err(|e| Error::malformed("trace", e.to_string()))?;
                    block.events.push(Event::Create {
                        key,
                        coin_start: start as u32,
                        coin_len: len,
                    });
                    self.creates += 1;
                }
                t => return Err(Error::malformed("trace", format!("event tag {t}"))),
            }
        }
        Ok(true)
    }
}

/// Inputs of [`extract`].
pub struct ExtractConfig {
    /// A `blocks/` directory (or a copy of its blk/rev files).
    pub blocks_dir: PathBuf,
    pub network_magic: [u8; 4],
    /// The hash of the last block of the trace (from the reference node).
    pub anchor: Hash256,
    pub output: PathBuf,
    /// Rev files kept in memory at once.
    pub rev_cache_files: usize,
}

/// What [`extract`] saw: the provenance and shape of the trace.
#[derive(Debug, Default)]
pub struct ExtractReport {
    pub last_height: u32,
    pub blk_files: usize,
    pub blocks_indexed: usize,
    pub txs: u64,
    pub spends: u64,
    pub creates: u64,
    /// Outputs Core does not add to the UTXO set (`IsUnspendable`).
    pub unspendable_outputs: u64,
    /// Undo records passed over because they belong to stale blocks.
    pub stale_undo_skipped: u64,
    pub blk_resync_bytes: usize,
    pub rev_resync_bytes: usize,
    pub live_coins: u64,
    pub live_bytes: u64,
    pub peak_live_coins: u64,
    pub peak_live_coins_height: u32,
    pub peak_live_bytes: u64,
    /// `(height, live coins, live coin bytes)` every 1,000 blocks and at the end.
    pub live_curve: Vec<(u32, u64, u64)>,
    /// Script kinds of spent coins, each checked byte-for-byte against Core's undo data.
    pub checked_spent_kinds: BTreeMap<ScriptKind, u64>,
    pub created_kinds: BTreeMap<ScriptKind, u64>,
    pub seconds: f64,
}

/// The coins a block spends and creates, computed against `utxo` without changing it.
struct Connect {
    events: TraceBlock,
    created_kinds: Vec<ScriptKind>,
    /// Kinds of the spent coins, each checked against the undo record.
    checked_kinds: Vec<ScriptKind>,
    unspendable: u64,
}

/// Connects `block` against `utxo` as Core would and checks every spent coin against `undo`.
/// Returns `None` when `undo` does not belong to this block (wrong shape or different coins);
/// an input missing from the UTXO set is an error.
fn connect(
    block: &Block,
    height: u32,
    undo: &[u8],
    utxo: &HashMap<OutKey, Box<[u8]>>,
) -> Result<Option<Connect>> {
    let Ok(undo) = read_block_undo(undo) else {
        return Ok(None);
    };
    if undo.len() + 1 != block.txs.len()
        || block.txs[1..].iter().zip(&undo).any(|(tx, u)| tx.inputs.len() != u.len())
    {
        return Ok(None);
    }
    let mut out = Connect {
        events: TraceBlock::default(),
        created_kinds: Vec::new(),
        checked_kinds: Vec::new(),
        unspendable: 0,
    };
    out.events.height = height;
    // Coins created and spent within this block, so `utxo` stays untouched.
    let mut created: HashMap<OutKey, (u32, u16)> = HashMap::new();
    let mut spent_from_utxo: HashSet<OutKey> = HashSet::new();
    let mut coin = Vec::with_capacity(64);
    for (i, tx) in block.txs.iter().enumerate() {
        if i > 0 {
            for (input, undo_coin) in tx.inputs.iter().zip(&undo[i - 1]) {
                let key = out_key(&input.txid, input.vout);
                let have: Option<&[u8]> = match created.remove(&key) {
                    Some((start, len)) => Some(out.events.coin(start, len)),
                    // A coin of the UTXO set, unless an earlier input of this block spent it.
                    None if spent_from_utxo.insert(key) => utxo.get(&key).map(|c| &c[..]),
                    None => None,
                };
                let Some(have) = have else {
                    return Err(Error::Inconsistent(format!(
                        "height {height}: input {}:{} spends a missing coin",
                        hash_to_hex(&input.txid),
                        input.vout
                    )));
                };
                if have != undo_coin.bytes.as_slice() {
                    return Ok(None);
                }
                out.checked_kinds.push(undo_coin.kind);
                out.events.events.push(Event::Spend(key));
            }
        }
        for (vout, output) in tx.outputs.iter().enumerate() {
            if is_unspendable(output.script) {
                out.unspendable += 1;
                continue;
            }
            coin.clear();
            let kind = coin_bytes(height, i == 0, output.value, output.script, &mut coin);
            let key = out_key(&tx.txid, vout as u32);
            if created.contains_key(&key)
                || (utxo.contains_key(&key) && !spent_from_utxo.contains(&key))
            {
                return Err(Error::Inconsistent(format!(
                    "height {height}: output {}:{vout} overwrites an unspent coin",
                    hash_to_hex(&tx.txid)
                )));
            }
            created.insert(key, out.events.push_create(key, &coin));
            out.created_kinds.push(kind);
        }
    }
    Ok(Some(out))
}

/// Parses a stored copy of the block `hash`, accepting it only if its header hashes to `hash` and
/// its merkle root matches its transactions. Bytes after the block within its record are
/// ignored, as Core's `ReadBlockFromDisk` ignores them.
fn intact_block<'a>(bytes: &'a [u8], hash: &Hash256) -> Result<Block<'a>> {
    let (block, _) = read_block_prefix(bytes)?;
    if crate::tx::block_hash(block.header) != *hash {
        return Err(Error::Inconsistent("header hash mismatch".into()));
    }
    if !block.merkle_root_matches() {
        return Err(Error::Inconsistent("merkle root mismatch".into()));
    }
    Ok(block)
}

/// Extracts the trace from genesis to `config.anchor`, checking every spend against Core's undo
/// data, and writes it to `config.output`. `progress` is called every 10,000 blocks.
pub fn extract(
    config: &ExtractConfig,
    mut progress: impl FnMut(u32, &ExtractReport),
) -> Result<ExtractReport> {
    let started = Instant::now();
    let index = HeaderIndex::build(&config.blocks_dir, config.network_magic)?;
    let chain = index.chain_to(&config.anchor)?;
    let last_height = (chain.len() - 1) as u32;
    let mut report = ExtractReport {
        last_height,
        blk_files: index.files.len(),
        blocks_indexed: index.blocks.len(),
        blk_resync_bytes: index.scan.resync_bytes,
        ..Default::default()
    };
    let mut reader = BlockReader::new(&index.files);
    let mut revs =
        RevCursors::new(&config.blocks_dir, config.network_magic, config.rev_cache_files);
    let file = File::create(&config.output).map_err(|e| Error::io(&config.output, e))?;
    let header = TraceHeader {
        network_magic: config.network_magic,
        first_height: 1,
        last_height,
        anchor: config.anchor,
    };
    let io_err = |e| Error::io(&config.output, e);
    let mut writer =
        TraceWriter::new(BufWriter::with_capacity(8 << 20, file), &header).map_err(io_err)?;
    let mut utxo: HashMap<OutKey, Box<[u8]>> = HashMap::new();
    let mut buf = Vec::new();

    // Core never connects the genesis block: its coinbase is not a coin.
    for (height, (hash, loc)) in chain.iter().enumerate().skip(1) {
        let height = height as u32;
        // The first copy is normally intact. A crash can leave an older copy whose header still
        // hashes to the block but whose body was overwritten; then take a later intact copy.
        reader.read(loc, &mut buf)?;
        let other_copy;
        let block = match intact_block(&buf, hash) {
            Ok(block) => block,
            Err(first_error) => {
                let mut found = None;
                for copy in index.copies(hash).skip(1) {
                    let mut bytes = Vec::new();
                    reader.read(copy, &mut bytes)?;
                    if intact_block(&bytes, hash).is_ok() {
                        found = Some(bytes);
                        break;
                    }
                }
                other_copy = found.ok_or_else(|| {
                    Error::Inconsistent(format!(
                        "height {height} ({}): no intact copy of the block: {first_error}",
                        hash_to_hex(hash)
                    ))
                })?;
                read_block_prefix(&other_copy)?.0
            }
        };
        // The undo record sits in the rev file of the copy Core connected; try each copy.
        let mut found = None;
        for copy in index.copies(hash) {
            let mut result = None;
            let skipped = revs.take(copy.file, &copy.prev, |undo| {
                match connect(&block, height, undo, &utxo) {
                    Ok(None) => false,
                    r => {
                        result = Some(r);
                        true
                    }
                }
            })?;
            if let (Some(skipped), Some(result)) = (skipped, result) {
                found = Some((skipped, result?.expect("accepted")));
                break;
            }
        }
        let Some((skipped, connected)) = found else {
            return Err(Error::Inconsistent(format!(
                "height {height} ({}): no matching undo record in rev{:05}.dat",
                hash_to_hex(hash),
                loc.file
            )));
        };
        report.stale_undo_skipped += skipped as u64;
        report.txs += block.txs.len() as u64;
        report.unspendable_outputs += connected.unspendable;
        for k in connected.checked_kinds {
            *report.checked_spent_kinds.entry(k).or_default() += 1;
        }
        for k in connected.created_kinds {
            *report.created_kinds.entry(k).or_default() += 1;
        }
        let events = &connected.events;
        for e in &events.events {
            match e {
                Event::Spend(key) => {
                    let coin = utxo.remove(key).expect("checked by connect");
                    report.live_coins -= 1;
                    report.live_bytes -= coin.len() as u64;
                }
                Event::Create {
                    key,
                    coin_start,
                    coin_len,
                } => {
                    utxo.insert(*key, events.coin(*coin_start, *coin_len).into());
                    report.live_coins += 1;
                    report.live_bytes += u64::from(*coin_len);
                }
            }
        }
        writer.write_block(events).map_err(io_err)?;
        if report.live_coins > report.peak_live_coins {
            report.peak_live_coins = report.live_coins;
            report.peak_live_coins_height = height;
        }
        report.peak_live_bytes = report.peak_live_bytes.max(report.live_bytes);
        if height.is_multiple_of(1000) || height == last_height {
            report.live_curve.push((height, report.live_coins, report.live_bytes));
        }
        if height.is_multiple_of(10_000) {
            progress(height, &report);
        }
    }
    report.spends = writer.spends;
    report.creates = writer.creates;
    writer.finish().map_err(io_err)?;
    report.rev_resync_bytes = revs.resync_bytes;
    report.seconds = started.elapsed().as_secs_f64();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_round_trip() {
        let header = TraceHeader {
            network_magic: [1, 2, 3, 4],
            first_height: 1,
            last_height: 2,
            anchor: [9; 32],
        };
        let mut b1 = TraceBlock::default();
        b1.clear(1);
        b1.push_create(out_key(&[1; 32], 0), &[0xaa, 0xbb]);
        let mut b2 = TraceBlock::default();
        b2.clear(2);
        b2.events.push(Event::Spend(out_key(&[1; 32], 0)));
        b2.push_create(out_key(&[2; 32], 7), &[0xcc]);
        let mut w = TraceWriter::new(Vec::new(), &header).unwrap();
        w.write_block(&b1).unwrap();
        w.write_block(&b2).unwrap();
        let bytes = w.finish().unwrap();

        let mut r = TraceReader::new(bytes.as_slice()).unwrap();
        assert_eq!(r.header, header);
        let mut b = TraceBlock::default();
        assert!(r.next_block(&mut b).unwrap());
        assert_eq!((b.height, b.events.len()), (1, 1));
        assert!(r.next_block(&mut b).unwrap());
        assert_eq!(b.events[0], Event::Spend(out_key(&[1; 32], 0)));
        let Event::Create {
            key,
            coin_start,
            coin_len,
        } = b.events[1]
        else {
            panic!()
        };
        assert_eq!((key, b.coin(coin_start, coin_len)), (out_key(&[2; 32], 7), &[0xcc][..]));
        assert!(!r.next_block(&mut b).unwrap());

        let mut corrupt = bytes.clone();
        let n = corrupt.len();
        corrupt[n - 1] ^= 1; // the trailer's creates total
        let mut r = TraceReader::new(corrupt.as_slice()).unwrap();
        assert!(r.next_block(&mut b).unwrap() && r.next_block(&mut b).unwrap());
        assert!(r.next_block(&mut b).is_err());
    }
}
