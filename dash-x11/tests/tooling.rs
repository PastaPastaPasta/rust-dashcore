//! The test tooling's own checks.

/// `contrib/differential.sh` passes only when every scheduled run left a passing log
/// (`contrib/test-differential.sh` drives it with stub test binaries).
#[cfg(unix)]
#[test]
fn differential_script_verdicts() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/contrib/test-differential.sh");
    let out = std::process::Command::new("bash").arg(script).output().expect("bash runs");
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
