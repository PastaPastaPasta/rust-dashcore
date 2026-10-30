//! EV addendum §2.4, node column (LQ1a-B): a connected block's MN list missing from the node's
//! map-backed source is `MemberError::Missing(work)`, which the node reads as
//! `NodeMemberError::Store` (D4): no members, no verdict, no snapshot row, no cache insert. CH7b
//! repeats it through the production facade.

use std::collections::BTreeMap;
use std::sync::Arc;

use dash_consensus::ctx::{Deployment, DeploymentView};
use dash_consensus::{BlockId, ChainAncestry};
use dash_dml::testkit::{MemChain, numbered};
use dash_dml::{Dmn, MnList, NodeMemberError, StoreError};
use dash_evo::bls_ctx_after;
use dash_evo::cbtx::CoinbaseFacts;
use dash_evo::llmq::reference::{ModelMembersFacade, ModelRows};
use dash_evo::llmq::{
    CompleteChain, LlmqCtx, LlmqSource, MemberError, QuorumSnapshot, SnapshotRows,
};
use dash_params::{ChainOption, ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::{BlockHash, LlmqType};
use dash_types::Numeric;

/// Buried deployments at their heights on `chain`.
struct Buried<'a> {
    chain: &'a MemChain,
    params: &'a ChainParams,
}

impl DeploymentView for Buried<'_> {
    fn active_after(&self, prev: Option<BlockId>, deployment: Deployment) -> bool {
        let next = prev.map_or(0, |p| self.chain.header(p).height + 1);
        match deployment {
            Deployment::Buried(d) => next >= self.params.consensus.deployment_height(d).to_u32(),
            Deployment::Versionbits(_) => false,
        }
    }
    fn active_at(&self, block: BlockId, deployment: Deployment) -> bool {
        self.active_after(self.chain.prev(block), deployment)
    }
}

/// The node's lists by block, and blocks without transactions (no coinbase ChainLock).
struct MapSource(BTreeMap<BlockId, Arc<MnList>>);

impl LlmqSource<MnList> for MapSource {
    type Error = StoreError;
    fn mn_list(&self, at: BlockId) -> Result<Option<Arc<MnList>>, StoreError> {
        Ok(self.0.get(&at).cloned())
    }
    fn coinbase_facts(&self, _: BlockId) -> Result<Option<CoinbaseFacts>, StoreError> {
        Ok(Some(CoinbaseFacts::NoTransactions))
    }
}

/// The runtime rows; a write is visible.
#[derive(Default)]
struct Rows(ModelRows);

impl SnapshotRows for Rows {
    type Error = StoreError;
    fn get(&self, t: LlmqType, b: &BlockHash) -> Result<Option<QuorumSnapshot>, StoreError> {
        Ok(self.0.get(&(t.to_base(), *b)).cloned())
    }
    fn store(&mut self, t: LlmqType, b: &BlockHash, s: &QuorumSnapshot) -> Result<(), StoreError> {
        self.0.insert((t.to_base(), *b), s.clone());
        Ok(())
    }
}

#[test]
fn a_missing_connected_work_block_list_is_a_node_store_error() {
    let params = [
        (ChainOption::Dip3Params, "2:2"),
        (ChainOption::TestActivationHeight, "v20@40"),
        (ChainOption::TestActivationHeight, "mn_rr@40"),
    ]
    .into_iter()
    .fold(ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0), |b, (o, v)| {
        b.arg(o, v.to_string())
    })
    .build()
    .expect("regtest parameters");
    let mut chain = MemChain::new();
    let tip = chain.extend(chain.genesis(), 80);
    let deployments = Buried {
        chain: &chain,
        params: &params,
    };
    let view = CompleteChain::<StoreError>::new(&chain);
    let ctx = LlmqCtx::new(&params, &view, &deployments, bls_ctx_after(&deployments, Some(tip)))
        .expect("in domain");
    let base = chain.ancestor(tip, 72).expect("connected");
    let work = chain.ancestor(tip, 64).expect("connected");
    let lists: BTreeMap<BlockId, Arc<MnList>> = (1..=80)
        .map(|h| {
            let at = chain.ancestor(tip, h).expect("connected");
            let mut list = MnList::new(chain.hash(at), h as i32, 0);
            for i in 1..=9 {
                list.try_add(numbered(i), true).expect("distinct masternodes");
            }
            (at, Arc::new(list))
        })
        .collect();

    // Control: with every list held, LLMQ_TEST's three members.
    let mut facade = ModelMembersFacade::<Arc<Dmn>>::new(&params);
    let held = MapSource(lists.clone());
    let (members, _) = facade
        .clone()
        .members(&ctx, &held, &mut Rows::default(), LlmqType::LlmqTest, base, false)
        .expect("held");
    assert_eq!(members.len(), 3);

    // The work block's list missing: `Missing(work)`, the node's store error, and nothing else.
    let mut missing = lists;
    missing.remove(&work);
    let mut rows = Rows::default();
    let e = facade
        .members(&ctx, &MapSource(missing), &mut rows, LlmqType::LlmqTest, base, false)
        .expect_err("missing");
    assert_eq!(e, MemberError::Missing(work));
    assert!(matches!(
        NodeMemberError::from(e),
        NodeMemberError::Store(StoreError::Inconsistent { .. })
    ));
    assert!(rows.0.is_empty(), "no snapshot row");
    let base_cache = facade.base_cache().cache(LlmqType::LlmqTest).expect("configured");
    assert!(base_cache.is_empty(), "no members-cache insert");
}
