# O2a-CJ1 CoinJoin oracle entries and `diff_coinjoin`: Opus-h review, round 2 (narrow)

Reviewed:
- rdc `node/o2a-cj1` `4007329ab..`**27d7e07e7**: 62be49188 (r1 fixes, pins, ABI 27), 9c632a6bc (SB4 regen) and 27d7e07e7 (D19). All three are `%G? = G`.
- Core `node/o2a-cj1-oracle-v24.0` `5e9abb6b28..`**bcf6144685** (one commit, `G`).

Both were reviewed in detached worktrees against the pinned archives (`/work/cache/oracle/bcf6144685…/{plain,fuzz}`; sha256 as `pins.txt`, `e88277d9…`/`ff071d9d…`; the fuzz archive's `oracle.h` is byte-equal to the branch's). Core line numbers are v24.0.0-rc.3 `0364ed626e`; `git diff 0364ed626e bcf6144685 -- src/coinjoin` is empty.

## Verdict: ACCEPT WITH NITS

Every r1 finding is closed, and I reproduced each claimed mutant kill. D19's two functions are byte-exact to Core on every DSTX shape I generated, special transactions included. Every caller in dash-coinjoin `src` and node-l1 uses them. The ABI 27 bookkeeping and the SB4 regen are provenance-only. The public wording is neutral.

The nits are A1 §1.3 consistency in dash-coinjoin's tests and docs (N1), two stale "ABI 20" strings (N2), and one note on the denomination-0 wrap (N3, no action needed).

## r1 findings: all closed

| r1 | fix | evidence I checked |
|---|---|---|
| **F0** (Medium) | The shim refuses `session_denom == 0` with `ORACLE_E_ARG` (`oracle_coinjoin.cpp:284-289`), after the flag check and before `WithView`. The harness requires that refusal to come exactly at denomination 0 (`node-l1/src/coinjoin.rs:646-652`: "ORACLE_E_ARG at a session denomination" / "denomination 0 not refused"). The node is still run there. `inouts.rs:179,191` now wrap. D6 and D16 are corrected. | See "F0 in detail" below. `denomination_zero_rows` (`l1_coinjoin.rs:193`), `is_valid_in_outs_at_denomination_zero_does_not_panic` (`core_ports.rs:860`), and 8 seeds. Reverting the wrap panics at `inouts.rs:179` in both suites. |
| **F1** (Low) | Collaterals at fee = `MAX_RAW_TX_FEE` and +1 (`coinjoin.rs:1198`, `collateral_bound_rows` `l1_coinjoin.rs:211`), on all four networks. | Mutant A is killed: "collateral verdict" in `collateral_bound_rows`, `session_rows` and the public seeds. |
| **F2** (Low) | When out of range, the harness checks "asked the mempool out of range" (`coinjoin.rs:768-770`). Seeds: a `MAX_MONEY + 1` coin and a −10,000 output. | H and H2 are killed ("asked the mempool out of range", plus the add-overflow panics at `collateral.rs:56/75`); H2 is also killed by `random_inputs`. |
| **F3** (nit) | On a predicted overflow refusal the node must answer invalid with no mempool call (`coinjoin.rs:752-756`). | `collateral_overflow_rows` passes with the check in place. |
| **F4** (nit) | Basic-scheme `Unsupported` diverges (`coinjoin.rs:846-850`). | Code read. No base-sdk input reaches it today. |
| **F5** (nit) | COVERAGE.md §21.2 has its own CoinJoin row (`:310`), and `oracle_coinjoin_valid_in_outs` is dropped from the unowned list. | Read. |
| Sol nit | Pool-size rows 180/181/200/201 (D11, PROVENANCE.md). | `pool_size_rows` passes. |

### F0 in detail

**Is the denomination-0 refusal exact?** Yes. In Core, only `session_denom == 0` lets `checkTxOut` (`coinjoin.cpp:335-357`) pass a non-denominated amount:
- `AmountToDenomination` (`common.h:71`) returns 0 or a single bit `1 << i`, so a non-zero invalid session denomination (3, 32, negative) never equals it.
- `GetLargerAdjacentDenom` (`common.h:208`) returns 0 for an invalid or the largest denomination. But a promotion or demotion entry is refused first by its validator (`coinjoin.cpp:678-683`, `:722-727`), and the final transaction's `nLargerDenom` of 0 never matches (`:337`).

So refusing exactly 0 is the whole hole. The harness's `DENOMS` (`coinjoin.rs:145`) still draws 32 and 3, which are compared and must not be refused. Denomination 0 is refused wholesale, including calls Core would decide without overflowing; r1 offered that domain cut. Nothing non-zero is refused.

**Is dash-coinjoin's wrapping arithmetic right?** Yes:
- At a real denomination every summed value is a denomination, so wrapping equals exact arithmetic.
- At denomination 0, wherever Core's `nFees` does not overflow, the wrapped sums equal Core's exact ones in the same order (outputs, then inputs), so the verdict and message are Core's.
- Nothing else in `is_valid_in_outs` can panic: `larger_*` counts are at most the lengths they are subtracted from.
- Near the limits: the crate test covers outputs `i64::MIN`, `i64::MAX` and −1 against a 30,000 coin, all `ErrFees`. I also traced outputs (`i64::MAX`) against a coin of `i64::MAX`. Core is defined there and accepts (0 − MAX + MAX), and so does the node. The only disagreements possible are where Core overflows, which is undefined there and refused by the oracle (N3).

## D19: dstx hashes from dash-consensus

**Byte-exactness.** `dstx_signature_hash` (`dash-coinjoin/src/dstx.rs:130`) writes `canonical::write_tx(tx)`, the 32-byte proTxHash and the 8-byte `sigTime` into a SHA256d `HashWriter`. That is `SerializeHash(*this, SER_GETHASH)` over Core's `SERIALIZE_METHODS` (`coinjoin.h:324-331`: `tx, m_protxHash`, no `vchSig` under `SER_GETHASH`, `sigTime`; `coinjoin.cpp:77-80`).
- `masternodeOutpoint` is not serialized in v24.
- `write_tx` reproduces `CTransaction::Serialize`, including `nVersion | (nType << 16)` with the int16 sign extension, and the payload iff `nVersion >= 3 && nType != 0`.
- `txid` (`:122`) goes through `Tx::new(..).txid()`, which applies Core's `ComputeHash` stable-unlock rule (`primitives/transaction.cpp:115-118`).

Checked with a scratch probe (deleted). It sent about 21.8k generated dstx messages to `oracle_cj_message_hash` and to `oracle_tx_decode` for the txid, against `dstx_signature_hash`/`txid` on base-sdk's decode:
- versions 0–5, 0x7fff, −1 and `INT16_MIN`;
- types 0–6, 8, 9, 0x100 and 0xffff;
- payloads empty, short, 145 bytes (two payload versions) and 300 bytes, plus payload bytes Core doesn't read;
- 0–3 inputs and outputs;
- 20,000 random byte mutations.

Results: 19,964 both decode, 1,826 both reject, and **0** decode-verdict, signature-hash or txid mismatches (19,963 txids compared).

**Callers.**
- `check_dstx_signature` (`dstx.rs:141`).
- `DstxManager::add` (`:190`).
- The server: dsa collateral `server.rs:500,548`, entry collateral `:593`, final tx `:814`, DSTX signing `:828`.
- node-l1: the view txids (`coinjoin.rs:329-342`, `:399`, `:426`), `compare_hashes` (`:790`) and `seed_dstx`'s signing.

All of them use the D19 functions.

**Remaining base-sdk hashing in dash-coinjoin `src`.** Only `DsQueue`'s `hash()`/`signature_hash()` remain (`queue.rs:45,64`, `server.rs:395`), and a dsq carries no transaction. No `Hashable` on a `Transaction`, and no base-sdk encode, is left in a hashing path. Tests are another matter (N1).

**Evidence.** Reverting either half of D19 passes `l1_coinjoin` and every dash-coinjoin test. D19's regression evidence is the owner's private replay (see manager). My scratch probe also kills both reverts.

## ABI 27 and the SB4 regen

**ABI 27.** The Core commit only:
- moves `ORACLE_ABI_VERSION` 20 → 27 and the section heading;
- documents the denomination-0 refusal;
- adds that refusal.

On the rdc side, `ffi::ABI_VERSION`, `smoke.rs`, the README, both module docs, D1, COVERAGE.md and CJ0 notes F2 say 27. `pins.txt` names bcf6144685 with both installed sha256s. Two strings were missed (N2).

**SB4 regen (9c632a6bc).** I decompressed all 54 vectors at 62be49188 and 9c632a6bc and diffed them:
- every changed key is under `provenance`: `core_commit` 5e9abb6b28 → bcf6144685, the binary and archive sha256s, and `instructions_u` in two files;
- every `vector` body is identical;
- no other file is touched.

## Public text

D19, the three commit messages, the comments at `dstx.rs:119-135` and PROVENANCE.md cite the A1 §1.3 policy only. None of them describe an input class, a fuzz run or a divergence. D18 doesn't mention D19's mutant. The 24 new public seeds are cases 2 and 3 only (in/outs and collateral), and the signed message seeds are unchanged. Nothing points at the private class.

## Findings

**N1. Nit: dash-coinjoin's tests and two doc comments still hash DSTXs with base-sdk, contrary to A1 §1.3 as D19 applies it.**
- **Tests:**
  - `tests/relay_rules.rs:69` signs a DSTX over `DsTx::signature_hash()`;
  - `:337`, `:364`, `:394` and `:503-511` key `validate_dstx`/`relay_inv_kind`/`announce_inv_kind`/`dstx_for_getdata` with `d.tx.hash()`;
  - `tests/core_ports.rs:216-290` and `tests/sessions.rs:382` do the same.
- **Docs:** `validate_dstx` (`dstx.rs:296-297`, "`txid` is the transaction's hash") and the relay helpers (`relay.rs:69,82,99`) take the txid from the caller without naming `dstx::txid`. Core's caller passes `tx.GetHash()` (`net_processing.cpp:4913-4914`).
- **Risk:** P6a will write the production caller (CJ0 F3). The idiom in these tests is the one it is most likely to copy.
- **Concrete:** `relay_rules.rs:394` calls `validate_dstx(.., d, &d.tx.hash())`. It should be `&dstx::txid(&d.tx)`, and `:69` should sign `dstx::dstx_signature_hash(&d)`.
- **Fix:** switch these call sites to the D19 functions, and say in the two docs that `txid` must be `dstx::txid(&dstx.tx)`. That's test and doc text only.

**N2. Nit: two "ABI 20" strings survived the renumbering.** `dash-oracle-sys/tests/coinjoin.rs:1` ("The CoinJoin rules (ABI 20, O2a-CJ1)") and `node-fuzz/targets.toml:547` ("… src/coinjoin (ABI 20)"). Both should say 27. CJ0 notes D5/D18 and STATUS.md are history and can stay.

**N3. Info: at denomination 0 the wrapped sums can reach 0 on unbalanced amounts.**
- **Example:** entry outputs (`i64::MIN`, P2PKH a) and (`i64::MIN`, P2PKH b), with two chain coins of value 0 (P2PKH c, d), `session_denom` 0.
- **Node:** `fees` wraps 0 → `MIN` → 0 → 0, so `is_valid_in_outs` returns `Ok`.
- **Core:** `nFees -= INT64_MIN` (`coinjoin.cpp:371`) is undefined there.
- **Why no action:** the oracle refuses the call (D6), and no session has denomination 0:
  - Core's server checks the denomination (`server.cpp:925`) before it sets it (`:1015`), and so does dash-coinjoin's (`server.rs:446` before `:496`);
  - Core's client sets one only from a queue it selected inputs for, or from a denominated amount (`client.cpp:1444`, `:1535`, `:1617`).

  So the brief's bar is met: Core's verdict wherever Core is defined, and never a panic.
- **Caveat:** dash-coinjoin's `ClientSession::enter_queue` (`client.rs:143`) takes the denomination from its wallet caller unchecked. CJ1's wallet glue should pass only a valid denomination, as Core's does. A `debug_assert!(denom::is_valid_denomination(..))` there would document that.
- **Optional:** `checked_*` → `Err(ErrFees)` would make the node refuse these instead. D6's "as Core's `CAmount` arithmetic does in practice" already describes the choice.

## What I ran

- **Required suites** (pinned plain archive, `RUSTC_WRAPPER=`, `CARGO_BUILD_JOBS=12`, `DASH_ORACLE_CACHE=/work/cache/oracle`, `DASH_ORACLE_ALLOW_UNPINNED` unset, own `CARGO_TARGET_DIR`):
  - `cargo test -p dash-coinjoin`: 9 + 48 + 12 + 21 passed;
  - `cargo test -p dash-oracle-sys --features oracle --test coinjoin --test smoke`: 7 + 18 passed;
  - `cargo test -p node-l1 --features oracle --test l1_coinjoin`: 12 passed.

  No wall-clock failures.
- **Mutants** (each applied to `dash-coinjoin/src`, run against `l1_coinjoin`, the dash-coinjoin tests and the D19 probe, then reverted; the worktree was clean afterwards):

  | mutant | `l1_coinjoin` | dash-coinjoin tests |
  |---|---|---|
  | A | killed (`collateral_bound_rows`, `session_rows`, seeds: "collateral verdict") | killed |
  | E | killed (`structure_rows`, seeds: "possibly valid post-V24") | missed, as in r1 |
  | H | killed ("asked the mempool out of range") | killed |
  | H2 | killed (also `random_inputs`) | killed |
  | F0 revert | killed (`denomination_zero_rows`, seeds: sub-overflow panic) | killed |
  | D19 txid revert | passes | passes |
  | D19 signature-hash revert | passes | passes |

  The D19 reverts are killed only by the scratch probe (above) and, per the owner, the private replay.
- **D19 probe:** described above. It was a throwaway test file in the scratch worktree, never committed.
- **SB4:** a JSON diff of all 54 vectors (above).
- **Fuzz:** `python3 -I node-fuzz/fuzzctl.py smoke --only diff_coinjoin --no-selftest --no-triage` on the pinned fuzz archive, with `DASH_EMBARGO_DIR` a fresh 0700 dir under this review's scratch (not the shared private tree). Result: **pass**, `divergences=0 crashes=0`, 60 CPU-s, 946 s wall (mostly the node-fuzz build). r1's F0 crash came after 8 CPU-s. The owner's 0.214 CPU-h run is longer; I didn't repeat it.
- **Hygiene:**
  - `git diff --check` is clean in both ranges;
  - Core's head equals bcf6144685;
  - rdc's `node/o2a-cj1` moved during the review to 42bfcbf86. That is one `G` commit on top of 27d7e07e7 adding only O2a-CJ1 notes row D20 (the tier-1 gate ruling). I didn't review D20's substance, which is the manager's ruling. No code differs from 27d7e07e7;
  - nothing was edited, committed or pushed on either branch;
  - the worktree, target and scratch were deleted at the end.
