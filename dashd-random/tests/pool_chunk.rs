//! The process state's chunk of the locked pool, in a binary of its own so nothing else
//! allocates from the pool: none before the first draw, one of Core's size after it, for good.

#[test]
fn process_state_holds_one_chunk_of_cores_size() {
    assert!(!dashd_random::state_created());
    assert_eq!(dashd_lockedpool::stats().chunks_used, 0);
    dashd_random::static_init();
    assert!(dashd_random::state_created());
    let after = dashd_lockedpool::stats();
    // Core's sizeof(RNGState) rounded to the arena's 16-byte alignment.
    assert_eq!(after.used, dashd_random::RNG_STATE_SIZE.next_multiple_of(16));
    #[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]
    assert_eq!((after.used, after.chunks_used, after.chunks_free), (240, 1, 1));
    dashd_random::random_init();
    dashd_random::get_rand_hash();
    dashd_random::get_strong_rand_bytes(&mut [0u8; 32]);
    dashd_random::rand_add_event(3);
    dashd_random::rand_add_periodic();
    let stats = dashd_lockedpool::stats();
    assert_eq!(
        (stats.used, stats.free, stats.total, stats.chunks_used, stats.chunks_free),
        (after.used, after.free, after.total, 1, 1)
    );
}
