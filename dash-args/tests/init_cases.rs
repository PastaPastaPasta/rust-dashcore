//! dashd's command-line and config-file initialization, replayed against Dash Core
//! v24.0.0-rc.5: tests/data/core-v24/init_cases.json records, for each case, what the release
//! `dashd -version ...` printed to stderr, its exit code and the files it created, changed or
//! removed. The replay runs the same steps as Core's `ParseArgs` (src/bitcoind.cpp) with this
//! crate and must produce the same stderr and files.
//!
//! On Windows the replay uses init_cases_win64.json instead, which the win64 release printed
//! under Wine: the cases that name -datadir, and Windows path and text-mode cases. `{T}` is the
//! case's directory, `{D}` its drive and `{R}` the rest.
//!
//! The default-datadir cases set `HOME`, which only names the default datadir on Linux and the
//! BSDs. Each case runs in its own directory, as the capture did.
#![cfg(any(unix, windows))]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use dash_args::init::{
    init_config, noui_error_line, parse_parameters_error_text, select_base_params,
};
use dash_args::{ArgsManager, Program, SettingsValue};

mod common;
use common::{cases_from, entries, field, strings};

/// Core's `ParseArgs` for dashd, then `-version` (which every case passes). Returns the exit
/// code and what went to stderr.
fn run_dashd(argv: &[String]) -> (i64, String) {
    let stderr = Arc::new(Mutex::new(String::new()));
    let args = ArgsManager::new();
    let sink = Arc::clone(&stderr);
    args.set_stderr_sink(Box::new(move |s| sink.lock().unwrap().push_str(s)));
    args.set_log_sink(Box::new(|_| {}));
    Program::Dashd.setup_args(&args);

    let init_error = |message: String| {
        stderr.lock().unwrap().push_str(&noui_error_line(&message));
        1
    };
    let code = (|| {
        if let Err(error) = args.parse_parameters(argv) {
            return init_error(parse_parameters_error_text(&error));
        }
        if let Err(error) = init_config(&args, &select_base_params, None) {
            return init_error(error.init_error_text());
        }
        // Error out when loose non-argument tokens are encountered on the command line.
        if let Some(token) = argv[1..].iter().find(|a| !a.starts_with('-')) {
            return init_error(format!(
                "Command line contains unexpected token '{token}', see dashd -h for a list of options."
            ));
        }
        0
    })();
    let out = stderr.lock().unwrap().clone();
    (code, out)
}

/// Files and directories under `root`: relative path to contents (`None` for a directory).
fn tree(root: &Path) -> BTreeMap<String, Option<String>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Option<String>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            // The fixture's paths use `/`.
            let rel = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            if path.is_dir() {
                out.insert(rel, None);
                walk(root, &path, out);
            } else {
                out.insert(rel, Some(std::fs::read_to_string(&path).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// Whether a case depends on the default datadir (no -datadir on the command line).
fn uses_home(argv: &[String]) -> bool {
    !argv.iter().any(|a| a.starts_with("-datadir") || a.starts_with("--datadir"))
}

#[test]
fn init_matches_core() {
    let fixture = if cfg!(windows) {
        "init_cases_win64.json"
    } else {
        "init_cases.json"
    };
    let cases = cases_from(fixture);
    assert!(cases.len() >= 70, "{} cases", cases.len());

    let mut ran = 0;
    for case in &cases {
        let name = field(case, "name").get_str().unwrap();
        let argv = strings(field(case, "argv"));
        if uses_home(&argv) && cfg!(target_os = "macos") {
            continue; // macOS keeps the default datadir under ~/Library.
        }

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let t = root.to_str().unwrap();
        let fill = |s: &str| s.replace("{T}", t).replace("{D}", &t[..2]).replace("{R}", &t[2..]);
        std::fs::create_dir(root.join("d")).unwrap();
        std::fs::create_dir(root.join("home")).unwrap();
        for (rel, contents) in entries(field(case, "files")) {
            let path = root.join(rel.trim_end_matches('/'));
            if rel.ends_with('/') {
                std::fs::create_dir_all(&path).unwrap();
            } else if let SettingsValue::Obj(_) = contents {
                let target = field(contents, "hardlink").get_str().unwrap();
                std::fs::hard_link(root.join(target), &path).unwrap();
            } else {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, fill(contents.get_str().unwrap())).unwrap();
            }
        }
        // SAFETY: this test binary has a single test, so no other thread reads the
        // environment concurrently.
        unsafe { std::env::set_var("HOME", root.join("home")) };
        // The capture ran dashd from the case's root.
        std::env::set_current_dir(root).unwrap();

        let before = tree(root);
        let mut full_argv = vec!["dashd".to_string()];
        full_argv.extend(argv.iter().map(|a| fill(a)));
        let (code, stderr) = run_dashd(&full_argv);
        let after = tree(root);

        let expected_exit = field(case, "exit").get_int64().unwrap();
        let expected_stderr = field(case, "stderr").get_str().unwrap();
        assert_eq!(
            (code, stderr.replace(t, "{T}").as_str()),
            (expected_exit, expected_stderr),
            "{name}"
        );

        let expect_map = |key: &str| -> BTreeMap<String, Option<String>> {
            entries(field(case, key))
                .iter()
                .map(|(k, v)| (k.clone(), v.get_str().ok().map(str::to_string)))
                .collect()
        };
        let created: BTreeMap<_, _> = after
            .iter()
            .filter(|(k, _)| !before.contains_key(*k))
            .map(|(k, v)| (k.clone(), v.as_ref().map(|s| s.replace(t, "{T}"))))
            .collect();
        assert_eq!(created, expect_map("created"), "{name}: created files");
        let changed: BTreeMap<_, _> = after
            .iter()
            .filter(|(k, v)| v.is_some() && before.get(*k).is_some_and(|b| b != *v))
            .map(|(k, v)| (k.clone(), v.as_ref().map(|s| s.replace(t, "{T}"))))
            .collect();
        assert_eq!(changed, expect_map("changed"), "{name}: changed files");
        let removed: Vec<String> =
            before.keys().filter(|k| !after.contains_key(*k)).cloned().collect();
        assert_eq!(removed, strings(field(case, "removed")), "{name}: removed files");
        // Leave the directory so it can be removed (Windows keeps a current directory).
        std::env::set_current_dir(env!("CARGO_MANIFEST_DIR")).unwrap();
        ran += 1;
    }
    eprintln!("{ran} init cases match Core");
}
