//! x86_64 backends and CPU feature detection (Core: the `HAVE_GETCPUID` part of
//! `SapphireAutoDetect`).

pub(crate) mod cubehash;
pub(crate) mod echo;
pub(crate) mod groestl;
pub(crate) mod jh;
pub(crate) mod luffa;
pub(crate) mod shavite;
pub(crate) mod simd;

use core::arch::x86_64::{__cpuid, __cpuid_count, _xgetbv};

use crate::Use;

/// The [`Use`] backends this CPU can run. Uses CPUID directly, so it works without `std`.
// `__cpuid` became safe after this crate's MSRV.
#[allow(unused_unsafe)]
pub(crate) fn cpu_features() -> Use {
    // SAFETY: CPUID exists on every x86_64 CPU.
    let (max, l1) = unsafe { (__cpuid(0).eax, __cpuid(1)) };
    let bit = |reg: u32, n: u32| (reg >> n) & 1 == 1;
    // Core requires SSSE3 for its ECHO ShiftAndMix and SSE4.1 with AES-NI for its AES code.
    let sse = bit(l1.ecx, 9) && bit(l1.ecx, 19);
    let aes = sse && bit(l1.ecx, 25);
    // AVX2 needs CPUID.7.0:EBX.AVX2, CPUID.1:ECX.AVX and the OS saving the YMM state
    // (CPUID.1:ECX.OSXSAVE and XCR0 bits 1 and 2).
    let avx2 = sse
        && max >= 7
        && bit(l1.ecx, 27)
        && bit(l1.ecx, 28)
        // SAFETY: leaf 7 exists (max >= 7).
        && bit(unsafe { __cpuid_count(7, 0) }.ebx, 5)
        // SAFETY: OSXSAVE is set, so XGETBV is enabled.
        && unsafe { xcr0() } & 6 == 6;
    let mut has = Use::STANDARD;
    if sse {
        has = has | Use::SSE;
    }
    if aes {
        has = has | Use::AES;
    }
    if avx2 {
        has = has | Use::AVX2;
    }
    has
}

/// XCR0.
#[target_feature(enable = "xsave")]
unsafe fn xcr0() -> u64 {
    // SAFETY: the caller checked that XGETBV is enabled.
    unsafe { _xgetbv(0) }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use crate::Use;
    use crate::portable;

    /// The CPUID decoding agrees with std's detection, so the backend tests below and the
    /// integration tests' `implementations()` really cover what this CPU has.
    #[test]
    fn cpu_features_match_std() {
        use std::is_x86_feature_detected as has;
        let got = super::cpu_features();
        let sse = has!("ssse3") && has!("sse4.1");
        assert_eq!(got.contains(Use::SSE), sse);
        assert_eq!(got.contains(Use::AES), sse && has!("aes"));
        assert_eq!(got.contains(Use::AVX2), sse && has!("avx") && has!("avx2"));
    }

    /// SplitMix64.
    fn inputs(seed: u64, n: usize) -> impl Iterator<Item = [u8; 64]> {
        let mut s = seed;
        (0..n).map(move |_| {
            core::array::from_fn::<u8, 64, _>(|_| {
                s = s.wrapping_add(0x9e37_79b9_7f4a_7c15);
                let mut z = s;
                z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                (z ^ (z >> 31)) as u8
            })
        })
    }

    type Fn64 = unsafe fn(&[u8; 64]) -> [u8; 64];

    /// `DASH_X11_BACKEND_CASES`, default 20000.
    fn cases() -> usize {
        std::env::var("DASH_X11_BACKEND_CASES").map_or(20_000, |v| v.parse().unwrap())
    }

    /// Each backend function against the portable one, when the CPU has its features.
    #[test]
    fn backends_match_portable() {
        let has = super::cpu_features();
        let backends: &[(&str, Use, Fn64, Fn64)] = &[
            ("echo", Use::AES, super::echo::hash, portable::echo::hash),
            ("cubehash sse2", Use::SSE, super::cubehash::hash_sse2, portable::cubehash::hash),
            ("cubehash avx2", Use::AVX2, super::cubehash::hash_avx2, portable::cubehash::hash),
            ("groestl aes", Use::AES, super::groestl::hash_aes, portable::groestl::hash),
            ("jh sse", Use::SSE, super::jh::hash_sse, portable::jh::hash),
            ("jh avx2", Use::AVX2, super::jh::hash_avx2, portable::jh::hash),
            ("luffa sse", Use::SSE, super::luffa::hash_sse, portable::luffa::hash),
            ("luffa avx2", Use::AVX2, super::luffa::hash_avx2, portable::luffa::hash),
            ("simd sse", Use::SSE, super::simd::hash_sse, portable::simd::hash),
            ("simd avx2", Use::AVX2, super::simd::hash_avx2, portable::simd::hash),
            ("shavite", Use::AES, super::shavite::hash, portable::shavite::hash),
        ];
        let only = std::env::var("DASH_X11_BACKEND").ok();
        for &(name, need, fast, slow) in backends {
            if !has.contains(need) || only.as_deref().is_some_and(|o| !name.starts_with(o)) {
                continue;
            }
            for (i, input) in inputs(name.len() as u64, cases()).enumerate() {
                // SAFETY: the CPU has the features `fast` needs.
                assert_eq!(unsafe { fast(&input) }, unsafe { slow(&input) }, "{name} case {i}");
            }
        }
    }
}
