//! Core's memory accounting for the disconnect pool (`memusage.h`, `core_memusage.h`): a logical
//! budget, computed by formula from the containers' sizes and never from Rust's own allocations
//! (CH3 §4.3, O17 CONS-25).
//!
//! The figures are Core's on x86_64 with libstdc++, the reference platform: the 64-bit
//! `MallocUsage` and libstdc++'s object sizes, on every platform rdashd runs on (D25).

use dash_consensus::Tx;

/// `sizeof(CTxIn)`: the `COutPoint` (36 bytes), the `CScript` (a 32-byte `prevector<28>`, aligned
/// to 8 at offset 40) and `nSequence` (4), padded to 80.
pub const SIZEOF_TX_IN: usize = 80;

/// `sizeof(CTxOut)`: `nValue` (8) and the `CScript` (32).
pub const SIZEOF_TX_OUT: usize = 40;

/// A `CScript`'s inline capacity (`prevector<28, unsigned char>`, `script.h:390`): a longer script
/// lives on the heap.
pub const SCRIPT_INLINE: usize = 28;

/// `sizeof(memusage::list_node<CTransactionRef>)` (`memusage.h:152-159`): two pointers and the
/// `shared_ptr`.
pub const SIZEOF_LIST_NODE: usize = 32;

/// `sizeof(memusage::unordered_node<std::pair<const uint256, std::list<CTransactionRef>::iterator>>)`
/// (`memusage.h:167-172`): the key and the iterator (32 + 8) and a pointer. Core's model leaves
/// out the hash code libstdc++ caches in the node.
pub const SIZEOF_UNORDERED_NODE: usize = 48;

/// `MallocUsage(alloc)` (`memusage.h:51-63`), 64-bit: Core's model of what a malloc of `alloc`
/// bytes costs, in 16-byte granules.
pub const fn malloc_usage(alloc: usize) -> usize {
    if alloc == 0 {
        0
    } else {
        ((alloc + 31) >> 4) << 4
    }
}

/// `DynamicUsage(const std::list<CTransactionRef>&)` (`memusage.h:161-165`) for `len` entries.
pub const fn list_usage(len: usize) -> usize {
    malloc_usage(SIZEOF_LIST_NODE) * len
}

/// `DynamicUsage(const std::unordered_map<uint256, …>&)` (`memusage.h:180-184`) for `len` entries
/// over `bucket_count` buckets (libstdc++'s count, [`super::buckets`]).
pub const fn unordered_map_usage(len: usize, bucket_count: usize) -> usize {
    malloc_usage(SIZEOF_UNORDERED_NODE) * len + malloc_usage(8 * bucket_count)
}

/// `RecursiveDynamicUsage(const CTransaction&)` (`core_memusage.h:28-36`): the `vin` and `vout`
/// arrays at their capacities, and each script's heap part. Core counts neither `vExtraPayload`,
/// the `CTransaction` itself nor its `shared_ptr`'s control block.
///
/// A capacity is the size. The pool's transactions come from `ReadBlockFromDisk`, and Core's
/// `Unserialize` reserves a vector exactly, in 5 MB batches, the last of which ends at its size
/// (`serialize.h:800-817`), then moves it into the `CTransaction` (`transaction.cpp:128`). It
/// sizes a script's heap exactly too (`serialize.h:1092-1104`, `prevector.h` `change_capacity`).
pub fn recursive_dynamic_usage(tx: &Tx) -> usize {
    let raw = tx.raw();
    let scripts: usize = raw
        .inputs
        .iter()
        .map(|txin| txin.script_sig.as_bytes().len())
        .chain(raw.outputs.iter().map(|txout| txout.script_pubkey.as_bytes().len()))
        .map(script_usage)
        .sum();
    malloc_usage(raw.inputs.len() * SIZEOF_TX_IN)
        + malloc_usage(raw.outputs.len() * SIZEOF_TX_OUT)
        + scripts
}

/// `RecursiveDynamicUsage(const CScript&)` (`core_memusage.h:12-14`): the heap part of a
/// `len`-byte script, none for a script that fits inline.
const fn script_usage(len: usize) -> usize {
    if len > SCRIPT_INLINE {
        malloc_usage(len)
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `MallocUsage` is 0 for nothing, then steps by 16 bytes at every request of 16k + 1 bytes.
    #[test]
    fn malloc_usage_steps_by_granules() {
        assert_eq!(malloc_usage(0), 0);
        let mut last = malloc_usage(1);
        assert_eq!(last, 32);
        for alloc in 2..=4096 {
            let usage = malloc_usage(alloc);
            assert!(usage == last || usage == last + 16, "{alloc}: {usage} after {last}");
            if usage != last {
                assert_eq!(alloc % 16, 1, "{alloc} starts a granule");
            }
            assert!(usage >= alloc + 16 && usage.is_multiple_of(16), "{alloc}: {usage}");
            last = usage;
        }
    }

    #[test]
    fn script_heap_starts_past_the_inline_capacity() {
        assert_eq!(script_usage(0), 0);
        assert_eq!(script_usage(SCRIPT_INLINE), 0);
        assert_eq!(script_usage(SCRIPT_INLINE + 1), malloc_usage(SCRIPT_INLINE + 1));
    }
}
