//! `dumptxoutset`'s path, temporary file, rename, reply and errors over a fake chainman.

use std::path::Path;

use super::super::tests::{dump_server, hash, method_err, reply};
use super::TxOutSetDump;
use crate::blockchain::TxOutSetError;
use crate::protocol::{
    RPC_CLIENT_NOT_CONNECTED, RPC_INTERNAL_ERROR, RPC_INVALID_PARAMETER, RPC_MISC_ERROR,
};

fn dump() -> TxOutSetDump {
    let mut h = [0; 32];
    h[0] = 0xcd;
    TxOutSetDump {
        base: hash(9),
        height: 9,
        coins: 10,
        hash: h,
        n_chain_tx: 11,
    }
}

/// A relative path is under the network's data directory and an absolute one is itself; the
/// file is written as `<path>.incomplete`, then renamed; the reply prints the absolute path.
// Core: rpc/blockchain.cpp:3115-3138, :3201-3209; util/fs_helpers.cpp:36-40 AbsPathJoin
#[test]
fn dumptxoutset_writes_the_temporary_file_then_renames_it() {
    let dir = tempfile::tempdir().unwrap();
    let net = dir.path().join("regtest");
    std::fs::create_dir(&net).unwrap();
    let (server, live) = dump_server(&net);
    live.lock().unwrap().dump = Some((b"snapshot".to_vec(), Ok(dump())));
    let other = dir.path().join("elsewhere.dat");
    for (name, path) in
        [("utxo.dat", net.join("utxo.dat")), (other.to_str().unwrap(), other.clone())]
    {
        let json =
            super::super::tests::call_method(&server, "dumptxoutset", &format!(r#"["{name}"]"#))
                .unwrap()
                .write();
        assert_eq!(
            json,
            format!(
                concat!(
                    r#"{{"coins_written":10,"base_hash":"{}","base_height":9,"path":"{}","#,
                    r#""txoutset_hash":"{}cd","nchaintx":11}}"#
                ),
                hash(9),
                path.display(),
                "00".repeat(31)
            )
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"snapshot");
        assert!(!incomplete(&path).exists(), "renamed");
    }
    assert_eq!(
        live.lock().unwrap().calls,
        [
            format!(
                "dump {} via {}",
                net.join("utxo.dat").display(),
                net.join("utxo.dat.incomplete").display()
            ),
            format!("dump {} via {}", other.display(), incomplete(&other).display()),
        ]
    );

    // A NUL ends the name for the file system (libstdc++'s `c_str()`), `.incomplete` too, so the
    // file is `a`, renamed onto itself; the reply prints the whole name.
    let json = super::super::tests::call_method(&server, "dumptxoutset", r#"["a\u0000b"]"#)
        .unwrap()
        .write();
    let whole = format!(r#""path":"{}/a\u0000b""#, net.display());
    assert!(json.contains(&whole), "{json}");
    assert_eq!(std::fs::read(net.join("a")).unwrap(), b"snapshot");
    assert!(!net.join("a.incomplete").exists());
}

fn incomplete(path: &Path) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".incomplete");
    name.into()
}

/// An existing path (the empty name is the directory itself) and a temporary file that cannot
/// be opened are refused before the snapshot; a failed snapshot leaves its temporary file and
/// no file at the path; a failed rename is libstdc++'s `filesystem_error`.
// Core: rpc/blockchain.cpp:3120-3133, :3176-3177; streams.cpp:49; rpc/server.cpp:343-346
#[test]
fn dumptxoutset_refuses_existing_and_unopenable_paths_and_reports_failed_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let net = dir.path().to_owned();
    let (server, live) = dump_server(&net);
    std::fs::write(net.join("taken.dat"), b"x").unwrap();
    let exists = |p: &Path| {
        let message = format!(
            "{} already exists. If you are sure this is what you want, move it out of the way first",
            p.display()
        );
        reply(RPC_INVALID_PARAMETER, &message)
    };
    assert_eq!(
        method_err(&server, "dumptxoutset", r#"["taken.dat"]"#),
        exists(&net.join("taken.dat"))
    );
    assert!(!net.join("taken.dat.incomplete").exists(), "refused before the open");
    assert_eq!(method_err(&server, "dumptxoutset", r#"[""]"#), exists(&net));
    assert!(!net.join(".incomplete").exists(), "refused before the open");
    let bad = net.join("invalid").join("path");
    let message = format!("Couldn't open file {}.incomplete for writing.", bad.display());
    assert_eq!(
        method_err(&server, "dumptxoutset", &format!(r#"["{}"]"#, bad.display())),
        reply(RPC_INVALID_PARAMETER, &message)
    );
    assert!(live.lock().unwrap().calls.is_empty(), "no snapshot for a refused path");

    for (answer, want) in [
        (None, reply(RPC_INTERNAL_ERROR, "Unable to read UTXO set")),
        (Some(TxOutSetError::Interrupted), reply(RPC_CLIENT_NOT_CONNECTED, "Shutting down")),
        (
            Some(TxOutSetError::Unwritable),
            reply(RPC_MISC_ERROR, "AutoFile::write: write failed: iostream error"),
        ),
    ] {
        live.lock().unwrap().dump = answer.map(|e| (b"part".to_vec(), Err(e)));
        assert_eq!(method_err(&server, "dumptxoutset", r#"["failed.dat"]"#), want);
        assert!(!net.join("failed.dat").exists());
        let left = std::fs::read(net.join("failed.dat.incomplete")).unwrap();
        assert_eq!(
            left,
            if answer.is_some() {
                &b"part"[..]
            } else {
                b""
            }
        );
    }

    let mut l = live.lock().unwrap();
    (l.dump, l.dump_blocks_path) = (Some((Vec::new(), Ok(dump()))), true);
    drop(l);
    let path = net.join("blocked.dat");
    let message = format!(
        "filesystem error: cannot rename: Is a directory [{}] [{}]",
        incomplete(&path).display(),
        path.display()
    );
    assert_eq!(
        method_err(&server, "dumptxoutset", r#"["blocked.dat"]"#),
        reply(RPC_MISC_ERROR, &message)
    );
}
