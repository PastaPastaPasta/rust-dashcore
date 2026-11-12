//! The shared funded blocks (E1 replaced, Sol ruling 3): the node side builds them
//! (dashd-node's `p2p::amb::funded`, whose test checks them against this file), and the Core
//! leg reads them from here, so dashd-node takes no oracle dependency (layers: N may not depend
//! on T).
//!
//! The prefix is [`PREFIX_BLOCKS`] blocks: the first's coinbase funds every filler, and the rest
//! mature it. The workload is a block per row of the checked-in mainnet transaction counts. Block
//! times are one second apart, the last workload block's at [`T0`], so the prefix tip is
//! `T0 − workload` seconds old at `-mocktime=T0`: out of IBD without `-maxtipage`.
//!
//! The file: [`MAGIC`], the prefix and workload counts (u32 LE), then each block as its u32 LE
//! length and Core's serialization.

use std::path::PathBuf;

use crate::{Result, refuse};

/// The last workload block's time, and both legs' mock time.
pub const T0: i64 = 1_761_955_200;
/// `COINBASE_MATURITY`: the funding coinbase and the blocks that mature it.
pub const PREFIX_BLOCKS: usize = 100;
/// The full fixture's workload: a block per row of the mainnet transaction counts.
pub const WORKLOAD_BLOCKS: usize = 201;
/// The full workload's transactions.
pub const WORKLOAD_TXS: u64 = 5704;
pub const MAGIC: &[u8; 8] = b"P3AMB01\0";
/// Both legs' scripted peers' services, `NODE_NETWORK | NODE_BLOOM | NODE_NETWORK_LIMITED`: no
/// `NODE_HEADERS_COMPRESSED`, so the node asks with `getheaders` and takes `headers`.
pub const PEER_SERVICES: u64 = 0x1 | 0x4 | 0x400;
/// The semantic smokes' rows.
pub const SMOKE_ROWS: usize = 8;
/// sha256 of [`path`]'s bytes.
pub const SHA256: &str = "0d14151a64dc138303fdeac80d820071e7ad85870fcdaaf058eeb3f3a199d746";

/// The checked-in fixture.
pub fn path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/p3_amb_blocks.bin")
}

/// The prefix and workload blocks, each serialized.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fixture {
    pub prefix: Vec<Vec<u8>>,
    pub workload: Vec<Vec<u8>>,
}

fn put_blocks(out: &mut Vec<u8>, blocks: &[Vec<u8>]) {
    for b in blocks {
        out.extend((b.len() as u32).to_le_bytes());
        out.extend(b);
    }
}

impl Fixture {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.extend((self.prefix.len() as u32).to_le_bytes());
        out.extend((self.workload.len() as u32).to_le_bytes());
        put_blocks(&mut out, &self.prefix);
        put_blocks(&mut out, &self.workload);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Fixture> {
        let Some(rest) = bytes.strip_prefix(MAGIC.as_slice()) else {
            refuse!("not a P3 (A − B) fixture");
        };
        let mut r = rest;
        let u32_le = |r: &mut &[u8]| -> Result<usize> {
            if r.len() < 4 {
                refuse!("a truncated fixture");
            }
            let (n, tail) = r.split_at(4);
            *r = tail;
            Ok(u32::from_le_bytes(n.try_into().expect("4 bytes")) as usize)
        };
        let (np, nw) = (u32_le(&mut r)?, u32_le(&mut r)?);
        if np + nw > r.len() / 4 {
            refuse!("a truncated fixture");
        }
        let mut blocks = Vec::with_capacity(np + nw);
        for _ in 0..np + nw {
            let len = u32_le(&mut r)?;
            if r.len() < len {
                refuse!("a truncated fixture block");
            }
            if len < 81 {
                refuse!("a fixture block of {len} bytes");
            }
            let (b, tail) = r.split_at(len);
            blocks.push(b.to_vec());
            r = tail;
        }
        if !r.is_empty() {
            refuse!("{} trailing fixture bytes", r.len());
        }
        let workload = blocks.split_off(np);
        Ok(Fixture {
            prefix: blocks,
            workload,
        })
    }

    /// The checked-in file, refused unless its sha256 is [`SHA256`].
    pub fn load() -> Result<Fixture> {
        let bytes = match std::fs::read(path()) {
            Ok(b) => b,
            Err(e) => refuse!("{}: {e}", path().display()),
        };
        let digest = sha256_hex(&bytes);
        if digest != SHA256 {
            refuse!("the fixture's sha256 is {digest}, not {SHA256}");
        }
        Fixture::decode(&bytes)
    }

    /// sha256 of the prefix as [`Fixture::encode`] lays it out (counts, then blocks).
    pub fn prefix_sha256(&self) -> String {
        let mut out = (self.prefix.len() as u32).to_le_bytes().to_vec();
        put_blocks(&mut out, &self.prefix);
        sha256_hex(&out)
    }

    /// The workload's transactions (each block's count after its 80-byte header; [`decode`]
    /// checked every block holds one).
    ///
    /// [`decode`]: Fixture::decode
    pub fn workload_txs(&self) -> u64 {
        self.workload.iter().map(|b| tx_count(b)).sum()
    }

    /// The checked-in file's first [`SMOKE_ROWS`] workload blocks.
    pub fn smoke() -> Result<Fixture> {
        Ok(Fixture::load()?.truncated(SMOKE_ROWS))
    }

    /// The first `n` workload blocks only (smoke and lifecycle runs).
    pub fn truncated(&self, n: usize) -> Fixture {
        Fixture {
            prefix: self.prefix.clone(),
            workload: self.workload[..n.min(self.workload.len())].to_vec(),
        }
    }
}

/// A serialized block's transaction count (its CompactSize after the header).
pub fn tx_count(block: &[u8]) -> u64 {
    match block[80] {
        n @ 0..0xfd => u64::from(n),
        0xfd => u64::from(u16::from_le_bytes([block[81], block[82]])),
        0xfe => u64::from(u32::from_le_bytes(block[81..85].try_into().expect("4 bytes"))),
        _ => u64::from_le_bytes(block[81..89].try_into().expect("8 bytes")),
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    dash_sha256::sha256(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fixture_round_trips_and_refuses_damage() {
        let f = Fixture {
            prefix: vec![vec![1; 81], vec![2; 81]],
            workload: vec![[vec![0; 80], vec![3, 9]].concat()],
        };
        let bytes = f.encode();
        assert_eq!(Fixture::decode(&bytes).unwrap(), f);
        assert_eq!(f.workload_txs(), 3);
        assert!(Fixture::decode(&bytes[1..]).is_err());
        assert!(Fixture::decode(&bytes[..bytes.len() - 1]).unwrap_err().0.contains("truncated"));
        assert!(Fixture::decode(&[&bytes[..], &[0]].concat()).unwrap_err().0.contains("trailing"));
        let huge = [&MAGIC[..], &u32::MAX.to_le_bytes(), &u32::MAX.to_le_bytes()].concat();
        assert!(Fixture::decode(&huge).unwrap_err().0.contains("truncated"));
        let short = Fixture {
            prefix: vec![vec![0; 80]],
            workload: vec![],
        };
        assert!(Fixture::decode(&short.encode()).unwrap_err().0.contains("80 bytes"));
        assert_eq!(f.truncated(0).workload.len(), 0);
    }

    /// The checked-in file matches its pinned digest and holds the workload's shape.
    #[test]
    fn the_checked_in_fixture_is_pinned() {
        let f = Fixture::load().unwrap();
        assert_eq!(f.prefix.len(), PREFIX_BLOCKS);
        assert_eq!((f.workload.len(), f.workload_txs()), (WORKLOAD_BLOCKS, WORKLOAD_TXS));
    }

    #[test]
    fn tx_counts_read_every_compact_size() {
        let header = [0u8; 80];
        assert_eq!(tx_count(&[&header[..], &[0xfc]].concat()), 0xfc);
        assert_eq!(tx_count(&[&header[..], &[0xfd, 0x34, 0x12]].concat()), 0x1234);
        assert_eq!(tx_count(&[&header[..], &[0xfe, 1, 0, 0, 1]].concat()), 0x0100_0001);
    }
}
