# combined-SDK probe

Does dash-spv-ffi link into one Android library beside a Platform-style
`rusqlite` with `bundled` (SB0 §8.1)? The probe is a cdylib on both, with its
own `[workspace]`, so it is outside rust-dashcore's workspace, lockfile and CI.

    source contrib/mobile/android-env.sh
    cargo build --manifest-path contrib/mobile/combined-sdk-probe/Cargo.toml \
        --target aarch64-linux-android --profile mobile

While Platform's rusqlite major and dash-kv-sqlite's differ (DEC-1468), the
expected result is Cargo's `links = "sqlite3"` resolver error naming both
`libsqlite3-sys` versions: record that error, it is the result. No lockfile is
committed, because none can be resolved until then. Once it resolves, record
the probe's stripped size minus dash-spv-ffi's and SQLite's.

It names no base SDK `[patch]`, so base SDK resolves from the upstream
revision rust-dashcore's root declares and contrib/node/base_sdk_pins.py and
bump-base-sdk.sh do not treat it as a pinned workspace. `profile.mobile` here
repeats the root's top-level settings only.
