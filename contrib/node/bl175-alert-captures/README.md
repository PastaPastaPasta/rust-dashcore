# BL-175 alert captures

Ground truth for Dash Core v24.0.0-rc.3's unknown-versionbits warning (`UpdateTip`, `DoWarning`,
`AlertNotify`, `validation.cpp:1995-2013`, `:3150-3236`). A regtest dashd mines blocks that
signal bits no deployment uses, and the capture records:

- the `warnings` field of three RPCs, `getblockchaininfo`, `getnetworkinfo` and
  `getmininginfo`, which is `GetWarnings(false)` (`warnings.cpp:36-65`);
- the command line `-alertnotify` ran;
- each `UpdateTip` line's ` warning='...'` suffix.

dashd-node's `node::tests::bl175::unknown_rules_warn_and_alert_as_dashd_v24_does` replays the same
blocks through V with the same `-alertnotify` and compares all three.

## Rerun

```
BL175_DASHD=~/.rust-dashcore-test/dashcore-24.0.0-rc.3/bin/dashd \
    python3 -I contrib/node/bl175-alert-captures/capture.py <work-dir> dashd-node/tests/data/bl175
```

- `<work-dir>` is wiped and receives the datadir and its `debug.log`.
- `BL175_RPC_PORT` (default 31975) is the RPC port; the node does not listen.
- `capture.py`'s docstring has the schedule and the output format.

The run is deterministic: the mock time is fixed and every coinbase pays `raw(51)`. The
committed capture is from 2026-10-20, made with the release dashd from
`dashcore-24.0.0-rc.3-x86_64-linux-gnu.tar.gz`, as fetched by `setup-dashd.py` (dashd sha256
`e204e316…a7ab5bb4`), on a host with glibc 2.43. A second run gave the same blocks. Nothing in
it was edited by hand.

## What it shows

Regtest's warning window is 144 blocks, with a threshold of 108 and `MinBIP9WarningHeight` 0.

- **Bit 22** is LOCKED_IN from tip 287. Tips 287–430 log
  ` warning='Unknown new rules activated (versionbit 22)'`.
- **Tip 431** turns bit 22 ACTIVE. Its line has no suffix: `DoWarning` sets the misc warning
  instead and runs `-alertnotify` once.
- **Bit 23** is signalled in 107 blocks of the period 144–287, and never warns.

The `-alertnotify` command is `sh '<dir>/record.sh' '<dir>/alert.txt' %s %s; true`.

- Both `%s` become the message in single quotes, after `SanitizeString` (which leaves this
  message unchanged).
- glibc's `system` runs it as `sh -c -- <command>`: the `--` is glibc 2.38's (older ones pass
  none), so the test compares the record without it. The record needs `/proc`: the test runs
  on Linux only.
- `record.sh` writes that argv, then the two arguments the shell passed it.
