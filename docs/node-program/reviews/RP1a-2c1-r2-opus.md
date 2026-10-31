# RP1a-2c-1 round 2 review (Opus-h, narrow: fix round only)

Branch `node/rp1a-2c`, r1 head 602bb9bf7, new head c40c11e38 (worktree `rdc-wt/rp1a-2c`, read
only). I reviewed `git diff 602bb9bf7 c40c11e38` without the two DEC-723 trim cherry-picks
(a1e022779 and c40c11e38; both carry the `-x` trailer). Ruling: DEC-719. Core: v24.0.0-rc.4
sources (the implementer's extracted copies in `/work/scratch/rp1a-2c/core`, and the binary in
`~/.rust-dashcore-test/dashcore-24.0.0-rc.4`).

**Verdict: ACCEPT-WITH-NITS** (0 M, 0 m, 3 n)

Every r1 finding DEC-719 required or took is fixed. The new fixtures are real rc.4 output: I
re-decoded all 5 crafted rows with my own rc.4 dashd and got byte-identical replies. The
`0.07000000000000001` text matches by construction, which I checked exhaustively over all 65,536
`u16` values. The stored-block DIFF does reach the non-genesis no-undo path. Mutants 1–5 die on
the rows meant to kill them. The three new nits are below; none blocks the merge.

## Per-finding table (r1 → c40c11e38)

Sol's r1 (RP1a-2c1-r1-sol.md) had no findings, and DEC-719 lists no Sol items, so there are none
to check.

| ID | Status | Where | Notes |
|---|---|---|---|
| m-1 (D41 premise; live stored-block DIFF) | **FIXED** | `docs/node-program/designs/RP1a-notes.md:237-246` (D41 reworded), `:252-254` (GetPlatformPort bullet), `:277-287` (D42), ALLOWED-DIFFS row `:328` (owner RP1b); `rdashd/tests/rpc_diff.rs:2257-2268`, `:2290-2308`, `:2331-2353`, `:2359-2421` | The edge is recorded, not ported, which DEC-719 allowed. The DIFF brings back 19 special shapes live. See §3. |
| m-2 (four surviving payload mutants) | **FIXED** | `dashd-rpc/tests/data/txtouniv/extract.py:53-114` (`net_info_kinds`, `shape`), `:129-132`; `craft.py`; `dashd-rpc/src/core_write/tests.rs:267-283` | The new buckets keep 4 `t1/pv3/mn0/ownreward` rows and 4 `t2/pv3/mn1/domain` rows. The crafted file adds `[invalid entry]` and an empty ProUpShareTx reward. Mutants 1, 3, 4 and 5 die (§4). |
| m-3 (D40 owner, ALLOWED-DIFFS, D42 list) | **FIXED** | `RP1a-notes.md:230-233` (LK1b, with getrawtransaction sharing the case), `:295` (D42 item 4), `:327` (ALLOWED-DIFFS row, closer LK1b) | |
| n-1 (equivalent guard mutant) | **FIXED** | `dashd-rpc/src/core_write/evo.rs:155-159` | The comment says the guards are defensive and cites `netinfo.h:419-424`. |
| n-2 (fractional `operatorReward`) | **FIXED** | crafted row `crafted/t1/pv2/mn0/opreward7` (with and without `service`); `rpc_diff.rs:2346` asserts it live | Matches by construction (§2c). But see new n-2 on the choice of 7. |
| n-3 (no-undo DIFF only at genesis) | **FIXED** | `rpc_diff.rs:2258-2268`, asserts `:2349-2351` | Height 112, `confirmations -1`, no `prevout`, and only `assetUnlockTx`'s `fee`. |
| n-4 (all prevouts `raw(51)`) | **FIXED** | `rpc_diff.rs:2212-2226` (dashd's `signrawtransactionwithkey` signs a spend of the `pkh` coinbase), assert `:2328-2330` | `getblock_next_3` is diffed and carries a `pubkeyhash` prevout with an `address`. |

## 2. The new fixtures

**a. Are the crafted bytes decoded by the real rc.4 dashd?** Yes. Using my own script
(`/work/scratch/rp1a-2c-r2-opus-tools/redecode.py`), I ran
`~/.rust-dashcore-test/dashcore-24.0.0-rc.4/bin/dashd` on regtest, offline, mining one block
first as decode.py does. I ran it twice, with and without `-deprecatedrpc=service`, and sent each
crafted row's `hex` to `decoderawtransaction`. All 5 replies are **byte-identical** to the
fixture's `reply`. The node reported subversion `/Dash Core:24.0.0/` and version 240000 (rc
builds print no rc suffix). That matches the fixture's `dashd` field. Each reply is a `result`
with `error: null`.

- The three edits do what `craft.py` says:
  - `invalid_entry` raises the PLATFORM_HTTPS count and appends a bare `0xff` entry. Core prints
    it as `[invalid entry]` (`netinfo.cpp:246`) and reads the rest of the payload as before:
    `platformNodeID`, the ports and `inputsHash` all print.
  - `empty_share_reward` zeroes the reward script's length, and the reply has no
    `rewardAddress`.
  - `operator_reward_7` writes 7 at offset 148: version, type, mode, outpoint, 18-byte legacy
    service, owner, operator key, voting. The reply has `"operatorReward":0.07000000000000001`,
    and every other field is unchanged.
- base-sdk decodes the same bytes the same way, which the passing crafted test shows.

**b. Is PROVENANCE honest about the crafted rows?** Yes. They live in their own file, their
`bucket` starts with `crafted/`, and `source` names the edit, the base bucket, the base txid and
its blk file. The text says plainly that they are valid on no chain. The ProUpShareTx `service`
row was dropped by the existing trim rule, and PROVENANCE says so. The main file's "nothing
written by hand" claim still holds; the remake is dated and attributed to r1 m-2. I found two
small wording slips (new n-3).

**c. Is `0.07000000000000001` right by construction or by luck?** By construction.
- Core computes `static_cast<double>(nOperatorReward) / 100` (`evo/core_write.cpp:223`) and
  formats it in `UniValue::setFloat` with `std::setprecision(16)` (`univalue.cpp:76-80`).
- rdashd computes `f64::from(operator_reward) / 100.0` (`evo.rs:375`), the same IEEE division,
  and formats with `dash-args`' `format_g16`. That function rounds to 16 significant digits, picks
  fixed or scientific from the rounded exponent (−4 ≤ X < 16), then strips trailing zeros, as
  `%g` does.
- I checked it exhaustively. A throwaway test in my copy dumped `UniValue::from_f64(n / 100.0)`
  for every `n` in 0..=65535. I compared each value with Python's `'%.16g' % (n / 100.0)`, which
  is correctly rounded like glibc's printf: **0 mismatches out of 65,536**.

## 3. The stored-block DIFF (2fbfbfffd)

**Does it compare the non-genesis no-undo path?** Yes.
- `unconnectable_block` builds a block on the tip at height 112, far below DIP0003 (432). It
  holds a plain coinbase plus 19 fixture special transactions, 3 of them crafted. Both nodes
  store it and refuse it at connect, so it has `HAVE_DATA` and no `HAVE_UNDO`.
- `both()` compares the full HTTP replies byte for byte for `submitblock_stored` and for
  `getblock_stored_{1,2,3}`.
- The asserts at `rpc_diff.rs:2331-2353` then show the path really ran:
  - `confirmations: -1`;
  - every payload key, `[invalid entry]` and `0.07000000000000001` are present;
  - there is no `prevout`;
  - there is exactly one `"fee":`, which is `assetUnlockTx`'s.
- The implementer's last run at c40c11e38 (`/work/scratch/rp1a-2c/r1/final.log`,
  `diff-final.log`) passed. I also reran the getblock DIFF from my copy (see "What I ran").

One thing this cannot catch: a mutant that wrongly set `HAVE_UNDO` on a stored, unconnected
block would still print without undo. `read_undo` answers `None` when no record exists, so the
no-undo assertion does not prove the status bit. That is fine for an RPC slice and needs no
change.

**Does the second node pair hide a difference?** It works around one, and that difference is
recorded rather than hidden.
- The implementer's first try (`/work/scratch/rp1a-2c/r1/diff4.log`) restarted both nodes with
  `-deprecatedrpc=service`. After the restart, rdashd answered `-5 "Block not found"` where dashd
  still served the stored block.
- The next try (`diff5.log`) used a fresh pair with the stored block at height 1. That differed
  on the BLS-flag fields, because Core prints those under its legacy flag below V19, as D41
  already states.
- The final version mines one block first (past V19), then compares getblock 2 and 3 at height 2.
- The restart loss is now an ALLOWED-DIFFS row (`RP1a-notes.md:329`), and D42 `:285-287` names
  it as a limit. Skipping verbosity 1 under `service` hides nothing, since `service` changes only
  payload objects.
- The row has no owner, and its scope is wider than what was observed. That is new n-1.

## 4. Mutants

I applied the implementer's `mut-apply.py` (the r1 table's mutants 1–5, env-gated `A6MUT=n`) to
my own reflink copy, then added three of my own (`/work/scratch/rp1a-2c-r2-opus-tools/mut-extra.py`).
I ran each against the whole `dashd-rpc --lib` suite. The baseline gave 195 passed (194 plus my
dump test).

| # | Mutant | Result | Killing row |
|---|---|---|---|
| 1 | share_list: reward always refund | killed | `t1/pv3/mn0/ownreward` (chain row, intended) |
| 2 | share_list: empty reward prints empty | killed | `t1/pv3/mn0/ownreward` (a sibling share with an empty reward) |
| 3 | domain entry without `:port` | killed | `t2/pv3/mn1/domain` and `crafted/…/invalid` (intended) |
| 4 | ProUpShareTx `rewardAddress:""` with no destination | killed | `crafted/t11/pv1/noreward` (intended) |
| 5 | `[invalid]` for `[invalid entry]` | killed | `crafted/t2/pv3/mn1/invalid` (intended) |
| 6 | `entry_port(Invalid)` → 1, not 0 | **survives** | none: the invalid entry is third, and `GetPlatformPort` reads `[0]` |
| 7 | ProUpShareTx: push `rewardAddress` (or `""`) iff the script is non-empty, instead of iff `ExtractDestination` succeeds | **survives** | none: no row has a non-empty, non-standard reward |
| 8 | `operatorReward` as `n * 0.01` instead of `n / 100.0` | **survives** | none: `7 * 0.01` and `7 / 100.0` are the same double. They differ for 486 of the 10,001 valid values, for example 57 (`0.57` vs `0.5700000000000001`) |

None of mutants 1–5 dies by coincidence: each fails on the row built for it. My numbers match
the implementer's `mut.log` (same rows).

## New findings

### n-1: the new "stored block lost across a restart" row has no owner, and its scope is not established

- **Where:** `RP1a-notes.md:329`, closer "to be named by the manager". This is the same gap r1
  m-3 raised for D40, so the manager needs to act on it.
- **What's known:** an invalid block (refused at connect) that rdashd stored is gone after a
  restart. Core keeps it: its block index is written with `BLOCK_FAILED_VALID | HAVE_DATA`.
- **The scope gap:** the row's text says "a block stored but never connected". That also covers
  a *valid* side-branch block, which nobody has tested.
  - If valid side-branch blocks are lost too, this is a chainstate durability gap, not just an
    RPC difference: a reorg after a restart would need the block downloaded again.
  - If only invalid blocks are lost, the row should say "a block that failed `ConnectBlock`".
- **Fix:**
  1. The manager names the owner (the block-index persistence slice, CH1-node or SB-store).
  2. The owner checks the side-branch case and narrows or widens the row to match.

### n-2: three cheap payload mutants still survive (6, 7, 8 above)

All three need crafted bytes to reach, since none is valid on chain, so they are low value. Each
costs one more `craft.py` edit:
- **Mutant 6:** a ProUpServTx or ProRegTx v3 whose *first* PLATFORM_HTTPS (or PLATFORM_P2P)
  entry is the `0xff` one, decoded under `service`. Core prints `platformHTTPPort: 0`.
- **Mutant 7:** a ProUpShareTx whose reward script is non-empty but non-standard (for example
  `51`). Core prints no `rewardAddress`.
- **Mutant 8:** use `nOperatorReward = 57` (or add a second row). It has a long `%.16g` tail
  (`0.5700000000000001` under `*0.01`) and tells the division apart from a multiply-by-0.01
  refactor. Choosing 7 pins the formatter but not the arithmetic.

### n-3: two wording slips in the fixture docs

- `craft.py:71`: the docstring says `nOperatorReward = 7` "prints 0.07 under %.16g". It prints
  `0.07000000000000001`, as PROVENANCE.md and the test say.
- `PROVENANCE.md:54`: "craft.py made three transactions from the `/domain` extract". Only the
  invalid-entry row is built from a `/domain` bucket. The other two come from `t11/pv1` and
  `t1/pv2/mn0` of the same remade extract. Something like "from the remade extract (the one with
  the `/domain` dimensions)" would be exact.

## What I ran

- Read the full fix diff (15 files) and `craft.py`. Read Core rc.4's `evo/core_write.cpp`
  (`GetPlatformPort`, `CProUpShareTx::ToJson`, `operatorReward`), `evo/netinfo.cpp` (`GetPort`,
  `ToStringAddrPort`), `providertx_util.cpp` (`ShareListToJson`) and `univalue.cpp`
  (`setFloat`).
- Reflink copy `/work/scratch/rp1a-2c-r2-opus` (left in place). Changes in it:
  - env-gated mutants 1–8 in `dashd-rpc/src/core_write/evo.rs`;
  - one `r2_dump_operator_rewards` test appended to `dashd-rpc/src/core_write/tests.rs`.

  Builds ran with `CARGO_BUILD_JOBS=8`, `nice 19`, and TMPDIR inside the copy. `/work` had 278G
  free before I started.
- `cargo test -p dashd-rpc --lib`: baseline 195 passed; mutants as in §4.
- Exhaustive `%.16g` check: `/work/scratch/rp1a-2c-r2-opus-tools/opreward.txt`, 0 of 65,536
  mismatched.
- rc.4 re-decode of the crafted fixture: 5 of 5 byte-identical
  (`/work/scratch/rp1a-2c-r2-opus-tools/redecode.py`; datadirs under `…-tools/w`).
- Live getblock DIFF rerun from the copy (`cargo test -p rdashd --test rpc_diff getblock`):
  **ok**, 1 passed in 177.8 s (`getblock_transactions_match_dashd_v24`, against the pinned rc.4
  dashd). The mutants are env-gated and were off, so the copy ran unmutated code.
- Nothing committed or pushed. The implementer's `r1mut` copy was not touched.
