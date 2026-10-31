//! The prune tests' block files: equal files of ten blocks each, with a blk and a rev file of
//! each on disk, so a test sees step 12's unlink.

use std::path::Path;

use dashd_store::flatfile::{BlockFileInfo, BlockFilesState};

/// `n` files of ten blocks each, of `size` and `undo_size` bytes; the last one is current.
pub(super) fn equal_files(n: u32, size: u32, undo_size: u32) -> BlockFilesState {
    let info = |f: u32| BlockFileInfo {
        blocks: 10,
        size,
        undo_size,
        height_first: f * 10,
        height_last: f * 10 + 9,
        time_first: 0,
        time_last: 0,
    };
    BlockFilesState {
        infos: (0..n).map(info).collect(),
        last_file: n - 1,
    }
}

/// Creates `blocks` with a one-byte blk and rev file for each of `state`'s files.
pub(super) fn create_on_disk(blocks: &Path, state: &BlockFilesState) {
    std::fs::create_dir_all(blocks).unwrap();
    for n in 0..state.infos.len() {
        for kind in ["blk", "rev"] {
            std::fs::write(blocks.join(format!("{kind}{n:05}.dat")), [0]).unwrap();
        }
    }
}

/// Whether file `n`'s blk file is in `blocks`.
pub(super) fn blk_on_disk(blocks: &Path, n: u32) -> bool {
    blocks.join(format!("blk{n:05}.dat")).exists()
}
