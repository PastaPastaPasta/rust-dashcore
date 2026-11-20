//! SB17 M3's mobile probe: an SPV sync through dash-spv-ffi's C functions, statically linked, on
//! an Android emulator or device (contrib/mobile/emulator-smoke.sh). It samples the process's
//! threads every millisecond and records VmRSS and anonymous memory (RssAnon + VmSwap) at its
//! phases and `/proc/self/io` writes, then writes one JSON object to `PROBE_JSON`. It asserts
//! nothing: the script checks the thread bound. Compare builds on the `*_anon_mib` fields; VmRSS
//! includes file pages and reclaim. `peak_anon_mib` is the sampler's largest reading, not a
//! high-water mark.
//!
//! Env: `PROBE_NETWORK` (`regtest|testnet|mainnet`), `PROBE_PEER` (`host:port`; empty = DNS),
//! `PROBE_DATA_DIR`, `PROBE_MNEMONIC`, `PROBE_SECONDS` (0 = stop 30 s after `on_sync_complete`,
//! and after `PROBE_MIN_REORGS` reorgs, waiting at most 120 s for them), `PROBE_MIN_REORGS`,
//! `PROBE_JSON` (required), `PROBE_MODE` (`emulator|device`), `PROBE_TARGET`, `PROBE_STAMP`.
//!
//! It runs on Linux and Android only (it reads `/proc` and finds the ext setter with `dlsym`);
//! elsewhere `main` prints a line and exits. RSS comes from this example build, which has the
//! dev-dependency features (`build_features` in the JSON: dash-spv's `test-utils`, key-wallet-ffi's
//! `keep-finalized-transactions`), so it overstates the shipped library's; threads are unaffected.

#[cfg(any(target_os = "linux", target_os = "android"))]
mod probe;

#[cfg(any(target_os = "linux", target_os = "android"))]
fn main() {
    probe::main();
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn main() {
    eprintln!("mobile_probe: linux/android only");
}
