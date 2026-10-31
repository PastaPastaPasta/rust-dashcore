//! Quirk 43 (D-b1-f), a registered deviation: a block-file info record that does not decode.
//! Core's `LoadBlockIndexDB` ignores `ReadBlockFileInfo`'s result for files `0..=last_file`, so
//! the file keeps a default or partly decoded `CBlockFileInfo` (`node/blockstorage.cpp:364-366`;
//! `CDBWrapper::Read` returns false on a decode failure, `dbwrapper.h:322-326`), and the first
//! such record past `last_file` ends its scan (`:368-375`). The node's open stops with
//! Corruption, Core's load text "Error opening block database", in both places: a difference on
//! a corruption path only, which the manager accepted.

use std::sync::Arc;

use dashd_chainstate::flush::NullObserver;
use dashd_chainstate::recovery::{ChainstateOpen, InitError};
use dashd_chainstate::store::{
    CHAINSTATE_SCHEMA, GlobalFailStop, GuardedEngine, Meta, RecordingSink,
};
use dashd_store::flatfile::{BlockFileInfo, BlockFilesConfig, OsFs};
use dashd_store::{Commit, ErrorKind, KvBatch, MemoryEngine, ValueCodec};

#[test]
fn quirk_43_block_file_info_decode() {
    // `last_file` is 0: file 0 is in Core's first loop, file 1 in its scan past it. A record
    // that decodes opens.
    let good = BlockFileInfo::default().to_value_bytes();
    for (file, record) in [(0_u32, &good[..]), (0, &[0x80]), (1, &[0x80])] {
        let db = GuardedEngine::new(
            MemoryEngine::new(&CHAINSTATE_SCHEMA),
            GlobalFailStop::new(),
            Arc::new(RecordingSink::default()),
        );
        let mut b = db.batch();
        Meta::put_last_file(&mut b, 0);
        // `[0x80]`: a VARINT cut off after its continuation bit.
        b.put(CHAINSTATE_SCHEMA.cf("block_files").unwrap(), &file.to_be_bytes(), record);
        db.write(b, Commit::Durable).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let blocks = BlockFilesConfig::new(dir.path(), [0xfc, 0xc1, 0xb7, 0xdc]);
        let opened = ChainstateOpen::open(&db, Arc::new(OsFs::new()), blocks, &NullObserver);
        if record == good {
            assert!(opened.is_ok());
            continue;
        }
        let err = opened.err().expect("the open stops");
        let kind = match &err {
            InitError::Store(e) => e.kind(),
            other => panic!("{other:?}"),
        };
        assert_eq!(kind, Some(ErrorKind::Corruption), "file {file}");
        assert_eq!(err.to_string(), "Error opening block database", "file {file}");
        assert!(db.latch().tripped().is_some(), "file {file}");
    }
}
