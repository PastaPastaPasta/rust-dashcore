//! Hardware randomness and the time stamp counter (`random.cpp`), detected at run time as Core
//! detects it (CPUID; `HWCAP2_RNG`).

#[cfg(target_arch = "x86_64")]
mod imp {
    use std::arch::x86_64::{_rdrand64_step, _rdseed64_step, _rdtsc};

    const RDSEED: &str = "Using RdSeed as an additional entropy source";
    const RDRAND: &str = "Using RdRand as an additional entropy source";

    fn rdrand() -> bool {
        std::arch::is_x86_feature_detected!("rdrand")
    }

    fn rdseed() -> bool {
        std::arch::is_x86_feature_detected!("rdseed")
    }

    /// `GetRdRand`: up to ten tries.
    // Core: random.cpp:113-142
    fn get_rdrand() -> u64 {
        let mut r = 0;
        for _ in 0..10 {
            // SAFETY: called only once CPUID reported RDRAND.
            if unsafe { _rdrand64_step(&mut r) } == 1 {
                break;
            }
        }
        r
    }

    /// `GetRdSeed`: until it succeeds, pausing after each failure.
    // Core: random.cpp:148-178
    fn get_rdseed() -> u64 {
        let mut r = 0;
        // SAFETY: called only once CPUID reported RDSEED.
        while unsafe { _rdseed64_step(&mut r) } != 1 {
            std::hint::spin_loop();
        }
        r
    }

    pub(crate) fn fast() -> Option<u64> {
        rdrand().then(get_rdrand)
    }

    pub(crate) fn slow(out: &mut [u8; 32]) -> bool {
        let word: fn() -> u64 = if rdseed() {
            get_rdseed
        } else if rdrand() {
            // XOR of 1024 results, so the generator reseeds between each.
            || (0..1024).fold(0, |x, _| x ^ get_rdrand())
        } else {
            return false;
        };
        for chunk in out.as_chunks_mut::<8>().0 {
            *chunk = word().to_ne_bytes();
        }
        true
    }

    // Core: random.cpp:62-65
    pub(crate) fn time_stamp_counter() -> Option<i64> {
        // SAFETY: RDTSC exists on every x86-64 processor.
        #[allow(unused_unsafe)]
        Some(unsafe { _rdtsc() } as i64)
    }

    pub(crate) fn report() -> &'static [&'static str] {
        match (rdseed(), rdrand()) {
            (true, true) => &[RDSEED, RDRAND],
            (true, false) => &[RDSEED],
            (false, true) => &[RDRAND],
            (false, false) => &[],
        }
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
mod imp {
    use std::arch::asm;

    fn rndr() -> bool {
        std::arch::is_aarch64_feature_detected!("rand")
    }

    /// `GetRNDR`, and with `reseed` `GetRNDRRS`: until the flags report success, yielding
    /// after each failure.
    // Core: random.cpp:204-234
    fn get(reseed: bool) -> u64 {
        loop {
            let r: u64;
            let ok: u64;
            // SAFETY: called only once HWCAP2_RNG reported RNDR and RNDRRS; reads the
            // register and the flags, touching no memory.
            unsafe {
                if reseed {
                    asm!("mrs {r}, s3_3_c2_c4_1", "cset {ok:w}, ne", r = out(reg) r, ok = out(reg) ok, options(nomem, nostack));
                } else {
                    asm!("mrs {r}, s3_3_c2_c4_0", "cset {ok:w}, ne", r = out(reg) r, ok = out(reg) ok, options(nomem, nostack));
                }
            }
            if ok != 0 {
                return r;
            }
            // SAFETY: a hint instruction.
            unsafe { asm!("yield", options(nomem, nostack, preserves_flags)) };
        }
    }

    pub(crate) fn fast() -> Option<u64> {
        rndr().then(|| get(false))
    }

    pub(crate) fn slow(out: &mut [u8; 32]) -> bool {
        if !rndr() {
            return false;
        }
        for chunk in out.as_chunks_mut::<8>().0 {
            *chunk = get(true).to_ne_bytes();
        }
        true
    }

    pub(crate) fn time_stamp_counter() -> Option<i64> {
        None
    }

    pub(crate) fn report() -> &'static [&'static str] {
        if rndr() {
            &["Using RNDR and RNDRRS as additional entropy sources"]
        } else {
            &[]
        }
    }
}

#[cfg(not(any(target_arch = "x86_64", all(target_arch = "aarch64", target_os = "linux"))))]
mod imp {
    pub(crate) fn fast() -> Option<u64> {
        None
    }

    pub(crate) fn slow(_: &mut [u8; 32]) -> bool {
        false
    }

    pub(crate) fn time_stamp_counter() -> Option<i64> {
        None
    }

    pub(crate) fn report() -> &'static [&'static str] {
        &[]
    }
}

pub(crate) use imp::{fast, report, slow, time_stamp_counter};
