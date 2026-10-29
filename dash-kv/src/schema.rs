//! Database schemas: the column families of one database and its versioned layout (FR-STO-02).

use std::cmp::Ordering;

use crate::codec::{BeU32, ValueCodec};
use crate::engine::{Cf, Commit, KvBatch, KvEngine};
use crate::error::{Result, StoreError};

/// The key of the schema record in column family 0 (`meta`): see [`schema_record`].
pub const SCHEMA_KEY: &[u8] = b"schema";

/// How a column family is used; the engine tunes it accordingly (A1 §2.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CfProfile {
    /// Random hash keys read by point lookups (`coins`, `block_index`): bloom filters, a hashed
    /// data-block index, no compression (hash keys do not compress).
    PointLookup,
    /// Small or ordered data scanned by key (`meta`, height-ordered indexes).
    Ordered,
    /// Bulky, compressible data (index DBs): LZ4 on upper levels, ZSTD on the bottommost level.
    Compressed,
}

/// One column family of a [`DbSchema`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CfSpec {
    pub name: &'static str,
    pub profile: CfProfile,
}

impl CfSpec {
    pub const fn new(name: &'static str, profile: CfProfile) -> Self {
        Self {
            name,
            profile,
        }
    }
}

/// The layout of one database: its name, schema version and column families.
///
/// Column family 0 must be `meta`: it holds the schema record (name and version), which is
/// written when the database is created and checked on every open. A [`Cf`] is an index into
/// `cfs`.
#[derive(Debug, PartialEq, Eq)]
pub struct DbSchema {
    pub name: &'static str,
    pub version: u32,
    pub cfs: &'static [CfSpec],
}

impl DbSchema {
    /// The `meta` column family every schema starts with.
    pub const META: Cf = Cf::new(0);

    /// The column family with this name.
    pub fn cf(&self, name: &str) -> Option<Cf> {
        self.cfs.iter().position(|c| c.name == name).map(|i| Cf::new(i as u16))
    }

    /// Every column family of the schema, in order.
    pub fn all_cfs(&self) -> impl Iterator<Item = Cf> + use<> {
        (0..self.cfs.len() as u16).map(Cf::new)
    }

    /// The specification of `cf`. Panics if `cf` does not belong to this schema.
    pub fn spec(&self, cf: Cf) -> &CfSpec {
        &self.cfs[cf.index()]
    }

    /// Panics unless column family 0 is `meta` and the names are unique. Engines call it on open.
    pub fn validate(&self) {
        assert!(
            self.cfs.first().is_some_and(|c| c.name == "meta"),
            "schema `{}`: column family 0 must be `meta`",
            self.name
        );
        for (i, c) in self.cfs.iter().enumerate() {
            assert!(
                !self.cfs[..i].iter().any(|d| d.name == c.name),
                "schema `{}`: duplicate column family `{}`",
                self.name,
                c.name
            );
        }
    }
}

/// The outcome of checking a database's schema record on open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaState {
    /// The database was empty; the current version has been stamped.
    Created,
    /// The database has the current version.
    Current,
    /// The database has an older version; the caller migrates it in place (FR-STO-02).
    Older(u32),
}

/// The value of the schema record: the version as a big-endian `u32`, then the database name.
/// The name keeps a different database whose column families happen to fit this schema from
/// being taken for this one. The layout is frozen: a later version must not append to it (older
/// builds would read a different name and report another database instead of
/// [`StoreError::SchemaTooNew`]); new metadata goes under other keys.
pub fn schema_record(schema: &DbSchema) -> Vec<u8> {
    let mut value = BeU32(schema.version).to_value_bytes();
    value.extend_from_slice(schema.name.as_bytes());
    value
}

/// Checks a stored schema record against `schema`, without writing anything: a record of another
/// database is [`StoreError::UnknownDatabase`], a newer version [`StoreError::SchemaTooNew`].
pub fn check_record(schema: &DbSchema, record: &[u8]) -> Result<SchemaState> {
    let Some((version, name)) = record.split_first_chunk::<4>() else {
        return Err(StoreError::Codec {
            cf: "meta",
            source: crate::codec::CodecError::Length {
                expected: 4,
                got: record.len(),
            },
        });
    };
    if name != schema.name.as_bytes() {
        return Err(StoreError::UnknownDatabase {
            db: schema.name,
            what: format!("its schema record names database `{}`", String::from_utf8_lossy(name)),
        });
    }
    let found = u32::from_be_bytes(*version);
    match found.cmp(&schema.version) {
        Ordering::Greater => Err(StoreError::SchemaTooNew {
            db: schema.name,
            found,
            supported: schema.version,
        }),
        Ordering::Less => Ok(SchemaState::Older(found)),
        Ordering::Equal => Ok(SchemaState::Current),
    }
}

/// Checks (or, for an empty database, stamps) the schema record. Refuses newer versions, other
/// databases and foreign data without writing anything.
pub fn check_schema<E: KvEngine>(engine: &E) -> Result<SchemaState> {
    let schema = engine.schema();
    if let Some(record) = engine.get(DbSchema::META, SCHEMA_KEY)? {
        return check_record(schema, &record);
    }
    for cf in schema.all_cfs() {
        if engine.iter_prefix(cf, &[]).next().transpose()?.is_some() {
            return Err(StoreError::UnknownDatabase {
                db: schema.name,
                what: "it holds data but no schema record".into(),
            });
        }
    }
    let mut batch = engine.batch();
    batch.put(DbSchema::META, SCHEMA_KEY, &schema_record(schema));
    engine.write(batch, Commit::Durable)?;
    Ok(SchemaState::Created)
}
