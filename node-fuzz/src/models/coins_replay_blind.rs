//! `coins_replay_blind` (CH2 §6.5, D-c6-10): one ST3a `sweep` case over `ChainSut` per input, from
//! `dashd_chainstate::crash::coins_replay_blind`'s windows of connects, reorgs and
//! connect-then-disconnect inside one flush, crashed on a seeded subset of the pending batches and
//! judged against the reference recomputed from the block list. The input seeds it as
//! `chainstate_crash`'s does. One leg.

use dashd_chainstate::crash::coins_replay_blind;

use super::crash_case;
use node_l1::Divergence;

pub(super) fn check(data: &[u8]) -> Result<(), Divergence> {
    crash_case(data, &coins_replay_blind)
}
