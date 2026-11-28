//! CH5-4b's regtest import, killed (`_exit(3)`) right after each of its writes by
//! `RDASHD_TEST_HOOK=core-import-exit`: every filesystem call that changes the network
//! directory, and the fresh store's reindex flag. The next start resumes from the journal, and
//! the node reaches the cache's tip with Core's databases in one backup and an earlier backup
//! left as it was.
#![cfg(unix)]

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::core_cache::*;
use common::net::{Binary, Node};
use common::*;

/// The hooked start's exit status, or `None` once it got through init with no write left to
/// stop at (then stopped).
fn hooked_start(home: &RetainedHome, datadir: &Path, step: usize) -> Option<i32> {
    let args = ["-printtoconsole=0".to_owned(), format!("-mocktime={CACHE_MOCKTIME}")];
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut cmd = regtest_on(Path::new(RDASHD), home.path(), datadir, &args);
    cmd.env("RDASHD_TEST_HOOK", "core-import-exit").env("RDASHD_TEST_HOOK_STEP", step.to_string());
    let log = debug_log(datadir);
    let before = std::fs::metadata(&log).map_or(0, |m| m.len() as usize);
    let mut running = Running::spawn(cmd, log.clone());
    let end = Instant::now() + Duration::from_secs(120);
    loop {
        if running.exited() {
            return running.wait().status.code();
        }
        if read(&log).get(before..).is_some_and(|l| l.contains("init message: Done loading")) {
            running.stop_ok(libc::SIGTERM);
            return None;
        }
        assert!(Instant::now() < end, "step {step}: the hooked start ends");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_kill_at_each_import_boundary_resumes() {
    let home = RetainedHome::new("ch54b-kill-");
    let (cache, tip) = build_cache(&home);
    let earlier_name = backup_name(1_600_000_000);
    let earlier = net(&cache).join(&earlier_name);
    std::fs::create_dir(&earlier).unwrap();
    std::fs::write(earlier.join("kept"), b"an earlier import's").unwrap();
    let core_members = members(&net(&cache));
    let mut steps = 0;
    for step in 1.. {
        let index = step;
        let datadir = copy_datadir_to(&cache, &home, index);
        let regtest = net(&datadir);
        let earlier = regtest.join(&earlier_name);
        match hooked_start(&home, &datadir, step) {
            Some(3) => {}
            None => {
                imported(&regtest, &core_members, std::slice::from_ref(&earlier));
                break;
            }
            other => panic!("step {step}: exit {other:?}\n{}", read(&debug_log(&datadir))),
        }
        steps = step;
        let mut node = Node::new(&Binary::rdashd(), &home, index, &[]);
        node.start_with(Some(CACHE_MOCKTIME), &[]);
        let end = Instant::now() + Duration::from_secs(120);
        while node.best_hash() != tip {
            assert!(Instant::now() < end, "step {step}: the resumed node reaches the tip");
            std::thread::sleep(Duration::from_millis(100));
        }
        node.stop();
        imported(&regtest, &core_members, std::slice::from_ref(&earlier));
        assert_eq!(contents(&earlier).len(), 1, "step {step}");
        assert_eq!(std::fs::read(earlier.join("kept")).unwrap(), b"an earlier import's");
        std::fs::remove_dir_all(&datadir).unwrap();
    }
    assert!(steps >= 20, "the import's writes: {steps}");
}
