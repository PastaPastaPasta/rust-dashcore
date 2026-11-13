//! Argon2id parameter types and their constants. Plain data: the KDF itself is
//! behind the `encryption` feature.

/// Argon2id cost parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KdfParams {
    /// Memory, in KiB.
    pub m_kib: u32,
    /// Passes.
    pub t: u32,
    /// Lanes.
    pub p: u32,
}

/// 4 GiB in KiB, clamped to the largest whole number of 1 KiB blocks one allocation
/// can address (`isize::MAX` bytes), which matters only on 32-bit targets.
const CEILING_M_KIB: u32 = {
    const FOUR_GIB_IN_KIB: u64 = 4 * 1024 * 1024;
    let addressable = (isize::MAX as u64) / 1024;
    if addressable < FOUR_GIB_IN_KIB {
        addressable as u32
    } else {
        FOUR_GIB_IN_KIB as u32
    }
};

impl KdfParams {
    /// Floor of the desktop policy (256 MiB, t = 3, p = 1).
    pub const DESKTOP_FLOOR: KdfParams = KdfParams {
        m_kib: 256 * 1024,
        t: 3,
        p: 1,
    };
    /// Floor of the default, mobile-safe policy (64 MiB, t = 3, p = 1).
    pub const MOBILE_FLOOR: KdfParams = KdfParams {
        m_kib: 64 * 1024,
        t: 3,
        p: 1,
    };
    /// Absolute ceiling: no budget can exceed it (4 GiB, t = 64, p = 16).
    pub const CEILING: KdfParams = KdfParams {
        m_kib: CEILING_M_KIB,
        t: 64,
        p: 16,
    };
}

/// The most KDF work a reader accepts before anything is authenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfBudget {
    /// Maximum memory, in KiB.
    pub max_m_kib: u32,
    /// Maximum passes.
    pub max_t: u32,
    /// Maximum lanes.
    pub max_p: u32,
}

impl KdfBudget {
    /// The library default: 64 MiB, t ≤ 24, p ≤ 4.
    pub const MOBILE: KdfBudget = KdfBudget {
        max_m_kib: 64 * 1024,
        max_t: 24,
        max_p: 4,
    };
    /// The desktop budget: 256 MiB, t ≤ 24, p ≤ 4.
    pub const DESKTOP: KdfBudget = KdfBudget {
        max_m_kib: 256 * 1024,
        max_t: 24,
        max_p: 4,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ceiling_is_four_gib_where_addressable() {
        if usize::BITS >= 64 {
            assert_eq!(KdfParams::CEILING.m_kib, 4 * 1024 * 1024);
        } else {
            assert_eq!(KdfParams::CEILING.m_kib as u64, (isize::MAX as u64) / 1024);
        }
    }

    #[test]
    fn floors_fit_their_budgets() {
        let fits = |p: KdfParams, b: KdfBudget| p.m_kib <= b.max_m_kib && p.t <= b.max_t;
        assert!(fits(KdfParams::MOBILE_FLOOR, KdfBudget::MOBILE));
        assert!(fits(KdfParams::DESKTOP_FLOOR, KdfBudget::DESKTOP));
    }
}
