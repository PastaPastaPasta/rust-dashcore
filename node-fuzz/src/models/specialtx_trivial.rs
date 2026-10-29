//! `specialtx_trivial` (EV1 §6.5): base-sdk's payload decode, then `validated_payload` (Core's
//! `GetValidatedPayload<T>`, with every `IsTriviallyValid`), on arbitrary provider payloads. No
//! oracle: the chain is total (a panic is libFuzzer's crash) and deterministic, each rejection
//! carries the class Core gives its reason (`specialtx::reasons::CLASSES`), and a payload valid
//! under a tip is valid under every later fork state, since only the version maximum depends on
//! the forks (`ProTxVersion::GetMax`).
//!
//! Input: byte 0 picks the type (mod 7: types 1 to 4, 10 to 12); the rest is the payload.

use dash_consensus::primitives::Tx;
use dash_consensus::validation::TxValidationError;
use dash_dml::specialtx::reasons::CLASSES;
use dash_dml::specialtx::validated_payload;
use dash_primitives::{
    ProDisTx, ProRegTx, ProUpRegTx, ProUpRevTx, ProUpServTx, ProUpShareTx, ProUpSharedRegTx,
    Transaction, TxType,
};
use node_l1::Divergence;

const TYPES: [TxType; 7] = [
    TxType::ProviderRegister,
    TxType::ProviderUpdateService,
    TxType::ProviderUpdateRegistrar,
    TxType::ProviderUpdateRevoke,
    TxType::ProviderDissolve,
    TxType::ProviderUpdateShare,
    TxType::ProviderUpdateSharedRegistrar,
];

/// The fork states a tip can be in, in activation order: none, V19, V19 and V24.
const FORKS: [(bool, bool); 3] = [(false, false), (true, false), (true, true)];

fn ensure(ok: bool, field: &'static str) -> Result<(), Divergence> {
    if ok {
        Ok(())
    } else {
        Err(Divergence {
            field,
        })
    }
}

/// `GetValidatedPayload<T>` for the type `tx` carries.
fn validate(tx: &Tx, v19: bool, v24: bool) -> Result<(), TxValidationError> {
    let (t, slot) = (tx.tx_type(), tx.payload());
    match t {
        TxType::ProviderRegister => validated_payload::<ProRegTx>(t, slot, v19, v24).map(drop),
        TxType::ProviderUpdateService => {
            validated_payload::<ProUpServTx>(t, slot, v19, v24).map(drop)
        }
        TxType::ProviderUpdateRegistrar => {
            validated_payload::<ProUpRegTx>(t, slot, v19, v24).map(drop)
        }
        TxType::ProviderUpdateRevoke => {
            validated_payload::<ProUpRevTx>(t, slot, v19, v24).map(drop)
        }
        TxType::ProviderDissolve => validated_payload::<ProDisTx>(t, slot, v19, v24).map(drop),
        TxType::ProviderUpdateShare => {
            validated_payload::<ProUpShareTx>(t, slot, v19, v24).map(drop)
        }
        _ => validated_payload::<ProUpSharedRegTx>(t, slot, v19, v24).map(drop),
    }
}

fn tx(tx_type: TxType, payload: &[u8]) -> Tx {
    Tx::new(Transaction {
        version: 3,
        tx_type,
        inputs: vec![],
        outputs: vec![],
        lock_time: 0,
        extra_payload: payload.to_vec(),
    })
}

pub fn check(data: &[u8]) -> Result<(), Divergence> {
    let Some((&selector, payload)) = data.split_first() else {
        return Ok(());
    };
    let tx_type = TYPES[usize::from(selector) % TYPES.len()];
    let mut valid_before = false;
    for (v19, v24) in FORKS {
        let verdict = validate(&tx(tx_type, payload), v19, v24);
        // A fresh decode gives the same verdict.
        ensure(validate(&tx(tx_type, payload), v19, v24) == verdict, "deterministic")?;
        match &verdict {
            Ok(()) => valid_before = true,
            Err(e) => {
                ensure(!valid_before, "valid under an earlier fork state")?;
                let class = CLASSES.iter().find(|(r, _)| *r == e.reject_reason());
                ensure(class.is_some_and(|(_, c)| *c == e.result()), "class")?;
            }
        }
    }
    Ok(())
}
