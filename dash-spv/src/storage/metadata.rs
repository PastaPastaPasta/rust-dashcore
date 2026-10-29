use crate::{error::StorageResult, storage::handle::SpvStore, StorageError};
use async_trait::async_trait;
use dashcore::prelude::CoreBlockHeight;

/// Metadata key for persisting the best known peer height.
const LAST_TARGET_HEIGHT_KEY: &str = "last_target_height";

#[async_trait]
pub trait MetadataStorage: Send + Sync + 'static {
    async fn store_metadata(&mut self, key: &str, value: &[u8]) -> StorageResult<()>;

    async fn load_metadata(&self, key: &str) -> StorageResult<Option<Vec<u8>>>;
    /// Persist the last target height to metadata storage.
    async fn store_last_target_height(&mut self, height: CoreBlockHeight) -> StorageResult<()>;
    /// Load the last target height from metadata storage.
    async fn load_last_target_height(&self) -> StorageResult<CoreBlockHeight>;
}

/// The legacy metadata values, in the SPV store's `meta` table (SB13 D6).
pub struct PersistentMetadataStorage {
    store: SpvStore,
}

impl PersistentMetadataStorage {
    pub(crate) fn new(store: SpvStore) -> Self {
        Self {
            store,
        }
    }
}

#[async_trait]
impl MetadataStorage for PersistentMetadataStorage {
    async fn store_metadata(&mut self, key: &str, value: &[u8]) -> StorageResult<()> {
        let (key, value) = (key.to_owned(), value.to_vec());
        self.store.run(move |s| s.put_metadata(&key, &value)).await
    }

    async fn load_metadata(&self, key: &str) -> StorageResult<Option<Vec<u8>>> {
        let key = key.to_owned();
        self.store.run(move |s| s.metadata(&key)).await
    }

    /// Persist the last target height to metadata storage.
    async fn store_last_target_height(&mut self, height: CoreBlockHeight) -> StorageResult<()> {
        match serde_json::to_vec(&height) {
            Ok(converted) => self.store_metadata(LAST_TARGET_HEIGHT_KEY, &converted).await,
            Err(e) => {
                let error = format!("Failed to serialize last target height: {}", e);
                tracing::warn!(error);
                Err(StorageError::Serialization(error))
            }
        }
    }

    /// Load the last target height from metadata storage. Used by the block headers manager to
    /// restore progress after restart.
    async fn load_last_target_height(&self) -> StorageResult<CoreBlockHeight> {
        match self.load_metadata(LAST_TARGET_HEIGHT_KEY).await {
            Ok(Some(bytes)) => match serde_json::from_slice::<CoreBlockHeight>(&bytes) {
                Ok(last_target_height) => {
                    tracing::debug!("Restored last target height {}", last_target_height);
                    Ok(last_target_height)
                }
                Err(e) => {
                    let error = format!("Failed to deserialize last target height: {}", e);
                    tracing::warn!(error);
                    Err(StorageError::Serialization(error))
                }
            },
            Ok(None) => {
                let error = "No last target height found (fresh start)".to_string();
                tracing::debug!(error);
                Err(StorageError::NotFound(error))
            }
            Err(e) => {
                let error = format!("Failed to load last target height: {}", e);
                tracing::warn!(error);
                Err(StorageError::Corruption(error))
            }
        }
    }
}
