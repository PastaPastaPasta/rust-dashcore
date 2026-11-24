//! LevelDB tables, logs, manifests and WriteBatch records through the read-only point reader.
#![no_main]
node_fuzz::model_target!("coreimport_leveldb");
