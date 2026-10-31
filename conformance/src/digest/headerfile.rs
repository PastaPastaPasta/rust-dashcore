//! A header chain from a file, for `digest l2 --headers` (CH3a a7-3, D162): Core's headers-first
//! chain runs past the blocks it has stored, and assumevalid's conditions read that chain
//! (`validation.cpp:2549-2553`, `:2568`), so a frozen datadir's blk files alone preload too short a
//! chain.
//!
//! - **Format:** 80-byte headers from genesis, in height order, nothing else.
//! - **Checks:** record 0 must hash to the chain's genesis, and each next record's `prev_hash`
//!   must be the hash of the one before. Nothing else is checked here: V's `process_headers` checks
//!   each header as Core's `AcceptBlockHeader` does when the preload sends it.
//! - **Memory:** the headers are held in memory, about 112 bytes each. The file's sha256 is of the
//!   bytes parsed.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use dash_consensus::primitives::{Header, decode_header};
use dash_primitives::BlockHash;

use super::frames::Sha256Reader;
use crate::split::HEADER_LEN;

/// A file's header chain, heights `0..=tip()`.
pub struct HeaderFile {
    path: PathBuf,
    sha256: String,
    headers: Vec<Header>,
}

impl HeaderFile {
    /// Reads `path`, whose record 0 must hash to `genesis` and whose records must link.
    pub fn open(path: &Path, genesis: BlockHash) -> Result<HeaderFile> {
        let name = path.display();
        let file = std::fs::File::open(path).with_context(|| format!("opening {name}"))?;
        let len = file.metadata().with_context(|| format!("stat {name}"))?.len();
        ensure!(len > 0, "{name}: empty");
        ensure!(
            len % HEADER_LEN as u64 == 0,
            "{name}: {len} bytes, not a whole number of {HEADER_LEN}-byte headers"
        );
        let count = len / HEADER_LEN as u64;
        ensure!(count <= u64::from(u32::MAX), "{name}: {count} headers, past a u32 height");
        let mut reader = Sha256Reader::new(std::io::BufReader::new(file));
        let mut headers: Vec<Header> = Vec::new();
        let mut raw = [0; HEADER_LEN];
        for h in 0..count {
            reader.read_exact(&mut raw).with_context(|| format!("{name}: reading height {h}"))?;
            let header = decode_header(&mut &raw[..])
                .map_err(|e| anyhow::anyhow!("{name}: height {h} does not decode: {e:?}"))?
                .into_value();
            if let Some(prev) = headers.last() {
                ensure!(
                    header.prev_hash() == prev.hash(),
                    "{name}: height {h}'s prev_hash is {}, not height {}'s hash {}",
                    header.prev_hash(),
                    h - 1,
                    prev.hash()
                );
            } else {
                ensure!(
                    header.hash() == genesis,
                    "{name}: height 0 hashes to {}, not the chain's genesis {genesis}",
                    header.hash()
                );
                // Record 0 is the chain's: the file is worth holding.
                headers.reserve_exact(count as usize);
            }
            headers.push(header);
        }
        let sha256 = reader.finish().with_context(|| format!("reading {name}"))?;
        Ok(HeaderFile {
            path: path.to_owned(),
            sha256,
            headers,
        })
    }

    /// The file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The sha256 of the bytes read, as lowercase hex.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// The last header's height.
    pub fn tip(&self) -> u32 {
        (self.headers.len() - 1) as u32
    }

    /// Height `h`'s hash.
    pub fn hash(&self, h: u32) -> Option<BlockHash> {
        self.headers.get(h as usize).map(Header::hash)
    }

    /// The headers of heights `from..=to`, each with its hash: none past the tip.
    pub fn headers(&self, from: u32, to: u32) -> impl Iterator<Item = (Header, BlockHash)> + '_ {
        let to = (to as usize).min(self.headers.len() - 1);
        self.headers.get(from as usize..=to).unwrap_or(&[]).iter().map(|h| (*h, h.hash()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash_consensus::primitives::core_serialize;
    use dash_params::{ChainParamsBuilder, Network, Release};
    use dashd_chainstate::chain::genesis_block;

    /// Regtest's genesis header and `n` more, linked, as a file's bytes; and their hashes.
    fn chain(n: u32) -> (Vec<u8>, Vec<BlockHash>) {
        let p = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
        let genesis = core_serialize(genesis_block(&p).header());
        let mut bytes = genesis.clone();
        let mut hashes = vec![genesis_block(&p).hash()];
        let mut raw: [u8; HEADER_LEN] = genesis.try_into().unwrap();
        for h in 1..=n {
            raw[4..36].copy_from_slice(hashes.last().unwrap().as_bytes());
            raw[76..].copy_from_slice(&h.to_le_bytes());
            hashes.push(decode_header(&mut &raw[..]).unwrap().into_value().hash());
            bytes.extend_from_slice(&raw);
        }
        (bytes, hashes)
    }

    fn open(bytes: &[u8]) -> Result<HeaderFile> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("headers.bin");
        std::fs::write(&path, bytes).unwrap();
        let p = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
        HeaderFile::open(&path, genesis_block(&p).hash())
    }

    #[test]
    fn a_linked_chain_from_genesis_opens() {
        let (bytes, hashes) = chain(5);
        let f = open(&bytes).unwrap();
        assert_eq!(f.tip(), 5);
        assert_eq!(
            (0..=6).map(|h| f.hash(h)).collect::<Vec<_>>(),
            [hashes.iter().copied().map(Some).collect::<Vec<_>>(), vec![None]].concat()
        );
        let read: Vec<_> = f.headers(2, 9).map(|(_, hash)| hash).collect();
        assert_eq!(read, hashes[2..]);
        assert_eq!(f.headers(6, 9).count(), 0);
        assert_eq!(f.sha256(), crate::digest::store::sha256_hex(&bytes));
        assert!(f.headers(1, 3).all(|(header, hash)| header.hash() == hash));
    }

    #[test]
    fn a_broken_link_is_refused_with_its_height() {
        let (mut bytes, hashes) = chain(5);
        bytes[3 * HEADER_LEN + 4] ^= 1;
        let e = open(&bytes).err().unwrap().to_string();
        assert!(e.ends_with(&format!("height 2's hash {}", hashes[2])), "{e}");
        assert!(e.contains(": height 3's prev_hash is "), "{e}");
    }

    #[test]
    fn a_file_not_from_genesis_is_refused() {
        let (bytes, hashes) = chain(5);
        let e = open(&bytes[HEADER_LEN..]).err().unwrap().to_string();
        assert!(
            e.contains(&format!("height 0 hashes to {}, not the chain's genesis", hashes[1])),
            "{e}"
        );
    }

    #[test]
    fn a_partial_or_empty_file_is_refused() {
        let (bytes, _) = chain(2);
        let e = open(&[]).err().unwrap().to_string();
        assert!(e.ends_with(": empty"), "{e}");
        let e = open(&[bytes.as_slice(), &[0]].concat()).err().unwrap().to_string();
        assert!(e.ends_with(": 241 bytes, not a whole number of 80-byte headers"), "{e}");
    }
}
