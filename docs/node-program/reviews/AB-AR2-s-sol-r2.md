# AB-AR2-s review — round r2

Verdict: **APPROVE**

Reviewed `node/ab-ar2-s` at `267d46864c4a6d7bbcb367d30f7a35d8f0f3f5ed`, the single follow-up commit `267d4686` (`fix(dash-rpc-spec): prove the dashd is ours, check both wallet modes`) on `e18345b956226cc5af27d9b52d14a09232cb226b`. Scope is confirmation of r1 Major 1 and Minor 2, including the requested ownership race. This is not a new full review of AR2. Followed AB-REVIEW.md, REVIEW-COMMON.md, and AB-AR2.md. All experiments used a detached worktree under `/work/scratch/ab-review-ar2-s-r2/`, with `TMPDIR=/dev/shm/ab-review-ar2-s-r2-tmp` (mode 700), `CARGO_BUILD_JOBS=6`, and the genuine v24.0.0-rc.3 release binary at `/work/scratch/bs4a-decode-domain/dashd/dashcore-24.0.0-rc.3/bin/dashd`. Core reference: tag v24.0.0-rc.3, commit `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`.

## Findings

No remaining findings in the requested scope. **Major 1 and Minor 2 are closed.**

### r1 Major 1 — closed: endpoint ownership

Relevant locations: `dash-rpc-spec/tests/dashd_help.rs:103`, `:229`, `:263`, `:454`; `dash-rpc-spec/generator/regenerate.sh:52`, `:58`, `:88`, `:112`; `dash-rpc-spec/generator/owned_node.py:53`; `dash-rpc-spec/generator/gen_spec.py:773`.

Both launchers use a per-run password and explicitly bind the queried IPv4 loopback address with both `-rpcbind` and `-rpcallowip`. Readiness checks the responding daemon's `getrpcinfo.logpath` against the fresh datadir and checks child liveness before and after that RPC. Rejected attempts dispose of the tracked child and retry. Shutdown targets that child PID rather than issuing RPC `stop`. The generator creates its wallet only after readiness and independently checks ownership again before collecting data. The Rust comparison no longer issues `createwallet` at all; wallet-enabled help remains comparable without loading a wallet.

Independent rerun of the r1 attacks, on reviewer-owned daemons:

- **Rust:** start a genuine incumbent on port 41430 with the old `spec:spec` credentials and no wallets; change only the detached test's `free_port()` to return 41430; use a wrapper which answers `-version` immediately but delays startup by 30 seconds. The wallet-enabled live test now **fails as expected, exit 101**, after all three attempts report `dashd exited before it was ready`. It does not produce a false pass. The incumbent remains alive and `listwallets` remains `[]`.
- **Generator:** start an incumbent on 41432 with the old `u:p` credentials and no wallets; run the unmodified `regenerate.sh` with the same delayed wrapper and first port 41432. Its own bind fails, it retries on 41433, and generation **exits 0** against its own node with wallet `spec`. The incumbent remains alive with `listwallets == []`. `git diff --exit-code` confirms byte-identical `src/generated.rs` and vendored `client.cpp` (289 commands, 347 conversion rows).
- The committed `a_foreign_dashd_on_the_port_is_never_used` test **passes 1/1**, exercising both the old credentials and an incumbent given the launcher's exact password. The latter is rejected by its different log path. The three Python ownership tests also pass.

**Ownership race:** Core's `src/httpserver.cpp:395–436` confirms these options yield one binding endpoint; a live child holding that endpoint prevents a second daemon from binding it. In a temporary reviewer test on port 41440, first start and establish readiness of our real daemon. Inside `ready()`, immediately after the child-liveness check but before `getrpcinfo`, kill/reap that child and start a genuine replacement on the same port with the **same password** and a different datadir. The replacement starts with zero wallets; readiness rejects it with `is not our dashd`. This race experiment **passes 1/1**. If instead the ownership response comes from our child and it exits during the probe, the post-response child check rejects readiness. No ownership race was demonstrated in the requested collision/turnover scenario.

### r1 Minor 2 — closed: wallet-disabled live coverage

Relevant locations: `dash-rpc-spec/tests/dashd_help.rs:312`, `:320`, `:329`, `:346`, `:369`, `:417`; `dash-rpc-spec/tests/table.rs:89`; `dash-rpc-spec/src/lib.rs:113`, `:160`.

The committed suite compares both wallet modes, derives registration from `Command::registered(wallet)` and accounts separately for build options. It asserts that `-disablewallet` took effect, compares unavailable commands' unknown-command help, and keeps `getcoinjoininfo`, `protx info`, and `protx list` in the disabled surface. Fresh live results:

- Wallet enabled: **289/289** commands' help equal, listing equal, **417** argument-map rows equal.
- Wallet disabled: **180/180** available commands' help equal, **109/109** wallet-only commands unknown, listing equal, **242** argument-map rows equal; all three hybrids remain.
- Repeat the exact r1 registration mutation: change only `abandontransaction` from `Registration::Wallet` to `Registration::Node`. The disabled-wallet live test now **fails as expected, exit 101**, with `the dashd does not know "abandontransaction"`. The former blind spot is covered.

These live comparisons cover more than ten named commands. Source spot checks also covered `abandontransaction`, `getbalance`, `listwallets`, `createwallet`, `getwalletinfo`, `sendtoaddress`, `walletpassphrase`, `getblockcount`, and the three hybrids. Core's `src/wallet/init.cpp:182–187` confirms the disabled-wallet loader path.

## Fresh gates and limits

- `cargo fmt --all --check`: pass.
- `cargo clippy -p dash-rpc-spec --all-features --all-targets -- -D warnings`: pass.
- `cargo test -p dash-rpc-spec --all-features -- --nocapture`, with `DASHD_V24_PATH` set and no skip flag: **12 passed / 0 failed** (3 live tests, 9 table tests; no unit/doc tests).
- `python3 -I -m unittest discover -s dash-rpc-spec/generator -p test_gen.py`: **13 passed / 0 failed**.
- Independent attack, turnover, and mutation experiments behaved as described above. All temporary source changes were restored; the detached worktree had no diff afterwards.

The version check now uses a fresh throwaway datadir and a `OnceLock` in the Rust test, avoiding concurrent `settings.json` writes to the user's default datadir. The generator likewise checks the version in its own scratch datadir. Fresh parallel live tests passed.

The claimed full-workspace gate (3139 passed, BL-20 rerun 54/0) was not independently repeated in this narrow confirmation. Other r1 limits remain outside this round's scope. Linux was exercised; no new platform claim is made.

Reviewer-owned daemons were stopped. The detached worktree, scratch, and temporary datadirs were removed. This report is uncommitted.

Verdict: **APPROVE**

blockers=0 majors=0 minors=0 nits=0
