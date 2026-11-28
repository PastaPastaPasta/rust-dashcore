//! CH5-4b: rdashd on regtest datadirs dashd v24 made (the framework's cache, `copy_datadir`'s and
//! `dynamically_initialize_datadir`'s copies). Core's databases move aside into a backup, the
//! node reindexes the block files Core left, and its chain is dashd's. Off regtest a Core
//! directory is still refused untouched.
#![cfg(unix)]

mod common;

use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::time::{Duration, Instant};

use common::core_cache::*;
use common::net::{Binary, Json, Node};
use common::*;
use dashd_store::codec::ValueCodec as _;

fn ours_on(home: &RetainedHome, index: usize, from: &Path) -> Node {
    copy_datadir_to(from, home, index);
    Node::new(&Binary::rdashd(), home, index, &[])
}

fn core_on(home: &RetainedHome, index: usize, from: &Path) -> Node {
    copy_datadir_to(from, home, index);
    Node::new(&Binary::dashd(), home, index, &[])
}

/// The chain both nodes report: best block, UTXO set summary and tips.
fn chain(node: &Node) -> (String, Json, Vec<Json>) {
    (node.best_hash(), utxo_summary(node), node.chain_tips())
}

/// rdashd's stored info for `blk00000.dat`/`rev00000.dat`, its chainstate opened after the stop.
fn file_0_undo_size(datadir: &Path) -> u32 {
    use dashd_chainstate::store::CHAINSTATE_SCHEMA;
    use dashd_store::flatfile::BlockFileInfo;
    use dashd_store::{KvRead as _, RocksConfig, RocksEngine};
    let path = net(datadir).join("chainstate");
    let (db, _) = RocksEngine::open(&path, &CHAINSTATE_SCHEMA, &RocksConfig::default()).unwrap();
    let cf = CHAINSTATE_SCHEMA.cf("block_files").unwrap();
    let value = db.get(cf, &0u32.to_be_bytes()).unwrap().expect("file 0's info");
    BlockFileInfo::decode_value(&value).unwrap().undo_size
}

/// The framework's cache, started as each node of a test starts from it: rdashd imports it and
/// reaches dashd's chain. Its undo file is rewritten from offset 0 in place, over the same bytes,
/// and not truncated (D81). A restart is then an ordinary start.
// Core: node/blockstorage.cpp:729-736, flatfile.cpp:39-52 at v24.0.2
#[test]
fn cache_start_matches_dashd_v24() {
    let home = RetainedHome::new("ch54b-cache-");
    let (cache, tip) = build_cache(&home);
    let mut core = core_on(&home, 1, &cache);
    let mut ours = ours_on(&home, 2, &cache);
    let regtest = net(&ours.datadir);
    let core_members = members(&regtest);
    assert_eq!(core_members.len(), MEMBERS.len(), "the cache holds every member");
    let rev = std::fs::read(regtest.join("blocks/rev00000.dat")).unwrap();
    core.start_with(Some(CACHE_MOCKTIME), &[]);
    ours.start_with(Some(CACHE_MOCKTIME), &[]);
    let want = chain(&core);
    assert_eq!(want.0, tip);
    let end = Instant::now() + Duration::from_secs(120);
    while ours.best_hash() != tip {
        assert!(Instant::now() < end, "rdashd reaches the cache's tip");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(chain(&ours), want);
    core.stop();
    ours.stop();
    let backup = imported(&regtest, &core_members, &[]);
    let log = read(&debug_log(&ours.datadir));
    assert!(
        log.contains(&format!(
            "Moved the Dash Core databases of {} aside to {}; reindexing its block files",
            regtest.display(),
            backup.display()
        )),
        "{log}"
    );

    let undo_size = file_0_undo_size(&ours.datadir) as usize;
    let after = std::fs::read(regtest.join("blocks/rev00000.dat")).unwrap();
    assert!(undo_size > 0 && undo_size <= rev.len(), "{undo_size} of {}", rev.len());
    assert!(after.len() >= rev.len(), "not truncated: {} < {}", after.len(), rev.len());
    assert!(after[..undo_size] == rev[..undo_size], "the undo records are Core's, from offset 0");

    let from = ours.log_len();
    ours.start_with(Some(CACHE_MOCKTIME), &[]);
    assert_eq!(chain(&ours), want);
    ours.stop();
    let restart = ours.log_since(from);
    assert!(!restart.contains("Moved the Dash Core databases"), "{restart}");
    assert!(!restart.contains("Reindexing"), "{restart}");
    assert_eq!(backups(&regtest), [backup]);
}

/// `copy_datadir`'s copy (`blocks`, `chainstate`, `evodb`, `llmq`, no `indexes`) into a node that
/// has none: both binaries start on it with the cache's chain.
// Core: test/functional/test_framework/util.py:503-514 at v24.0.2
#[test]
fn copy_datadir_from_dashd_starts() {
    let home = RetainedHome::new("ch54b-copy-");
    let (cache, tip) = build_cache(&home);
    let copied = home.path().join("copied");
    for dir in ["blocks", "chainstate", "evodb", "llmq"] {
        copy_tree(&net(&cache).join(dir), &net(&copied).join(dir));
    }
    let mut core = core_on(&home, 1, &copied);
    let mut ours = ours_on(&home, 2, &copied);
    let core_members = members(&net(&ours.datadir));
    core.start_with(Some(CACHE_MOCKTIME), &[]);
    ours.start_with(Some(CACHE_MOCKTIME), &[]);
    let want = chain(&core);
    assert_eq!(want.0, tip);
    let end = Instant::now() + Duration::from_secs(120);
    while ours.best_hash() != tip {
        assert!(Instant::now() < end, "rdashd reaches the cache's tip");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(chain(&ours), want);
    core.stop();
    ours.stop();
    imported(&net(&ours.datadir), &core_members, &[]);
}

/// `dynamically_initialize_datadir`: dashd's directory copied while it runs, after its
/// `gettxoutsetinfo`, without `llmq` or any file but the kept directories'. rdashd starts on it
/// with the running node's chain.
// Core: test/functional/test_framework/test_framework.py:632-650 at v24.0.2
#[test]
fn dynamically_initialize_datadir_from_a_running_dashd_starts() {
    let home = RetainedHome::new("ch54b-dyn-");
    let (cache, tip) = build_cache(&home);
    let mut core = core_on(&home, 1, &cache);
    core.start_with(Some(CACHE_MOCKTIME), &[]);
    let want = chain(&core);
    let live = home.path().join("node2");
    copy_tree(&core.datadir, &live);
    let regtest = net(&live);
    std::fs::remove_dir_all(regtest.join("llmq")).unwrap();
    for entry in std::fs::read_dir(&regtest).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !["chainstate", "blocks", "indexes", "evodb"].contains(&name.as_str()) {
            if path.is_dir() {
                std::fs::remove_dir_all(&path).unwrap();
            } else {
                std::fs::remove_file(&path).unwrap();
            }
        }
    }
    let core_members = members(&regtest);
    let mut ours = Node::new(&Binary::rdashd(), &home, 2, &[]);
    ours.start_with(Some(CACHE_MOCKTIME), &[]);
    let end = Instant::now() + Duration::from_secs(120);
    while ours.best_hash() != tip {
        assert!(Instant::now() < end, "rdashd reaches the copy's tip");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(chain(&ours), want);
    ours.stop();
    core.stop();
    imported(&regtest, &core_members, &[]);
}

/// A dashd running on the datadir holds its lock: rdashd gets Core's lock error before any
/// database is read, and nothing moves.
#[test]
fn a_live_dashd_on_the_datadir_is_refused_before_anything_moves() {
    let home = RetainedHome::new("ch54b-live-");
    let (cache, _) = build_cache(&home);
    let mut core = core_on(&home, 1, &cache);
    core.start_with(Some(CACHE_MOCKTIME), &[]);
    let regtest = net(&core.datadir);
    let out = run_regtest(home.path(), &core.datadir, &["-printtoconsole=0"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr(&out),
        format!(
            "Error: Cannot obtain a lock on data directory {}. rdashd is probably already \
             running.\n",
            regtest.display()
        )
    );
    assert!(MEMBERS.iter().all(|m| regtest.join(m).is_dir()));
    assert!(backups(&regtest).is_empty());
    assert!(!regtest.join(JOURNAL).exists());
    core.stop();
}

/// Every backup name the import could take this minute is taken: it takes the next suffix, and
/// leaves every other backup as it was.
#[test]
fn an_existing_backup_gets_a_new_name() {
    let home = RetainedHome::new("ch54b-name-");
    let (cache, tip) = build_cache(&home);
    let mut ours = ours_on(&home, 1, &cache);
    let regtest = net(&ours.datadir);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let mut taken = Vec::new();
    for secs in now - 5..now + 600 {
        let dir = regtest.join(backup_name(secs));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("kept"), secs.to_string()).unwrap();
        taken.push(dir);
    }
    let core_members = members(&regtest);
    ours.start_with(Some(CACHE_MOCKTIME), &[]);
    let end = Instant::now() + Duration::from_secs(120);
    while ours.best_hash() != tip {
        assert!(Instant::now() < end, "rdashd reaches the cache's tip");
        std::thread::sleep(Duration::from_millis(100));
    }
    ours.stop();
    let backup = imported(&regtest, &core_members, &taken);
    let name = backup.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        taken.iter().any(|t| format!("{}-1", t.file_name().unwrap().to_string_lossy()) == name),
        "{name}"
    );
    for (dir, secs) in taken.iter().zip(now - 5..) {
        assert_eq!(contents(dir).len(), 1, "{}", dir.display());
        assert_eq!(std::fs::read_to_string(dir.join("kept")).unwrap(), secs.to_string());
    }
}

/// A FIFO at `blk00000.dat` parks the scan in its first open: by then the import's journal is
/// `Complete` and in the backup. The FIFO then feeds the real file, which takes its place, and
/// rdashd reaches the cache's tip. Init waits for genesis, which the parked scan has not read, so
/// the node is not waited for.
#[test]
fn the_journal_is_complete_before_the_scan_opens_blk00000() {
    let home = RetainedHome::new("ch54b-fifo-");
    let (cache, tip) = build_cache(&home);
    let datadir = copy_datadir_to(&cache, &home, 1);
    let regtest = net(&datadir);
    let blk = regtest.join("blocks/blk00000.dat");
    let real = regtest.join("blocks/blk00000.real");
    std::fs::rename(&blk, &real).unwrap();
    let path = std::ffi::CString::new(blk.as_os_str().as_bytes()).unwrap();
    // SAFETY: a NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let port = free_port();
    let args = [
        "-printtoconsole=0".to_owned(),
        "-disablewallet".to_owned(),
        format!("-mocktime={CACHE_MOCKTIME}"),
        format!("-rpcport={port}"),
    ];
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut running = Running::spawn(
        regtest_on(Path::new(RDASHD), home.path(), &datadir, &args),
        debug_log(&datadir),
    );

    let end = Instant::now() + Duration::from_secs(120);
    let writer = loop {
        // SAFETY: a NUL-terminated path; the descriptor is owned below.
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
        if fd >= 0 {
            // SAFETY: the descriptor this call opened, owned here alone.
            break unsafe { OwnedFd::from_raw_fd(fd) };
        }
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ENXIO));
        assert!(!running.exited(), "rdashd exited before the scan");
        assert!(Instant::now() < end, "the scan opens blk00000.dat");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(!regtest.join(JOURNAL).exists(), "the journal left the network directory");
    let made = backups(&regtest);
    assert_eq!(made.len(), 1);
    assert!(complete_journal(&made[0]), "Complete before the scan's first open");

    std::fs::remove_file(&blk).unwrap();
    std::fs::rename(&real, &blk).unwrap();
    // SAFETY: the descriptor `writer` owns; clearing `O_NONBLOCK` leaves it open.
    assert_eq!(unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_SETFL, 0) }, 0);
    let mut writer = std::fs::File::from(writer);
    std::io::Write::write_all(&mut writer, &std::fs::read(&blk).unwrap()).unwrap();
    drop(writer);
    let best = || {
        let reply = rpc(port, &cookie(&datadir), "getbestblockhash", "[]");
        reply.result().trim_matches('"').to_owned()
    };
    while !debug_log_has(&datadir, "init message: Done loading") || best() != tip {
        assert!(!running.exited(), "rdashd exited");
        assert!(Instant::now() < end, "rdashd reaches the cache's tip");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(rpc(port, &cookie(&datadir), "stop", "[]").result(), r#""rdashd stopping""#);
    let out = running.wait();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
}

fn debug_log_has(datadir: &Path, needle: &str) -> bool {
    read(&debug_log(datadir)).contains(needle)
}

/// `-reindex-chainstate` on a Core directory: the index is gone with Core's databases, so the
/// start is a full reindex of the block files (rdashd-only: Core replays its own block index).
#[test]
fn a_reindex_chainstate_start_imports_with_a_full_reindex() {
    let home = RetainedHome::new("ch54b-rc-");
    let (cache, tip) = build_cache(&home);
    let mut core = core_on(&home, 1, &cache);
    let mut ours = ours_on(&home, 2, &cache);
    let core_members = members(&net(&ours.datadir));
    core.start_with(Some(CACHE_MOCKTIME), &[]);
    let from = ours.log_len();
    ours.start_with(Some(CACHE_MOCKTIME), &["-reindex-chainstate".to_owned()]);
    let end = Instant::now() + Duration::from_secs(120);
    while ours.best_hash() != tip {
        assert!(Instant::now() < end, "rdashd reaches the cache's tip");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(chain(&ours), chain(&core));
    core.stop();
    ours.stop();
    let log = ours.log_since(from);
    assert!(log.contains("Reindexing block file blk00000.dat..."), "{log}");
    imported(&net(&ours.datadir), &core_members, &[]);
}

/// A testnet directory dashd v24 made and stopped: rdashd refuses it and changes nothing in it,
/// given on the command line (the base's refusal, before the config file is read) or by a
/// config file's `datadir=` (the network directory's, after the chain is selected; regtest's
/// import never runs off regtest).
#[test]
fn a_dashd_made_testnet_datadir_is_refused_untouched() {
    let dashd = require_dashd();
    let (home, datadir) = home_and_datadir();
    let testnet = datadir.join("testnet3");
    let args = [
        &datadir_arg(&datadir),
        "-testnet",
        "-listen=0",
        "-connect=0",
        "-dnsseed=0",
        "-server=0",
        "-printtoconsole=0",
    ];
    spawn_until_loaded(command(&dashd, home.path(), &args), testnet.join("debug.log"))
        .stop_ok(libc::SIGTERM);
    assert!(testnet.join("evodb").is_dir() && testnet.join("chainstate").is_dir());
    let before = tree(&datadir);
    let out = run(home.path(), &["-testnet", &datadir_arg(&datadir), "-server=0"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stderr(&out), core_datadir_refusal(&testnet));
    assert_eq!(tree(&datadir), before);

    let conf_dir = home.path().join(".rdashcore");
    std::fs::create_dir_all(&conf_dir).unwrap();
    std::fs::write(conf_dir.join("dash.conf"), format!("datadir={}\n", datadir.display())).unwrap();
    let out = run(home.path(), &["-testnet", "-server=0"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stderr(&out), core_datadir_refusal(&testnet));
    assert_eq!(tree(&datadir), before);
}
