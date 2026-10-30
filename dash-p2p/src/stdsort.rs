//! libstdc++'s `std::sort(first, last, comp)` from GCC 13.3.0, the compiler of Dash Core's
//! release builds (`contrib/guix/manifest.scm:96`, `base-gcc gcc-13` = 13.3.0), ported from
//! `libstdc++-v3/include/bits/stl_algo.h` and `bits/stl_heap.h` (tag `releases/gcc-13.3.0`).
//!
//! `std::sort` is not stable, and Core's eviction (`node/eviction.cpp` `EraseLastKElements`)
//! makes its ties observable: which of several equal candidates survive decides which peer is
//! evicted. The port makes the same comparator calls in the same order as libstdc++, so it
//! leaves equal elements in the same order (D36). `tests/stdsort.rs` checks the call sequence
//! against a libstdc++ trace.
//!
//! libstdc++ moves a value out of the range into a temporary (`__val`, `__value`), leaving a
//! hole that it fills later. The port keeps that value inside the slice at the hole and moves it
//! with `swap`: each `*hole = move(*src); hole = src` becomes `v.swap(hole, src)`. The C++ code
//! never reads a moved-from hole, so every comparison sees the same elements, without `Clone`
//! or `unsafe`.
//!
//! `less(a, b)` is the C++ `comp(a, b)` and must be a strict weak ordering. With an invalid
//! comparator libstdc++'s unguarded loops read out of bounds; the port panics instead.

/// `enum { _S_threshold = 16 };`
const S_THRESHOLD: usize = 16;

/// Sorts `v` as libstdc++ (GCC 13.3.0) `std::sort(v.begin(), v.end(), less)` does.
pub fn sort_by<T, F>(v: &mut [T], less: F)
where
    F: FnMut(&T, &T) -> bool,
{
    sort_by_counting_heap_fallbacks(v, less);
}

/// [`sort_by`], returning how many times the introsort depth limit was hit (the
/// `std::__partial_sort` heap-sort fallback ran). For tests.
#[doc(hidden)]
pub fn sort_by_counting_heap_fallbacks<T, F>(v: &mut [T], less: F) -> usize
where
    F: FnMut(&T, &T) -> bool,
{
    let mut s = Sorter {
        less,
        heap_fallbacks: 0,
    };
    s.sort(v);
    s.heap_fallbacks
}

struct Sorter<F> {
    less: F,
    heap_fallbacks: usize,
}

impl<F> Sorter<F> {
    #[inline]
    fn lt<T>(&mut self, v: &[T], a: usize, b: usize) -> bool
    where
        F: FnMut(&T, &T) -> bool,
    {
        (self.less)(&v[a], &v[b])
    }

    /// `std::sort(first, last, comp)` -> `std::__sort`.
    fn sort<T>(&mut self, v: &mut [T])
    where
        F: FnMut(&T, &T) -> bool,
    {
        let n = v.len();
        if n != 0 {
            self.introsort_loop(v, 0, n, lg(n) * 2);
            self.final_insertion_sort(v, 0, n);
        }
    }

    /// `std::__introsort_loop`.
    fn introsort_loop<T>(
        &mut self,
        v: &mut [T],
        first: usize,
        mut last: usize,
        mut depth_limit: usize,
    ) where
        F: FnMut(&T, &T) -> bool,
    {
        while last - first > S_THRESHOLD {
            if depth_limit == 0 {
                self.heap_fallbacks += 1;
                self.partial_sort(v, first, last, last);
                return;
            }
            depth_limit -= 1;
            let cut = self.unguarded_partition_pivot(v, first, last);
            self.introsort_loop(v, cut, last, depth_limit);
            last = cut;
        }
    }

    /// `std::__unguarded_partition_pivot`.
    fn unguarded_partition_pivot<T>(&mut self, v: &mut [T], first: usize, last: usize) -> usize
    where
        F: FnMut(&T, &T) -> bool,
    {
        let mid = first + (last - first) / 2;
        self.move_median_to_first(v, first, first + 1, mid, last - 1);
        self.unguarded_partition(v, first + 1, last, first)
    }

    /// `std::__move_median_to_first`.
    fn move_median_to_first<T>(&mut self, v: &mut [T], result: usize, a: usize, b: usize, c: usize)
    where
        F: FnMut(&T, &T) -> bool,
    {
        if self.lt(v, a, b) {
            if self.lt(v, b, c) {
                v.swap(result, b);
            } else if self.lt(v, a, c) {
                v.swap(result, c);
            } else {
                v.swap(result, a);
            }
        } else if self.lt(v, a, c) {
            v.swap(result, a);
        } else if self.lt(v, b, c) {
            v.swap(result, c);
        } else {
            v.swap(result, b);
        }
    }

    /// `std::__unguarded_partition`.
    fn unguarded_partition<T>(
        &mut self,
        v: &mut [T],
        mut first: usize,
        mut last: usize,
        pivot: usize,
    ) -> usize
    where
        F: FnMut(&T, &T) -> bool,
    {
        loop {
            while self.lt(v, first, pivot) {
                first += 1;
            }
            last -= 1;
            while self.lt(v, pivot, last) {
                last -= 1;
            }
            if first >= last {
                return first;
            }
            v.swap(first, last);
            first += 1;
        }
    }

    /// `std::__partial_sort(first, middle, last, comp)`.
    fn partial_sort<T>(&mut self, v: &mut [T], first: usize, middle: usize, last: usize)
    where
        F: FnMut(&T, &T) -> bool,
    {
        self.heap_select(v, first, middle, last);
        self.sort_heap(v, first, middle);
    }

    /// `std::__heap_select`.
    fn heap_select<T>(&mut self, v: &mut [T], first: usize, middle: usize, last: usize)
    where
        F: FnMut(&T, &T) -> bool,
    {
        self.make_heap(v, first, middle);
        for i in middle..last {
            if self.lt(v, i, first) {
                self.pop_heap(v, first, middle, i);
            }
        }
    }

    /// `std::__make_heap` (stl_heap.h).
    fn make_heap<T>(&mut self, v: &mut [T], first: usize, last: usize)
    where
        F: FnMut(&T, &T) -> bool,
    {
        if last - first < 2 {
            return;
        }
        let len = last - first;
        let mut parent = (len - 2) / 2;
        loop {
            // `__value = move(*(first + parent))`: the value stays in place;
            // `parent` is the hole.
            self.adjust_heap(v, first, parent, len);
            if parent == 0 {
                return;
            }
            parent -= 1;
        }
    }

    /// `std::__sort_heap` (stl_heap.h).
    fn sort_heap<T>(&mut self, v: &mut [T], first: usize, mut last: usize)
    where
        F: FnMut(&T, &T) -> bool,
    {
        while last - first > 1 {
            last -= 1;
            self.pop_heap(v, first, last, last);
        }
    }

    /// `std::__pop_heap(first, last, result, comp)` (stl_heap.h).
    fn pop_heap<T>(&mut self, v: &mut [T], first: usize, last: usize, result: usize)
    where
        F: FnMut(&T, &T) -> bool,
    {
        // `__value = move(*result); *result = move(*first);` leaves the value
        // at the hole `first` (index 0 of the heap). `result` is never inside
        // [first, last).
        v.swap(result, first);
        self.adjust_heap(v, first, 0, last - first);
    }

    /// `std::__adjust_heap(first, holeIndex, len, value, comp)` (stl_heap.h).
    /// The value being sifted lives at `first + hole_index` on entry.
    fn adjust_heap<T>(&mut self, v: &mut [T], first: usize, mut hole_index: usize, len: usize)
    where
        F: FnMut(&T, &T) -> bool,
    {
        let top_index = hole_index;
        let mut second_child = hole_index;
        // Distances are signed in C++, but len >= 1 here so `(len - 1) / 2`
        // and `(len - 2) / 2` (guarded by `len` even, so len >= 2) agree.
        while second_child < (len - 1) / 2 {
            second_child = 2 * (second_child + 1);
            if self.lt(v, first + second_child, first + (second_child - 1)) {
                second_child -= 1;
            }
            v.swap(first + hole_index, first + second_child);
            hole_index = second_child;
        }
        if (len & 1) == 0 && second_child == (len - 2) / 2 {
            second_child = 2 * (second_child + 1);
            v.swap(first + hole_index, first + (second_child - 1));
            hole_index = second_child - 1;
        }
        self.push_heap(v, first, hole_index, top_index);
    }

    /// `std::__push_heap(first, holeIndex, topIndex, value, comp)`
    /// (stl_heap.h). The value lives at `first + hole_index`.
    fn push_heap<T>(&mut self, v: &mut [T], first: usize, mut hole_index: usize, top_index: usize)
    where
        F: FnMut(&T, &T) -> bool,
    {
        // C++: `(holeIndex - 1) / 2` with signed truncation; when
        // holeIndex == 0 it is 0 and the loop guard `holeIndex > topIndex`
        // fails first, so the value is never used.
        let mut parent = hole_index.saturating_sub(1) / 2;
        // `__comp(first + parent, value)`, value at `first + hole_index`.
        while hole_index > top_index && self.lt(v, first + parent, first + hole_index) {
            v.swap(first + hole_index, first + parent);
            hole_index = parent;
            parent = hole_index.saturating_sub(1) / 2;
        }
        // `*(first + holeIndex) = move(value)`: already there.
    }

    /// `std::__final_insertion_sort`.
    fn final_insertion_sort<T>(&mut self, v: &mut [T], first: usize, last: usize)
    where
        F: FnMut(&T, &T) -> bool,
    {
        if last - first > S_THRESHOLD {
            self.insertion_sort(v, first, first + S_THRESHOLD);
            self.unguarded_insertion_sort(v, first + S_THRESHOLD, last);
        } else {
            self.insertion_sort(v, first, last);
        }
    }

    /// `std::__insertion_sort`.
    fn insertion_sort<T>(&mut self, v: &mut [T], first: usize, last: usize)
    where
        F: FnMut(&T, &T) -> bool,
    {
        if first == last {
            return;
        }
        for i in first + 1..last {
            if self.lt(v, i, first) {
                // `__val = move(*i); move_backward(first, i, i + 1);
                //  *first = move(__val);`
                v[first..=i].rotate_right(1);
            } else {
                self.unguarded_linear_insert(v, i);
            }
        }
    }

    /// `std::__unguarded_insertion_sort`.
    fn unguarded_insertion_sort<T>(&mut self, v: &mut [T], first: usize, last: usize)
    where
        F: FnMut(&T, &T) -> bool,
    {
        for i in first..last {
            self.unguarded_linear_insert(v, i);
        }
    }

    /// `std::__unguarded_linear_insert` (with `__val_comp_iter`, i.e.
    /// `comp(val, *next)`). The value lives at `last`.
    fn unguarded_linear_insert<T>(&mut self, v: &mut [T], mut last: usize)
    where
        F: FnMut(&T, &T) -> bool,
    {
        let mut next = last - 1;
        while self.lt(v, last, next) {
            v.swap(last, next);
            last = next;
            next -= 1;
        }
    }
}

/// `std::__lg(n)` = `__bit_width(n) - 1` for n > 0.
fn lg(n: usize) -> usize {
    (usize::BITS - 1 - n.leading_zeros()) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts() {
        for n in 0..300u64 {
            let mut v: Vec<u64> =
                (0..n).map(|i| i.wrapping_mul(0x9e37_79b9_7f4a_7c15) % 7).collect();
            let mut w = v.clone();
            sort_by(&mut v, |a, b| a < b);
            w.sort();
            assert_eq!(v, w);
        }
    }

    #[test]
    fn lg_is_floor_log2() {
        assert_eq!(lg(1), 0);
        assert_eq!(lg(16), 4);
        assert_eq!(lg(17), 4);
        assert_eq!(lg(5000), 12);
    }
}
