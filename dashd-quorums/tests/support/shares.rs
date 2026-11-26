//! The in-memory store, quorum lookup and seeded RNG the sig-share runtime tests wire the real
//! signing runtime with (`sg1b_runtime.rs`, `lane_c_sigshares.rs`).

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::Arc;

use dash_llmq::sigenv::QuorumLookup;
use dash_llmq::sigenv::fakes::{FakeLookup, FakeQuorum};
use dash_llmq::signing::db::{RecSigBatch, RecSigKv, RecSigOp, RecSigScan};
use dash_llmq::sigshares::verify::WeightedVerifier;
use dash_params::LlmqType;
use dash_primitives::QuorumHash;
use dashd_quorums::signing::SigningLookup;

#[derive(Default)]
pub struct Mem(BTreeMap<Vec<u8>, Vec<u8>>);

impl RecSigKv for Mem {
    type Error = Infallible;

    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self.0.get(key).cloned())
    }

    fn scan_from(&self, start: &[u8]) -> Result<RecSigScan<'_, Self::Error>, Self::Error> {
        Ok(Box::new(self.0.range(start.to_vec()..).map(|(k, v)| Ok((k.clone(), v.clone())))))
    }

    fn write(&mut self, batch: RecSigBatch) -> Result<(), Self::Error> {
        for op in batch.ops {
            match op {
                RecSigOp::Put(k, v) => {
                    self.0.insert(k, v);
                }
                RecSigOp::Delete(k) => {
                    self.0.remove(&k);
                }
            }
        }
        Ok(())
    }
}

pub struct Lookup(pub FakeLookup);

impl QuorumLookup for Lookup {
    type Quorum = FakeQuorum;
    type Error = Infallible;

    fn get_quorum(
        &self,
        t: LlmqType,
        h: &QuorumHash,
    ) -> Result<Option<Arc<FakeQuorum>>, Infallible> {
        self.0.get_quorum(t, h)
    }

    fn is_quorum_active(&self, t: LlmqType, h: &QuorumHash) -> Result<bool, Infallible> {
        self.0.is_quorum_active(t, h)
    }

    fn select_quorum_for_signing(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        height: i32,
        offset: i32,
    ) -> Result<Option<Arc<FakeQuorum>>, Infallible> {
        self.0.select_quorum_for_signing(t, id, height, offset)
    }
}

impl SigningLookup for Lookup {
    fn base_height(&self, _: &FakeQuorum) -> i32 {
        0
    }
}

pub struct Xs(pub u64);

impl rand_core::TryRng for Xs {
    type Error = Infallible;
    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        Ok((self.try_next_u64()? >> 32) as u32)
    }
    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        Ok(self.0)
    }
    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Infallible> {
        for b in dst {
            *b = self.try_next_u64()? as u8;
        }
        Ok(())
    }
}

impl rand_core::TryCryptoRng for Xs {}

pub fn verifier() -> WeightedVerifier<Xs> {
    WeightedVerifier::new(Xs(9))
}
