//! Typed tables: one column family seen through a key codec and a value codec.

use std::marker::PhantomData;

use crate::codec::{KeyCodec, ValueCodec};
use crate::engine::{Cf, KvBatch, KvRead};
use crate::error::{Result, StoreError};
use crate::schema::DbSchema;

/// A typed view of one column family. Reads go through any [`KvRead`] (an engine or a
/// snapshot); writes go into a [`KvBatch`].
pub struct Table<K, V> {
    cf: Cf,
    name: &'static str,
    _codecs: PhantomData<fn() -> (K, V)>,
}

impl<K, V> Clone for Table<K, V> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V> Copy for Table<K, V> {}

impl<K: KeyCodec, V: ValueCodec> Table<K, V> {
    /// The table over column family `name` of `schema`. Panics if the schema has no such
    /// column family (a programming error, caught by any test that opens the table).
    pub fn new(schema: &'static DbSchema, name: &'static str) -> Self {
        let cf = schema
            .cf(name)
            .unwrap_or_else(|| panic!("schema `{}` has no column family `{name}`", schema.name));
        Self {
            cf,
            name,
            _codecs: PhantomData,
        }
    }

    pub fn cf(&self) -> Cf {
        self.cf
    }

    fn decode_key(&self, bytes: &[u8]) -> Result<K> {
        K::decode_key(bytes).map_err(|source| StoreError::Codec {
            cf: self.name,
            source,
        })
    }

    fn decode_value(&self, bytes: &[u8]) -> Result<V> {
        V::decode_value(bytes).map_err(|source| StoreError::Codec {
            cf: self.name,
            source,
        })
    }

    pub fn get(&self, db: &impl KvRead, key: &K) -> Result<Option<V>> {
        db.get(self.cf, &key.to_key_bytes())?.map(|v| self.decode_value(&v)).transpose()
    }

    pub fn multi_get(&self, db: &impl KvRead, keys: &[K]) -> Result<Vec<Option<V>>> {
        let encoded: Vec<Vec<u8>> = keys.iter().map(KeyCodec::to_key_bytes).collect();
        let refs: Vec<&[u8]> = encoded.iter().map(Vec::as_slice).collect();
        db.multi_get(self.cf, &refs)?
            .into_iter()
            .map(|v| v.map(|v| self.decode_value(&v)).transpose())
            .collect()
    }

    pub fn put(&self, batch: &mut impl KvBatch, key: &K, value: &V) {
        batch.put(self.cf, &key.to_key_bytes(), &value.to_value_bytes());
    }

    pub fn delete(&self, batch: &mut impl KvBatch, key: &K) {
        batch.delete(self.cf, &key.to_key_bytes());
    }

    /// Entries whose encoded key starts with `prefix`, decoded, in key order.
    pub fn iter_prefix<'a>(
        &'a self,
        db: &'a impl KvRead,
        prefix: &[u8],
    ) -> impl Iterator<Item = Result<(K, V)>> + 'a {
        db.iter_prefix(self.cf, prefix).map(move |entry| {
            let (k, v) = entry?;
            Ok((self.decode_key(&k)?, self.decode_value(&v)?))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{BeU32, CodecError};
    use crate::engine::{Commit, KvEngine};
    use crate::memory::MemoryEngine;
    use crate::schema::{CfProfile, CfSpec};

    static SCHEMA: DbSchema = DbSchema {
        name: "table-test",
        version: 1,
        cfs: &[CfSpec::new("meta", CfProfile::Ordered), CfSpec::new("heights", CfProfile::Ordered)],
    };

    #[test]
    fn typed_round_trip_and_order() {
        let db = MemoryEngine::new(&SCHEMA);
        let table: Table<BeU32, [u8; 2]> = Table::new(&SCHEMA, "heights");
        let mut batch = db.batch();
        for h in [300u32, 2, 70_000] {
            table.put(&mut batch, &BeU32(h), &[h as u8, 1]);
        }
        db.write(batch, Commit::Buffered).unwrap();
        assert_eq!(table.get(&db, &BeU32(2)).unwrap(), Some([2, 1]));
        assert_eq!(table.get(&db, &BeU32(3)).unwrap(), None);
        let keys: Vec<u32> = table.iter_prefix(&db, &[]).map(|e| e.unwrap().0.0).collect();
        assert_eq!(keys, [2, 300, 70_000]);
        assert_eq!(
            table.multi_get(&db, &[BeU32(70_000), BeU32(1)]).unwrap(),
            [Some([(70_000u32 as u8), 1]), None]
        );
    }

    #[test]
    fn decode_errors_name_the_column_family() {
        let db = MemoryEngine::new(&SCHEMA);
        let raw: Table<Vec<u8>, Vec<u8>> = Table::new(&SCHEMA, "heights");
        let typed: Table<BeU32, BeU32> = Table::new(&SCHEMA, "heights");
        let mut batch = db.batch();
        raw.put(&mut batch, &BeU32(1).to_key_bytes(), &vec![1, 2, 3]);
        db.write(batch, Commit::Buffered).unwrap();
        let err = typed.get(&db, &BeU32(1)).unwrap_err();
        assert!(matches!(
            err,
            StoreError::Codec {
                cf: "heights",
                source: CodecError::Length { .. }
            }
        ));
    }

    #[test]
    #[should_panic(expected = "no column family `missing`")]
    fn unknown_column_family_panics() {
        let _: Table<BeU32, BeU32> = Table::new(&SCHEMA, "missing");
    }
}
