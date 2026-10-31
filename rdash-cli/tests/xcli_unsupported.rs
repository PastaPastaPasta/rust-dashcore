//! XCLI needs Unix (pseudo-terminals, signals, `libc`). Where it cannot be built, this test
//! says so in the test list instead of the differential gate vanishing without a trace.
//! Full parity is proven on Linux only; see `tests/xcli/main.rs`.

#![cfg(not(unix))]

#[test]
#[ignore = "XCLI cannot run on this platform (it needs Unix); full parity is proven on Linux only"]
fn xcli_not_run_on_this_platform() {
    eprintln!("XCLI is not run on this platform; full parity is proven on Linux only");
}
