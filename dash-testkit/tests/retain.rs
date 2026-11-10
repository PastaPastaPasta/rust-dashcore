//! Failed starts and failing tests keep their node directories under `DASHD_TEST_RETAIN_DIR`, as
//! dash-spv's harness keeps its datadirs. One test per binary: it sets process environment.
#![cfg(unix)]

use std::path::Path;

use dash_testkit::{
    DashdTestContext, MixedNetwork, RustNode, RustNodeConfig, StartError, TestChain, TestDir,
};

/// Runs `case` on a thread named `name`, as the test harness names a test's thread; the
/// retained directories are under `<retain>/<name>`.
fn on_thread<T: Send + 'static, F: Future<Output = T>>(
    name: &str,
    case: impl FnOnce() -> F + Send + 'static,
) -> std::thread::Result<T> {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            let runtime =
                tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            runtime.block_on(case())
        })
        .unwrap()
        .join()
}

#[test]
fn failed_starts_and_failing_tests_retain_their_directories() {
    let retain = tempfile::tempdir().unwrap();
    // SAFETY: the only test in this binary, and no other thread is running yet.
    unsafe { std::env::set_var("DASHD_TEST_RETAIN_DIR", retain.path()) };

    a_panicking_test_retains_its_node(retain.path());
    a_refused_rdashd_retains_the_network_without_a_panic(retain.path());
    a_switched_dash_spv_start_that_panics_retains_its_datadir(retain.path());
}

fn a_panicking_test_retains_its_node(retain: &Path) {
    let failed = on_thread("failing_case", || async {
        let dir = TestDir::new("retained").unwrap();
        let _node =
            RustNode::start(RustNodeConfig::new(dir.subdir("node").unwrap())).await.unwrap();
        panic!("the test fails with its node running");
    });
    assert!(failed.is_err());

    let node = retain.join("failing_case").join("retained").join("node");
    let log = std::fs::read_to_string(node.join("regtest").join("debug.log")).unwrap();
    assert!(log.contains("init message: Done loading"), "{log}");
    assert!(node.join("rdashd.stderr").exists());
}

fn a_refused_rdashd_retains_the_network_without_a_panic(retain: &Path) {
    let err = on_thread("refused_case", || async {
        MixedNetwork::builder().rdashd(1).rdashd_args(["-printcrashinfo=x"]).start().await.err()
    })
    .unwrap()
    .expect("the refused rdashd fails the start");
    assert!(
        matches!(err.error, StartError::Refused { ref owner, .. } if owner == "unplanned"),
        "{err}"
    );
    let dir = retain.join("refused_case").join("mixed-network");
    assert_eq!(err.retained.as_deref(), Some(dir.as_path()));
    let stderr = std::fs::read_to_string(dir.join("rdashd-0").join("rdashd.stderr")).unwrap();
    assert!(stderr.contains("-printcrashinfo is not supported by rdashd yet"), "{stderr}");
}

/// dash-spv's switched suites panic inside `DashdTestContext`'s start, before the context that
/// retains the datadir exists. Today rdashd refuses the Core data directory (CH5).
fn a_switched_dash_spv_start_that_panics_retains_its_datadir(retain: &Path) {
    if std::env::var_os("SKIP_DASHD_TESTS").is_some() {
        eprintln!("Skipping the dash-spv case (SKIP_DASHD_TESTS is set)");
        return;
    }
    // Otherwise the harness panics before the start, and the case would blame retention.
    for var in ["DASHD_TEST_DATA", "RDASHD_PATH"] {
        assert!(std::env::var_os(var).is_some(), "{var} is not set (see dash-testkit/README.md)");
    }
    // SAFETY: no other thread is running.
    unsafe { std::env::set_var("DASHD_IMPL", "rdashd") };
    let failed = on_thread("switched_case", || async {
        drop(DashdTestContext::new(TestChain::Minimal).await);
    });
    // SAFETY: as above; the case's thread has ended.
    unsafe { std::env::remove_var("DASHD_IMPL") };
    assert!(
        failed.is_err(),
        "rdashd started on a Core data directory: CH5 landed, update this case"
    );

    let entries: Vec<_> = std::fs::read_dir(retain.join("switched_case"))
        .expect("the failed start was retained")
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    let [name] = entries.as_slice() else {
        panic!("expected one dashd-<port> directory, got {entries:?}");
    };
    assert!(name.starts_with("dashd-"), "{name}");
    assert!(retain.join("switched_case").join(name).join("regtest").exists());
}
