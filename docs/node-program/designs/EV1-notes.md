# EV1: implementation notes

Slice decision rows that are not design changes (the design is `EV1-specialtx.md`; its D-rows are cited "design D*n*"). Core citations are at `v24.0.0-rc.4`.

## EV1-5a: `check_cbtx` (DEC-749)

| Row | Decision | Why |
|---|---|---|
| 5a-D1 | **Branch.** `node/ev1-5a` from `node/integration` a64e41abe. Local, signed, not pushed | manager brief |
| 5a-D2 | **Scope.** `dash_dml::specialtx::cbtx`: `check_cbtx` is Core's `CheckCbTx` (`cbtx.cpp:24-55`) with design §2.4's signature, pure over the decoded `CoinbaseCommitment`. `cbtx_payload` holds the two rejects both callers make before it, `bad-cbtx-invalid` (not a coinbase) then `bad-cbtx-payload` (not a `CCbTx` read to its last byte). The dispatcher's type-5 arm stays `Unported`: its binding, the merkle roots, the ChainLock check and anything on A-1 are EV1-5 | DEC-749 |
| 5a-D3 | **The pin, not design D18's wording, below DIP0003.** §6.1's D18 vector says a pre-DIP0003 regtest block with "a valid v1 or v2 CbTx" is accepted. At the pin that holds for v2 only. Regtest's `DIP0008Height` is 1 (`chainparams.cpp:822`), so from block 2 `DeploymentActiveAt(*pindexPrev, DIP0008)` is true and v1 is `bad-cbtx-version` (`cbtx.cpp:35-38`). v1 passes only in block 1, or with `-testactivationheight=dip0008@N` above DIP0003. Versions 3 and 4 are always `bad-cbtx-version` below DIP0003: V20 is never active there, because Core asserts `V20Height >= DIP0003Height` (`chainparams.cpp:963-964`) after both `-testactivationheight` and `-dip3params` (`:879-880`), and `!isV20 && nVersion >= 3` refuses them (`cbtx.cpp:41`). Mainnet and testnet activate DIP0008 and V20 after DIP0003, so v1 and v2 pass below it. Devnet activates DIP0003, DIP0008 and V20 together at 2 (`chainparams.cpp:576-585`), so only block 1 is below DIP0003, and it takes v1 or v2; `-testactivationheight` and `-dip3params` are regtest's (`CRegTestParams`) and do not move them. The code follows the pin; the tests `below_dip0003_*` pin each case. **Ask:** EV1-5's D18 vector should name v2 (or v1 in block 1) | DEC-730 needs Core-exact pre-DIP0003 verdicts |
| 5a-D4 | **Height.** Step 2 compares `cbTx.nHeight` with `cons.height`, which `ConsensusCtx` defines as `pindexPrev`'s height plus one, as `i64` (Core's `int`, no overflow) | `cbtx.cpp:31` |
| 5a-D5 | **Version constants.** `CBTX_VERSION_INVALID` (0), `_MERKLE_ROOT_ASSETUNLOCKS` (4) and `_UNKNOWN` (5) join dash-evo's `_MERKLE_ROOT_QUORUMS` and `_CLSIG_AND_BALANCE`, so `CCbTx::Version` lives in one place | `cbtx.h:32-39` |
| 5a-D6 | **Vectors.** The testnet coinbases at 900,000 (v2, before V20), 905,101 (v3, V20's second block) and 1,569,000 (v3) are copied from dash-evo's Core-produced `cbtx_testnet.json` into `dash-dml/tests/data/cbtx_testnet_coinbases.txt`, with its provenance. Core accepted each block, so each passes both functions at its height. `check_cbtx` is also run directly on each decoded payload one block later and with the other side of V20's version (a boundary test, not a caller's verdict); the callers' verdict on edited wire bytes is its own test (5a-D9). The refusal tests build CbTxs field by field: `CheckCbTx` reads only `nVersion` and `nHeight` | IMPLEMENTER: Core-produced vectors |
| 5a-D7 | **Mutants.** 16 env-gated mutants in a reflink copy, one build, the test binary run per mutant. All are killed, each by the test whose doc names it: 0c, 0p, 1a, 1b, 2g, 2, 2o, 2r, 3, 3a, 4a, 4l, 4h, 5v, 5l, 5h | DEC-749 |
| 5a-D8 | **node-l1's CbTx walk.** `cbtx.rs` landing trips `missing_checks_have_not_landed` (D56), so this slice retires the row: `dml_checks`' CbTx table runs `cbtx_payload` as steps 3.8 (`bad-cbtx-invalid`) and 3.9 (`bad-cbtx-payload`), then `check_cbtx` as §3.10, all ours, over the replay's buried deployments. Core's catch (`failed-check-special-tx`) leaves the row: nothing in the type-5 arm throws (`GetTxPayload` catches its own exceptions, `specialtx.h:25-27`). The missing-check floor goes from 10 to 9. `l1_dml_block` passes (18), probed and checked sequences included | D56; acceptance of the landing slice |
| 5a-D9 | **Round 1 fixes (DEC-775).** Opus m-1: a version-1 or version-2 coinbase with type bits 5 has no payload field, so its slot is `None` and Core says `bad-cbtx-payload`; `cbtx_payload`'s doc and the binding below say the caller keys on the type bits, and a test pins it. Sol m-1: the edited-version cases are now `check_cbtx_boundaries_on_testnet_payloads`, and `testnet_coinbases_edited_on_the_wire` edits the fixtures' payload bytes: the height edit is `bad-cbtx-height`; v2 read as v3 runs short and v3 read as v2 leaves bytes, so both are `bad-cbtx-payload` (`cbtx.h:54-67`, `specialtx.h:20-27`) and never reach `CheckCbTx`. Opus n-1: a type-5 transaction whose first of two inputs is null is not a coinbase (`bad-cbtx-invalid`). Opus n-2 / Sol n-1: design D18's vector amended (v2; v1 in block 1). Opus n-3: devnet in 5a-D3. New env-gated mutants M-0n (a `None` slot as `bad-cbtx-invalid`), M-0i (the coinbase test as "the first input is null") and M-0e (a failed decode as `bad-cbtx-version`) join the 16 of 5a-D7, in a reflink copy: all 19 are killed | DEC-775 |

### How CH7a-2b-2 binds it

Both functions return `TX_CONSENSUS` rejects. `cbtx_payload` borrows the payload from the slot, so it decodes nothing.

**Block path** (`ProcessSpecialTxsInBlock`, `specialtxman.cpp:876-897`), only when `fCheckCbTxMerkleRoots` (`check_sigs`) holds and `vtx[0]`'s type is 5, with no DIP0003 gate (design D18). The gate is `tx0.tx_type() == TxType::CoinbaseCommitment`, Core's `nType`, not whether `tx0` has a payload field: below DIP0003 a version-1 or version-2 coinbase may carry type bits 5, and `cbtx_payload` turns its `None` slot into `bad-cbtx-payload`, as Core does:

```rust
let tx0 = &block.txs()[0];
let cbtx = match cbtx_payload(tx0, payloads.get(0)) {
    Ok(cbtx) => cbtx,
    // :878-880, :890-892: BLOCK_CONSENSUS, the same reason, no debug message.
    Err(e) => return Err(BlockValidationError::new(BlockValidationResult::Consensus, e.reject_reason().to_owned())),
};
// cons.prev = pindex->pprev, cons.height = pindex's height; v24 = GetSpecialTxRules(pprev).v24.
if let Err(e) = check_cbtx(cbtx, cons, rules.v24) {
    // :882-889: BLOCK_CONSENSUS, the same reason, and Core's debug message:
    // "Special Transaction check failed (tx hash {txid}) {tx debug}" (CheckCbTx sets none).
}
// opt_cbTx = Some(cbtx); then CH7a's own :894-897: DeploymentActiveAt(*pindex, DIP0003) and no
// opt_cbTx gives `bad-cbtx-version`.
```

The loop's `i == 0 && nType == 5` skip (`:925`) is CH7a's.

**Mempool and dispatcher path** (`CheckSpecialTxInner`, `:247-255`): EV1-5 binds the type-5 arm as `cbtx_payload(tx, payload).and_then(|c| check_cbtx(c, ctx.evo.cons, ctx.rules.v24))`, after the dispatcher's DIP0003 gate.
