//! T6 D3: an operator key that does not resolve is refused by the trivial check, which runs
//! before any sign hash or signature check of `check_pro_reg_tx` (its sign string and its
//! shared consent hash), `check_pro_up_reg_tx` and `check_pro_up_shared_reg_tx`.

use super::super::provider::{check_pro_reg_tx, check_pro_up_reg_tx};
use super::super::shared::check_pro_up_shared_reg_tx;
use super::*;

/// 48 bytes that are a public key under neither scheme.
const UNRESOLVABLE: [u8; 48] = [0xff; 48];

/// `tx` with the operator key's wire bytes replaced by [`UNRESOLVABLE`]: the key is read from
/// the wire, so the side-car's key is the decoder's own object.
fn with_unresolvable_key(tx: &Tx) -> Tx {
    let Some(Ok(decoded)) = tx.payload() else {
        panic!("a decodable payload");
    };
    let key = decoded.operator_key().expect("an operator key");
    let key = key.clone().get().to_bytes(key.scheme());
    let mut extra = tx.raw().extra_payload().clone();
    let at = extra.windows(key.len()).position(|w| w == key).expect("the key on the wire");
    assert_eq!(extra.windows(key.len()).filter(|w| *w == key).count(), 1, "one key image");
    extra[at..at + key.len()].copy_from_slice(&UNRESOLVABLE);
    let patched = tx_of(tx.tx_type(), tx.inputs().to_vec(), tx.outputs().to_vec(), extra);
    let Some(Ok(decoded)) = patched.payload() else {
        panic!("the patched payload decodes");
    };
    let key = decoded.operator_key().expect("an operator key");
    assert_eq!(key.clone().to_bytes(), UNRESOLVABLE, "the side-car holds the wire bytes");
    patched
}

/// `check` of `tx` with a resolvable key reaches a signature reject, so signatures are checked;
/// with an unresolvable key it is the trivial check's `bad-protx-key-null`.
fn trivial_reject_comes_first(
    tx: &Tx,
    check: impl Fn(&Tx) -> Verdict,
    sig_reject: (TxValidationResult, &str),
) {
    assert_reject(check(tx), sig_reject.0, sig_reject.1);
    assert_reject(check(&with_unresolvable_key(tx)), S, BAD_PROTX_KEY_NULL);
}

#[test]
fn pro_reg_unresolvable_key_rejects_before_the_sign_string() {
    let (mut fx, mut p) = ext_reg();
    fx.check_sigs = true;
    let tx = reg_tx(&mut p, vec![txout(COIN, p2pkh([0x55; 20]))]);
    let check = |tx: &Tx| verdict(fx.with(|cx| check_pro_reg_tx(tx, tx.payload(), cx)));
    trivial_reject_comes_first(&tx, check, (C, BAD_PROTX_SIG));
}

#[test]
fn pro_reg_shared_unresolvable_key_rejects_before_the_consent_hash() {
    let (fx, tx) = signed_shared(true);
    let check = |tx: &Tx| verdict(fx.with(|cx| check_pro_reg_tx(tx, tx.payload(), cx)));
    trivial_reject_comes_first(&tx, check, (S, BAD_PROTX_SHARES_SIG));
}

#[test]
fn pro_up_reg_unresolvable_key_rejects_before_the_sign_hash() {
    let (mut fx, mut p) = up_reg();
    fx.check_sigs = true;
    let tx = up_reg_tx(&mut p);
    let check = |tx: &Tx| verdict(fx.with(|cx| check_pro_up_reg_tx(tx, tx.payload(), cx)));
    trivial_reject_comes_first(&tx, check, (C, BAD_PROTX_SIG));
}

#[test]
fn pro_up_shared_reg_unresolvable_key_rejects_before_the_sign_hash() {
    let (fx, _) = share_fixture();
    let tx = up_shared_reg(&[], |_| {});
    let check = |tx: &Tx| fx.with(|cx| check_pro_up_shared_reg_tx(tx, tx.payload(), cx));
    trivial_reject_comes_first(&tx, check, (S, BAD_PROUPSHAREDREG_SIG));
}
