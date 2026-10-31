# MG-04b slice 2a review (Sol r1)

Verdict: **ACCEPT-WITH-NITS**

## Findings

### n-1 — Add a send-path regression for D83's dust refusal

`rust/crates/dw-engine/src/masternodes/shared/dissolve.rs:279-283` correctly refuses `dissolve` when the actor output is in `(0, dust)`, while `preview_dissolve` only reports `ActorOutputDust`. The current full send test fixture has 300/400-DASH shares and cannot produce that state; the only coverage is the pure warning helper at `shared/dissolve.rs:503-518`. Thus the E11 mutant (removing the send guard) survives. A confirmed registration whose actor share leaves, for example, 1–545 duffs after penalty and fee would otherwise be signed, saved, and handed to the broadcaster even though Core will not relay it, leaving a stuck maintenance row. Add a fixture or focused send test that makes the actor output dust, asserts `invalid_argument`, and asserts that no operation or network send is created.

## What I checked

- Traced preview, send, resend, lease debits, durable row creation, and D23's mutual `update_share`/`dissolve` pending gate. The first send and resend charge penalty + fee exactly once per hand-off; dissolve rows have no fee inputs or locks.
- Compared the builder and signer to Dash Core v24.0.0-rc.3: `providertx_service.cpp:639-681` (`BuildDissolution`), `:1242-1279` (`DissolveShared`), `specialtxman.cpp:1676-1787` (`CheckProDisTxForList`), and `providertx.cpp:506-533` (`CProDisTx::MakeSignHash`). Inputs, sequence/locktime, output order, pro-rata remainder, validity height, signature count, hash, fee ceiling, and dust/relay floors align.
- Checked D81's scan gate and reorg behavior: only a sent, actor-output-omitted row uses the list; it requires a synced list and wallet scan through the list height, and reverts to `sent` if the list later reappears. Other rows remain history-derived.
- Verified E10's surviving mutant is covered by the vault's actor-key check before signing (`rust/crates/dw-vault/src/shared.rs:154-181`); the engine's post-sign check is useful defense-in-depth.

## Tests and commands

- `CARGO_BUILD_JOBS=8 TMPDIR=/work/scratch/mg04b-2a-r1-sol/tmp cargo test -p dw-protx shared::dissolve::tests -- --nocapture` — **4 passed**.
- `CARGO_BUILD_JOBS=8 TMPDIR=/work/scratch/mg04b-2a-r1-sol/tmp cargo test -p dw-engine test_QT_127 -- --nocapture` — **12 passed** (the 12 slice 2a/update-share tests selected by the name filter).
- Read-only diff review of `190d5b62..b55948f9`, including the generated types and regtest additions.
