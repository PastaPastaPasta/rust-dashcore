> **EMBARGO-1:** an unfixed Dash Core security issue, referred to only by this codename. EMBARGO-1: see ~/private/embargo/EMBARGO-1.md (access per PLAN §7). Scrubbed by A1-rev1 on 2026-10-08.

> R4 appendix — sub-agent working notes, verified against Dash Core develop @ 15db478750 and rust-dashcore dev @ 67b307c6 (2026-10-08). Main report: ../R4-consensus-and-verification.md. Items marked 'unverified' were not confirmed empirically.

# R4-A: Dash Core transaction and script layer (consensus + policy)

Source of truth: `/work/home/agent/workspace/dash` @ `15db478750` (branch `develop`). All `file:line`
citations are relative to that tree unless prefixed with `rust-dashcore:`. Everything below was read
from code; history comes from `git log -S` / `git show` in the dash repo.

Legend: **[CONSENSUS]** = affects block validity. **[POLICY]** = mempool/relay only. **[QUIRK]** =
must be replicated bug-for-bug. Difficulty for a Rust port: S/M/L/XL.

---

## 0. TL;DR for the Rust node

- Legacy-only script system. No segwit, no taproot, no BIP143, no SIGHASH_FORKID. There's one sighash
  algorithm (legacy), and it also commits to the DIP2 `nType` and `vExtraPayload`.
- The DIP0020 opcodes (OP_CAT, OP_SPLIT, OP_AND/OR/XOR, OP_DIV, OP_MOD, OP_NUM2BIN, OP_BIN2NUM,
  OP_CHECKDATASIG, OP_CHECKDATASIGVERIFY) are **unconditionally enabled from genesis**. The
  `SCRIPT_ENABLE_DIP0020_OPCODES` flag was deleted in Oct 2024. You don't need any height gating
  in the interpreter.
- There are only 5 consensus script flags: P2SH (always), DERSIG, CLTV, CSV, NULLDUMMY. Each turns on
  at a buried height.
- rust-dashcore has **no interpreter**. Its only verification path is an optional FFI to
  *Bitcoin's* libbitcoinconsensus 0.20.2, which is wrong for Dash. Its opcode table is Bitcoin's (it
  treats 0xba as OP_CHECKSIGADD and OP_CAT as illegal).

---

## 1. Context-free transaction checks: `CheckTransaction` — difficulty S

`src/consensus/tx_check.cpp:14-75`. It is called from `CheckBlock` (`src/validation.cpp:4403-4413`,
for every tx) and from mempool `PreChecks` (`src/validation.cpp:846`).

Rules, in order:

| # | Rule | Reject string | Line |
|---|------|---------------|------|
| 1 | Empty-vin/vout exemptions: `nType == TRANSACTION_QUORUM_COMMITMENT (6)` or `TRANSACTION_MNHF_SIGNAL (7)` allows empty vin **and** empty vout. `nType == TRANSACTION_ASSET_UNLOCK (9)` allows empty vin. | — | 16-24 |
| 2 | `vin.empty()` (unless exempt) | `bad-txns-vin-empty` | 27-28 |
| 3 | `vout.empty()` (unless exempt) | `bad-txns-vout-empty` | 29-30 |
| 4 | `GetSerializeSize(tx) > MAX_LEGACY_BLOCK_SIZE (1,000,000)`. Always 1 MB, even after DIP0001. | `bad-txns-oversize` | 32-33 |
| 5 | `vExtraPayload.size() > MAX_TX_EXTRA_PAYLOAD (10000)` | `bad-txns-payload-oversize` | 34-35 |
| 6 | Each `txout.nValue < 0` | `bad-txns-vout-negative` | 40-41 |
| 7 | Each `txout.nValue > MAX_MONEY` | `bad-txns-vout-toolarge` | 42-43 |
| 8 | Running sum leaves `MoneyRange` | `bad-txns-txouttotal-toolarge` | 44-46 |
| 9 | Duplicate prevouts (`std::set<COutPoint>`) | `bad-txns-inputs-duplicate` | 54-58 |
| 10 | Coinbase: `scriptSig.size()` must be in `[minCbSize, 100]`. `minCbSize = 2`, or `1` when `nType == TRANSACTION_COINBASE (5)` | `bad-cb-length` | 60-67 |
| 11 | Non-coinbase: no input may have `prevout.IsNull()` (null hash **and** `n == 0xffffffff`) | `bad-txns-prevout-null` | 68-72 |

**[QUIRK]** Rules 1 and 10 test `nType` **without** checking `nVersion >= 3`. A tx with
`nVersion=1, nType=6` passes CheckTransaction with no inputs or outputs. After DIP3 activates,
`ContextualCheckTransaction` rejects `nType != 0` on non-special versions (§2.1). Before DIP3 nothing
rejects it, so replicate the exact predicate.

`IsCoinBase()` is `vin.size()==1 && vin[0].prevout.IsNull()` (`src/primitives/transaction.h:308-311`).
An asset-unlock tx (vin empty) is therefore *not* a coinbase and goes down the non-coinbase paths.

There are no height-dependent rules in CheckTransaction.

### 1.1 Constants (`src/consensus/consensus.h`, `src/consensus/amount.h`, `src/script/script.h`)

| Constant | Value | Where |
|---|---|---|
| `MAX_LEGACY_BLOCK_SIZE` | 1,000,000 | consensus.h:10 |
| `MAX_DIP0001_BLOCK_SIZE` | 2,000,000 | consensus.h:11 |
| `MaxBlockSize(fDIP0001Active=true)` | 2 MB if active, else 1 MB (**default arg is true**) | consensus.h:12-15 |
| `MaxBlockSigOps(fDIP0001Active=true)` | `MaxBlockSize(..)/50` → 40,000 / 20,000 (**default true**) | consensus.h:17-20 |
| `MAX_TX_EXTRA_PAYLOAD` | 10,000 | consensus.h:22 |
| `COINBASE_MATURITY` | 100 | consensus.h:24 |
| `LOCKTIME_VERIFY_SEQUENCE` | `1<<0` (the only remaining locktime flag) | consensus.h:28 |
| `COIN` / `MAX_MONEY` / `MoneyRange` | 1e8 / 21e6·COIN / `0<=v<=MAX_MONEY` | amount.h:15,26,27 |
| `MAX_SCRIPT_ELEMENT_SIZE` | 520 | script.h:24 |
| `MAX_OPS_PER_SCRIPT` | 201 | script.h:27 |
| `MAX_PUBKEYS_PER_MULTISIG` | 20 | script.h:30 |
| `MAX_SCRIPT_SIZE` | 10,000 | script.h:33 |
| `MAX_STACK_SIZE` | 1,000 | script.h:36 |
| `LOCKTIME_THRESHOLD` | 500,000,000 | script.h:40 |
| `SEQUENCE_FINAL` / `DISABLE_FLAG` / `TYPE_FLAG` / `MASK` / `GRANULARITY` | 0xffffffff / 1<<31 / 1<<22 / 0xffff / 9 | primitives/transaction.h:113,130,136,141,151 |
| `CTransaction::CURRENT_VERSION` / `SPECIAL_VERSION` | 2 / 3 | primitives/transaction.h:242,244 |
| `MAX_STANDARD_TX_SIZE` (**used by consensus**, see §2.1) | 100,000 | policy/policy.h:27 |

`LOCKTIME_MEDIAN_TIME_PAST` was removed by `85b0b78d47` (merge bitcoin#24565). BIP113 is now keyed
directly on the CSV deployment (§2.2).

### 1.2 Transaction types (`src/primitives/transaction.h:28-42`)

0 NORMAL, 1 PROVIDER_REGISTER, 2 PROVIDER_UPDATE_SERVICE, 3 PROVIDER_UPDATE_REGISTRAR,
4 PROVIDER_UPDATE_REVOKE, 5 COINBASE, 6 QUORUM_COMMITMENT, 7 MNHF_SIGNAL, 8 ASSET_LOCK, 9 ASSET_UNLOCK,
10 PROVIDER_DISSOLVE, 11 PROVIDER_UPDATE_SHARE, 12 PROVIDER_UPDATE_SHARED_REGISTRAR (types 10-12 are
V24 "shared masternode" work, `38122e6212` #7437; V24 is `NEVER_ACTIVE` on mainnet:
`src/chainparams.cpp:212-213`).

### 1.3 Serialization (DIP2) — `src/primitives/transaction.h:268-278, 352-365`

```
int32 field: nVersion in the low 16 bits, nType in the high 16 bits
vin, vout, nLockTime
if (nVersion >= 3 && nType != 0) vExtraPayload   // compact-size + bytes
```

On read, the two halves are split (lines 359-360).
There's no witness/marker byte. Empty-vin txs (asset unlock, qc, mnhf) are not ambiguous because there
is no segwit. CompactSize must be canonical (`src/serialize.h:304-316`), and `MAX_SIZE = 0x02000000`
(`src/serialize.h:42`).

`IsSpecialTxVersion()` is `nVersion >= 3` (`transaction.h:325-328`), and
`HasExtraPayloadField()` is `IsSpecialTxVersion() && nType != 0` (lines 335-338).

### 1.4 Txid special case: v2 asset unlocks (new)

`src/primitives/transaction.cpp:74-125`. If `nVersion>=3 && nType==9 && vExtraPayload.size()==145
&& vExtraPayload[0] >= 2`, then txid = SHA256d(serialization with the trailing 132 bytes, the quorum
signing info, zeroed). `GetInstanceHash()` is the full hash. Commits `ab1ceb33e9`, `c93ee22258`.
Payload v2 is only *valid* after V24 (`src/evo/assetlocktx.cpp:209-211`), but the txid rule depends
only on the bytes. Pre-V24, a block carrying a v2 unlock gets a different merkle-root computation
before it fails `bad-assetunlocktx-version-2`. The block is invalid either way, but the *reject path*
(merkle mismatch vs special-tx failure) differs.

### 1.5 Asset-unlock fee path

`Consensus::CheckTxInputs` returns early for `IsSpecialTxVersion() && nType==ASSET_UNLOCK` with
`txfee = payload.fee` (`src/consensus/tx_verify.cpp:165-167`, `src/evo/assetlocktx.cpp:255-267`:
fee must be `!=0 && MoneyRange`, else `bad-txns-assetunlock-fee-outofrange`). There's no
HaveInputs/value-balance check: the money comes from the credit pool. ConnectBlock adds this fee to
`nFees` (`src/validation.cpp:2716`).

### 1.6 EMBARGO-1

EMBARGO-1: see ~/private/embargo/EMBARGO-1.md.

rust-dashcore stores `version: u16` (`rust-dashcore:dash/src/blockdata/transaction/mod.rs:158`).
It decodes the type with `TransactionType::try_from` whenever `version != 0` (lines 609-621), so
unknown types fail to decode. Per its commit `d67cc034`,
at least one mainnet pre-DIP2 tx has version bytes with nonzero type bits (`00002a00…` in the test).
Core handles that tx naturally (nVersion=0, nType=42, no payload).

---

## 2. Contextual transaction checks — difficulty M

### 2.1 `ContextualCheckTransaction` — `src/validation.cpp:351-386`

Called from `ContextualCheckBlock` (`validation.cpp:4561`) and mempool `PreChecks` (`:850`) with
`pindexPrev`.

- When `DeploymentActiveAfter(pindexPrev, DIP0003)`:
  - if `IsSpecialTxVersion()`, then `nType` must be one of 0..12, else `bad-txns-type` (lines
    358-374), and a coinbase must have `nType == TRANSACTION_COINBASE`, else `bad-txns-cb-type`
    (375-376);
  - otherwise (nVersion < 3) `nType != 0` gives `bad-txns-type` (377-379).
- When `DeploymentActiveAfter(pindexPrev, DIP0001)`: `GetSerializeSize(tx) > MAX_STANDARD_TX_SIZE
  (100,000)` gives `bad-txns-oversize` (382-383). **[QUIRK]** A policy constant is used as a
  consensus rule. The policy check is `>=` (`policy.cpp:88`) but consensus is `>`, so a tx of exactly
  100,000 bytes is consensus-valid but nonstandard. History: `cd262bf641` (DIP0001), moved here in
  `b6896387ae`.
- Per-type activation and payload validity (DIP3 required, V20 for asset lock/MNHF, V24 for 10-12,
  payload version gating) live in `CSpecialTxProcessor::CheckSpecialTxInner`
  (`src/evo/specialtxman.cpp:217-275`; `bad-tx-type-dip3-inactive` at 227-229). That is out of scope
  here and belongs to the special-tx notes. `GetTxPayload` requires the payload to be fully consumed
  (`src/evo/specialtx.h:24`).

### 2.2 IsFinalTx / BIP113 — `src/consensus/tx_verify.cpp:19-39`, `validation.cpp:4535-4559`

```
final if nLockTime == 0
final if nLockTime < (nLockTime < LOCKTIME_THRESHOLD ? nBlockHeight : nBlockTime)   // int64 compare
else final iff every vin.nSequence == 0xffffffff
```

In `ContextualCheckBlock`: `nHeight = pindexPrev+1`. `nLockTimeCutoff = pindexPrev->GetMedianTimePast()`
if `DeploymentActiveAfter(pindexPrev, CSV)`, else `block.GetBlockTime()` (lines 4535-4543). Failure
gives `bad-txns-nonfinal` (4557-4558). The coinbase is included in the loop.

Mempool [POLICY]: `CheckFinalTxAtTip` always uses `tip+1` and `MTP(tip)` (`validation.cpp:205-225`),
giving `non-final`.

### 2.3 BIP68 sequence locks — `tx_verify.cpp:41-116`, `validation.cpp:2626-2630, 2722-2733`

- Enforced iff `(uint32)nVersion >= 2 && (flags & LOCKTIME_VERIFY_SEQUENCE)`. ConnectBlock sets that
  flag when `DeploymentActiveAt(*pindex, CSV)` (2627-2630).
- Per input: if `nSequence & (1<<31)`, skip the input and set `prevHeights[i]=0`. If
  `nSequence & (1<<22)` (time-based): `coinTime = MTP(ancestor(max(coinHeight-1,0)))` and
  `minTime = max(minTime, coinTime + ((seq & 0xffff) << 9) - 1)`. Otherwise
  `minHeight = max(minHeight, coinHeight + (seq & 0xffff) - 1)`. Both start at -1.
- `EvaluateSequenceLocks`: fail if `minHeight >= block.nHeight || minTime >= MTP(block.pprev)`
  (103-111).
- `prevheights[j] = view.AccessCoin(prevout).nHeight` (`validation.cpp:2724-2727`). A coin created
  earlier in the same block has `nHeight = pindex->nHeight`, because UpdateCoins runs in-loop.
- Reject string: `bad-txns-nonfinal` (2730-2733). Coinbase is skipped (inside the `!IsCoinBase()`
  block).
- Mempool [POLICY]: `CalculateLockPointsAtTip`/`CheckSequenceLocksAtTip` (`validation.cpp:262-324`)
  treat mempool parents as height `tip+1`, always with `STANDARD_LOCKTIME_VERIFY_FLAGS`
  (`policy.h:83`), giving `non-BIP68-final` (`:1000-1002`).

### 2.4 `Consensus::CheckTxInputs` — `tx_verify.cpp:163-208`

1. Asset-unlock early return (§1.5).
2. `!inputs.HaveInputs(tx)` gives `bad-txns-inputs-missingorspent` (170-173).
3. Per input: if the coin is a coinbase and `nSpendHeight - coin.nHeight < 100`,
   `bad-txns-premature-spend-of-coinbase` (182-185). `nSpendHeight` is `pindex->nHeight` in
   ConnectBlock (`validation.cpp:2697`) and `tip+1` in the mempool (`:1005`). The CbTx coinbase is a
   coinbase, so the same maturity applies.
4. If `!MoneyRange(coin.value) || !MoneyRange(running nValueIn)`, `bad-txns-inputvalues-outofrange`
   (188-191).
5. If `nValueIn < GetValueOut()`, `bad-txns-in-belowout` (194-198).
6. If `!MoneyRange(fee)`, `bad-txns-fee-outofrange` (201-204).

In ConnectBlock, `!MoneyRange(nFees)` across the block gives `bad-txns-accumulated-fee-outofrange`
(`validation.cpp:2716-2720`).

V24-only, not active on mainnet: `CheckSharedCollateralSpends` (`validation.cpp:2704-2713`) and the
template script `0x04 'DSHC' OP_DROP OP_TRUE` (`src/evo/sharedcollateral.h:11-27`). At the script
level this template is anyone-can-spend; consensus restricts both its creation and its spend.

### 2.5 Sigop counting (no witness, no "cost") — `tx_verify.cpp:118-161`, `src/script/script.cpp:154-200`

- `CScript::GetSigOpCount(fAccurate)` (`script.cpp:154-176`) parses with `GetOp` and **stops at the
  first parse failure**. `CHECKSIG/CHECKSIGVERIFY` count +1. `CHECKMULTISIG(VERIFY)` counts +N if
  `fAccurate` and the previous opcode is OP_1..OP_16, else +20. **`OP_CHECKDATASIG(VERIFY)` counts
  0.** [QUIRK] Unlike BCH, CHECKDATASIG is free.
- P2SH variant `GetSigOpCount(scriptSig)` (`script.cpp:178-200`): if the scriptPubKey isn't exact
  P2SH, use the accurate count of self. Otherwise walk the scriptSig. Any opcode `> OP_16` or a parse
  failure returns 0. Then accurately count the *last pushed data* as a script; OP_0..16 leave
  `vData` empty, which gives 0.
- `GetLegacySigOpCount(tx)`: inaccurate count over all scriptSigs and scriptPubKeys.
- `GetP2SHSigOpCount`: 0 for coinbase. For each input whose prevout script `IsPayToScriptHash()`,
  add `prevScript.GetSigOpCount(scriptSig)`.
- `GetTransactionSigOpCount(tx, view, flags)`: legacy, plus P2SH if `flags & SCRIPT_VERIFY_P2SH`
  (always set in blocks).

Block limits [CONSENSUS]:
- `CheckBlock` (context-free, "relaxed"): sum of legacy counts `> MaxBlockSigOps()` = **40,000**
  (default arg), giving `bad-blk-sigops` (`validation.cpp:4414-4420`).
- `ContextualCheckBlock`: sum of legacy counts `> MaxBlockSigOps(DIP0001 active after pindexPrev)`,
  i.e. 20,000 pre-DIP0001 and 40,000 after (`:4568-4573`).
- `ConnectBlock`: running sum of legacy+P2SH per tx (coinbase included), checked after each tx
  against `MaxBlockSigOps(DeploymentActiveAt(*pindex, DIP0001))` (`:2736-2743`). The check runs
  **before** that tx's CheckInputScripts and UpdateCoins, so P2SH inputs spending same-block outputs
  resolve via the in-loop view.

DIP0001 changed only the divisor input (block size 1 MB to 2 MB). The ratio /50 is unchanged.

### 2.6 Where these run (block path)

- `CheckBlock` (`validation.cpp:4364-4430`): size (`vtx.size()` and serialized size `> MaxBlockSize()`
  = 2 MB relaxed), coinbase position, `CheckTransaction` for every tx, legacy sigops ≤ 40k.
- `ContextualCheckBlock` (`:4528-4593`): context size limit, IsFinalTx, ContextualCheckTransaction,
  legacy sigops, BIP34 height-in-coinbase only for `BIP34Height <= h` and pre-DIP3 (`:4576-4585`),
  and post-DIP3 the coinbase must be CbTx (`:4587-4591`).
- `ConnectBlock` (`:2470-…`), in order:
  1. CheckBlock again.
  2. ChainLock conflict.
  3. Genesis skip.
  4. `fScriptChecks` (assumevalid).
  5. BIP30. `IsBIP30Repeat` always returns false for Dash (`:6781-6785`); enforcement is skipped
     above `BIP34Height` when on the BIP34Hash chain (`:2591-2612`).
  6. Superblock-start hash check.
  7. `nLockTimeFlags`.
  8. `flags = GetBlockScriptFlags`.
  9. **`ProcessSpecialTxsInBlock` runs before the per-tx UTXO loop** (`:2664`).
  10. Per tx: CheckTxInputs, fees, SequenceLocks, sigops, CheckInputScripts (queued), UpdateCoins.
  11. `control.Wait()`, which gives `block-validation-failed` on failure (`:2777-2780`).
- Mempool: see §5.

Note `CScript::IsUnspendable()` (`script.h:534-537`: first byte OP_RETURN **or** size > 10,000).
Such outputs are never added to the UTXO set (`src/coins.cpp:72`). That doesn't affect validity, but
it matters for UTXO-set hashes and snapshots.

---

## 3. Script interpreter — difficulty L

Files: `src/script/interpreter.cpp` (1717 lines), `interpreter.h`, `script.h`, `script.cpp`,
`script_error.h`. **Confirmed absent:** `SigVersion` has only `BASE` (`interpreter.h:133-136`). There
is no WITNESS_V0/TAPROOT and no `FORKID` anywhere (grep of `src/script/`, `src/validation.cpp`, and
`git log -S SIGHASH_FORKID` is empty). `PrecomputedTransactionData` carries unused
`hashPrevouts/hashSequence/hashOutputs` fields, and `Init` only stores spent outputs
(`interpreter.cpp:1477-1485`). `GetPrevoutsSHA256` etc. (`:1442-1473`) are dead code. There is no
witness field in `CTxIn` (`primitives/transaction.h:161`). `miniscript.cpp` exists but is
wallet/descriptor-only (`src/Makefile.am:1017`, in common, not consensus).

### 3.1 Script flags (`interpreter.h:38-114`), exact values

| Bit | Flag | Effect | Consensus? |
|---|---|---|---|
| 0 (0x1) | `P2SH` | BIP16 redeemScript eval (VerifyScript 1676-1702) | yes, always |
| 1 (0x2) | `STRICTENC` | sig must be strict DER + defined hashtype; pubkey must be 33/65 with 02/03/04 prefix (no hybrid 06/07) | policy |
| 2 (0x4) | `DERSIG` | sig strict DER (BIP66) | yes, from BIP66 height |
| 3 (0x8) | `LOW_S` | strict DER + S ≤ n/2 | policy |
| 4 (0x10) | `NULLDUMMY` | CHECKMULTISIG dummy must be empty | yes, from BIP147 height |
| 5 (0x20) | `SIGPUSHONLY` | scriptSig push-only (checked first in VerifyScript, 1656-1658) | policy (not in STANDARD flags; IsStandardTx enforces separately) |
| 6 (0x40) | `MINIMALDATA` | minimal pushes (executed only) + minimal CScriptNum | policy |
| 7 (0x80) | `DISCOURAGE_UPGRADABLE_NOPS` | executed NOP1, NOP4-10 fail | policy |
| 8 (0x100) | `CLEANSTACK` | exactly 1 stack item at the end; **asserts P2SH is set** (1710) | policy |
| 9 (0x200) | `CHECKLOCKTIMEVERIFY` | 0xb1 acts as CLTV, else NOP2 | yes, from BIP65 height |
| 10 (0x400) | `CHECKSEQUENCEVERIFY` | 0xb2 acts as CSV, else NOP3 | yes, from CSV height |
| 11-13 | unused (Bitcoin's WITNESS/DISCOURAGE_WITNESS/MINIMALIF don't exist) | — | — |
| 14 (0x4000) | `NULLFAIL` | failed CHECK(MULTI/DATA)SIG requires empty sig(s) | policy |
| 15 (0x8000) | retired `SCRIPT_ENABLE_DIP0020_OPCODES`, commented out (101-105), ignored | — | — |
| 16 (0x10000) | `CONST_SCRIPTCODE` | OP_CODESEPARATOR anywhere fails (even unexecuted); FindAndDelete hit fails | policy |

`MANDATORY_SCRIPT_VERIFY_FLAGS` = P2SH|DERSIG|NULLDUMMY|CLTV|CSV = **0x615**
(`src/script/standard.h:47-51`). `STANDARD_SCRIPT_VERIFY_FLAGS` = mandatory | STRICTENC | MINIMALDATA |
DISCOURAGE_UPGRADABLE_NOPS | CLEANSTACK | NULLFAIL | LOW_S | CONST_SCRIPTCODE = **0x147DF**
(`src/policy/policy.h:70-77`).

### 3.2 EvalScript main loop (`interpreter.cpp:346-1338`), exact ordering

1. `script.size() > 10000` gives SCRIPT_SIZE (364-365). This applies to scriptSig, scriptPubKey and
   redeemScript alike.
2. Per opcode, **regardless of fExec**:
   - `GetOp` failure (truncated push) gives BAD_OPCODE (378-379).
   - push size > 520 gives PUSH_SIZE (380-381).
   - `opcode > OP_16` increments nOpCount; > 201 gives OP_COUNT (384-385). OP_RESERVED (0x50) does
     not count. Unexecuted ops count.
   - **Always-disabled**: `OP_INVERT 0x83, OP_2MUL 0x8d, OP_2DIV 0x8e, OP_MUL 0x95, OP_LSHIFT 0x98,
     OP_RSHIFT 0x99` give DISABLED_OPCODE even in unexecuted branches (387-393).
   - CONST_SCRIPTCODE plus OP_CODESEPARATOR gives OP_CODESEPARATOR error (396-397).
3. If `fExec` and the opcode is a push (0x00..0x4e): MINIMALDATA check via `CheckMinimalPush`
   (`script.cpp:386-409`), then push (399-403).
4. Else if `fExec || OP_IF<=op<=OP_ENDIF (0x63..0x68)`: switch. **OP_VERIF (0x65)/OP_VERNOTIF (0x66)
   fall into the switch even when unexecuted and hit `default:` (BAD_OPCODE).** OP_RESERVED,
   OP_VER, OP_RESERVED1/2 and 0xbc..0xff give BAD_OPCODE only when executed (1320-1321).
5. After every opcode (executed or not): `stack.size()+altstack.size() > 1000` gives STACK_SIZE
   (1324-1326).
6. Any C++ exception (scriptnum overflow or non-minimal, `stack.at` OOB, `popstack` on empty) is
   caught, giving **UNKNOWN_ERROR** (1329-1332). A Rust port must map those cases to the same error.
7. At the end, a non-empty condition stack gives UNBALANCED_CONDITIONAL (1334-1335).

`ConditionStack` (253-314) is the O(1) Bitcoin implementation. OP_ELSE toggles (multiple ELSEs
allowed), and OP_IF/NOTIF use plain `CastToBool`. **There is no MINIMALIF in Dash.**
`CastToBool` (35-48) treats negative zero (`…0x80` last byte) as false.

### 3.3 Opcode inventory (enabled unless noted)

- Push: OP_0, direct 0x01-0x4b, PUSHDATA1/2/4, OP_1NEGATE, OP_1..OP_16 (410-434).
- Control: NOP, IF/NOTIF/ELSE/ENDIF, VERIFY, RETURN (440-577). The NOP1 and NOP4..NOP10 group is a
  no-op unless DISCOURAGE (516-522).
- CLTV 0xb1 (443-480): 5-byte CScriptNum; negative gives NEGATIVE_LOCKTIME; `CheckLockTime` gives
  UNSATISFIED_LOCKTIME. Checker (1564-1599): same height/time class as tx.nLockTime, operand ≤
  nLockTime, **and this input's nSequence != 0xffffffff**.
- CSV 0xb2 (482-514): 5-byte num; negative gives NEGATIVE_LOCKTIME; if operand bit31 is set it's a
  NOP. Checker (1601-1646): `(uint32)tx.nVersion >= 2`, input seq bit31 clear, mask `(1<<22)|0xffff`,
  same class, operand ≤ seq.
- Stack: TOALTSTACK, FROMALTSTACK, 2DROP, 2DUP, 3DUP, 2OVER, 2ROT, 2SWAP, IFDUP, DEPTH, DROP, DUP,
  NIP, OVER, PICK, ROLL, ROT, SWAP, TUCK, SIZE (583-787). PICK/ROLL take `n` via 4-byte CScriptNum,
  then `getint()`.
- Bitwise: EQUAL, EQUALVERIFY; **AND/OR/XOR** (821-860: operands must be equal size, else
  INVALID_OPERAND_SIZE; result written in place into the second-from-top element).
- Numeric (4-byte operands; results may overflow to 5 bytes and only fail if reused): 1ADD, 1SUB,
  NEGATE, ABS, NOT, 0NOTEQUAL (866-890); ADD, SUB, **DIV, MOD**, BOOLAND, BOOLOR, NUMEQUAL(VERIFY),
  NUMNOTEQUAL, LT, GT, LE, GE, MIN, MAX (892-965); WITHIN (967-981). DIV/MOD by 0 give
  DIV_BY_ZERO/MOD_BY_ZERO. Semantics are C++ `/` and `%` on int64 (truncate toward zero; remainder
  sign follows the dividend). Overflow can't happen because operands are ≤ 2^31-1.
- Crypto: RIPEMD160, SHA1, SHA256, HASH160, HASH256 (987-1011); CODESEPARATOR (1013-1021: sets
  `pbegincodehash = pc` only when executed); CHECKSIG(VERIFY) (1023-1046); **CHECKDATASIG(VERIFY)**
  (1048-1089); CHECKMULTISIG(VERIFY) (1091-1199).
- Splice/conversion: **CAT** (1204-1220), **SPLIT** (1222-1255), **NUM2BIN** (1261-1302),
  **BIN2NUM** (1304-1318).
- Disabled: INVERT, 2MUL, 2DIV, MUL, LSHIFT, RSHIFT. Unknown or bad when executed: 0x50, 0x62,
  0x89, 0x8a, 0xbc..0xff. Bad even unexecuted: 0x65, 0x66.

### 3.4 DIP0020 opcodes: exact semantics and edge cases

**History** (all verified with `git show`):
- Pre-Dec-2020: Bitcoin's always-disabled list (CAT, SUBSTR=0x7f, LEFT=0x80, RIGHT=0x81, INVERT,
  AND, OR, XOR, 2MUL, 2DIV, MUL, DIV, MOD, LSHIFT, RSHIFT) failed even in unexecuted branches.
  0xba/0xbb were unknown (BAD_OPCODE only when executed).
- `5a026be52c` (2020-12-12, #3824): CAT/SPLIT behind `SCRIPT_ENABLE_DIP0020_OPCODES`.
  `4ce3635d99` (2021-01-13, #3893): the rest, plus CHECKDATASIG(VERIFY). Without the flag, all 11
  gave **DISABLED_OPCODE even in unexecuted branches** (the check sat before the fExec test).
  `GetBlockScriptFlags` set the flag at `DIP0020Height` (mainnet 1516032, testnet 414100).
- `61bc3002b0` (2024-10-02, merged as #6302 `5318d68213`): **flag removed; opcodes enabled from
  genesis.** The `CheckInputScripts` special-case was removed too. That makes this a pure relaxation:
  every historic block stays valid. Today `DIP0020Height` only gates `LLMQ_100_67`
  (`validation.cpp:6691-6692`).

Edge cases (current code):
- **OP_CAT** `(x1 x2 -- x1||x2)`: `|x1|+|x2| > 520` gives PUSH_SIZE. Fewer than 2 items gives
  INVALID_STACK_OPERATION.
- **OP_SPLIT** `(x n -- x[0:n] x[n:])`: `n` is a 4-byte CScriptNum (MINIMALDATA-sensitive), then
  `getint`. `n<0 || n>|x|` gives INVALID_SPLIT_RANGE. n==0 and n==|x| give an empty piece.
- **OP_NUM2BIN** `(num size -- bin)`: `size` is a 4-byte CScriptNum cast to **uint64** (negative
  becomes huge). `size > 520` gives PUSH_SIZE. Then `MinimallyEncode(num)` **in place**; the num
  operand has **no 4-byte limit and no MINIMALDATA check** (it can be any length up to 520).
  `|num| > size` gives IMPOSSIBLE_ENCODING. Equal returns as is. Otherwise strip the sign bit, pad
  with 0x00 to size-1, and append the sign byte.
- **OP_BIN2NUM** `(bin -- num)`: `MinimallyEncode` (`script.cpp:339-384`), then
  `IsMinimallyEncoded(n, 4)`, else INVALID_NUMBER_RANGE. In effect the minimal encoding must be
  ≤ 4 bytes.
- **OP_AND/OR/XOR**: sizes must be equal, else INVALID_OPERAND_SIZE.
- **OP_DIV/OP_MOD**: see above.
- **OP_CHECKDATASIG** `(sig msg pubkey -- bool)` (1048-1089):
  1. `CheckSignatureEncoding(sig)` is the **same function as CHECKSIG**, so it expects the last
     byte to be a "hashtype". Under DERSIG (consensus), a non-empty sig must be strict DER **plus
     exactly one trailing byte** (any value at consensus; STRICTENC policy requires 0x01-03/0x81-83;
     LOW_S checks S without the trailing byte).
  2. `CheckPubKeyEncoding`.
  3. If the sig is non-empty, `hash = SHA256(msg)` (single SHA256) and
     `CPubKey(pubkey).Verify(hash, sig)` on the **whole sig including the trailing byte**. The lax
     DER parser ignores trailing garbage (`src/pubkey.cpp:40-180`; doc comment at 29-38 says
     "garbage at the end").
  4. NULLFAIL if it fails with a non-empty sig.
  5. Pop 3, push the result; VERIFY variant gives CHECKDATASIGVERIFY.

  It does **not** go through `BaseSignatureChecker`, so no sigcache, and mocked checkers in fuzz
  targets don't intercept it. There is no FindAndDelete, and sigops are not counted.
  `checkdatasig_tests.cpp:117-119` confirms that valid sigs have `0x01` appended.

### 3.5 Signature / pubkey encoding — `interpreter.cpp:63-222`, `src/pubkey.cpp`

- `IsValidSignatureEncoding` (107-170) is the BIP66 strict-DER check: 9 to 73 bytes including the
  hashtype byte, `sig[1]==size-3`, no negative or over-padded R/S.
- `CheckSignatureEncoding` (200-215): an empty sig is always OK. With any of DERSIG|LOW_S|STRICTENC,
  require strict DER. LOW_S then requires `CheckLowS(sig[0..len-1])`. STRICTENC then requires
  `IsDefinedHashtypeSignature` (`(last & ~0x80) ∈ [1,3]`).
- `CheckPubKeyEncoding` (217-222) applies only with STRICTENC: 33 bytes with 02/03, or 65 bytes with
  04. Hybrid 06/07 is rejected.
- **Consensus verify** (`pubkey.cpp:182-197`): `CPubKey::IsValid` uses the length table `GetLen`
  (02/03 → 33, 04/06/07 → 65; `pubkey.h:59-65`), so a length mismatch makes the key invalid and
  CheckSig returns false. Then `secp256k1_ec_pubkey_parse`, which **accepts hybrid 06/07 with a
  y-parity check** (`src/secp256k1/src/eckey_impl.h:21-31`). Then `ecdsa_signature_parse_der_lax`,
  which tolerates negative ints, padding, long length descriptors and **trailing garbage**. R/S
  overflow does **not** fail the parse; it yields an all-zero sig that later fails verification
  (`pubkey.cpp:170-178`). Then **`secp256k1_ecdsa_signature_normalize`**, so high-S verifies at
  consensus. Then verify.
- `CheckLowS` (`pubkey.cpp:348-354`): lax parse plus "normalize did nothing". [QUIRK] A strict-DER
  sig with S ≥ n parses as zero, so it counts as "low S". Under LOW_S+NULLFAIL you get NULLFAIL
  instead of SIG_HIGH_S. This only affects policy error codes.
- `CheckSig` (1542-1562): `IsValid` pubkey, non-empty sig, `nHashType = sig.back()` (0..255),
  sighash, Verify.

### 3.6 CHECKSIG / CHECKMULTISIG details

- `EvalChecksig` (321-343): `scriptCode = [pbegincodehash, pend)`. Then
  `FindAndDelete(scriptCode, CScript() << sig)` (BASE always); a hit plus CONST_SCRIPTCODE gives
  SIG_FINDANDDELETE. Then the encoding checks, then CheckSig, then NULLFAIL. [QUIRK] For an **empty
  sig**, `CScript() << {}` is the single byte `0x00`, so FindAndDelete removes every OP_0 from
  scriptCode (and under CONST_SCRIPTCODE that fails).
- `FindAndDelete` (224-250) matches only at opcode boundaries, matches repeated adjacent occurrences,
  and copies the rest verbatim after a parse failure. The pattern is the `operator<<(vector)` push
  form (`script.h:448-475`: <0x4c direct, ≤0xff PUSHDATA1, ≤0xffff PUSHDATA2, else PUSHDATA4; **no**
  OP_N substitution). A non-canonically pushed sig in the scriptCode is not removed.
- CHECKMULTISIG (1091-1199): `nKeys ∈ [0,20]` else PUBKEY_COUNT, and `nOpCount += nKeys` (>201 gives
  OP_COUNT). `nSigs ∈ [0,nKeys]` else SIG_COUNT. FindAndDelete runs for every sig first. Then the
  in-order matching loop with early exit when `nSigs > nKeys` remain. Encoding is checked only for
  the (sig, key) pairs actually reached. NULLFAIL uses `ikey2` bookkeeping (1168-1175), and the
  dummy element needs NULLDUMMY → SIG_NULLDUMMY (1183-1187).

### 3.7 Sighash (legacy only) — `interpreter.cpp:1340-1534`

`SignatureHash(scriptCode, tx, nIn, nHashType, amount, sigversion, cache)`. `amount` and `cache` are
**ignored**.

1. `assert(nIn < vin.size())`.
2. **SIGHASH_SINGLE bug**: if `(nHashType & 0x1f)==3 && nIn >= vout.size()`, return
   `uint256::ONE` = bytes `01 00…00` (1516-1522; `src/uint256.cpp:73`). The signature is then
   verified against that constant.
3. Serialize (`CTransactionSignatureSerializer`, 1346-1440):
   - the 32-bit version/type field, encoded exactly as the tx serializer encodes it;
   - compactsize n_in = ANYONECANPAY(0x80) ? 1 : |vin|;
   - each input: prevout; script = scriptCode with **all OP_CODESEPARATOR opcodes removed** for
     nIn, else empty; nSequence = 0 for other inputs if SINGLE or NONE;
   - compactsize n_out = NONE ? 0 : SINGLE ? nIn+1 : |vout|. For SINGLE, other outputs are the
     **null CTxOut: nValue = -1 (ff×8) and an empty script** (`primitives/transaction.h:192-206`);
   - nLockTime;
   - **if `nVersion>=3 && nType!=0`, vExtraPayload** (1437-1438), i.e. special-tx payloads are
     signed by input scripts.
4. Append `nHashType` as a **4-byte LE int** (the full value; tests pass 32-bit values), then
   SHA256d.

Base type is `nHashType & 0x1f`, so 0x23 and 0x43 are SINGLE. Undefined bases (0, 4..31) act as ALL.
STRICTENC is the only thing restricting them, and it's policy.

`SerializeScriptCode` (1364-1385) with a malformed tail can write fewer bytes than its compactsize
claims. That's harmless, because a script with a parse error always fails in EvalScript anyway.

`sighash_tests.cpp:24-81` keeps `SignatureHashOld` (FindAndDelete-of-OP_CODESEPARATOR reference) for
random cross-checks. `RandomTransaction` only makes versions 1-2, so special-tx sighash is
**untested by vectors**.

### 3.8 VerifyScript — `interpreter.cpp:1652-1717`

1. SIGPUSHONLY.
2. Eval scriptSig.
3. Copy the stack if P2SH.
4. Eval scriptPubKey.
5. Non-empty and CastToBool, else EVAL_FALSE.
6. If P2SH and `scriptPubKey.IsPayToScriptHash()` (exact `a9 14 <20> 87`, `script.cpp:213-220`):
   scriptSig must be `IsPushOnly` (OP_RESERVED counts as push, `script.cpp:237-252`) else
   SIG_PUSHONLY. Restore the copied stack, pop the redeemScript, eval it, require non-empty and true.
7. CLEANSTACK, which requires stack size == 1.

There is no Bitcoin "BIP16 exception block". P2SH has been on from genesis on all networks: it was
`BIP16Height = 0` everywhere, and `5033bdef73` (2024-10-17) hardcoded it (`validation.cpp:2424-2426`).

### 3.9 CScriptNum — `script.h:211-382`, `script.cpp:311-384`

Default max 4 bytes; CLTV/CSV use 5. A non-minimal encoding when `fRequireMinimal` throws, which
becomes UNKNOWN_ERROR. `getint()` clamps to int32. `serialize` is little-endian sign-magnitude.
`IsMinimallyEncoded` is a static helper (BCH-derived) with semantics equal to Bitcoin's.

---

## 4. Block script flags, caching, assumevalid — difficulty S (flags) / M (cache + parallel)

### 4.1 `GetBlockScriptFlags` — `src/validation.cpp:2420-2448`

```
flags = P2SH                                     // always
if DeploymentActiveAt(*pindex, DERSIG)  |= DERSIG       // BIP66Height
if DeploymentActiveAt(*pindex, CLTV)    |= CHECKLOCKTIMEVERIFY  // BIP65Height
if DeploymentActiveAt(*pindex, CSV)     |= CHECKSEQUENCEVERIFY  // CSVHeight
if DeploymentActiveAt(*pindex, BIP147)  |= NULLDUMMY    // BIP147Height
```

`DeploymentActiveAt` for buried deployments is `index.nHeight >= height` (`src/deploymentstatus.h:34-38`).
`DeploymentActiveAfter(prev)` is `prev.nHeight+1 >= height` (14-18). Heights map via
`Consensus::Params::DeploymentHeight` (`src/consensus/params.h:201-238`).

Mainnet (`src/chainparams.cpp:178-196`):

| Rule | Height | Block hash comment |
|---|---|---|
| BIP34 (height in cb) | 951 | 000001f35e70… |
| BIP66 DERSIG | 245,817 | 00000000000b1fa2… |
| BIP65 CLTV | 619,382 | 00000000000076d8… |
| CSV (BIP68/112/113) | 622,944 | 00000000000002e3… |
| DIP0001 (2 MB, 40k sigops, 100 kB tx) | 782,208 | |
| BIP147 NULLDUMMY | 939,456 | |
| DIP0003 / enforcement | 1,028,160 / 1,047,200 | |
| DIP0020 (now LLMQ_100_67 only) | 1,516,032 | |

So mainnet flag eras are: [0,245817) P2SH; +DERSIG; [619382) +CLTV; [622944) +CSV; [939456) +NULLDUMMY.

Testnet (`chainparams.cpp:378-390`): BIP34 76, BIP66 2075, BIP65 2431, **BIP147 4300 comes before
CSV 8064**, DIP0001 5500, DIP0003 7000/7300, DIP0020 414100.

Devnet (552-563): all 1, DIP0001/0003 2. Regtest (784-797): BIP147 0, others 1, DIP0003 432/500.
Regtest heights can be overridden with `-testactivationheight` (1015-1031).

There are no per-block script-flag exceptions in Dash.

### 4.2 CheckInputScripts / caching — `validation.cpp:2154-2238`, `2107-2111`

- Coinbase is skipped.
- Script-execution cache key = `SHA256(salt ‖ txid ‖ flags as host-endian uint32)` (2172-2177). Hits
  skip all inputs.
- Per input: `CScriptCheck(coin.out, tx, i, flags)`, which runs
  `VerifyScript(scriptSig, prevScriptPubKey, flags, CachingTransactionSignatureChecker)`.
- On failure with any non-mandatory flags set, it retries without them. If that passes, the result is
  `TX_NOT_STANDARD` "non-mandatory-script-verify-flag (...)"; otherwise `TX_CONSENSUS`
  "mandatory-script-verify-flag-failed (...)" (2197-2225).
- Signature cache: `src/script/sigcache.cpp:47-51`, entry = `SHA256(nonce64 ‖ sighash ‖ pubkey ‖ sig)`.
  It's local-only and has no consensus effect.
- ConnectBlock passes `fCacheResults = fJustCheck` (2749), so it consults the cache but doesn't
  insert when actually connecting. With parallel checks, results come from `control.Wait()`.

### 4.3 assumevalid — `validation.cpp:2543-2571`

`fScriptChecks=false` iff all of these hold:
- the assumevalid hash is in the block index;
- pindex is its ancestor;
- pindex is an ancestor of `m_best_header`;
- `best_header.nChainWork >= MinimumChainWork`;
- the proof-equivalent time between the best header and pindex is **> 2 weeks**.

Defaults: mainnet `defaultAssumeValid` = …dbab492 @ 2,487,500 and `nMinimumChainWork` …d14d @
2,487,500 (`chainparams.cpp:221-222`); testnet @ 1,400,000 (421-422).

When script checks are skipped:
- CheckInputScripts is skipped entirely.
- **Dash-specific: the same flag is passed as `fCheckCbTxMerkleRoots` into `ProcessSpecialTxsInBlock`
  (`validation.cpp:2664`; `src/evo/specialtxman.cpp:801, 817, 835, 872`), so ProTx payload
  signatures and the CbTx merkle-root checks (MN list/quorums) are skipped too.**

These still run: CheckTxInputs, BIP68, sigop limits, BIP30, the special-tx state transitions, and
merkle/PoW.

---

## 5. POLICY (not consensus) — difficulty M

All [POLICY]. Constants are in `src/policy/policy.h:22-95`:

| Constant | Value |
|---|---|
| `MAX_STANDARD_TX_SIZE` | 100,000 (rule is `sz >= limit` → "tx-size", `policy.cpp:87-91`) |
| `MIN_STANDARD_TX_SIZE` | 65 (full serialized size → "tx-size-small", `validation.cpp:874-875`) |
| `MAX_P2SH_SIGOPS` | 15 |
| `MAX_STANDARD_TX_SIGOPS` | 4000 |
| `DEFAULT_BYTES_PER_SIGOP` | 20 |
| `MAX_STANDARD_SCRIPTSIG_SIZE` | 1650 |
| `DUST_RELAY_TX_FEE` | 3000 sat/kB |
| `DEFAULT_MIN_RELAY_TX_FEE` | 1000 |
| `DEFAULT_INCREMENTAL_RELAY_FEE` | 1000 |
| `DEFAULT_PERMIT_BAREMULTISIG` | true |
| `TX_MAX_STANDARD_VERSION` | 3 |
| `MAX_OP_RETURN_RELAY` (default `-datacarriersize`) | 83 (`src/script/standard.h:37`; `DEFAULT_ACCEPT_DATACARRIER=true` at :17) |

- `GetDustThreshold`: 0 if unspendable, else `fee(serSize(txout)+148)` at the dust rate.
  `CFeeRate::GetFee` uses `ceil`, with a minimum of 1 sat (`src/policy/feerate.cpp:23-37`). P2PKH
  dust is 546 sat.
- `IsStandard` (`policy.cpp:52-74`): `Solver` types are PUBKEY, PUBKEYHASH, SCRIPTHASH, MULTISIG,
  NULL_DATA (`src/script/standard.cpp:115-159`; no witness types). Bare multisig must have n ≤ 3 and
  1 ≤ m ≤ n. NULL_DATA must satisfy `size ≤ max_datacarrier_bytes`.
- `IsStandardTx` (`policy.cpp:76-152`):
  - version in [1,3], so **negative and 0 are nonstandard**;
  - size < 100k;
  - scriptSig ≤ 1650 bytes and push-only;
  - every output standard (exception: the V24 shared-collateral template as the declared output of
    a shared ProRegTx, 118-129);
  - no bare multisig if not permitted;
  - no dust;
  - at most 1 OP_RETURN.
- `AreInputsStandard` (`policy.cpp:170-205`): the prevout type must not be NONSTANDARD (exception:
  ProDisTx spending the shared-collateral template). For P2SH, eval the scriptSig with
  `SCRIPT_VERIFY_NONE`; the redeemScript accurate sigops must be ≤ 15.
- `IsStandardSpecialTx` (`src/evo/specialtxman.cpp:1841-1867`): asset lock with ≤ 100 inputs and
  ≤ 20480 bytes, and payload version 2 is nonstandard.
- `MemPoolAccept::PreChecks` (`validation.cpp:828-1121`), in order:
  1. CheckTransaction.
  2. ContextualCheckTransaction (tip).
  3. `qc-not-allowed` (quorum commitment, TX_CONSENSUS) and `coinbase`.
  4. IsStandardTx and IsStandardSpecialTx (if `require_standard`; `-acceptnonstdtxn` is only
     allowed on test chains, `src/node/mempool_args.cpp:91-92`).
  5. tx-size-small; non-final; already in mempool.
  6. InstantSend lock conflict `tx-txlock-conflict`.
  7. **No RBF**: any mempool conflict gives `txn-mempool-conflict`.
  8. Asset-unlock index checks (`txn-already-known`, `assetunlock-stale-instance`).
  9. Missing inputs; non-BIP68-final; CheckTxInputs (tip+1).
  10. V24 shared-collateral checks.
  11. AreInputsStandard; sigops ≤ 4000; min relay fee (skipped for MNHF_SIGNAL); mempool min fee;
      ancestor limits with CPFP carve-out.
  12. `CheckSpecialTxForMempool`; `protx-dup` (`existsProviderTxConflict`) and the cross-scheme
      operator-key `protx-dup`.
- `PolicyScriptChecks` uses `STANDARD_SCRIPT_VERIFY_FLAGS` (1176-1191). `ConsensusScriptChecks`
  re-runs with `GetBlockScriptFlags(Tip)`; note it is **tip**, not tip+1 (1194-1225).
- vsize = `max(size, sigops*20)` (`policy.cpp:207-215`).

---

## 6. libdashconsensus — difficulty M (to use for differential testing)

- **Builds by default**: `--with-libs` defaults to yes (`configure.ac:775-778`). `--enable-fuzz`
  forces `build_bitcoin_libs=no` (`configure.ac:1412-1421`). `HAVE_CONSENSUS_LIB` and
  `libdashconsensus.pc` come from 1765-1767. The Makefile builds `libdashconsensus.la` from
  `libbitcoin_consensus_a_SOURCES` plus crypto (`src/Makefile.am:1415-1427`, sources 934-972), links
  **dashbls, secp256k1, GMP**, and builds with `-DDISABLE_OPTIMIZED_SHA256`. The installed header is
  `script/bitcoinconsensus.h`. Root `CMakeLists.txt` is a CLion dummy and is not a real build.
- **Deprecated**: "will be removed for v28" (`doc/release-notes-29189.md`, `doc/shared-libraries.md:4`).
  Expect it to vanish when the upstream removal is backported.
- **API** (`src/script/bitcoinconsensus.h:34-67`, `.cpp:73-103`):
  - `int dashconsensus_verify_script(spk, spkLen, txTo, txToLen, nIn, flags, err*)`;
  - `unsigned dashconsensus_version()`, which returns `BITCOINCONSENSUS_API_VER = 0`.
  - There's **no amount or spent-outputs variant**: it uses amount 0, which is fine since there's
    no BIP143.
  - Error enum: OK, TX_INDEX, TX_SIZE_MISMATCH, TX_DESERIALIZE, INVALID_FLAGS.
  - Accepted flags are **only** P2SH, DERSIG, NULLDUMMY, CLTV, CSV (`VERIFY_ALL`); anything else gives
    ERR_INVALID_FLAGS (68-71).
  - It deserializes a full Dash `CTransaction` (special payloads included) and requires an exact
    length match.
- **DIP0020**: verified unconditionally, since the opcodes have no flag anymore. CHECKDATASIG works.
- **For differential fuzzing**: usable for consensus-flag parity only. It returns just 0/1: no
  `ScriptError`, no policy flags, no sighash, and no EvalScript-level access. Better: write a small
  C++ shim against `libbitcoin_consensus.a` (or the experimental `libdashkernel`,
  `configure.ac:780-784`) exposing `EvalScript`, `VerifyScript` (all flags plus ScriptError),
  `SignatureHash`, `CheckTransaction` and `GetSigOpCount`. Then cross-check the Rust interpreter in
  cargo-fuzz. `script_tests.cpp:131-139` already cross-checks libdashconsensus inside `DoTest` when
  the flags are a subset.
- **Relevant fuzz targets** (`src/test/fuzz/`):
  - `script_flags` (VerifyScript monotonicity across flag add/remove; uses `IsValidFlagCombination` =
    CLEANSTACK ⇒ P2SH, `src/test/util/script.cpp:8-12`);
  - `eval_script` (EvalScript with arbitrary flags and a null checker);
  - `script_interpreter` (SignatureHash, CastToBool);
  - `signature_checker` (fuzzed CheckSig/LockTime/Sequence results; **CHECKDATASIG bypasses it**);
  - `script_bitcoin_consensus` (libdashconsensus API);
  - `script`, `script_ops`, `scriptnum_ops`, `script_sigcache`, `script_format`, `parse_script`;
  - `secp256k1_ecdsa_signature_parse_der_lax`;
  - `transaction`, `decode_tx`, `primitives_transaction`, `tx_in`, `tx_out`;
  - `deserialize`, `block`, `coins_view`, `tx_pool`, `utxo_total_supply`.

  None of them checks tx serialize round-trips, which would catch §1.6.

---

## 7. Test vectors — difficulty S-M (harness)

| File | Rows | Format / flags | Consumer |
|---|---|---|---|
| `src/test/data/script_tests.json` | 1334 rows: 1282 tests (770 with 4 cols, 512 with a comment col), 52 comment rows | `[scriptSig, scriptPubKey, flags, expected_error, comment?]` (no witness/amount column). Flags used: P2SH 957, STRICTENC 970, MINIMALDATA 115, NULLFAIL 40, DERSIG 37, DISCOURAGE_UPGRADABLE_NOPS 12, CSV 6, LOW_S/NULLDUMMY/SIGPUSHONLY/CLEANSTACK 3 each. **No CLTV or CONST_SCRIPTCODE.** | `script_tests.cpp:879-907` (`DoTest` 117-140) |
| `tx_valid.json` | 162 rows / 83 tests (53 v1, 30 v2) | `[[[prevhash, n, spk],…], rawtx, EXCLUDED_flags]`; the tx must pass `~flags`; "NONE" means 0 | `transaction_tests.cpp:154-236` |
| `tx_invalid.json` | 154 rows / 76 tests (62 v1, 14 v2) | same shape; flags are *applied*; `BADTX` means CheckTransaction must fail (10 cases) | `transaction_tests.cpp:238-318` |
| `sighash.json` | 1 header + 500 | `[rawtx, script, nIn, hashType(int32), expected]`; **all version 1/2, type 0, no special-tx vectors** | `sighash_tests.cpp:160-208` |
| `trivially_valid.json` / `trivially_invalid.json` | 25 / 12 vectors | ProTx payload trivial validation (proreg/upserv/upreg/uprev; "basic" vs "legacy" BLS). Not script-layer. | `evo_trivialvalidation.cpp` |

- Flag parsing: `mapFlagNames` / `ParseScriptFlags` (`transaction_tests.cpp:39-69`; shared with
  script_tests via extern decl at `script_tests.cpp:44`). Names are P2SH, STRICTENC, DERSIG, LOW_S,
  SIGPUSHONLY, MINIMALDATA, NULLDUMMY, DISCOURAGE_UPGRADABLE_NOPS, CLEANSTACK, NULLFAIL,
  CHECKLOCKTIMEVERIFY, CHECKSEQUENCEVERIFY, CONST_SCRIPTCODE. Unknown names give a test error.
- Error names: `script_tests.cpp:53-104`. Dash adds CHECKDATASIGVERIFY, SPLIT_RANGE, OPERAND_SIZE,
  INVALID_NUMBER_RANGE, DIV_BY_ZERO, MOD_BY_ZERO. IMPOSSIBLE_ENCODING is **missing** from that table,
  and no JSON vector uses it.
- Script text parsing: `ParseScript` (`src/core_read.cpp:63-103`):
  - a decimal word becomes `CScript << int64` (OP_N for -1, 0, 1..16; ±0xFFFFFFFF limit);
  - `0x..` hex is inserted raw (not pushed);
  - `'str'` is pushed;
  - opcode names are accepted with or without `OP_`, built from `GetOpName` up to `MAX_OPCODE =
    0xbb`.

  Note that 0xb1/0xb2 parse as `CHECKLOCKTIMEVERIFY`/`CHECKSEQUENCEVERIFY`, not NOP2/NOP3.
- Dash-specific content in script_tests.json:
  - sections for NUM2BIN (864), BIN2NUM (881), CAT/SPLIT (~833-863), AND/OR/XOR (+invalid, 921-1128),
    DIV (1129), MOD (1150), CHECKDATASIG (1170), CHECKDATASIGVERIFY (1205);
  - `Disabled opcodes` (912-918) covers only INVERT/2MUL/2DIV/MUL/LSHIFT/RSHIFT.

  There's **no positive CHECKDATASIG signature vector** in JSON. Positive cases live only in
  `checkdatasig_tests.cpp`. The extra DIP0020 C++ suites are `dip0020opcodes_tests.cpp` (787 lines;
  bitwise, string, type-conversion, div/mod) and `checkdatasig_tests.cpp` (233 lines, LCG-random
  flags).
- Other script/tx unit suites: `script_p2sh_tests`, `multisig_tests`, `sigopcount_tests`,
  `scriptnum_tests` (+`scriptnum10.h` reference), `script_standard_tests`, `txvalidationcache_tests`,
  `txvalidation_tests`, `script_p2pk(h)_tests`.
- Harness quirk: the flag-perturbation loops use `1U << i` for `i < mapFlagNames.size()` (13), which
  covers bits 0..12. Dash's NULLFAIL (bit 14) and CONST_SCRIPTCODE (bit 16) are never perturbed
  (`transaction_tests.cpp:215-224, 297-306`). `ExcludeIndividualFlags` (140-151) never pushes
  anything because its `find(...) != end()` test is inverted, so the "flags are maximal" check is a
  no-op. Both are test-only. Don't copy them blindly.

---

## 8. Historical variations and quirks to replicate

1. **P2SH from genesis** on every network (no BIP16 switch time or exception block). Hardcoded in
   `5033bdef73` (2024-10-17). Before that it was `BIP16Height=0`.
2. **DIP0020 opcodes** went from Bitcoin-disabled, to gated at height (2020-12 / 2021-01), to enabled
   from genesis (`61bc3002b0`, #6302, 2024-10). Today's rule is a strict relaxation of all prior
   rules, so implement it with no gating. Old nodes would reject blocks the current rule accepts
   (e.g. OP_CAT below 1516032), but no such blocks exist on the canonical chain.
3. **DIP2 tx format** (`a3c4ee3fda`, 2018-02-12) split the 32-bit version into a version half and a type half
   (the type became `uint16` in `ead46a6236`, 2021-06-05). Historic pre-DIP2 txs are reinterpreted with
   the new split. For example, rust-dashcore reports a mainnet tx with type bytes 0x2a; Core reads it
   as nVersion=0 (so no BIP68), which is a relaxation.
4. **BIP113 / LOCKTIME_MEDIAN_TIME_PAST** is now tied to the CSV height. The flag constant was removed
   (`85b0b78d47`, `1c1fcc60a8`).
5. **DIP0001 tx-size** consensus rule uses the policy constant (`cd262bf641`, `b6896387ae`).
6. **BIP30**: `IsBIP30Repeat`/`IsBIP30Unspendable` always return false. Enforcement is skipped above
   BIP34 on the known chain, and there's no Bitcoin-style `BIP34_IMPLIES_BIP30_LIMIT`.
7. **CHECKDATASIG** needs a trailing "hashtype" byte under DERSIG, verifies over the full sig, is free
   of sigops, and bypasses the checker.
8. **SIGHASH_SINGLE bug** returns ONE, hashtype bits are masked with 0x1f, and the hashtype is
   serialized as 4 bytes.
9. **FindAndDelete** on BASE scripts, including the empty-sig `0x00` deletion case.
10. **Exceptions become UNKNOWN_ERROR** (overflowed CScriptNum, empty pops). Disabled opcodes and
    OP_VERIF/OP_VERNOTIF fail even in unexecuted branches.
11. **v2 asset-unlock txid** zeroes the trailing 132 bytes (new, V24-era).

---

## 9. What rust-dashcore has today

- `rust-dashcore:dash/src/blockdata/script/` (borrowed.rs, owned.rs, builder.rs, instruction.rs,
  push_bytes.rs) covers parsing, building, push iteration and template predicates (p2pkh/p2sh plus
  inherited witness helpers such as `is_witness_program`/`is_v0_p2wpkh`). **No EvalScript or
  VerifyScript, no sigop counting, no CScriptNum semantics.**
- Verification is only `Script::verify[_with_flags]` / `Transaction::verify[_with_flags]` behind the
  optional `bitcoinconsensus` feature (`dash/Cargo.toml:52`: `bitcoinconsensus = "0.20.2-0.5.0"`,
  which wraps **Bitcoin Core 0.20.2's** libbitcoinconsensus; `borrowed.rs:393-430`,
  `transaction/mod.rs:454-470`). It's unusable for Dash: special txs carry a trailing payload, which
  gives a size mismatch or deserialize error; DIP0020 opcodes fail as disabled or bad; and its
  VERIFY_ALL includes witness flags.
- The opcode table is Bitcoin's: `opcodes.rs:202-203,264,358-395` classify OP_CAT/OP_SUBSTR/AND/OR/
  XOR/DIV/MOD as illegal and 0xba as `OP_CHECKSIGADD`, with ≥ 0xba treated as ReturnOp. That's wrong
  for Dash (0xba/0xbb are CHECKDATASIG(VERIFY)).
- `crypto/sighash.rs`: `legacy_encode_signing_data_to` (618-750) does include
  `special_transaction_payload` and handles SINGLE via `UINT256_ONE` (36-43, correct byte order).
  It does **not** strip OP_CODESEPARATOR or FindAndDelete (the caller has to), it carries a version
  `u16`, and it still exposes segwit/taproot sighash, which is irrelevant for
  Dash.
- `Transaction` encode/decode (`transaction/mod.rs:542-675`) still has BIP144 witness
  serialization paths (empty-vin uses the segwit marker except for 4 special types). That
  representation isn't faithful to Core's parser, which has no witness at all.
- Constants (`blockdata/constants.rs`, `policy.rs`) are Bitcoin-segwit (MAX_BLOCK_WEIGHT,
  80k sigop cost, 400k standard weight). They're wrong for Dash.

**Conclusion:** the Rust full node needs a fresh, Core-faithful consensus crate: a raw tx model
(Core's version/type halves + raw payload bytes), the interpreter, sighash, sigops and flags. Reuse rust-dashcore only
for hashing primitives and maybe script byte parsing.

---

## 10. Difficulty summary

| Area | Est. |
|---|---|
| CheckTransaction and constants | S |
| Contextual (IsFinalTx, BIP68, CheckTxInputs, sigops, ContextualCheckTransaction) | M (needs MTP/ancestor index and a coins view) |
| Interpreter (≈1700 C++ LOC, exact error codes, DIP0020, CHECKDATASIG quirks) | L |
| Sighash (+ version/type encoding, payload) | S-M |
| ECDSA layer (lax DER, normalize, hybrid keys; use rust-secp256k1 `from_der_lax`+`normalize_s`, then diff-fuzz against Core's `ecdsa_signature_parse_der_lax`) | M |
| Block flags, assumevalid, caches, parallel check queue | M |
| Policy / mempool (excluding special-tx mempool logic) | M-L |
| Differential fuzz infra (C++ shim + cargo-fuzz) | M |
| Vector harness (ParseScript, flag maps, 5 JSON files) | S-M |
| Historical audit (chain scan for version/type oddities, DIP0020 pre-activation use) | M |

## Erratum (2026-10-08, from CS1 design)
- R4-A §3.7: the sighash scriptCode with a malformed trailing push is not harmless. Core writes a length counting the whole script but stops writing bytes where its parser stopped, so the error code differs from a naive port. Registered as **quirk 39** (DEC-25).
