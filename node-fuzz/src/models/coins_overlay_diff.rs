//! `coins_overlay_diff` (CH2 §6.5): one block overlay over a random parent (active entries with
//! random flags, F, the database) against a child `RefCoinsCache` over the tip twin, then its
//! commit. The cache does not depend on the release; one leg.

use dashd_chainstate::coins::model::{self, Mismatch};
use node_l1::Divergence;

pub(super) fn check(data: &[u8]) -> Result<(), Divergence> {
    model::coins_overlay_diff(data).map(drop).map_err(|Mismatch(field)| Divergence {
        field,
    })
}
