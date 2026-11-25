//! `diff_payload_<type>` (BS6b): special-transaction payloads, Core's `GetTxPayload<T>()`.
//!
//! The input's first byte holds Core's process-wide BLS flag in bit 0 (set: legacy); the rest
//! is the payload. Compared: the verdict, whether the decode read every byte, and Core's
//! re-serialization of the object it decoded (A1 §1.4.1, Rule DDE).
//!
//! Core serializes a freshly decoded object, so its re-serialization differs from base-sdk's
//! `encode`, which writes every BLS field's wire bytes, in two kinds of BLS field. An operator
//! signature is Core's object under the payload version's scheme, which a signature canonical
//! only in the other scheme decoded under. A field read under the global flag (the CbTx and
//! asset-unlock signatures) is Core's object under that flag. A lazy operator key has not been
//! `Get()`, so Core writes its raw bytes, as base-sdk does. `core_bytes` writes the signatures as
//! Core does.

use dash_consensus::primitives::{CorePayload, protx_key_scheme};
use dash_oracle_sys::{Oracle, Pin};
use dash_pkc::bls::{BLS_SIG_LEN, BlsCtx};
use dash_primitives::{SpecialPayload, TxType};
use dash_types::Numeric as _;
use dash_types::codec::BaseCodec;

use crate::{Compared, Outcome, check, core_verdict, mismatched_verdicts};

/// `CCbTx`'s credit pool balance, which follows `bestCLSignature` from version 3.
const CBTX_CREDIT_POOL_LEN: usize = 8;
/// `CCbTx`'s asset-unlock merkle root, which version 4 appends.
const CBTX_ASSET_UNLOCK_ROOT_LEN: usize = 32;

fn encode(value: &impl BaseCodec) -> Vec<u8> {
    let mut out = Vec::new();
    value.encode(&mut out);
    out
}

fn patch(out: &mut [u8], offset: usize, field: &[u8]) {
    if let Some(dst) = out.get_mut(offset..offset + field.len()) {
        dst.copy_from_slice(field);
    }
}

/// Core's serialization of the object it decodes from a payload base-sdk decoded as `decoded`,
/// under the BLS flag of `ctx`. The operator signature is the side-car's.
fn core_bytes(decoded: &CorePayload, ctx: BlsCtx) -> Option<Vec<u8>> {
    let with_operator_sig = |mut out: Vec<u8>, version| {
        let offset = out.len().checked_sub(BLS_SIG_LEN)?;
        patch(&mut out, offset, &decoded.operator_sig()?.to_bytes(protx_key_scheme(version)));
        Some(out)
    };
    Some(match decoded.payload() {
        SpecialPayload::CoinbaseCommitment(p) => {
            let mut out = encode(p);
            if let Some(sig) = p.best_cl_signature() {
                let tail = CBTX_CREDIT_POOL_LEN
                    + if p.version().to_base() >= 4 {
                        CBTX_ASSET_UNLOCK_ROOT_LEN
                    } else {
                        0
                    };
                let offset = out.len().checked_sub(tail + BLS_SIG_LEN)?;
                patch(&mut out, offset, &sig.get(ctx).to_bytes(ctx.scheme()));
            }
            out
        }
        SpecialPayload::AssetUnlock(p) => {
            let mut out = encode(p);
            let offset = out.len().checked_sub(BLS_SIG_LEN)?;
            patch(&mut out, offset, &p.quorum_sig().get(ctx).to_bytes(ctx.scheme()));
            out
        }
        // Unsupported versions are refused.
        SpecialPayload::UnknownProTxVersion {
            version,
            ..
        } => encode(version),
        SpecialPayload::ProviderUpdateService(p) => with_operator_sig(encode(p), p.version())?,
        SpecialPayload::ProviderUpdateRevoke(p) => with_operator_sig(encode(p), p.version())?,
        SpecialPayload::ProviderRegister(p) => encode(p),
        SpecialPayload::ProviderUpdateRegistrar(p) => encode(p),
        SpecialPayload::ProviderUpdateSharedRegistrar(p) => encode(p),
        SpecialPayload::QuorumCommitment(p) => encode(p),
        SpecialPayload::MnhfSignal(p) => encode(p),
        SpecialPayload::AssetLock(p) => encode(p),
        SpecialPayload::ProviderDissolve(p) => encode(p),
        SpecialPayload::ProviderUpdateShare(p) => encode(p),
        SpecialPayload::Unknown {
            ..
        } => return None,
    })
}

/// `diff_payload_<tx_type>`: one payload of `tx_type` under the flag in the input's first byte.
pub fn compare_payload(tx_type: u16, bytes: &[u8], pin: Pin) -> Compared {
    let (flags, payload) = bytes.split_first().map_or((0, bytes), |(f, p)| (*f, p));
    let legacy = flags & 1 == 1;
    let core = core_verdict(Oracle::new(pin).payload_decode(tx_type, legacy, payload));
    let ours = CorePayload::decode(TxType::new(tx_type), payload);
    let (core, ours) = match (core, ours) {
        (Ok(core), Ok(ours)) => (core, ours),
        (core, ours) => return mismatched_verdicts(core, ours),
    };
    let ctx = if legacy {
        BlsCtx::LEGACY
    } else {
        BlsCtx::BASIC
    };
    check(ours.is_complete() == core.consumed_all, "consumed all")?;
    check(core_bytes(&ours, ctx).is_some_and(|ours| ours == core.reencoded), "serialization")?;
    Ok(Outcome::BothAccept)
}
