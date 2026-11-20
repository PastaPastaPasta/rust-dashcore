//! What the anchor driver's tests need of a real store: a memory-engine store, and one
//! off-chain record lost or stored under another hash, as a damaged store holds it.

use super::*;

impl Store<MemoryEngine> {
    pub(crate) fn in_memory(dir: &tempfile::TempDir) -> Self {
        mem_store(dir)
    }

    /// Deletes the off-chain record `id`: its row and every index entry naming it.
    pub(crate) fn lose_offchain(&self, id: BlockId) {
        let row = self.stored_row(id).unwrap().expect("an off-chain row");
        let mut w = self.lease();
        let mut b = self.db().unwrap().batch();
        b.delete(HDR, &id_key(id));
        b.delete(HDR_HASH, &row.hash);
        b.delete(HDR_H, &height_id_key(row.height, id));
        b.delete(HDR_PREV, &prev_id_key(&prev_of(&row), id));
        self.commit(&mut w, b, Commit::Durable).unwrap();
    }

    /// Stores the off-chain record `id` under `hash`: its row's hash and its `hdr_hash` key.
    pub(crate) fn rekey_offchain(&self, id: BlockId, hash: [u8; 32]) {
        let row = self.stored_row(id).unwrap().expect("an off-chain row");
        let mut w = self.lease();
        let mut b = self.db().unwrap().batch();
        b.delete(HDR_HASH, &row.hash);
        b.put(HDR_HASH, &hash, &hash_value(id, row.height));
        b.put(
            HDR,
            &id_key(id),
            &encode_row(&HeaderRow {
                hash,
                ..row
            }),
        );
        self.commit(&mut w, b, Commit::Durable).unwrap();
    }
}
