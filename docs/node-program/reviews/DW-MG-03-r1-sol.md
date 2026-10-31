# DW MG-03 — independent security and correctness review, Sol round 1

Target: detached worktree at `e8c21620` (`dw/mg-03-masternodes`), with the MG-03 design, Contract-Version 1, MG-00 rows, ROADMAP, pinned library code and Dash Core v24 checked. No source edits were made.

## Verdict: CHANGES REQUIRED

### Major 1 — A 64-hex secret pasted as a ProTx hash is echoed in errors

Location: `rust/crates/dw-engine/src/masternodes/mod.rs:64-71`, `rust/crates/dw-engine/src/masternodes/tracking.rs:134-140,191-194`, `rust/crates/dw-engine/src/masternodes/errors.rs:19-24`, and `rust/bin/dwcli/src/masternodes.rs:113-119,253-260`.

Scenario: a user accidentally puts a 64-hex private key in `mn untrack <hash>` or `mn track <hash>`. Because it is valid hex, `parse_pro_tx_hash` accepts and reverses it. The lookup fails, and the error's `Display` includes the same 64-hex string. I reproduced `mn untrack` with a dummy secret: the value appeared in both stdout JSON and stderr. This also applies to facade errors from `masternode`, `payouts` and `attach_key` on unknown hashes. The malformed-input guard prevents only invalid hex from being echoed; it does not protect an operator or other secret of hash length pasted by mistake. Stop including caller-supplied hash text in error messages, and test valid-length secret-shaped input at the facade and CLI boundaries. Preserve stable error codes.

### Major 2 — An attached voting key stays reported as a live role after key rotation

Location: `rust/crates/dw-engine/src/masternodes/reads.rs:227-255`; the only key comparison is at attach time in `rust/crates/dw-engine/src/masternodes/tracking.rs:102-110,267-274`.

Scenario: track a listed masternode and attach its current voting key (`matched`). Its owner later changes the voting key with ProUpRegTx. The SPV list now supplies the new key ID, but `attached_roles` returns only the stored role name and `roles()` ORs that name into `voting: true`. After a restart it still reports `voting: true`, although the stored secret cannot vote for the live list entry. The same false positive occurs if a key was attached as `unchecked` while the tracked entry was absent, then the entry appears with a different key. MG-00 D13 calls for a live comparison, and the facade gives a future UI no key ID or check state with which to make it. Store or derive a non-secret identifier for an attached voting key, compare it to every current list entry, and return a distinct mismatch or unchecked state. Test a voting-key rotation and restart.

### Major 3 — v24 shared payout and refund roles cannot be represented

Location: `rust/crates/dw-engine/src/masternodes/aggregate.rs:51-65,92-125`, `rust/crates/dw-engine/src/masternodes/reads.rs:167-191,245-249`, and `rust/crates/dw-engine/src/masternodes/records.rs:125-147`. The pinned rust-dashcore `ProviderRegistrationPayload` also has only `script_payout`; Core v24 `src/evo/providertx.h:129-211` uses `payouts` and `shares` for version 3.

Scenario: a v24 shared masternode names this wallet in one payout share or collateral refund share, while its top-level owner and voting keys belong to someone else. The facade can read only one `payout_script` and one `owner_key_hash`, does not aggregate ProUpShareTx / ProUpSharedRegTx, and cannot expose the share table. It can omit the masternode from Mine, omit this wallet's reward from `payouts()` and the 30-day total, and cannot report the promised multiple-payee split. This fails MG-00 D1 and D28, both cited as MG-03 inputs; D42/D43 are explicitly deferred, but D1/D28 are not. Add version-3 decoding and a per-share record/ownership model, or explicitly revise the MG-03 scope and contract before calling this acceptance complete. Cover a version-3 shared registration and share update against Core.

### Major 4 — Vault unlock failures bypass the `mn` JSON error contract

Location: `rust/bin/dwcli/src/main.rs:523-526` and `rust/bin/dwcli/src/masternodes.rs:245-266`.

Scenario: with an encrypted vault and a wrong `--passphrase-file`, `unlock_if_needed(...)?` returns before `masternodes::run`; `dwcli mn list-state` exits 1 with only `error: vault: wrong passphrase (1 failed)` on stderr and no JSON on stdout. I reproduced this with a fresh private regtest data directory. A client consuming the documented one-line `{ok:false,error:{code,message}}` protocol cannot parse this failure. Report unlock and other command setup failures through the same `mn` JSON path, while preserving the generic, non-echoing vault error.

### Low 5 — Labels can grow app.sqlite for hashes with no masternode

Location: `rust/crates/dw-engine/src/masternodes/tracking.rs:220-247` and `rust/crates/dw-engine/src/masternodes/follow.rs:112-129`.

Scenario: a caller submits many distinct 64-hex hashes to `set_label`; the untracked branch writes a setting for each without checking owned, tracked or listed membership. Every subsequent overview and follower recomputation loads all `masternodes.` settings into memory. The 64-character per-label bound limits one value, not the number of persistent rows. This is a local API, currently without a renderer binding, so I would not block on this alone. Reject unknown hashes or impose a bounded count before exposing it through IPC.

### Low 6 — A wallet added at an unchanged list height does not trigger the promised event

Location: `rust/crates/dw-engine/src/masternodes/follow.rs:185-189,238-279`; `local_change()` is called only by tracking methods in `tracking.rs:185,210,247,274,291`.

Scenario: the follower has processed height H, then a wallet containing a ProRegTx is loaded while the chain remains at H. Its interest set changes, but neither `height` nor `generation` changes, so every 5-second tick returns before recomputing it. `list()` can see the masternode, but `EngineEvent::Masternodes` is absent until a new block or a tracking mutation, contrary to contract §6's wallet-set change case. Bump the generation on wallet load/import/removal, or let the follower observe a wallet-set generation. A paused regtest chain makes this deterministic.

## Validation and limits

- The Dash Core v24 functional suite `DWD_FUNC_TMP=/work/scratch/dw-mg03-r1-sol/regtest ... regtest/functional/run.sh dwd_masternodes.py --portseed=6123` passed. It checked standard ProRegTx roles, external and internal collateral, key attachment/deletion, coinbase payouts and maturity, and `enabled → pose_banned → removed`. Its cases do not rotate an attached voting key or register a version-3 shared masternode.
- I read the supplied T2 evidence: `dwcli mn credits` returned a testnet evonode balance and a `duffs = credits / 1000` result. I did not repeat the live testnet read.
- A fresh encrypted vault plus wrong `--passphrase-file` reproduced Major 4: exit 1, no stdout JSON, generic stderr error. A dummy 64-hex secret passed as `mn untrack` reproduced Major 1: the value appeared in stdout JSON and stderr. No real keys or secrets were used.
- With private `CARGO_TARGET_DIR=/work/scratch/dw-mg03-r1-sol/target`, `cargo test -p dw-engine masternodes` passed 14/14 selected unit tests, `cargo test -p dw-engine --test mn_masternodes_contract` passed 7/7, and `cargo build -p dwcli` succeeded. The detached source worktree remained clean.
- The facade and CLI offer only owner, voting and payout attachment roles, and the vault's allowed roles match; no operator key is stored or used for signing. A payout key whose public key differs from the wallet's last known payout script is deliberately stored as `unchecked` under D19; that state is not a match assertion. The wrong-passphrase vault error did not echo the attempted passphrase. `IdentityBalance::fetch` uses the pinned SDK's proof-parsing path; no unproved tracked-registration snapshot is used for credits or payouts. The follower's `Drop` aborts its tasks if session open fails after construction.
- The design explicitly defers the D42 full ProTx update watch and D43 tracked-collateral backfill; this review does not count their declared `payout_checked_height: None` and `collateral_lookup` refusal as new findings. The one-address payout matcher also cannot reconstruct earlier payouts after a payout-address change; callers must continue to treat its address as unverified while D42 remains open.
