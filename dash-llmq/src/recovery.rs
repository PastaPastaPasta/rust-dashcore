//! Core's threshold signature recovery (`bls/bls.cpp:374-404`, v24.0.0).

use std::collections::BTreeMap;

use dash_evo::types::ProTxHash;
use dash_params::{LlmqParams, LlmqParamsExt};
use ff::{Field, PrimeField};

use crate::dkg::bls::{self, BlsSigElem, BlsSigValue, Fr, member_id};

/// Core's `a / b_i` coefficients as big-endian scalars; `None` for fewer than two ids,
/// a zero id modulo the order, or duplicate ids modulo the order.
// Core: v24.0.0 src/dashbls/src/threshold.cpp:190-248 Poly::LagrangeInterpolate
pub fn lagrange_at_zero(ids: &[[u8; 32]]) -> Option<Vec<[u8; 32]>> {
    if ids.len() < 2 {
        return None;
    }
    let xs = ids.iter().map(Fr::from_bendian_reduce).collect::<Result<Vec<_>, _>>().ok()?;
    let a: Fr = xs.iter().copied().product();
    let mut denominators = xs.clone();
    for (i, xi) in xs.iter().enumerate() {
        for (j, xj) in xs.iter().enumerate().skip(i + 1) {
            let difference = *xj - xi;
            if bool::from(difference.is_zero()) {
                return None;
            }
            denominators[i] *= difference;
            denominators[j] *= -difference;
        }
    }
    let mut product = Fr::ONE;
    let prefixes: Vec<_> = denominators
        .iter()
        .map(|b| {
            let prefix = product;
            product *= b;
            prefix
        })
        .collect();
    let mut inverse = Option::<Fr>::from(product.invert())? * a;
    let mut coefficients = vec![[0; 32]; ids.len()];
    for i in (0..ids.len()).rev() {
        let mut bytes = (inverse * prefixes[i]).to_repr();
        bytes.reverse();
        coefficients[i] = bytes;
        inverse *= denominators[i];
    }
    Some(coefficients)
}

/// Core's recovery fold, including its refusal cases and element arithmetic.
// Core: v24.0.0 src/bls/bls.cpp:374-404 CBLSSignature::Recover
pub fn recover_naive(sigs: &[&BlsSigValue], ids: &[[u8; 32]]) -> Option<BlsSigValue> {
    bls::recover_signature(sigs, ids)
}

/// Core's recovery sum with signed-digit buckets; invalid or infinite inputs use Core's fold.
/// Finite shares with a linear relation may leave a different internal coordinate tag when
/// a partial sum in either fold reaches infinity; serialized bytes, validity and `==` remain
/// identical.
// Core: v24.0.0 src/dashbls/src/threshold.cpp:190-248 Poly::LagrangeInterpolate
pub fn recover_msm(sigs: &[&BlsSigValue], ids: &[[u8; 32]]) -> Option<BlsSigValue> {
    if sigs.len() < 2 || sigs.len() != ids.len() {
        return recover_naive(sigs, ids);
    }
    let points: Vec<_> = sigs.iter().map(|sig| BlsSigElem::from_impl(sig)).collect();
    if sigs.iter().any(|sig| !sig.is_valid())
        || points.iter().any(|p| !p.is_valid() || p.core_eq(&BlsSigElem::default()))
    {
        return recover_naive(sigs, ids);
    }
    let coefficients = lagrange_at_zero(ids)?;
    let width = match sigs.len() {
        0..=3 => 2,
        4..=15 => 3,
        16..=63 => 5,
        64..=255 => 6,
        _ => 7,
    };
    let windows = 257usize.div_ceil(width);
    let digits: Vec<_> = coefficients.iter().map(|k| signed_digits(k, width, windows)).collect();
    let negatives: Vec<_> = points.iter().map(BlsSigElem::neg).collect();
    let mut sum = BlsSigElem::default();
    for window in (0..windows).rev() {
        for _ in 0..width {
            sum = sum.add(&sum);
        }
        let mut buckets = vec![None::<BlsSigElem>; 1 << (width - 1)];
        for (i, digits) in digits.iter().enumerate() {
            let digit = digits[window];
            if digit == 0 {
                continue;
            }
            let point = if digit < 0 {
                &negatives[i]
            } else {
                &points[i]
            };
            let bucket = &mut buckets[usize::from(digit.unsigned_abs()) - 1];
            *bucket = Some(bucket.as_ref().map_or_else(|| point.clone(), |b| b.add(point)));
        }
        let mut running = BlsSigElem::default();
        for bucket in buckets.iter().rev() {
            if let Some(bucket) = bucket {
                running = running.add(bucket);
            }
            sum = sum.add(&running);
        }
    }
    Some(sum.into())
}

fn signed_digits(scalar: &[u8; 32], width: usize, windows: usize) -> Vec<i16> {
    let mut carry = 0;
    (0..windows)
        .map(|window| {
            let mut digit = carry;
            for bit in 0..width {
                let offset = window * width + bit;
                if offset < 256 {
                    digit += i16::from((scalar[31 - offset / 8] >> (offset % 8)) & 1) << bit;
                }
            }
            carry = (digit + (1 << (width - 1))) >> width;
            digit - (carry << width)
        })
        .collect()
}

/// Core's recovered signature through the bucketed recovery sum.
// Core: v24.0.0 src/bls/bls.cpp:374-404 CBLSSignature::Recover
pub fn recover_signature(sigs: &[&BlsSigValue], ids: &[[u8; 32]]) -> Option<BlsSigValue> {
    recover_msm(sigs, ids)
}

/// Core's recovery outcome before the recovered signature's optional verification.
#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "recovery returns the signature without an allocation"
)]
pub enum TryRecover {
    NotEnough,
    Failed,
    Recovered {
        sig: BlsSigValue,
        self_verify: bool,
    },
}

/// Core's first-threshold-share recovery in ascending member index, with its wrapping counter.
// Core: v24.0.0 src/llmq/signing_shares.cpp:681-762 CSigSharesManager::TryRecoverSig
pub fn try_recover(
    params: &LlmqParams,
    members: &[ProTxHash],
    shares: &BTreeMap<u16, BlsSigValue>,
    counter: &mut u32,
) -> TryRecover {
    if shares.len() < usize::from(params.threshold) {
        return TryRecover::NotEnough;
    }
    if params.is_single_member()
        && let Some((_, sig)) = shares.first_key_value()
    {
        return TryRecover::Recovered {
            sig: sig.clone(),
            self_verify: false,
        };
    }
    let mut sigs = Vec::with_capacity(usize::from(params.threshold));
    let mut ids = Vec::with_capacity(usize::from(params.threshold));
    for (index, sig) in shares.iter().take(usize::from(params.threshold)) {
        let Some(member) = members.get(usize::from(*index)) else {
            return TryRecover::Failed;
        };
        sigs.push(sig);
        ids.push(member_id(member));
    }
    let Some(sig) = recover_signature(&sigs, &ids) else {
        return TryRecover::Failed;
    };
    let self_verify = counter.is_multiple_of(100);
    *counter = counter.wrapping_add(1);
    TryRecover::Recovered {
        sig,
        self_verify,
    }
}

#[cfg(test)]
mod tests;
