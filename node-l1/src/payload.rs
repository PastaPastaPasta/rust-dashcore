//! `diff_payload_<type>` (BS6b): special-transaction payloads, Core's `GetTxPayload<T>()`.
//!
//! The input's first byte holds Core's process-wide BLS flag in bit 0 (set: legacy); the rest
//! is the payload. Compared: the verdict, whether the decode read every byte, and Core's
//! re-serialization of the object it decoded (A1 §1.4.1, Rule DDE).
//!
//! Core serializes a freshly decoded object, so its re-serialization differs from base-sdk's
//! `encode` in two kinds of BLS field. A lazy operator key has not been `Get()`, so Core writes
//! its raw bytes, where base-sdk writes the key it resolves: zeros for a key that does not
//! resolve. A key that resolves is compared as base-sdk encodes it. A field read under the global flag
//! (the CbTx and asset-unlock signatures) is Core's object under that flag, where base-sdk keeps
//! the wire bytes. [`core_bytes`] writes both as Core does.

use dash_oracle_sys::{Oracle, Pin};
use dash_pkc::bls::{BLS_SIG_LEN, BlsCtx, BlsLazyPk};
use dash_primitives::{NetInfo, SpecialPayload, TxType};
use dash_types::codec::BaseCodec;

use crate::{Compared, Outcome, check, core_verdict, mismatched_verdicts};

/// Bytes before `pubKeyOperator` in a `CProUpRegTx`: version, proTxHash, mode.
const PROUPREGTX_KEY_OFFSET: usize = 2 + 32 + 2;
/// Bytes before `pubKeyOperator` in a `CProUpSharedRegTx`: version, proTxHash.
const PROUPSHAREDREGTX_KEY_OFFSET: usize = 2 + 32;
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

/// Writes the raw bytes of a lazy operator key that does not resolve, as Core's fresh object does.
fn patch_lazy_key(out: &mut [u8], offset: usize, key: &BlsLazyPk) {
    let raw = key.to_bytes();
    if !key.clone().get().is_valid() {
        patch(out, offset, &raw);
    }
}

/// The length of `CProRegTx::netInfo` as base-sdk writes it.
fn net_info_len(net_info: &NetInfo) -> usize {
    match net_info {
        NetInfo::Legacy(svc) => encode(svc).len(),
        NetInfo::Extended(ext) => encode(ext).len(),
    }
}

/// Core's serialization of the object it decodes from a payload base-sdk decoded as `payload`,
/// under the BLS flag of `ctx`.
fn core_bytes(payload: &SpecialPayload, ctx: BlsCtx) -> Option<Vec<u8>> {
    Some(match payload {
        SpecialPayload::ProviderRegister(p) => {
            let mut out = encode(p);
            // version, type, mode, collateral outpoint, netInfo, keyIDOwner.
            let offset = 2 + 2 + 2 + 32 + 4 + net_info_len(&p.net_info) + 20;
            patch_lazy_key(&mut out, offset, &p.pub_key_operator);
            out
        }
        SpecialPayload::ProviderUpdateRegistrar(p) => {
            let mut out = encode(p);
            patch_lazy_key(&mut out, PROUPREGTX_KEY_OFFSET, &p.pub_key_operator);
            out
        }
        SpecialPayload::ProviderUpdateSharedRegistrar(p) => {
            let mut out = encode(p);
            patch_lazy_key(&mut out, PROUPSHAREDREGTX_KEY_OFFSET, &p.pub_key_operator);
            out
        }
        SpecialPayload::CoinbaseCommitment(p) => {
            let mut out = encode(p);
            if let Some(sig) = &p.best_cl_signature {
                let tail = CBTX_CREDIT_POOL_LEN
                    + if p.version >= 4 {
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
            patch(&mut out, offset, &p.quorum_sig.get(ctx).to_bytes(ctx.scheme()));
            out
        }
        // Unsupported versions are refused.
        SpecialPayload::UnknownProTxVersion {
            version,
            ..
        } => encode(version),
        SpecialPayload::ProviderUpdateService(p) => encode(p),
        SpecialPayload::ProviderUpdateRevoke(p) => encode(p),
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
    let mut cursor = payload;
    let ours = SpecialPayload::decode(TxType::new(tx_type), &mut cursor);
    let (core, ours) = match (core, ours) {
        (Ok(core), Ok(ours)) => (core, ours),
        (core, ours) => return mismatched_verdicts(core, ours),
    };
    let ctx = if legacy {
        BlsCtx::LEGACY
    } else {
        BlsCtx::BASIC
    };
    check(cursor.is_empty() == core.consumed_all, "consumed all")?;
    check(core_bytes(&ours, ctx).is_some_and(|ours| ours == core.reencoded), "serialization")?;
    Ok(Outcome::BothAccept)
}
