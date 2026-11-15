use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use dash_statedigest::{Record, parse_line};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tail {
    pub path: PathBuf,
    pub offset: u64,
    pub line: usize,
    #[serde(default)]
    pub dev: Option<u64>,
    #[serde(default)]
    pub inode: Option<u64>,
}

impl Tail {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            offset: 0,
            line: 0,
            dev: None,
            inode: None,
        }
    }

    pub fn read(&mut self, limit: usize) -> Result<Vec<(usize, Record)>> {
        let mut file = File::open(&self.path)?;
        let metadata = file.metadata()?;
        ensure!(metadata.len() >= self.offset, "{} was truncated", self.path.display());
        let dev = metadata.dev();
        let inode = metadata.ino();
        if let (Some(want_dev), Some(want_inode)) = (self.dev, self.inode) {
            ensure!(
                want_dev == dev && want_inode == inode,
                "{} changed (device/inode {}:{}, expected {}:{})",
                self.path.display(),
                dev,
                inode,
                want_dev,
                want_inode
            );
        } else {
            self.dev = Some(dev);
            self.inode = Some(inode);
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut reader = BufReader::new(file);
        let mut records = Vec::new();
        let mut bytes = Vec::new();
        for _ in 0..limit {
            bytes.clear();
            reader.read_until(b'\n', &mut bytes)?;
            if bytes.last() != Some(&b'\n') {
                break;
            }
            let record = parse_line(&bytes)
                .with_context(|| format!("{}:{}", self.path.display(), self.line + 1))?;
            self.offset += bytes.len() as u64;
            self.line += 1;
            records.push((self.line, record));
        }
        Ok(records)
    }
}
