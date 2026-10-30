//! L0 for [`check_asset_lock_tx`] (EV1 §3.12): Core's `evo_assetlocks_tests.cpp` asset-lock cases
//! (v24.0.0-rc.4 `src/test/evo_assetlocks_tests.cpp:73-427`, transcribed with Core's amounts and
//! script shapes), then one refusal per step, each naming the mutant it kills. The per-step cases
//! are the shapes of the L1 catalogue's type-8 mutations, whose verdicts Core gives in
//! `l1_specialtx` (EV1 §6.2).

use dash_consensus::MAX_MONEY;
use dash_consensus::primitives::Tx;
use dash_consensus::validation::TxValidationResult;
use dash_primitives::{AssetLock, TxOut, TxType};

use super::{ASSET_LOCK_CURRENT_VERSION, ASSET_LOCK_INITIAL_VERSION, check_asset_lock_tx};
use crate::specialtx::reasons::{
    BAD_ASSETLOCKTX_CREDIT_OUTOFRANGE, BAD_ASSETLOCKTX_CREDITAMOUNT,
    BAD_ASSETLOCKTX_EMPTYCREDITOUTPUTS, BAD_ASSETLOCKTX_MULTIPLE_RETURN, BAD_ASSETLOCKTX_NO_RETURN,
    BAD_ASSETLOCKTX_NON_EMPTY_RETURN, BAD_ASSETLOCKTX_OPRETURN_OUTOFRANGE, BAD_ASSETLOCKTX_PAYLOAD,
    BAD_ASSETLOCKTX_PUBKEYHASH, BAD_ASSETLOCKTX_SCRIPT_PUBKEY, BAD_ASSETLOCKTX_TYPE,
    BAD_ASSETLOCKTX_VERSION, BAD_ASSETLOCKTX_VERSION_2,
};
use crate::specialtx::testctx::{encoded, tx_of, txin, txout};
use crate::specialtx::trivial::tests::{p2pkh, p2sh};

/// `CENT`.
pub(crate) const CENT: i64 = 1_000_000;
const OP_RETURN: u8 = 0x6a;
pub(crate) const OP_1: u8 = 0x51;

/// `CScript() << OP_RETURN << ParseHex("")`.
pub(crate) fn op_return() -> Vec<u8> {
    vec![OP_RETURN, 0]
}

/// `GetScriptForDestination(PKHash(key.GetPubKey()))` for the test's one key.
pub(crate) fn to_key() -> Vec<u8> {
    p2pkh([7; 20])
}

/// `CreateAssetLockTx`'s credit outputs: 17 and 13 CENT to the key.
pub(crate) fn credits() -> Vec<TxOut> {
    vec![txout(17 * CENT, to_key()), txout(13 * CENT, to_key())]
}

/// `CreateAssetLockTx`'s outputs: the 30 CENT `OP_RETURN` and 20 CENT change to the key.
pub(crate) fn outputs() -> Vec<TxOut> {
    vec![txout(30 * CENT, op_return()), txout(20 * CENT, to_key())]
}

/// A type-`tx_type` transaction with `outputs` and an asset-lock payload of `version` over
/// `credit_outputs`, spending one input as `CreateAssetLockTx` does.
pub(crate) fn typed_lock(
    tx_type: TxType,
    outputs: Vec<TxOut>,
    version: u8,
    credit_outputs: Vec<TxOut>,
) -> Tx {
    let payload = AssetLock {
        version,
        credit_outputs,
    };
    tx_of(tx_type, vec![txin([1; 32], 1)], outputs, encoded(&payload))
}

pub(crate) fn lock(outputs: Vec<TxOut>, version: u8, credit_outputs: Vec<TxOut>) -> Tx {
    typed_lock(TxType::AssetLock, outputs, version, credit_outputs)
}

/// `CheckAssetLockTx(tx, state, v24)`: the reason, after asserting it is TX_BAD_SPECIAL.
fn verdict(tx: &Tx, v24: bool) -> Result<(), String> {
    check_asset_lock_tx(tx, tx.payload(), v24).map_err(|e| {
        assert_eq!(e.result(), TxValidationResult::BadSpecial, "{}", e.reject_reason());
        e.reject_reason().to_owned()
    })
}

/// `CheckAssetLockCommon(version, is_v24_active)` (`evo_assetlocks_tests.cpp:159-331`).
fn check_asset_lock_common(version: u8, v24: bool) {
    let base =
        |outputs: Vec<TxOut>, credit_outputs: Vec<TxOut>| lock(outputs, version, credit_outputs);
    assert_eq!(verdict(&base(outputs(), credits()), v24), Ok(()));

    // Core: test/evo_assetlocks_tests.cpp:198-204
    let wrong_type = typed_lock(TxType::AssetUnlock, outputs(), version, credits());
    assert_eq!(verdict(&wrong_type, v24), Err(BAD_ASSETLOCKTX_TYPE.into()));

    // Core: test/evo_assetlocks_tests.cpp:215-224
    let mut big = outputs();
    big[0].value += 1;
    assert_eq!(verdict(&base(big, credits()), v24), Err(BAD_ASSETLOCKTX_CREDITAMOUNT.into()));
    let mut small = outputs();
    small[1].value -= 1;
    assert_eq!(verdict(&base(small, credits()), v24), Ok(()));

    // Core: test/evo_assetlocks_tests.cpp:227-246
    let mut wrong = credits();
    wrong[0].value += CENT;
    assert_eq!(
        verdict(&base(outputs(), wrong.clone()), v24),
        Err(BAD_ASSETLOCKTX_CREDITAMOUNT.into())
    );
    wrong[1].value -= 2 * CENT;
    assert_eq!(verdict(&base(outputs(), wrong), v24), Err(BAD_ASSETLOCKTX_CREDITAMOUNT.into()));

    // Core: test/evo_assetlocks_tests.cpp:248-270
    let mut out_of_range = credits();
    for value in [0, MAX_MONEY + 1, MAX_MONEY + 1 - out_of_range[1].value] {
        out_of_range[0].value = value;
        let tx = base(outputs(), out_of_range.clone());
        assert_eq!(verdict(&tx, v24), Err(BAD_ASSETLOCKTX_CREDIT_OUTOFRANGE.into()), "{value}");
    }

    // Core: test/evo_assetlocks_tests.cpp:272-286
    let mut not_pubkey = credits();
    not_pubkey[0].script_pubkey = vec![OP_1].into();
    let reason = if version >= 2 {
        BAD_ASSETLOCKTX_SCRIPT_PUBKEY
    } else {
        BAD_ASSETLOCKTX_PUBKEYHASH
    };
    assert_eq!(verdict(&base(outputs(), not_pubkey), v24), Err(reason.into()));

    // Core: test/evo_assetlocks_tests.cpp:288-297
    let mut multiple = outputs();
    multiple[1].script_pubkey = op_return().into();
    assert_eq!(
        verdict(&base(multiple, credits()), v24),
        Err(BAD_ASSETLOCKTX_MULTIPLE_RETURN.into())
    );

    // Core: test/evo_assetlocks_tests.cpp:299-311
    for value in [0, MAX_MONEY + 1] {
        let mut out_of_range = outputs();
        out_of_range[0].value = value;
        let tx = base(out_of_range, credits());
        assert_eq!(verdict(&tx, v24), Err(BAD_ASSETLOCKTX_OPRETURN_OUTOFRANGE.into()), "{value}");
    }

    // Core: test/evo_assetlocks_tests.cpp:314-321
    let mut no_return = outputs();
    no_return[0].script_pubkey = to_key().into();
    assert_eq!(verdict(&base(no_return, credits()), v24), Err(BAD_ASSETLOCKTX_NO_RETURN.into()));

    // Core: test/evo_assetlocks_tests.cpp:323-330
    let mut data = outputs();
    data[0].script_pubkey = vec![OP_RETURN, 2, 0xab, 0xcd].into();
    assert_eq!(verdict(&base(data, credits()), v24), Err(BAD_ASSETLOCKTX_NON_EMPTY_RETURN.into()));
}

// Core: test/evo_assetlocks_tests.cpp:333-338
#[test]
fn evo_assetlock() {
    check_asset_lock_common(ASSET_LOCK_INITIAL_VERSION, false);
    check_asset_lock_common(ASSET_LOCK_INITIAL_VERSION, true);
    check_asset_lock_common(ASSET_LOCK_CURRENT_VERSION, true);
}

/// `GetScriptForDestination(ScriptHash(CScript() << OP_1))`; the check reads only its shape.
pub(crate) fn to_script() -> Vec<u8> {
    p2sh([9; 20])
}

// Core: test/evo_assetlocks_tests.cpp:340-427
#[test]
fn evo_assetlock_v2() {
    let v2 = ASSET_LOCK_CURRENT_VERSION;
    assert_eq!(verdict(&lock(outputs(), v2, credits()), true), Ok(()));
    assert_eq!(
        verdict(&lock(outputs(), v2, credits()), false),
        Err(BAD_ASSETLOCKTX_VERSION_2.into())
    );

    let to_script_hash = vec![txout(30 * CENT, to_script())];
    assert_eq!(verdict(&lock(outputs(), v2, to_script_hash.clone()), true), Ok(()));
    let v1 = lock(outputs(), ASSET_LOCK_INITIAL_VERSION, to_script_hash);
    assert_eq!(verdict(&v1, true), Err(BAD_ASSETLOCKTX_PUBKEYHASH.into()));

    let bad = vec![txout(30 * CENT, vec![OP_1])];
    assert_eq!(verdict(&lock(outputs(), v2, bad), true), Err(BAD_ASSETLOCKTX_SCRIPT_PUBKEY.into()));

    let v1 = lock(outputs(), ASSET_LOCK_INITIAL_VERSION, credits());
    assert_eq!(verdict(&v1, true), Ok(()));
    assert_eq!(verdict(&lock(outputs(), 3, credits()), true), Err(BAD_ASSETLOCKTX_VERSION.into()));
}

/// Step 1 (`assetlocktx.cpp:31-33`): any type but 8 is refused before the outputs are read; the
/// dispatcher sends only type 8 here, so this is the step's only vector. Kills 8t.
#[test]
fn another_type_is_refused_first() {
    for t in [TxType::ProviderRegister, TxType::QuorumCommitment, TxType::AssetUnlock] {
        let tx = typed_lock(t, vec![], 0, vec![]);
        assert_eq!(verdict(&tx, true), Err(BAD_ASSETLOCKTX_TYPE.into()), "{t:?}");
    }
}

/// Step 2 (`:36-40`): an output is an `OP_RETURN` one by its first byte only, and then must be
/// exactly `OP_RETURN OP_0`; an empty script is skipped. Kills 8r.
#[test]
fn op_return_script_is_exactly_two_bytes() {
    for script in
        [vec![OP_RETURN], vec![OP_RETURN, 0, 0], vec![OP_RETURN, 1], vec![OP_RETURN, 1, 0]]
    {
        let mut outs = outputs();
        outs[0].script_pubkey = script.clone().into();
        let tx = lock(outs, 1, credits());
        assert_eq!(verdict(&tx, true), Err(BAD_ASSETLOCKTX_NON_EMPTY_RETURN.into()), "{script:?}");
    }
    // The script check precedes the value check.
    let mut outs = outputs();
    outs[0] = txout(0, vec![OP_RETURN, 0, 0]);
    assert_eq!(
        verdict(&lock(outs, 1, credits()), true),
        Err(BAD_ASSETLOCKTX_NON_EMPTY_RETURN.into())
    );
    // An empty script, and one ending in `OP_RETURN`, are not `OP_RETURN` outputs.
    let mut outs = outputs();
    outs.push(txout(5 * CENT, vec![]));
    outs.push(txout(5 * CENT, vec![0, OP_RETURN]));
    assert_eq!(verdict(&lock(outs, 1, credits()), true), Ok(()));
}

/// Step 2's value (`:42`): 0, negative or above `MAX_MONEY` is refused; `MAX_MONEY` itself is
/// in range. Kills 8o.
#[test]
fn op_return_value_is_in_money_range() {
    for value in [0, -1, i64::MIN, MAX_MONEY + 1, i64::MAX] {
        let mut outs = outputs();
        outs[0].value = value;
        let tx = lock(outs, 1, credits());
        assert_eq!(verdict(&tx, true), Err(BAD_ASSETLOCKTX_OPRETURN_OUTOFRANGE.into()), "{value}");
    }
    let mut outs = outputs();
    outs[0].value = MAX_MONEY;
    let tx = lock(outs, 1, vec![txout(MAX_MONEY, to_key())]);
    assert_eq!(verdict(&tx, true), Ok(()));
}

/// Step 2's count (`:44-45`): a second `OP_RETURN` output is refused once its own script and
/// value pass, wherever it is. Kills 8m.
#[test]
fn second_op_return_is_refused_after_its_own_checks() {
    let mut outs = outputs();
    outs.insert(0, txout(CENT, op_return()));
    assert_eq!(
        verdict(&lock(outs, 1, credits()), true),
        Err(BAD_ASSETLOCKTX_MULTIPLE_RETURN.into())
    );
    let mut outs = outputs();
    outs.push(txout(0, op_return()));
    assert_eq!(
        verdict(&lock(outs, 1, credits()), true),
        Err(BAD_ASSETLOCKTX_OPRETURN_OUTOFRANGE.into())
    );
    let mut outs = outputs();
    outs.push(txout(CENT, vec![OP_RETURN, 0, 1]));
    assert_eq!(
        verdict(&lock(outs, 1, credits()), true),
        Err(BAD_ASSETLOCKTX_NON_EMPTY_RETURN.into())
    );
}

/// Step 3 (`:49`): no `OP_RETURN` output, including no output at all, is refused before the
/// payload is read. Kills 8n.
#[test]
fn no_op_return_is_refused_before_the_payload() {
    assert_eq!(verdict(&lock(vec![], 0, vec![]), true), Err(BAD_ASSETLOCKTX_NO_RETURN.into()));
    let tx = tx_of(TxType::AssetLock, vec![txin([1; 32], 1)], vec![txout(CENT, to_key())], vec![]);
    assert_eq!(verdict(&tx, true), Err(BAD_ASSETLOCKTX_NO_RETURN.into()));
}

/// Step 4 (`:51-54`): a payload that does not decode, or decodes short of its last byte, is
/// refused. Kills 8p.
#[test]
fn payload_must_decode_to_its_last_byte() {
    let one = vec![txin([1; 32], 1)];
    let lock_bytes = encoded(&AssetLock {
        version: 1,
        credit_outputs: credits(),
    });
    let mut trailing = lock_bytes.clone();
    trailing.push(0);
    let short = lock_bytes[..lock_bytes.len() - 1].to_vec();
    for bytes in [vec![], vec![1], short, trailing] {
        let tx = tx_of(TxType::AssetLock, one.clone(), outputs(), bytes.clone());
        assert_eq!(verdict(&tx, true), Err(BAD_ASSETLOCKTX_PAYLOAD.into()), "{bytes:?}");
    }
}

/// Step 5 (`:56-58`): versions 0 and 3 up are refused, with or without V24, before the V24
/// gate. Kills 8v.
#[test]
fn version_outside_one_and_two_is_refused() {
    for version in [0, 3, 4, u8::MAX] {
        for v24 in [false, true] {
            let tx = lock(outputs(), version, credits());
            assert_eq!(verdict(&tx, v24), Err(BAD_ASSETLOCKTX_VERSION.into()), "v{version} {v24}");
        }
    }
}

/// Step 6 (`:59-61`): version 2 needs V24, and the gate precedes the credit outputs. Kills 8w.
#[test]
fn version_two_needs_v24() {
    let tx = lock(outputs(), 2, vec![]);
    assert_eq!(verdict(&tx, false), Err(BAD_ASSETLOCKTX_VERSION_2.into()));
    assert_eq!(verdict(&tx, true), Err(BAD_ASSETLOCKTX_EMPTYCREDITOUTPUTS.into()));
    assert_eq!(verdict(&lock(outputs(), 1, credits()), false), Ok(()));
}

/// Step 7 (`:63-65`): no credit output is refused. Kills 8e.
#[test]
fn no_credit_output_is_refused() {
    for version in [1, 2] {
        let tx = lock(outputs(), version, vec![]);
        assert_eq!(
            verdict(&tx, true),
            Err(BAD_ASSETLOCKTX_EMPTYCREDITOUTPUTS.into()),
            "v{version}"
        );
    }
}

/// Step 8's range (`:68-70`): a credit output of 0, negative or above `MAX_MONEY` is refused,
/// before its script. Kills 8c.
#[test]
fn credit_value_is_in_money_range() {
    for value in [0, -1, i64::MIN, MAX_MONEY + 1, i64::MAX] {
        let credit = vec![txout(value, vec![OP_1])];
        let tx = lock(outputs(), 1, credit);
        assert_eq!(verdict(&tx, true), Err(BAD_ASSETLOCKTX_CREDIT_OUTOFRANGE.into()), "{value}");
    }
}

/// Step 8's running sum (`:68`): the sum is checked as each output is added, so a sum above
/// `MAX_MONEY` is refused at the output that crosses it, before a later output's script; a sum
/// of exactly `MAX_MONEY` passes. Kills 8s.
#[test]
fn running_sum_is_in_money_range() {
    let half = MAX_MONEY / 2 + 1;
    let credit = vec![txout(half, to_key()), txout(half, to_key()), txout(CENT, vec![OP_1])];
    let tx = lock(outputs(), 1, credit);
    assert_eq!(verdict(&tx, true), Err(BAD_ASSETLOCKTX_CREDIT_OUTOFRANGE.into()));
    // Before the crossing output, the earlier one's script is read.
    let credit = vec![txout(half, vec![OP_1]), txout(half, to_key())];
    assert_eq!(verdict(&lock(outputs(), 1, credit), true), Err(BAD_ASSETLOCKTX_PUBKEYHASH.into()));

    let mut outs = outputs();
    outs[0].value = MAX_MONEY;
    let credit = vec![txout(MAX_MONEY - CENT, to_key()), txout(CENT, to_key())];
    assert_eq!(verdict(&lock(outs, 1, credit), true), Ok(()));
}

/// Step 8's script (`:74-82`): version 2 takes P2PKH or P2SH, version 1 P2PKH only; near misses
/// of either shape are refused. Kills 8k.
#[test]
fn credit_script_by_version() {
    let mut near = vec![vec![], vec![OP_1]];
    let mut long = to_key();
    long.push(0);
    near.push(long);
    near.push(to_key()[..24].to_vec());
    let mut wrong_op = to_script();
    wrong_op[0] = OP_1;
    near.push(wrong_op);
    for script in &near {
        let credit = |s: &Vec<u8>| vec![txout(30 * CENT, s.clone())];
        let v2 = lock(outputs(), 2, credit(script));
        assert_eq!(verdict(&v2, true), Err(BAD_ASSETLOCKTX_SCRIPT_PUBKEY.into()), "{script:?}");
        let v1 = lock(outputs(), 1, credit(script));
        assert_eq!(verdict(&v1, true), Err(BAD_ASSETLOCKTX_PUBKEYHASH.into()), "{script:?}");
    }
    let mixed = vec![txout(10 * CENT, to_key()), txout(20 * CENT, to_script())];
    assert_eq!(verdict(&lock(outputs(), 2, mixed.clone()), true), Ok(()));
    assert_eq!(verdict(&lock(outputs(), 1, mixed), true), Err(BAD_ASSETLOCKTX_PUBKEYHASH.into()));
}

/// Step 9 (`:84-86`): the credit sum must equal the `OP_RETURN` value, either way off by one; the
/// transaction's other outputs do not count. Kills 8a.
#[test]
fn credit_sum_equals_the_op_return_value() {
    for delta in [-1, 1] {
        let mut credit = credits();
        credit[1].value += delta;
        let tx = lock(outputs(), 1, credit);
        assert_eq!(verdict(&tx, true), Err(BAD_ASSETLOCKTX_CREDITAMOUNT.into()), "{delta}");
    }
    let mut outs = outputs();
    outs[1].value = MAX_MONEY;
    assert_eq!(verdict(&lock(outs, 1, credits()), true), Ok(()));
}
