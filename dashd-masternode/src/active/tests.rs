use dash_args::ArgsManager;

use super::*;

// Core: active/masternode.cpp:67-110 at v24.0.0
#[test]
fn core_state_and_status_table() {
    let rows = [
        (State::WaitingForProtx, "WAITING_FOR_PROTX", "Waiting for ProTx to appear on-chain"),
        (State::PoseBanned, "POSE_BANNED", "Masternode was PoSe banned"),
        (State::Removed, "REMOVED", "Masternode removed from list"),
        (State::OperatorKeyChanged, "OPERATOR_KEY_CHANGED", "Operator key changed or revoked"),
        (State::ProtxIpChanged, "PROTX_IP_CHANGED", "IP address specified in ProTx changed"),
        (State::Ready, "READY", "Ready"),
        (State::SomeError, "ERROR", "Error. detail"),
    ];
    let args = ArgsManager::new();
    args.force_set_arg("-masternodeblsprivkey", &"01".repeat(32));
    let active = ActiveMasternode::new(parse(&args).unwrap().unwrap());
    assert_eq!(active.pro_tx_hash(), None);
    assert_eq!(active.snapshot(), ActiveState::default());
    for (state, name, status) in rows {
        *active.st.write() = ActiveState {
            state,
            error: "detail".into(),
            pro_tx_hash: Some([42; 32]),
            ..Default::default()
        };
        assert_eq!(active.state_string(), name);
        assert_eq!(active.status(), status);
        assert_eq!(active.pro_tx_hash(), Some([42; 32]));
    }
}
