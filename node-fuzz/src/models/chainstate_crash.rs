//! `chainstate_crash` (CH2 §6.5, b3): one ST3a `sweep` case over `ChainSut` per input. The
//! input's first 8 bytes (little-endian, zero-padded) seed the workload of §6.4's operation
//! families (`dashd_chainstate::crash::chainstate_crash`, with or without the test subscriber), its
//! crash point, kind, fault and nested crash; a finding reports its invariant. The model does not
//! depend on the release; one leg.

use dashd_chainstate::crash::chainstate_crash;

use super::crash_case;
use node_l1::Divergence;

pub(super) fn check(data: &[u8]) -> Result<(), Divergence> {
    crash_case(data, &chainstate_crash)
}
