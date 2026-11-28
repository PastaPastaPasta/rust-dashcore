//! The first `log_capture::capture` installs the global subscriber while other threads keep
//! hitting fresh event callsites; afterwards every callsite must still reach the capture.
//! Its own binary, because the install runs once per process.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

mod log_capture;

const TARGET: &str = "log_capture_install";
const HAMMERS: usize = 32;

macro_rules! callsites {
    ($($a:literal)*; $b:tt; $c:tt) => { [$(callsites!(@b $a; $b; $c)),*] };
    (@b $a:literal; [$($b:literal)*]; $c:tt) => { [$(callsites!(@c $a; $b; $c)),*] };
    (@c $a:literal; $b:literal; [$($c:literal)*]) => {
        [$((|| tracing::info!(target: TARGET, "{}", $a * 100 + $b * 10 + $c)) as fn()),*]
    };
}

#[test]
fn install_leaves_no_callsite_never() {
    let grid = callsites!(0 1 2 3; [0 1 2 3 4 5 6 7 8 9]; [0 1 2 3 4 5 6 7 8 9]);
    let emitters: Arc<Vec<fn()>> = Arc::new(grid.into_iter().flatten().flatten().collect());
    let count = emitters.len();
    let stop = Arc::new(AtomicBool::new(false));
    let start = Arc::new(Barrier::new(HAMMERS + 1));
    let hammers: Vec<_> = (0..HAMMERS)
        .map(|h| {
            let (emitters, stop, start) = (emitters.clone(), stop.clone(), start.clone());
            thread::spawn(move || {
                start.wait();
                let mut i = h * count / HAMMERS;
                while !stop.load(Ordering::Relaxed) {
                    emitters[i]();
                    i = (i + 1) % count;
                }
            })
        })
        .collect();

    start.wait();
    let log = log_capture::capture(&[TARGET]);
    stop.store(true, Ordering::Relaxed);
    for hammer in hammers {
        hammer.join().unwrap();
    }

    for emit in emitters.iter() {
        emit();
    }
    let expected: Vec<String> = (0..count).map(|i| i.to_string()).collect();
    let lines = log.lines();
    let missing: Vec<&String> = expected.iter().filter(|e| !lines.contains(e)).collect();
    assert!(missing.is_empty(), "callsites left `never` after the capture started: {missing:?}");
    assert_eq!(*lines, expected);
}
