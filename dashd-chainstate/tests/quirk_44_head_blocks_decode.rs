//! Quirk 44 (the manager's ruling on CH2b c5 item 8), a registered deviation: a head-blocks
//! record that does not decode. Core's `CDBWrapper::Read` returns false on a decode failure
//! (`dbwrapper.h:322-326`), `GetHeadBlocks` then returns an empty vector (`txdb.cpp:105-111`), and
//! `ReplayBlocks` takes the state as consistent (`validation.cpp:5230`). The node's open stops
//! with Corruption, Core's load text "Error opening block database". The record is Core's vector
//! without its size, so it does not decode when its length is no whole number of 32-byte hashes.

use std::sync::Arc;

use dashd_chainstate::flush::NullObserver;
use dashd_chainstate::recovery::{ChainstateOpen, InitError};
use dashd_chainstate::store::{CHAINSTATE_SCHEMA, GlobalFailStop, GuardedEngine, RecordingSink};
use dashd_store::flatfile::{BlockFilesConfig, OsFs};
use dashd_store::{Commit, DbSchema, ErrorKind, KvBatch, MemoryEngine};

#[test]
fn quirk_44_head_blocks_decode() {
    // Whole numbers of hashes open: none is no marker, two a marker, three Core's "unknown
    // inconsistent state" (reported by recovery, not the open).
    for len in [0, 64, 96, 1, 31, 63, 65, 97] {
        let db = GuardedEngine::new(
            MemoryEngine::new(&CHAINSTATE_SCHEMA),
            GlobalFailStop::new(),
            Arc::new(RecordingSink::default()),
        );
        let mut b = db.batch();
        b.put(DbSchema::META, b"head_blocks", &vec![7; len]);
        db.write(b, Commit::Durable).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let blocks = BlockFilesConfig::new(dir.path(), [0xfc, 0xc1, 0xb7, 0xdc]);
        let opened = ChainstateOpen::open(&db, Arc::new(OsFs::new()), blocks, &NullObserver);
        if len % 32 == 0 {
            assert!(opened.is_ok(), "{len} bytes");
            continue;
        }
        let err = opened.err().expect("the open stops");
        let kind = match &err {
            InitError::Store(e) => e.kind(),
            other => panic!("{other:?}"),
        };
        assert_eq!(kind, Some(ErrorKind::Corruption), "{len} bytes");
        assert_eq!(err.to_string(), "Error opening block database", "{len} bytes");
        assert!(db.latch().tripped().is_some(), "{len} bytes");
    }
}
