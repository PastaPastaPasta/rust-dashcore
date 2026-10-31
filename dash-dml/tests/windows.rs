//! The build's mainnet windows (`benches/data`) build Core's lists: the last one has Core's
//! `CanonicalMNListHash` (node-l1's window from the same start, EV2b D26 and D32), and for w1
//! the reference twin agrees with it. w2's twin (5,131 masternodes) takes minutes unoptimised;
//! node-l1's window compared it at every block and the generator at the end.

use dash_dml::codec::canonical_mn_list_hash;
use dash_dml::reference::xcheck;
use dash_dml::testkit::replay::{mainnet, payee_rules};
use dash_dml::testkit::window::Window;
use dash_dml::{MnList, NoTrace};

/// Builds `bytes`' 2,000 blocks: the window and its last list, at `height` with hash `hash`.
fn builds_cores_list(bytes: &[u8], height: i32, hash: &str) -> (Window, MnList) {
    let window = Window::decode(bytes).expect("the window");
    assert_eq!(window.blocks().len(), 2_000);
    let last = window.build(window.start().clone(), &mut NoTrace, |_, _| {}).expect("builds");
    assert_eq!(last.height(), height);
    assert_eq!(canonical_mn_list_hash(&last).to_string(), hash);
    (window, last)
}

#[test]
fn the_w1_window_builds_cores_list() {
    let (window, last) = builds_cores_list(
        include_bytes!("../benches/data/mainnet-dml-1028160-1030159.bin"),
        1_030_159,
        "2e6ac0121845496697ee5d80baa5fa30d38d2f833fa8fe04aee7353f620e6000",
    );
    let twin = window.twin_start();
    let twin = window.build_ref(twin).expect("the twin builds");
    xcheck(&last, &twin, payee_rules(mainnet(), last.height())).expect("the twin agrees");
}

#[test]
fn the_w2_window_builds_cores_list() {
    builds_cores_list(
        include_bytes!("../benches/data/mainnet-dml-1371028-1373027.bin"),
        1_373_027,
        "c65d020d929c74508afdba3c5fcedaba1ff5bc16488313c5199c010e35e6539e",
    );
}
