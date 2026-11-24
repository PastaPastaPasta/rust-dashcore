# Changes from the internal build tree

This snapshot is the rdashd v0.47.0 integration tree that the rdashd testnet masternode runs (internal tree `4fefb3c6d00d055021f66d61a04dfc30184ffbfd`), with these publication edits:

- Removed `docs/node-program/` (267 files of internal planning notes). No code reads them.
- `dashd-chainstate/src/chain/tests/connector.rs`, `dashd-chainstate/src/chain/tests/failure.rs`: removed two internal doc-comment blocks above tests. Comments only; the tests are unchanged.
- Replaced an internal build-host name with "the build host" in comments, docs and provenance notes, and in the default emulator AVD name in `contrib/mobile/emulator-smoke.sh`:

  - `.github/workflows/ci-plan.yml`
  - `.github/workflows/node-gate.yml`
  - `.github/workflows/rust.yml`
  - `.github/workflows/series.yml`
  - `contrib/mobile/emulator-smoke.sh`
  - `contrib/node/cs2-diff/README.md`
  - `contrib/node/gate-rulings.toml`
  - `contrib/node/node-l1-nightly.sh`
  - `contrib/node/perfgate/README.md`
  - `contrib/node/perfgate/perfgate/build.py`
  - `contrib/node/pf1/run_window.py`
  - `contrib/node/sb4-core-scenarios/README.md`
  - `contrib/node/sb4-core-scenarios/regen.py`
  - `contrib/node/selftest-checks.sh`
  - `contrib/oracle-cli`
  - `dash-consensus/benches/REPORT.md`
  - `dash-dml/tests/data/evo_snapshot/PROVENANCE.md`
  - `dash-netaddr/src/numeric.rs`
  - `dash-oracle-sys/README.md`
  - `dash-oracle-sys/tests/data/PROVENANCE.md`
  - `dash-x11/README.md`
  - `dash-x11/contrib/extract-vectors.py`
  - `dashd-chainstate/src/crash/mod.rs`
  - `dashd-coinjoin/tests/fixtures/dashd-v24/PROVENANCE.md`
  - `dashd-node/tests/asserted_log_lines.rs`
  - `dashd-node/tests/v24/mod.rs`
  - `node-bench/README.md`
  - `node-bench/nodebench/noise.py`
  - `node-fuzz/README.md`
  - `node-fuzz/fuzzctl.py`
  - `node-l1/tests/data/proupserv-mainnet-1028257.json`
  - `test-vectors/lane-c/PROVENANCE.md`
  - `test-vectors/lq4/PROVENANCE.md`

No Rust source line outside comments changed, so the rdashd binary built from this tree is the same build.
