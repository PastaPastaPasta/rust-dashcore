//! The check queue's loom model (CS6): [`Shared`] with loom's lock and atomics, two helpers
//! started as `add` asks, and the master adding then draining as `CheckQueue::control` does. In
//! every interleaving the verdict is the AND of the checks, no check runs twice, every check
//! runs when all pass, and no helper is left counted as active.

use loom::sync::Arc;
use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::thread;

use super::{Check, Shared};

struct Flag {
    ok: bool,
    bit: usize,
    ran: Arc<AtomicUsize>,
}

impl Check for Flag {
    fn check(self) -> bool {
        let before = self.ran.fetch_or(1 << self.bit, Ordering::Relaxed);
        assert_eq!(before & (1 << self.bit), 0, "check {} ran twice", self.bit);
        self.ok
    }
}

fn model(oks: &'static [&'static [bool]]) {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(3);
    builder.check(move || {
        let ran = Arc::new(AtomicUsize::new(0));
        let shared = Arc::new(Shared::new(2));
        let mut helpers = Vec::new();
        let mut bit = 0;
        for batch in oks {
            let checks: Vec<Flag> = batch
                .iter()
                .map(|&ok| {
                    bit += 1;
                    Flag {
                        ok,
                        bit: bit - 1,
                        ran: Arc::clone(&ran),
                    }
                })
                .collect();
            for _ in 0..shared.add(checks) {
                let shared = Arc::clone(&shared);
                helpers.push(thread::spawn(move || shared.drain(true)));
            }
        }
        shared.drain(false);
        for helper in helpers {
            helper.join().unwrap();
        }
        let all = oks.iter().flat_map(|b| b.iter()).all(|&ok| ok);
        assert_eq!(shared.all_ok(), all);
        if all {
            assert_eq!(ran.load(Ordering::Relaxed), (1 << bit) - 1);
        }
        assert_eq!(shared.lock().active, 0);
    });
}

/// Two adds, the second while helpers may be draining the first; one check fails.
#[test]
fn verdict_is_the_and_in_every_interleaving() {
    model(&[&[true, false], &[true]]);
    model(&[&[true, true], &[true]]);
}
