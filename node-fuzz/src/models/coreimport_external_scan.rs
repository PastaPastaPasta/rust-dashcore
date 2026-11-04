//! `coreimport_external_scan`: Core's `-reindex`/`-loadblock` scanner
//! (`dash_coreimport::blockfiles::external`) on arbitrary files, under the runner (DEC-30 N5).
//! Every block it hands over must be exactly one block as the independent cursor decoder
//! (`dash_coreimport::tx::read_block`) reads it, and every header position must lie inside the
//! file. The checks of the honggfuzz target `fuzz/fuzz_targets/dash_coreimport/external_scan.rs`,
//! each reported as a divergence instead of a panic. The scanner decodes blocks: a private-route
//! target (PLAN §7.5). The scanner does not depend on the Core release.

use dash_coreimport::Hash256;
use dash_coreimport::blockfiles::external::{
    ExternalBlockSink, Flow, HeaderStatus, MAX_BLOCK_SIZE, load_external_block_file,
};
use node_l1::Divergence;

const MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];

struct Sink<'a> {
    file: &'a [u8],
    blocks: u64,
    diverged: Option<&'static str>,
}

impl Sink<'_> {
    fn diverge(&mut self, field: &'static str) {
        self.diverged.get_or_insert(field);
    }

    /// `len` bytes of the file at `pos`, when they are inside it.
    fn at(&self, pos: u64, len: usize) -> Option<&[u8]> {
        let start = usize::try_from(pos).ok()?;
        self.file.get(start..start.checked_add(len)?)
    }
}

impl ExternalBlockSink for Sink<'_> {
    fn header(&mut self, header: &[u8; 80], _: &Hash256, pos: u64) -> HeaderStatus {
        if self.at(pos, 80) != Some(&header[..]) {
            self.diverge("header position");
        }
        // Exercise every path, driven by the input.
        match header[0] % 4 {
            0 => HeaderStatus::UnknownParent,
            1 => HeaderStatus::Stored,
            _ => HeaderStatus::New,
        }
    }

    fn block(&mut self, block: &[u8], _: &Hash256, pos: u64) -> Flow {
        if self.at(pos, block.len()) != Some(block) {
            self.diverge("block position");
        }
        if dash_coreimport::tx::read_block(block).is_err() {
            self.diverge("scanner and cursor decoder");
        }
        self.blocks += 1;
        if block.get(1) == Some(&0xff) {
            Flow::Stop
        } else {
            Flow::Continue
        }
    }

    fn after(&mut self, hash: &Hash256) -> Flow {
        if hash[0] == 0xff && hash[1] == 0xff {
            Flow::Stop
        } else {
            Flow::Continue
        }
    }

    fn header_read(&mut self, pos: u64) {
        if self.at(pos, 80).is_none() {
            self.diverge("header read position");
        }
    }

    fn unexpected_data(&mut self, offset: u64) {
        // Core logs `nRewind - 1`, which is past the end of the file after a torn record.
        if offset >= self.file.len() as u64 + u64::from(MAX_BLOCK_SIZE) {
            self.diverge("unexpected data offset");
        }
    }
}

pub(super) fn check(data: &[u8]) -> Result<(), Divergence> {
    let mut sink = Sink {
        file: data,
        blocks: 0,
        diverged: None,
    };
    let report = load_external_block_file(data, MAGIC, &mut sink);
    if sink.blocks > report.headers.count {
        sink.diverge("block count");
    }
    match sink.diverged {
        Some(field) => Err(Divergence {
            field,
        }),
        None => Ok(()),
    }
}
