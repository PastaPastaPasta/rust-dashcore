//! `CHAINSTATE_SCHEMA` (CH2 §2.6): every column family of A1 §2.9 from version 1, so the evo
//! state needs no migration.

use dashd_store::{CfProfile, CfSpec, DbSchema};

pub static CHAINSTATE_SCHEMA: DbSchema = DbSchema {
    name: "chainstate",
    version: 1,
    cfs: &[
        CfSpec::new("meta", CfProfile::Ordered),
        CfSpec::new("block_index", CfProfile::PointLookup),
        CfSpec::new("block_files", CfProfile::Ordered),
        CfSpec::new("coins", CfProfile::PointLookup),
        CfSpec::new("mn_diff", CfProfile::Compressed),
        CfSpec::new("mn_snapshot", CfProfile::Compressed),
        CfSpec::new("credit_pool", CfProfile::Compressed),
        CfSpec::new("mnhf", CfProfile::Ordered),
        CfSpec::new("llmq_mined", CfProfile::Compressed),
        CfSpec::new("llmq_mined_h", CfProfile::Ordered),
        CfSpec::new("llmq_snapshot", CfProfile::Compressed),
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_is_valid_and_meta_comes_first() {
        // An engine validates its schema and stamps it on creation.
        drop(dashd_store::MemoryEngine::new(&CHAINSTATE_SCHEMA));
        assert_eq!(CHAINSTATE_SCHEMA.cf("meta"), Some(DbSchema::META));
        assert!(CHAINSTATE_SCHEMA.cf("coins").is_some());
        assert_eq!(CHAINSTATE_SCHEMA.cfs.len(), 11);
    }
}
