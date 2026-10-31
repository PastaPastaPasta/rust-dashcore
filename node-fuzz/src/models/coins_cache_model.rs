//! `coins_cache_model` (CH2 §6.5): the coins cache's tip and block overlays against
//! `RefCoinsCache`, a line-by-line port of Core's `CCoinsViewCache`, op by op, with the tagging
//! allocator checked against native Σ − R after every op (§6.7). The cache does not depend on the
//! release; one leg.

use dashd_chainstate::coins::model::{self, Mismatch};
use node_l1::Divergence;

pub(super) fn check(data: &[u8]) -> Result<(), Divergence> {
    model::coins_cache_model(data).map(drop).map_err(|Mismatch(field)| Divergence {
        field,
    })
}
