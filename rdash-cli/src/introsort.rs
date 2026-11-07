//! libstdc++'s `std::sort` (introsort, then a final insertion sort), step for step.
//!
//! `std::sort` is not stable, so the order of elements that compare equal depends on the exact
//! algorithm. Core's `-netinfo` sorts its peers with it by `(is_outbound, min_ping)`, and many
//! peers tie on that key, so the table is only byte-identical with the same algorithm.
//! The code follows `bits/stl_algo.h` and `bits/stl_heap.h`; elements are `Copy` handles (indices),
//! which makes libstdc++'s moves plain copies.

const THRESHOLD: usize = 16;

/// `std::sort(v.begin(), v.end(), less)`.
pub fn std_sort<K: Copy>(v: &mut [K], less: &impl Fn(&K, &K) -> bool) {
    let n = v.len();
    if n == 0 {
        return;
    }
    introsort_loop(v, 0, n, 2 * lg(n), less);
    final_insertion_sort(v, less);
}

/// `std::__lg`: the floor of the binary logarithm.
fn lg(n: usize) -> usize {
    (usize::BITS - 1 - n.leading_zeros()) as usize
}

fn introsort_loop<K: Copy>(
    v: &mut [K],
    first: usize,
    mut last: usize,
    mut depth_limit: usize,
    less: &impl Fn(&K, &K) -> bool,
) {
    while last - first > THRESHOLD {
        if depth_limit == 0 {
            partial_sort(v, first, last, last, less);
            return;
        }
        depth_limit -= 1;
        let cut = unguarded_partition_pivot(v, first, last, less);
        introsort_loop(v, cut, last, depth_limit, less);
        last = cut;
    }
}

fn unguarded_partition_pivot<K: Copy>(
    v: &mut [K],
    first: usize,
    last: usize,
    less: &impl Fn(&K, &K) -> bool,
) -> usize {
    let mid = first + (last - first) / 2;
    move_median_to_first(v, first, first + 1, mid, last - 1, less);
    unguarded_partition(v, first + 1, last, first, less)
}

fn move_median_to_first<K: Copy>(
    v: &mut [K],
    result: usize,
    a: usize,
    b: usize,
    c: usize,
    less: &impl Fn(&K, &K) -> bool,
) {
    if less(&v[a], &v[b]) {
        if less(&v[b], &v[c]) {
            v.swap(result, b);
        } else if less(&v[a], &v[c]) {
            v.swap(result, c);
        } else {
            v.swap(result, a);
        }
    } else if less(&v[a], &v[c]) {
        v.swap(result, a);
    } else if less(&v[b], &v[c]) {
        v.swap(result, c);
    } else {
        v.swap(result, b);
    }
}

fn unguarded_partition<K: Copy>(
    v: &mut [K],
    mut first: usize,
    mut last: usize,
    pivot: usize,
    less: &impl Fn(&K, &K) -> bool,
) -> usize {
    loop {
        while less(&v[first], &v[pivot]) {
            first += 1;
        }
        last -= 1;
        while less(&v[pivot], &v[last]) {
            last -= 1;
        }
        if first >= last {
            return first;
        }
        v.swap(first, last);
        first += 1;
    }
}

fn final_insertion_sort<K: Copy>(v: &mut [K], less: &impl Fn(&K, &K) -> bool) {
    let n = v.len();
    if n > THRESHOLD {
        insertion_sort(v, 0, THRESHOLD, less);
        for i in THRESHOLD..n {
            unguarded_linear_insert(v, i, less);
        }
    } else {
        insertion_sort(v, 0, n, less);
    }
}

fn insertion_sort<K: Copy>(v: &mut [K], first: usize, last: usize, less: &impl Fn(&K, &K) -> bool) {
    if first == last {
        return;
    }
    for i in first + 1..last {
        if less(&v[i], &v[first]) {
            let value = v[i];
            v.copy_within(first..i, first + 1);
            v[first] = value;
        } else {
            unguarded_linear_insert(v, i, less);
        }
    }
}

fn unguarded_linear_insert<K: Copy>(v: &mut [K], mut last: usize, less: &impl Fn(&K, &K) -> bool) {
    let value = v[last];
    while less(&value, &v[last - 1]) {
        v[last] = v[last - 1];
        last -= 1;
    }
    v[last] = value;
}

/// `std::__partial_sort(first, middle, last)`: the heapsort fallback of a too-deep introsort.
fn partial_sort<K: Copy>(
    v: &mut [K],
    first: usize,
    middle: usize,
    last: usize,
    less: &impl Fn(&K, &K) -> bool,
) {
    // heap_select
    make_heap(v, first, middle, less);
    for i in middle..last {
        if less(&v[i], &v[first]) {
            pop_heap(v, first, middle, i, less);
        }
    }
    // sort_heap
    let mut end = middle;
    while end - first > 1 {
        end -= 1;
        pop_heap(v, first, end, end, less);
    }
}

fn make_heap<K: Copy>(v: &mut [K], first: usize, last: usize, less: &impl Fn(&K, &K) -> bool) {
    let len = last - first;
    if len < 2 {
        return;
    }
    let mut parent = (len - 2) / 2;
    loop {
        let value = v[first + parent];
        adjust_heap(v, first, parent, len, value, less);
        if parent == 0 {
            return;
        }
        parent -= 1;
    }
}

fn pop_heap<K: Copy>(
    v: &mut [K],
    first: usize,
    last: usize,
    result: usize,
    less: &impl Fn(&K, &K) -> bool,
) {
    let value = v[result];
    v[result] = v[first];
    adjust_heap(v, first, 0, last - first, value, less);
}

fn adjust_heap<K: Copy>(
    v: &mut [K],
    first: usize,
    mut hole: usize,
    len: usize,
    value: K,
    less: &impl Fn(&K, &K) -> bool,
) {
    let top = hole;
    let mut child = hole;
    while child < len.saturating_sub(1) / 2 {
        child = 2 * (child + 1);
        if less(&v[first + child], &v[first + child - 1]) {
            child -= 1;
        }
        v[first + hole] = v[first + child];
        hole = child;
    }
    if len & 1 == 0 && child == (len - 2) / 2 {
        child = 2 * (child + 1);
        v[first + hole] = v[first + child - 1];
        hole = child - 1;
    }
    // push_heap
    let mut parent = hole.wrapping_sub(1) / 2;
    while hole > top && less(&v[first + parent], &value) {
        v[first + hole] = v[first + parent];
        hole = parent;
        parent = hole.wrapping_sub(1) / 2;
    }
    v[first + hole] = value;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sorts by the first field only, so equal keys expose the order of ties.
    fn run(keys: &[u8]) -> Vec<(u8, usize)> {
        let mut v: Vec<(u8, usize)> = keys.iter().copied().zip(0..).collect();
        std_sort(&mut v, &|a: &(u8, usize), b: &(u8, usize)| a.0 < b.0);
        v
    }

    #[test]
    fn sorts_and_permutes() {
        let mut state = 12345u32;
        for n in [0usize, 1, 2, 3, 15, 16, 17, 18, 40, 100, 1000] {
            let keys: Vec<u8> = (0..n)
                .map(|_| {
                    state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                    (state >> 24) as u8 % 7
                })
                .collect();
            let out = run(&keys);
            assert!(out.windows(2).all(|w| w[0].0 <= w[1].0), "n={n}");
            let mut ids: Vec<usize> = out.iter().map(|p| p.1).collect();
            ids.sort_unstable();
            assert_eq!(ids, (0..n).collect::<Vec<_>>());
        }
    }

    #[test]
    fn heapsort_fallback_sorts() {
        let mut v: Vec<u32> = (0..200).map(|i| (i * 7919) % 101).collect();
        let n = v.len();
        partial_sort(&mut v, 0, n, n, &|a: &u32, b: &u32| a < b);
        assert!(v.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn small_inputs_use_insertion_sort_and_stay_stable() {
        // Insertion sort moves an element past strictly greater ones only.
        let out = run(&[2, 1, 2, 1, 2, 1]);
        assert_eq!(out, vec![(1, 1), (1, 3), (1, 5), (2, 0), (2, 2), (2, 4)]);
    }
}
