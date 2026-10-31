# DW E0-13 round 1 — Sol review

Review target: detached `706c032` against `309103a`; source, generated bindings, capabilities, D1–D15, facade coverage, and M1/M4 contracts reviewed. Mutants were confined to this worktree and restored.

## Findings

### F1 — HIGH: payment authentication policy is outside the IPC trust boundary

Location: `rust/crates/dw-app/src/api/vault.rs:620,649` and `rust/crates/dw-vault/src/vault.rs:1157`.

Scenario: the documented default per-payment authentication policy is not enforced by these IPC grant commands. A renderer-side prompt cannot enforce a rule against a compromised renderer. Fix: persist the policy in trusted Rust state, enforce it for payment grants, and test the IPC path for both policy states. **possible security note: see manager**.

### F2 — MEDIUM: prepared-handle cap can consume a grant before rejection

Location: `rust/crates/dw-app/src/api/send.rs:156` and `rust/crates/dw-app/src/state.rs:238,245`.

Scenario: with 63 prepared handles, two concurrent `draftPrepare` calls can both pass `room_for_prepared`. Each awaits signing; only one can insert when the table reaches 64. The other returns `app.too_many_handles` after its single-use grant was spent and its prepared transaction was discarded. Fix: reserve capacity atomically before the await, count in-flight reservations, and release a reservation on every failure, close, or network switch. Add a barrier test with two prepares at 63 open handles.

### F3 — MEDIUM: the event channel has no memory bound or overflow signal

Location: `rust/crates/dw-app/src/bridge.rs:342,358` and `docs/design/E0-13-bindings.md` D3.

Scenario: the bridge uses `std::sync::mpsc::channel`, which is unbounded. A slow or stalled forwarder can accumulate events without a limit; rate limiting applies only to some engine event kinds. Neither overflow nor failed delivery is available as a renderer state. Fix: define a bounded, nonblocking bridge with an explicit overflow/reload signal, while preserving critical one-shot events through a durable or queryable state path. Align D3 and the acceptance claim with the chosen policy.

### F4 — LOW: closing a draft does not revoke an in-flight broadcast

Location: `rust/crates/dw-app/src/api/send.rs:173` and `rust/crates/dw-app/src/state.rs:231`.

Scenario: `draftBroadcast` clones the draft and prepared transaction, then awaits the engine. `closeDraft` can remove both table entries and return while those clones remain live; the earlier broadcast can still proceed. This is not a memory-safety fault, but callers must not treat `closeDraft` as cancellation. Fix: either serialize close and broadcast intent before hand-off, or document that close only prevents subsequent calls and present the signed-before-lock warning for an in-flight send.

## Mutation table

| Mutant in review worktree | Gate | Result |
|---|---|---|
| Changed generated `IdentitySummary.balance` from `string` to `number` | `pnpm typecheck` | Failed at the typed fixture (`TS2322`); restored. |
| Removed the explicit `settings: null` argument from generated `openNetwork` invoke payload | `pnpm test --run src/api/commands.test.ts` | Failed: fixture received `undefined` instead of `null`; restored. |

## Validation and limits

- Generated manifest: 211 command entries and 187 error-code entries; the coverage table states 248 methods (195 bound, 53 excluded).
- Renderer: `pnpm typecheck` passed; `pnpm test --run` passed 29/29 at the unmodified head. Both mutants were killed by their focused gates. Working tree was clean after restoration.
- Rust: `cargo test -p dw-app --lib` passed 98/98 with the requested isolated target and temp directory (28m36s wall time under host load). `cargo fmt --all -- --check` passed.
- Capability files grant named commands to `main` only; no wildcard, filesystem, or shell plugin grant was found. CSP, frozen prototype, no global Tauri, and navigation restriction are present. Mainnet/testnet endpoint, quorum, peer, and CA settings are rejected from the renderer.
- The coverage test accounts for claims and exclusions by string and checks source method presence. It does not prove that a command body calls the facade named in its claim. I spot-checked high-risk bindings; I found no unsupported exclusion among the 53 listed, but did not execute every facade against a live network. Several underlying DashPay facade methods are stubs by design; a binding can be reachable while the engine returns `not_implemented`.
- Error matches are exhaustive and the inspected credit amounts, including event/lease/error payloads, use decimal strings. `creditsPerDuff` remains a number; its current engine constant is 1000. I did not find a secret returned without the intended reveal path or an ungranted raw `PreparedTx` command. The signed PSBT export remains an intentional grant-protected path. Secret inputs are wrapped after Tauri deserialization; JavaScript strings, Tauri request bodies and parsed `serde_json::Value` copies are not zeroized, as D10 expressly accepts. Secret outputs become renderer strings by design.
- No live GUI, mainnet spend, or external endpoint test was run. The reported CI result was supplied in the task brief, not independently rechecked.

**Verdict:** REQUIRES CHANGES

Sol gate satisfied: yes
