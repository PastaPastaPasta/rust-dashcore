//! CH5-4b: a regtest node started on a Dash Core network directory imports it, and every other
//! chain still refuses one.

use std::sync::Mutex;

use dash_coreimport::datadir_import::OsImportFs;
use dash_coreimport::regtest_import::{BACKUP_PREFIX, JOURNAL, MEMBERS, Phase, read_journal};

use super::*;
use crate::chain::{BlkFiles, BlkOpen, BlocksDir};
use crate::core_import::tests::{contents, write_core};

/// The backups in `net`, by name.
fn backups(net: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(net)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(BACKUP_PREFIX))
        .collect();
    out.sort();
    out
}

/// The blocks directory, noting for each file the scan opens whether the import had completed:
/// no journal in the network directory, and a `Complete` one in a backup.
struct Watched {
    net: PathBuf,
    opened: Arc<Mutex<Vec<(u32, bool)>>>,
}

impl BlkFiles for Watched {
    fn open(&self, file: u32) -> BlkOpen {
        let fs = OsImportFs::new();
        let complete = read_journal(&fs, &self.net).unwrap().is_none()
            && backups(&self.net)
                .iter()
                .any(|b| read_journal(&fs, b).unwrap().is_some_and(|j| j.phase == Phase::Complete));
        self.opened.lock().unwrap().push((file, complete));
        BlocksDir(self.net.join("blocks")).open(file)
    }
}

fn tip(node: &Node) -> BlockHash {
    node.steps.chain().unwrap().handle().view_handle().view().snap.tip.tip_hash
}

/// Each member of `core`, a network directory's contents, is in `backup` unchanged.
fn moved_unchanged(core: &BTreeMap<PathBuf, Option<Vec<u8>>>, backup: &Path) {
    for name in MEMBERS {
        let moved = contents(&backup.join(name));
        let was: BTreeMap<_, _> = core
            .iter()
            .filter_map(|(p, c)| {
                let rel = p.strip_prefix(name).ok()?;
                (!rel.as_os_str().is_empty()).then(|| (rel.to_path_buf(), c.clone()))
            })
            .collect();
        assert_eq!(moved, was, "{name}");
    }
}

#[test]
fn a_core_regtest_directory_is_imported_and_its_journal_completes_before_the_scan() {
    let fresh = tempfile::tempdir().unwrap();
    let genesis = {
        let node = super::txindex::started(fresh.path(), &[]);
        tip(&node)
    };
    let dir = tempfile::tempdir().unwrap();
    let net = dir.path().join("regtest");
    write_core(&net);
    let earlier = net.join("core-import-20200101T000000Z");
    std::fs::create_dir(&earlier).unwrap();
    std::fs::write(earlier.join("kept"), b"kept").unwrap();
    let core = contents(&net);

    let (mut node, log, stderr) = start(dir.path(), &[]);
    let opened = Arc::default();
    node.steps.blk_files = Some(Box::new(Watched {
        net: net.clone(),
        opened: Arc::clone(&opened),
    }));
    assert!(node.app_init_early() && node.app_init_main(), "{stderr:?}");
    assert!(node.steps.join_import());
    assert_eq!(tip(&node), genesis);
    node.shutdown();
    let opened = opened.lock().unwrap().clone();
    assert_eq!(opened.first(), Some(&(0, true)), "{opened:?}");
    assert!(opened.iter().all(|(_, complete)| *complete), "{opened:?}");

    let made = backups(&net);
    assert_eq!(made.len(), 2, "{made:?}");
    let backup = made.iter().find(|b| **b != earlier).unwrap();
    assert!(has(&log, &format!("aside to {}; reindexing its block files", backup.display())));
    moved_unchanged(&core, backup);
    assert_eq!(std::fs::read(earlier.join("kept")).unwrap(), b"kept");
    assert!(net.join("chainstate").join(dashd_store::STORE_MARKER).is_file(), "rdashd's own");
    for flat in ["peers.dat", "banlist.json", "sporks.dat"] {
        assert!(net.join(flat).is_file(), "{flat} is written once the import has moved");
    }

    let (mut node, log, stderr) = start(dir.path(), &[]);
    let rescanned = Arc::default();
    node.steps.blk_files = Some(Box::new(Watched {
        net: net.clone(),
        opened: Arc::clone(&rescanned),
    }));
    assert!(node.app_init_early() && node.app_init_main(), "{stderr:?}");
    assert!(node.steps.join_import());
    assert_eq!(tip(&node), genesis);
    node.shutdown();
    assert_eq!(backups(&net), made, "a restart is an ordinary start");
    assert!(!has(&log, "Moved the Dash Core databases"));
    assert_eq!(*rescanned.lock().unwrap(), [], "no scan");
}

/// An unsupported `peers.dat` the network core read while the import waited, which cannot be
/// moved aside to `peers.dat.bak`, refuses init with Core's text once the databases have moved,
/// and nothing queued after it is written: `peers.dat` is kept, and `banlist.json` stays as it
/// was, absent or unreadable, since Core stops before its ban manager exists. The journal stays
/// at `Moved`, and once `peers.dat.bak` is free the next start completes the import.
// Core: addrdb.cpp:219-221, init.cpp:1683-1684, :1706 at v24.0.2
#[test]
fn an_unrenamable_peers_dat_refuses_after_the_move_and_is_kept() {
    for banlist in [None, Some(&b"{"[..])] {
        let dir = tempfile::tempdir().unwrap();
        let net = dir.path().join("regtest");
        write_core(&net);
        if let Some(bytes) = banlist {
            std::fs::write(net.join("banlist.json"), bytes).unwrap();
        }
        let core = contents(&net);
        let (mut node, _, stderr) = start(dir.path(), &[]);
        let mut peers = node.params.chain.message_start.to_vec();
        peers.extend_from_slice(&[4, 255]);
        std::fs::write(net.join("peers.dat"), &peers).unwrap();
        std::fs::create_dir(net.join("peers.dat.bak")).unwrap();
        assert!(node.app_init_early());
        assert!(!node.app_init_main());
        node.shutdown();
        let stderr = stderr.lock().unwrap().concat();
        assert!(
            stderr.contains(
                "Failed to rename invalid peers.dat file. Please move or delete it and try again."
            ),
            "{banlist:?}: {stderr}"
        );
        assert_eq!(std::fs::read(net.join("peers.dat")).unwrap(), peers, "{banlist:?}");
        assert!(net.join("peers.dat.bak").is_dir());
        assert_eq!(std::fs::read(net.join("banlist.json")).ok().as_deref(), banlist);
        let fs = OsImportFs::new();
        assert_eq!(read_journal(&fs, &net).unwrap().unwrap().phase, Phase::Moved);

        std::fs::remove_dir(net.join("peers.dat.bak")).unwrap();
        let (mut node, _, stderr) = start(dir.path(), &[]);
        assert!(node.app_init_early() && node.app_init_main(), "{stderr:?}");
        assert!(node.steps.join_import());
        node.shutdown();
        assert!(read_journal(&fs, &net).unwrap().is_none());
        let made = backups(&net);
        assert_eq!(made.len(), 1, "{made:?}");
        assert_eq!(read_journal(&fs, &made[0]).unwrap().unwrap().phase, Phase::Complete);
        moved_unchanged(&core, &made[0]);
        assert_eq!(std::fs::read(net.join("peers.dat.bak")).unwrap(), peers);
    }
}

/// Core's checks from step 11 on come after the import, in Core's order: a `-whitebind` without
/// a port is refused at step 12 once the import is complete, and the next start is an ordinary
/// one, Core's databases still in the backup.
// Core: init.cpp:2664-2668 at v24.0.2
#[test]
fn a_step_12_refusal_comes_after_the_import_and_the_next_start_is_ordinary() {
    let dir = tempfile::tempdir().unwrap();
    let net = dir.path().join("regtest");
    write_core(&net);
    let core = contents(&net);
    let (mut node, log, stderr) = start(dir.path(), &["-whitebind=127.0.0.1"]);
    assert!(node.app_init_early());
    assert!(!node.app_init_main());
    node.shutdown();
    let stderr = stderr.lock().unwrap().concat();
    assert!(stderr.contains("Need to specify a port with -whitebind: '127.0.0.1'"), "{stderr}");
    assert!(has(&log, "Moved the Dash Core databases"));
    let made = backups(&net);
    assert_eq!(made.len(), 1, "{made:?}");
    let fs = OsImportFs::new();
    assert!(read_journal(&fs, &net).unwrap().is_none());
    assert_eq!(read_journal(&fs, &made[0]).unwrap().unwrap().phase, Phase::Complete);
    moved_unchanged(&core, &made[0]);

    let (mut node, log, stderr) = start(dir.path(), &[]);
    assert!(node.app_init_early() && node.app_init_main(), "{stderr:?}");
    assert!(node.steps.join_import());
    node.shutdown();
    assert!(!has(&log, "Moved the Dash Core databases"));
    assert_eq!(backups(&net), made);
    moved_unchanged(&core, &made[0]);
}

#[test]
fn a_request_reindex_refusal_is_an_init_error_that_leaves_the_journal_at_moved() {
    let dir = tempfile::tempdir().unwrap();
    let net = dir.path().join("regtest");
    drop(super::txindex::started(dir.path(), &[]));
    std::fs::rename(net.join("chainstate"), dir.path().join("own")).unwrap();
    std::fs::rename(net.join("llmq"), dir.path().join("own-llmq")).unwrap();
    write_core(&net);
    let aside = dash_coreimport::regtest_import::begin(&OsImportFs::new(), &net).unwrap();
    std::fs::rename(dir.path().join("own"), net.join("chainstate")).unwrap();
    std::fs::rename(dir.path().join("own-llmq"), net.join("llmq")).unwrap();

    let (mut node, _, stderr) = start(dir.path(), &[]);
    assert!(!(node.app_init_early() && node.app_init_main()));
    node.shutdown();
    let stderr = stderr.lock().unwrap().concat();
    assert!(
        stderr.contains(&format!(
            "the reindex could not be stored (the block index is not empty); {} holds the \
             import's journal and {} Core's databases",
            net.join(JOURNAL).display(),
            aside.backup().display()
        )),
        "{stderr}"
    );
    let journal = read_journal(&OsImportFs::new(), &net).unwrap().unwrap();
    assert_eq!(journal.phase, Phase::Moved);
}

fn refusal(dir: &Path) -> String {
    format!(
        "{} holds a Dash Core data directory; rdashd does not write to it; to import it, start \
         rdashd on an empty -datadir with -importcoredatadir=<its base datadir>.",
        dir.display()
    )
}

/// Off regtest a Core directory is refused as before, the base's `regtest` included, with
/// nothing written; on regtest the other chains' directories are still refused before the
/// config file is read.
#[test]
fn core_on_main_test_devnet_still_refuses_with_todays_text() {
    let at = |dir: &Path, sub: &str| {
        if sub.is_empty() {
            dir.to_path_buf()
        } else {
            dir.join(sub)
        }
    };
    for flag in ["-chain=main", "-testnet", "-devnet=d"] {
        for sub in ["", "testnet3", "devnet-d", "regtest"] {
            let dir = tempfile::tempdir().unwrap();
            let core = at(dir.path(), sub);
            write_core(&core);
            let before = contents(dir.path());
            let (_, params) = config_on(dir.path(), flag, &[]);
            let Err(InitFailure::Error(msg)) = params else {
                panic!("{flag} {sub:?} not refused");
            };
            assert_eq!(msg, refusal(&core), "{flag} {sub:?}");
            assert_eq!(contents(dir.path()), before, "{flag} {sub:?}");
        }
    }
    for sub in ["", "testnet3", "devnet-d"] {
        let dir = tempfile::tempdir().unwrap();
        write_core(&at(dir.path(), sub));
        let before = contents(dir.path());
        let (_, params) = config(dir.path(), &[]);
        let Err(InitFailure::Error(msg)) = params else {
            panic!("regtest {sub:?} not refused");
        };
        assert_eq!(msg, refusal(&at(dir.path(), sub)));
        assert_eq!(contents(dir.path()), before);
    }
}

/// The base's `regtest` waits for the chain, so a testnet start with both `testnet3` and
/// `regtest` in Core's layout names `testnet3`, which the base check refuses first.
#[test]
fn a_testnet_start_names_testnet3_when_regtest_is_core_s_too() {
    let dir = tempfile::tempdir().unwrap();
    write_core(&dir.path().join("regtest"));
    write_core(&dir.path().join("testnet3"));
    let (_, params) = config_on(dir.path(), "-testnet", &[]);
    let Err(InitFailure::Error(msg)) = params else {
        panic!("not refused");
    };
    assert_eq!(msg, refusal(&dir.path().join("testnet3")));
}

/// The base's `regtest` is judged after the config file is read, so a config error comes
/// first, on every chain.
#[test]
fn a_config_error_comes_before_a_regtest_core_refusal() {
    for flag in ["-regtest", "-testnet"] {
        let dir = tempfile::tempdir().unwrap();
        let core = dir.path().join("regtest");
        write_core(&core);
        std::fs::remove_file(core.join("chainstate/000005.ldb")).unwrap();
        std::fs::write(dir.path().join("dash.conf"), "-server=1\n").unwrap();
        let (_, params) = config_on(dir.path(), flag, &[]);
        let Err(InitFailure::Error(msg)) = params else {
            panic!("{flag} not refused");
        };
        assert!(msg.contains("Error reading configuration file"), "{flag}: {msg}");
        std::fs::remove_file(dir.path().join("dash.conf")).unwrap();
        let (_, params) = config_on(dir.path(), flag, &[]);
        let Err(InitFailure::Error(msg)) = params else {
            panic!("{flag} not refused");
        };
        assert_eq!(msg, refusal(&core), "{flag}");
    }
}

/// dashd v24.0.2's `settings.json` and `banlist.json` as a stopped regtest node leaves them.
const CORE_SETTINGS: &[u8] = b"{\n    \"_warning_\": \"This file is automatically generated and updated by \
Dash Core. Please do not edit this file while the node is running, as any changes might be ignored or \
overwritten.\"\n}\n";
const CORE_BANLIST: &[u8] = b"{\n    \"_warning_\": \"This file is automatically generated and updated by \
Dash Core. Please do not edit this file while the node is running, as any changes might be ignored or \
overwritten.\",\n    \"banned_nets\": [\n    ]\n}\n";

/// A start refused after the network core and the sporks exist, for rdashd's own reason (an
/// option it does not support) or for Core's (an argument check), leaves every file of the data
/// directory byte-identical and removes none, with three exceptions, as Core's own start: it
/// adds `regtest/.lock`, `regtest/debug.log` and, where there was none, `regtest/settings.json`,
/// which `InitConfig` writes as Core's does; and a stale `dashd.pid` and `.cookie`, as a killed
/// dashd leaves them, are rewritten at step 4a and RPC start and removed at shutdown.
/// Regression for `prepare`'s place after those refusals and for the held flat files (CH5-4b).
// Core: init.cpp:1519, :231-239, :518, httprpc.cpp:283, rpc/server.cpp:333 at v24.0.2
#[test]
fn a_refused_start_leaves_a_core_regtest_directory_untouched() {
    let unsupported = ("-forcecompactdb", "-forcecompactdb is not supported by rdashd yet (CH2b).");
    let core_check = ("-maxmempool=1", "-maxmempool must be at least 5 MB");
    let stale_pid = b"4242\n";
    let stale_cookie = b"__cookie__:stale";
    for ((arg, refusal), settings) in
        [(unsupported, true), (core_check, true), (unsupported, false)]
    {
        let dir = tempfile::tempdir().unwrap();
        let net = dir.path().join("regtest");
        write_core(&net);
        if settings {
            std::fs::write(net.join("settings.json"), CORE_SETTINGS).unwrap();
        }
        std::fs::write(net.join("banlist.json"), CORE_BANLIST).unwrap();
        std::fs::write(net.join(datadir::PID_FILENAME), stale_pid).unwrap();
        std::fs::write(net.join(".cookie"), stale_cookie).unwrap();
        let mut before = contents(dir.path());
        let port = format!("-rpcport={}", node_test_ports::free_port());
        let (mut node, _, stderr) = start(dir.path(), &[arg, "-server=1", &port]);
        assert!(node.app_init_early());
        assert!(!node.app_init_main());
        let pid = std::fs::read(net.join(datadir::PID_FILENAME)).unwrap();
        assert_eq!(pid, format!("{}\n", std::process::id()).into_bytes(), "{arg}");
        let cookie = std::fs::read(net.join(".cookie")).unwrap();
        assert!(cookie.starts_with(b"__cookie__:") && cookie != stale_cookie, "{arg}");
        node.shutdown();
        let stderr = stderr.lock().unwrap().concat();
        assert!(stderr.contains(refusal), "{arg}: {stderr}");
        let mut after = contents(dir.path());
        for name in [datadir::PID_FILENAME, ".cookie"] {
            assert!(before.remove(&Path::new("regtest").join(name)).is_some(), "{arg}: {name}");
            assert!(!after.contains_key(&Path::new("regtest").join(name)), "{arg}: {name} removed");
        }
        let created: &[&str] = if settings {
            &[".lock", "debug.log"]
        } else {
            &[".lock", "debug.log", "settings.json"]
        };
        for name in created {
            let name = Path::new("regtest").join(name);
            assert!(!before.contains_key(&name), "{arg}: {}", name.display());
            assert!(after.remove(&name).is_some(), "{arg}: {}", name.display());
        }
        assert_eq!(after, before, "{arg}, settings.json before: {settings}");
    }
}

/// The four caches rdashd cannot read: init names them in order, and writes nothing.
#[test]
fn a_refusal_names_every_cache_rdashd_cannot_read() {
    let dir = tempfile::tempdir().unwrap();
    let net = dir.path().join("regtest");
    write_core(&net);
    std::fs::write(net.join("governance.dat"), b"\x14magicGovernanceCache").unwrap();
    std::fs::write(net.join("mncache.dat"), b"\x14magicMasternodeCache").unwrap();
    std::fs::write(net.join("netfulfilled.dat"), b"\x13magicFulfilledCache").unwrap();
    std::fs::write(net.join("sporks.dat"), b"\x0fmagicSporkCache").unwrap();
    let before = contents(dir.path());
    let (_, params) = config(dir.path(), &[]);
    let Err(InitFailure::Error(msg)) = params else {
        panic!("not refused");
    };
    assert_eq!(
        msg,
        format!(
            "{} holds sporks.dat, netfulfilled.dat, mncache.dat, governance.dat, which rdashd \
             cannot read; these are caches that are recreated when missing, so move them out of \
             the directory to import it.",
            net.display()
        )
    );
    assert_eq!(contents(dir.path()), before);
}

/// Core's `governance.dat` alone is refused, and the directory left as it was, with governance
/// on or off: Core clears it on a reindex only when governance is on, but rdashd refuses it
/// either way (DEC-1940).
// Core: init.cpp:2260-2268 at v24.0.2
#[test]
fn a_core_governance_dat_is_refused_even_with_governance_disabled() {
    for args in [&[][..], &["-disablegovernance=1"]] {
        let dir = tempfile::tempdir().unwrap();
        let net = dir.path().join("regtest");
        write_core(&net);
        std::fs::write(net.join("governance.dat"), b"\x14magicGovernanceCache").unwrap();
        let before = contents(dir.path());
        let (_, params) = config(dir.path(), args);
        let Err(InitFailure::Error(msg)) = params else {
            panic!("{args:?}: not refused");
        };
        assert_eq!(
            msg,
            format!(
                "{} holds governance.dat, which rdashd cannot read; these are caches that are \
                 recreated when missing, so move them out of the directory to import it.",
                net.display()
            ),
            "{args:?}"
        );
        assert_eq!(contents(dir.path()), before, "{args:?}");
    }
}
