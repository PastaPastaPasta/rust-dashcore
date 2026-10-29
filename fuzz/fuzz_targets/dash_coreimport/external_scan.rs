//! `external_scan`: Core's `-reindex`/`-loadblock` scanner (`dash_coreimport::blockfiles::external`)
//! on arbitrary files. Every block it hands over must be exactly one block as the independent
//! cursor decoder (`dash_coreimport::tx::read_block`) reads it, and every header position must lie
//! inside the file.

use dash_coreimport::Hash256;
use dash_coreimport::blockfiles::external::{
    ExternalBlockSink, Flow, HeaderStatus, MAX_BLOCK_SIZE, load_external_block_file,
};
use honggfuzz::fuzz;

const MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];

struct Sink<'a> {
    file: &'a [u8],
    blocks: usize,
}

impl ExternalBlockSink for Sink<'_> {
    fn header(&mut self, header: &[u8; 80], _: &Hash256, pos: u64) -> HeaderStatus {
        assert_eq!(&self.file[pos as usize..pos as usize + 80], header);
        // Exercise every path, driven by the input.
        match header[0] % 4 {
            0 => HeaderStatus::UnknownParent,
            1 => HeaderStatus::Stored,
            _ => HeaderStatus::New,
        }
    }

    fn block(&mut self, block: &[u8], _: &Hash256, pos: u64) -> Flow {
        assert_eq!(&self.file[pos as usize..pos as usize + block.len()], block);
        if let Err(e) = dash_coreimport::tx::read_block(block) {
            panic!("the scanner and the cursor decoder disagree on a block at {pos}: {e}");
        }
        self.blocks += 1;
        if block[1] == 0xff {
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
        assert!(pos as usize + 80 <= self.file.len());
    }

    fn unexpected_data(&mut self, offset: u64) {
        // Core logs `nRewind - 1`, which is past the end of the file after a torn record.
        assert!(offset < self.file.len() as u64 + u64::from(MAX_BLOCK_SIZE));
    }
}

fn do_test(data: &[u8]) {
    let mut sink = Sink {
        file: data,
        blocks: 0,
    };
    let report = load_external_block_file(data, MAGIC, &mut sink);
    assert!(sink.blocks as u64 <= report.headers.count);
}

fn main() {
    loop {
        fuzz!(|data| {
            do_test(data);
        });
    }
}

#[cfg(all(test, fuzzing))]
mod tests {
    #[test]
    fn duplicate_crash() {
        super::do_test(&[]);
    }
}
