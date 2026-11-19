//! Inspection of the small Core database records needed for safe block-file import.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::blockfiles::{list_files, scan_block_records};
use crate::leveldb::{LevelDbReader, open_file};
use crate::{Error, Hash256, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Pruned,
    Gap(u32),
    NoGenesisFile,
    WrongGenesis,
    Reindexing,
    CorruptBlockFile {
        file: PathBuf,
        offset: u64,
    },
    MissingBestBlock,
    BestBlockNotInFiles,
}

#[derive(Debug)]
pub struct SourceInspection {
    pub net_dir: PathBuf,
    pub files: Vec<(u32, PathBuf)>,
    pub tail: u32,
    pub genesis: Hash256,
    pub best_block: Option<Hash256>,
    pub last_file: Option<u32>,
    pub pruned: bool,
    pub reindexing: bool,
    pub obfuscation_key: [u8; 8],
    expected_genesis: Hash256,
    corrupt_block_file: Option<Refusal>,
    best_block_in_files: bool,
}

impl SourceInspection {
    pub fn refusal(&self) -> Option<Refusal> {
        if let Some(refusal) = &self.corrupt_block_file {
            return Some(refusal.clone());
        }
        if self.reindexing {
            return Some(Refusal::Reindexing);
        }
        if self.pruned {
            return Some(Refusal::Pruned);
        }
        if self.files.first().is_none_or(|(n, _)| *n != 0) {
            return Some(Refusal::NoGenesisFile);
        }
        if self.genesis != self.expected_genesis {
            return Some(Refusal::WrongGenesis);
        }
        for (expected, (actual, _)) in self.files.iter().enumerate() {
            if *actual != expected as u32 {
                return Some(Refusal::Gap(expected as u32));
            }
        }
        if self.best_block.is_none() {
            return Some(Refusal::MissingBestBlock);
        }
        if !self.best_block_in_files {
            return Some(Refusal::BestBlockNotInFiles);
        }
        None
    }
}

pub fn inspect(net_dir: &Path, magic: [u8; 4], genesis: Hash256) -> Result<SourceInspection> {
    let files = list_files(&net_dir.join("blocks"), "blk")?;
    let tail = files.last().map_or(0, |(n, _)| *n);
    let index = LevelDbReader::open(&net_dir.join("blocks/index"))?;
    if let Some(key) = index.get(b"\x0e\0obfuscate_key")?
        && (key.len() != 9 || key[0] != 8 || key[1..].iter().any(|&byte| byte != 0))
    {
        return Err(Error::malformed(
            "block index obfuscation key",
            "expected an all-zero eight-byte key",
        ));
    }
    let reindexing = match index.get(b"R")? {
        None => false,
        Some(value) if value == b"1" => true,
        Some(_) => return Err(Error::malformed("reindex flag", "expected '1'")),
    };
    let pruned = match index.get(b"F\x10prunedblockfiles")? {
        None => false,
        Some(value) if value == b"0" => false,
        Some(value) if value == b"1" => true,
        Some(_) => return Err(Error::malformed("prune flag", "expected '0' or '1'")),
    };
    let last_file = index
        .get(b"l")?
        .map(|value| {
            let bytes = value
                .try_into()
                .map_err(|_| Error::malformed("last block file", "expected four bytes"))?;
            let n = i32::from_le_bytes(bytes);
            u32::try_from(n)
                .map_err(|_| Error::malformed("last block file", "negative file number"))
        })
        .transpose()?;
    let chainstate = LevelDbReader::open(&net_dir.join("chainstate"))?;
    let obfuscate = chainstate.get(b"\x0e\0obfuscate_key")?;
    let xor = match obfuscate.as_deref() {
        None => [0; 8],
        Some([8, bytes @ ..]) if bytes.len() == 8 => bytes
            .try_into()
            .map_err(|_| Error::malformed("obfuscation key", "expected eight bytes"))?,
        Some(_) => {
            return Err(Error::malformed(
                "obfuscation key",
                "expected serialized eight-byte vector",
            ));
        }
    };
    let best_block = chainstate
        .get(b"B")?
        .map(|mut bytes| {
            for (n, byte) in bytes.iter_mut().enumerate() {
                *byte ^= xor[n % 8];
            }
            bytes.try_into().map_err(|_| Error::malformed("best block", "expected 32 bytes"))
        })
        .transpose()?;
    let mut actual_genesis = [0; 32];
    let mut corrupt_block_file = None;
    let mut best_block_in_files = false;
    for (number, path) in &files {
        let mut data = Vec::new();
        open_file(path)?
            .take(128 * 1024 * 1024 + 1)
            .read_to_end(&mut data)
            .map_err(|e| Error::io(path, e))?;
        let (records, _) = scan_block_records(&data, magic);
        let mut end = 0;
        let mut invalid_offset = None;
        for record in records {
            if record.offset - 8 != end {
                invalid_offset = Some(end);
                break;
            }
            let payload = &data[record.offset..record.offset + record.len];
            if let Ok(block) = crate::tx::read_block(payload) {
                if !block.merkle_root_matches() {
                    invalid_offset = Some(record.offset);
                    break;
                }
                let hash = crate::tx::block_hash(block.header);
                if *number == 0 && end == 0 {
                    actual_genesis = hash;
                }
                best_block_in_files |= Some(hash) == best_block;
            } else {
                invalid_offset = Some(record.offset);
                break;
            }
            end = record.offset + record.len;
        }
        if invalid_offset.is_none()
            && (data.len() > 128 * 1024 * 1024 || data[end..].iter().any(|&byte| byte != 0))
        {
            invalid_offset = Some(end);
        }
        if let Some(offset) = invalid_offset {
            corrupt_block_file.get_or_insert_with(|| Refusal::CorruptBlockFile {
                file: path.clone(),
                offset: offset as u64,
            });
        }
    }
    Ok(SourceInspection {
        net_dir: net_dir.to_owned(),
        files,
        tail,
        genesis: actual_genesis,
        best_block,
        last_file,
        pruned,
        reindexing,
        obfuscation_key: xor,
        expected_genesis: genesis,
        corrupt_block_file,
        best_block_in_files,
    })
}
