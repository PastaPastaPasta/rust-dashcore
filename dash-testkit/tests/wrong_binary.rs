//! `DASHD_IMPL=rdashd` with Dash Core at `RDASHD_PATH`: dash-spv's harness must refuse it, not
//! run the suites Core against Core. One test per binary: it sets process environment.
#![cfg(unix)]

use dash_testkit::{DashdTestContext, NodeImpl, TestChain};

#[test]
fn dash_core_at_rdashd_path_is_refused_by_dash_spv_harness() {
    if std::env::var_os("SKIP_DASHD_TESTS").is_some() {
        eprintln!("Skipping (SKIP_DASHD_TESTS is set)");
        return;
    }
    let dashd = NodeImpl::Dashd.binary().unwrap_or_else(|missing| panic!("{missing}"));
    assert!(std::env::var_os("DASHD_TEST_DATA").is_some(), "DASHD_TEST_DATA is not set");
    // SAFETY: the only test in this binary, and no other thread is running yet.
    unsafe {
        std::env::set_var("DASHD_IMPL", "rdashd");
        std::env::set_var("RDASHD_PATH", &dashd);
    }
    let started = std::thread::spawn(|| {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async { drop(DashdTestContext::new(TestChain::Minimal).await) });
    })
    .join();

    let panic = started.expect_err("Dash Core started as rdashd");
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or_default();
    assert!(
        message.contains("is not Rdashd") && message.contains("`rdashd version `"),
        "{message}"
    );
}
