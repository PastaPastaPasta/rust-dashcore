//! BL-755: `setmnthreadactive`'s `NodeNet::set_masternode_thread_active` through a regtest
//! node's own slot sets `NetCore`'s masternode-loop flag and answers its new value; without the
//! network it is `None`. That the answer is the getter's read, not the argument, is not
//! observable here (both agree); the adapter is reviewed for it (RP1a-notes D78).
// Core: rpc/net.cpp:1122-1124; net.h:1325-1326

use dashd_rpc::peers::NodeNet;

use super::amb::node;
use crate::rpc::NetSlot;

#[test]
fn the_slot_sets_and_reads_back_the_masternode_thread_flag() {
    let dir = tempfile::tempdir().unwrap();
    let node = node(dir.path(), &["-connect=0", "-dnsseed=0", "-fixedseeds=0", "-listen=0"]);
    let slot = node.rpc_net();
    let core = slot.core().expect("attached");
    assert!(core.masternode_thread_active(), "true at start");
    let net: &dyn NodeNet = &*slot;
    for active in [false, true, false] {
        assert_eq!(net.set_masternode_thread_active(active), Some(active));
        assert_eq!(core.masternode_thread_active(), active);
    }
    assert_eq!(NodeNet::set_masternode_thread_active(&NetSlot::default(), true), None);
}
