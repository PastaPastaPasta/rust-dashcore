//! The node's list entry seen through `dash_evo`'s views from another crate (EV addendum §1.3,
//! §3 commit 3): `impl MnEntryView for Dmn` lives here, `Arc<Dmn>` is an entry through
//! `dash_evo`'s forwarding impl, and shared generic code reads the `Dmn`'s own fields.

use std::sync::Arc;

use dash_dml::testkit::{dmn, pro_tx};
use dash_dml::{Dmn, MnList, MnType, ProTxHash};
use dash_evo::llmq::{MnEntryView, MnListView};
use dash_primitives::OutPoint;

/// Shared code, generic over the entry as LQ1's algorithms are.
fn fields<E: MnEntryView>(e: &E) -> (ProTxHash, OutPoint, bool, bool, bool, [u8; 32]) {
    (
        *e.pro_tx_hash(),
        *e.collateral(),
        e.is_evo(),
        e.is_banned(),
        e.confirmed_hash_is_null(),
        *e.confirmed_hash_with_protx(),
    )
}

/// A view-generic count, as LQ1's enabled filter reads a list.
fn valid_entries<L: MnListView>(list: &L) -> Vec<ProTxHash> {
    let mut out = Vec::new();
    list.for_each(true, &mut |e| out.push(*e.pro_tx_hash()));
    out
}

#[test]
fn an_arc_dmn_reads_the_dmns_fields_through_the_shared_views() {
    let mut banned = (*dmn(2, MnType::Evo)).clone();
    banned.hot.ban_if_not_banned(10);
    let banned: Arc<Dmn> = Arc::new(banned);
    let regular = dmn(1, MnType::Regular);

    assert_eq!(fields(&banned), fields(&*banned));
    assert_eq!(fields(&banned), (pro_tx(2), banned.collateral, true, true, true, [0u8; 32]));
    assert_eq!(fields(&regular), (pro_tx(1), regular.collateral, false, false, true, [0u8; 32]));

    let mut list = MnList::default_pre_dip3();
    list.insert_for_tests(Arc::clone(&regular));
    list.insert_for_tests(banned);
    assert_eq!(valid_entries(&list), [pro_tx(1)]);
}
