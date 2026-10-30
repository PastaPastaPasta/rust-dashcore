# DW MG-03 — independent security and correctness review, Sol round 2

Target: detached worktree at `cde7e167` (`dw/mg-03-masternodes`); fix delta reviewed against `e8c21620`, with merge-only desktop changes excluded. I made no source edits, commits or pushes.

## Verdict: CHANGES REQUIRED

### Major 1 — `mn` setup errors still echo caller input

Location: `rust/bin/dwcli/src/main.rs:609-612`, `rust/bin/dwcli/src/masternodes.rs:37-40`, and `rust/crates/dw-engine/src/session.rs:229-236,863-867`.

Scenario: pass a secret-shaped string as `--peer` to `dwcli ... mn list-state`. `NetworkSession::open` embeds the supplied peer string in `EngineError::InvalidArgument`; `run` returns its `Display`; `report_setup_failure` copies it into the JSON `message` and `main` copies it to stderr. An invalid `--ca-cert` path is likewise embedded in the setup error. The fixed-text changes protect the ProTx hash, wallet id and SDK-credit paths, but D23's no-input-echo ruling does not hold for all `mn` errors. The integration test exercises secret-shaped subcommand arguments but not global setup arguments.

### Major 2 — an ambiguous vault write can restore an ID for the wrong key

Location: `rust/crates/dw-engine/src/masternodes/tracking.rs:332-355`; vault behavior at `rust/crates/dw-vault/src/vault.rs:1723-1776,637-642` and `rust/crates/dw-vault/src/file.rs:208-237`.

Scenario: attach voting key A while the synced list lacks the tracked masternode, then replace it with B, also `unchecked`. The caller clears A's ID and writes B to the vault. If the post-write read-back fails, `store_masternode_key` returns an error after B is in the vault's in-memory and disk files. A directory `fsync` error after rename can likewise leave B on disk, visible after restart. The caller restores A's ID on every error. Once the list entry appears with voting key A, the live ID comparison reports `matched` and `voting: true`, although the vault holds B. A crash strictly between the three ordered steps leaves no stale ID, but the failed-write restoration reintroduces the false held-role state. This is not covered by the current restart/rotation test.

### Major 3 — a shared payout to a known address is counted as a standard payout without an incomplete flag

Location: `rust/crates/dw-engine/src/masternodes/reads.rs:223-233,360-387,407-443`.

Scenario: wallet W owns a version-2 masternode paying address X and receives a version-3 shared masternode's share at X. W has no provider transaction or tracking row for the shared masternode. Each coinbase record at X is assigned to the version-2 row and included in `payouts_30d`; `unattributed_payouts` sees no unmatched generated address and returns false even though the synced list has a version-3 entry. The overview reports `payouts_incomplete: false` and attributes the shared reward to the standard row. The overview counts the output once, but it silently includes a payout D25 says is excluded and presents an exact-looking total. If the only version-3 masternode was removed and W only held its share, its recent reward is instead omitted with `payouts_incomplete: false` because the current-list check is false.

### Low 4 — a teardown failure can exit 1 after a success JSON line

Location: `rust/bin/dwcli/src/masternodes.rs:297-311` and `rust/bin/dwcli/src/main.rs:559-568,575-588,605-617`.

Scenario: `mn list-state` prints `{ok:true,...}` before `finish` shuts down the engine. If shutdown fails or misses its deadline, `finish` returns an error or abandons the engine and the process exits 1. The once flag prevents an error JSON line from replacing the success line. The debug build's `DWCLI_FAULT_INJECT=stall-spawn-fail` path can force this, and the same state can arise from a real teardown failure. A client that treats process failure as a failed `mn` call sees a success object.

## Disposition of the six round-1 findings

1. **Input echo:** the original ProTx-hash, wallet-id and SDK-credit leaks are addressed; Major 1 remains in the `mn` setup path. SDK diagnostic text is logged and the returned credits error is fixed text (`reads.rs:818-824`).
2. **Live attached-key checks:** reads now compare recorded IDs with current references, and `unchecked` voting keys hold no role (`reads.rs:263-338`, `records.rs:100-106`). Major 2 is a separate inconsistency when an ambiguous vault write restores an old ID.
3. **Shared masternodes:** the version-3 flag, explicit `unsupported` payout refusal, MG-03b split and incomplete marker are present. Major 3 shows the marker and exclusion are not reliable for a shared payout using a known address.
4. **JSON failures:** vault unlock, setup, parse, bare `mn`, and pre-line panic paths now emit one JSON line. Low 4 covers a failure after a success line.
5. **Label bound:** `set_label` now accepts only a tracked, loaded-wallet or synced-list masternode; otherwise it returns `not_found` or `list_unavailable` (`tracking.rs:267-298`). This closes the round-1 low finding as ruled.
6. **Wallet-set generation:** the session's sink is wrapped, and the three wallet-set events bump the generation and wake the event coalescer (`session.rs:420-424`, `follow.rs:194-232,336-374`). This closes the round-1 low finding as ruled.

## Pinned-library finding (facts only)

`rust/Cargo.lock` pins rust-dashcore-dashpay `d3d50520a`. Core v24 assigns provider transaction types 10–12 to dissolve, update share and update shared registrar (`src/primitives/transaction.h:39-41`); those types are unknown at the pin (`dash/src/blockdata/transaction/special_transaction/mod.rs:363-379`). Its ProRegTx/ProUpRegTx payload decoders still read the version-2 single `script_payout` layout (`provider_registration.rs:253-295`, `provider_update_registrar.rs:124-144`). A `block` network message decodes every transaction before producing `NetworkMessage::Block` (`dash/src/network/message.rs:537-560`). On any decode error, dash-spv's peer discards/resynchronizes the frame instead of delivering it (`dash-spv/src/network/peer.rs:559-600`); the block sync manager only accepts delivered `NetworkMessage::Block` (`dash-spv/src/sync/blocks/sync_manager.rs:69-82`). Thus a requested block with type 10–12 cannot be delivered through that path; a version-3 registration/update can fail in its old-layout decoder. This is a general block and wallet sync limit wherever such a block is requested, not only an MG-03 limit. I did not test a live version-3 block or propose a fix here.

## Validation

- With `CARGO_TARGET_DIR=/work/scratch/dw-mg03-r2-sol/target` (owner-only), `cargo test -p dw-engine masternodes` passed 23 selected unit tests and `cargo test -p dw-engine --test mn_masternodes_contract` passed 8 tests.
- Against the built `dwcli`, a dummy 64-hex input passed as invalid `--peer` and as a missing `--ca-cert` path produced one `setup` JSON error line and exit 1; the dummy input appeared in both stdout and stderr in each case (Major 1). `DWCLI_FAULT_INJECT=probe-spawn-fail` on `mn list-state` produced exit 1 with one `{ok:true}` JSON line (Low 4). No real secret was used.
- `cargo test -p dwcli` passed: 40 unit tests plus all integration groups, including `tests/mn_cli.rs` (1/1). The existing panic/teardown tests ran slowly under load but passed without reruns.
- With Dash Core v24.0.0-rc.2, `DASHCORE_DIR=~/.cache/dwd-dashcore/24.0.0-rc.2 DWD_PYTHON=~/.cache/dwd-dashcore/func-venv/bin/python DWD_FUNC_TMP=/work/scratch/dw-mg03-r2-sol/regtest DWCLI=/work/scratch/dw-mg03-r2-sol/target/debug/dwcli regtest/functional/run.sh dwd_masternodes.py --portseed=6277` passed. It covered the voting-key rotation/restart, unchecked vote before list sync, tracking/label/key deletion, coinbase payouts and maturity, and status transitions. It does not exercise an actual version-3 shared masternode or the ambiguous vault-write path.
- The detached source worktree remained clean. No source edits, commits or pushes were made.

Verdict: **CHANGES REQUIRED**.
