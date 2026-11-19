//! BL-502 test 11(a): init's validation caches draw their salts from the process RNG, in a
//! binary of its own because the state is process-wide. The state does not exist until the
//! salts' draws make it; each salt is a 32-byte draw, so the draws pass the RNG's 32-byte limit
//! (a larger one aborts), and the two differ from each other and from zero.

use dash_args::ArgsManager;
use dash_consensus::script::ScriptFlags;
use dashd_chainstate::sigcache::{ScriptExecCache, SigCache};
use dashd_node::init::caches::init_validation_caches;
use dashd_node::logging::Logger;

#[test]
fn validation_cache_salts_come_from_the_process_rng() {
    assert!(!dashd_random::state_created());
    let caches = init_validation_caches(&Logger::new(), &ArgsManager::new()).unwrap();
    assert!(dashd_random::state_created(), "the salts were not drawn from the process RNG");
    // A signature entry over (x, flags, "") hashes the bytes an execution entry over (x, flags)
    // does, so the two entries are equal exactly when the two salts are.
    let x = [0x42; 32];
    let flags = ScriptFlags::NONE;
    let sig = caches.sig.entry(&x, &flags.bits().to_le_bytes(), &[]);
    let exec = caches.exec.entry(&x, flags);
    assert_ne!(sig, exec, "both caches have the same salt");
    let unsalted = SigCache::with_nonce(1 << 20, [0; 32]).unwrap();
    assert_ne!(sig, unsalted.entry(&x, &flags.bits().to_le_bytes(), &[]), "a zero salt");
    let unsalted_exec = ScriptExecCache::with_nonce(1 << 20, [0; 32]).unwrap().entry(&x, flags);
    assert_ne!(exec, unsalted_exec, "a zero salt");
}
