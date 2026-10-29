# RP1a-1b review, round 1

Verdict: **ACCEPT-WITH-NITS**

Scope reviewed: commits `0337f4741`, `50f9a262c`, and `1d28684ca` on
`node/rp1a-1b` (`50f9a262c`). The Core comparison checkout is v24.0.0-rc.4,
commit `49caf372bb`.

## Findings

### n-1 — The unit test does not verify raw transaction ordering or bytes

- **File:** `dashd-rpc/src/mining/tests/generate.rs:376-381`
- **Core:** `src/rpc/mining.cpp:347-369,390-400`
- **Scenario:** `generateblock` receives two different raw transactions, or a
  transaction whose bytes are changed while preserving the transaction count.
  The current test uses the same transaction twice and asserts only
  `Call::Check(3)`, so a regression that reorders, drops, or substitutes the
  appended transactions would still pass this unit test. The dashd-backed
  case exercises one valid spend, but does not assert the complete transaction
  list in the unsubmitted block.
- **Suggested fix:** use two distinct raw transactions and have the stub retain
  the checked block (or its txids/serialized bytes), then assert coinbase plus
  the two transactions in input order. Also assert the unsubmitted `hex`
  contains that exact ordering. This would directly cover Core's
  `block.vtx.insert(block.vtx.end(), txs.begin(), txs.end())` behavior.

## Checked and ran

- Read the three requested commits with `git show`, including all production
  files, the descriptor/key-I/O vectors, the generate tests, and D15–D19.
- Compared mining behavior against Core `rpc/mining.cpp`, descriptor parsing
  against `script/descriptor.cpp`, Base58/key decoding against `key_io.cpp`
  and `base58.cpp`, and transaction decoding against `core_read.cpp`.
- Confirmed the 112-character bound is after whitespace trimming and covers the
  largest Core caller limit (82 decoded bytes including checksum); it does not
  reject a Core-accepted key/address input.
- Built a reflink scratch copy and ran `cargo test -p dashd-rpc --all-features`:
  133 passed.
- With `eval $(python3 contrib/setup-dashd.py)`, ran
  `cargo test -p rdashd --test rpc_diff generate_commands_match_dashd_v24`:
  1 passed, 10 filtered out.

No production defects requiring a change were found in this review round.
