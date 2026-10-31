//! Core v24.0.0-rc.3 `src/test/logging_tests.cpp`, ported (the expected lines are Core's), and
//! the DEC-200 routing of `tracing` events.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use tempfile::TempDir;

use super::*;
use crate::InitFailure;

/// Core's `LogSetup` on a started `BasicTestingSetup` logger: lines go to a fresh file, without
/// timestamps, threads or source locations, at the `Debug` level, with every category on but
/// `libevent` and `leveldb` (`-debug -debugexclude=libevent -debugexclude=leveldb`).
fn log_setup() -> (TempDir, Arc<Logger>, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let logger = Arc::new(Logger::new());
    assert!(logger.start_logging());
    let path = dir.path().join("tmp_debug.log");
    logger.set_options(LogOptions {
        print_to_file: true,
        file_path: path.clone(),
        log_timestamps: false,
        ..LogOptions::default()
    });
    logger.request_reopen();
    logger.set_log_level(Level::Debug);
    logger.enable_category(LogFlags::ALL);
    logger.disable_category(LogFlags::LIBEVENT);
    logger.disable_category(LogFlags::LEVELDB);
    (dir, logger, path)
}

fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path).unwrap_or_default().lines().map(str::to_owned).collect()
}

fn with_tracing(logger: &Arc<Logger>, f: impl FnOnce()) {
    // BL-479: never the only registered dispatcher, or a thread without one that registers a
    // callsite first caches `Interest::never` for it (dashd-store's `keep_callsites_registered`).
    static KEEP: OnceLock<tracing::Dispatch> = OnceLock::new();
    KEEP.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
    tracing::subscriber::with_default(CoreSubscriber::new(Arc::clone(logger)), f);
}

/// Core's `LogPrint(category, …)`: logged when the category is on at `Debug`.
fn log_print(logger: &Logger, category: LogFlags, s: &str) {
    if logger.will_log_category_level(category, Level::Debug) {
        logger.log_print_str(s, "", "", 0, category, Level::Debug);
    }
}

#[test]
fn logging_log_printf_() {
    let (_dir, logger, path) = log_setup();
    let mut opts = logger.options();
    opts.log_sourcelocations = true;
    logger.set_options(opts);
    let calls = [
        ("fn1", "src1", 1, LogFlags::NET, Level::Debug, "foo1: bar1\n"),
        ("fn2", "src2", 2, LogFlags::NET, Level::Info, "foo2: bar2\n"),
        ("fn3", "src3", 3, LogFlags::ALL, Level::Debug, "foo3: bar3\n"),
        ("fn4", "src4", 4, LogFlags::ALL, Level::Info, "foo4: bar4\n"),
        ("fn5", "src5", 5, LogFlags::NONE, Level::Debug, "foo5: bar5\n"),
        ("fn6", "src6", 6, LogFlags::NONE, Level::Info, "foo6: bar6\n"),
    ];
    for (function, file, line, category, level, s) in calls {
        logger.log_print_str(s, function, file, line, category, level);
    }
    assert_eq!(
        lines(&path),
        [
            "[src1:1] [fn1] [net] foo1: bar1",
            "[src2:2] [fn2] [net:info] foo2: bar2",
            "[src3:3] [fn3] [debug] foo3: bar3",
            "[src4:4] [fn4] foo4: bar4",
            "[src5:5] [fn5] [debug] foo5: bar5",
            "[src6:6] [fn6] foo6: bar6",
        ]
    );
}

#[test]
fn logging_log_print_macros_deprecated() {
    let (_dir, logger, path) = log_setup();
    with_tracing(&logger, || {
        tracing::info!("foo5: {}", "bar5"); // LogPrintf
        tracing::debug!(target: "net", "foo6: {}", "bar6"); // LogPrint(NET)
        tracing::trace!(target: "net", "foo4: {}", "bar4"); // not logged
        tracing::debug!(target: "net", "foo7: {}", "bar7");
        tracing::info!(target: "net", "foo8: {}", "bar8");
        tracing::warn!(target: "net", "foo9: {}", "bar9");
        tracing::error!(target: "net", "foo10: {}", "bar10");
        tracing::info!(target: "validation", "foo11: {}", "bar11"); // LogPrintfCategory
    });
    assert_eq!(
        lines(&path),
        [
            "foo5: bar5",
            "[net] foo6: bar6",
            "[net] foo7: bar7",
            "[net:info] foo8: bar8",
            "[net:warning] foo9: bar9",
            "[net:error] foo10: bar10",
            "[validation:info] foo11: bar11",
        ]
    );
}

#[test]
fn logging_log_print_macros() {
    let (_dir, logger, path) = log_setup();
    with_tracing(&logger, || {
        tracing::trace!(target: "net", "foo6: {}", "bar6"); // LogTrace: not logged
        tracing::debug!(target: "net", "foo7: {}", "bar7"); // LogDebug
        tracing::info!("foo8: {}", "bar8"); // LogInfo
        tracing::warn!("foo9: {}", "bar9"); // LogWarning
        tracing::error!("foo10: {}", "bar10"); // LogError
    });
    assert_eq!(
        lines(&path),
        ["[net] foo7: bar7", "foo8: bar8", "[warning] foo9: bar9", "[error] foo10: bar10"]
    );
}

#[test]
fn logging_log_print_macros_category_name() {
    let (_dir, logger, path) = log_setup();
    logger.enable_category(LogFlags::ALL);
    let mut expected = Vec::new();
    for name in logger.log_categories_string(false).split(',') {
        let name = name.trim();
        let category = get_log_category(name).unwrap();
        // A `tracing` target naming the category is logged under it (DEC-200).
        assert_eq!(category_of_target(name), Some(category));
        log_print(&logger, category, "foo: bar\n");
        expected.push(format!("[{name}] foo: bar"));
    }
    assert_eq!(lines(&path), expected);
}

#[test]
fn logging_severity_levels() {
    let (_dir, logger, path) = log_setup();
    logger.enable_category(LogFlags::ALL);
    logger.set_log_level(Level::Debug);
    assert!(logger.set_category_log_level_str("net", "info"));
    with_tracing(&logger, || {
        // The global level.
        tracing::info!(target: "http", "foo1: {}", "bar1");
        tracing::trace!(target: "mempool", "foo2: {}. This log level is lower than the global one.", "bar2");
        tracing::warn!(target: "validation", "foo3: {}", "bar3");
        tracing::error!(target: "rpc", "foo4: {}", "bar4");
        // The category's level.
        tracing::warn!(target: "net", "foo5: {}", "bar5");
        tracing::debug!(target: "net", "foo6: {}. Below the category's level, which wins.", "bar6");
        tracing::error!(target: "net", "foo7: {}", "bar7");
    });
    assert_eq!(
        lines(&path),
        [
            "[http:info] foo1: bar1",
            "[validation:warning] foo3: bar3",
            "[rpc:error] foo4: bar4",
            "[net:warning] foo5: bar5",
            "[net:error] foo7: bar7",
        ]
    );
}

fn dashd_args(argv: &[&str]) -> dash_args::ArgsManager {
    let args = dash_args::ArgsManager::new();
    dash_args::Program::Dashd.setup_args(&args);
    let mut full = vec!["dashd"];
    full.extend_from_slice(argv);
    args.parse_parameters(&full).unwrap();
    args
}

#[test]
fn logging_conf() {
    // The global level.
    let logger = Logger::new();
    set_logging_level(&logger, &dashd_args(&["-loglevel=debug"])).unwrap();
    assert_eq!(logger.log_level(), Level::Debug);

    // A category's level.
    let logger = Logger::new();
    set_logging_level(&logger, &dashd_args(&["-loglevel=net:trace"])).unwrap();
    assert_eq!(logger.log_level(), Level::DEFAULT);
    assert_eq!(logger.category_levels().get(&LogFlags::NET), Some(&Level::Trace));

    // Both.
    let logger = Logger::new();
    let args = dashd_args(&["-loglevel=debug", "-loglevel=net:trace", "-loglevel=http:info"]);
    set_logging_level(&logger, &args).unwrap();
    assert_eq!(logger.log_level(), Level::Debug);
    let levels = logger.category_levels();
    assert_eq!(levels.len(), 2);
    assert_eq!(levels.get(&LogFlags::NET), Some(&Level::Trace));
    assert_eq!(levels.get(&LogFlags::HTTP), Some(&Level::Info));
}

#[test]
fn bad_logging_options_give_core_s_init_errors() {
    let level = |arg: &str| set_logging_level(&Logger::new(), &dashd_args(&[arg])).unwrap_err();
    let categories =
        |arg: &str| set_logging_categories(&Logger::new(), &dashd_args(&[arg])).unwrap_err();
    assert_eq!(
        level("-loglevel=warning"),
        InitFailure::Error(
            "Unsupported global logging level -loglevel=warning. Valid values: info, debug, trace."
                .into()
        )
    );
    // `find_first_of(':', 3)`: a colon before the fourth byte is part of a global level.
    assert_eq!(
        level("-loglevel=ne:x"),
        InitFailure::Error(
            "Unsupported global logging level -loglevel=ne:x. Valid values: info, debug, trace."
                .into()
        )
    );
    let InitFailure::Error(text) = level("-loglevel=net:error") else {
        panic!()
    };
    let categories_list = Logger::new().log_categories_string(false);
    assert_eq!(
        text,
        format!(
            "Unsupported category-specific logging level -loglevel=net:error. Expected \
             -loglevel=<category>:<loglevel>. Valid categories: {categories_list}. Valid \
             loglevels: info, debug, trace."
        )
    );
    assert_eq!(
        categories("-debug=nosuch"),
        InitFailure::Error("Unsupported logging category -debug=nosuch.".into())
    );
    assert_eq!(
        categories("-debugexclude=lock"),
        InitFailure::Error("Unsupported logging category -debugexclude=lock.".into())
    );
}

#[test]
fn debug_categories_follow_debug_and_debugexclude() {
    let logger = Logger::new();
    set_logging_categories(&logger, &dashd_args(&["-debug=net", "-debug=llmq"])).unwrap();
    assert_eq!(logger.category_mask(), LogFlags(LogFlags::NET.0 | LogFlags::LLMQ.0));
    assert!(!logger.default_shrink_debug_file());

    let logger = Logger::new();
    let args = dashd_args(&["-debug", "-debugexclude=net"]);
    set_logging_categories(&logger, &args).unwrap();
    assert_eq!(logger.category_mask(), LogFlags(!LogFlags::NET.0));

    // `-debug=0` or `-debug=none` among the values turns every `-debug` off; the bad name then
    // goes unchecked, as in Core.
    let logger = Logger::new();
    set_logging_categories(&logger, &dashd_args(&["-debug=nosuch", "-debug=none"])).unwrap();
    assert_eq!(logger.category_mask(), LogFlags::NONE);
    assert!(logger.default_shrink_debug_file());
}

#[test]
fn other_targets_debug_only_under_every_category() {
    let (_dir, logger, path) = log_setup();
    // `log_setup` excludes two categories: not `-debug=1`.
    with_tracing(&logger, || tracing::debug!("hidden while a category is off"));
    logger.enable_category(LogFlags::ALL);
    with_tracing(&logger, || {
        tracing::debug!("shown under -debug=1");
        tracing::trace!("below the global level");
    });
    logger.set_log_level(Level::Trace);
    with_tracing(&logger, || tracing::trace!("at the global level"));
    assert_eq!(lines(&path), ["[debug] shown under -debug=1", "[trace] at the global level"]);
}

#[test]
fn fields_follow_the_message_and_control_bytes_are_escaped() {
    let (_dir, logger, path) = log_setup();
    with_tracing(&logger, || {
        tracing::info!(height = 7, hash = "00ab", "UpdateTip: new best");
        tracing::info!("bell\x07 tab\t del\x7f é");
        tracing::info!("trailing newline is not doubled\n");
    });
    assert_eq!(
        lines(&path),
        [
            "UpdateTip: new best height=7 hash=00ab",
            "bell\\x07 tab\\x09 del\\x7f é",
            "trailing newline is not doubled",
        ]
    );
}

#[test]
fn a_line_s_prefix_goes_only_on_its_first_piece() {
    let (_dir, logger, path) = log_setup();
    let mut opts = logger.options();
    opts.always_print_category_level = true;
    logger.set_options(opts);
    logger.log_print_str("part one, ", "", "", 0, LogFlags::NET, Level::Debug);
    logger.log_print_str("part two\n", "", "", 0, LogFlags::NET, Level::Debug);
    logger.log_printf("plain\n");
    assert_eq!(lines(&path), ["[net:debug] part one, part two", "[all:info] plain"]);
}

#[test]
fn lines_buffer_until_start_and_the_file_gets_five_newlines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("debug.log");
    std::fs::write(&path, "previous run\n").unwrap();
    let logger = Logger::new();
    logger.set_options(LogOptions {
        print_to_file: true,
        file_path: path.clone(),
        log_timestamps: false,
        log_threadnames: true,
        ..LogOptions::default()
    });
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    logger.push_back_callback(move |s| sink.lock().unwrap().push(s.to_owned()));
    std::thread::Builder::new()
        .name("unnamed-by-core".into())
        .spawn({
            let logger = Arc::new(logger);
            let path = path.clone();
            let seen = Arc::clone(&seen);
            move || {
                set_thread_name("init");
                logger.log_printf("before\n");
                assert!(std::fs::read_to_string(&path).unwrap() == "previous run\n");
                assert!(seen.lock().unwrap().is_empty());
                assert!(logger.start_logging());
                set_thread_name("");
                logger.log_printf("after\n");
            }
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "previous run\n\n\n\n\n\n[      init] before\n[unnamed-by-core] after\n"
    );
    assert_eq!(*seen.lock().unwrap(), ["[      init] before\n", "[unnamed-by-core] after\n"]);
}

/// A thread named only for the OS, as the script-check pool's are (`d-scriptch.N`), shows
/// Core's internal name, without `ThreadRename`'s `d-`. Any other `d-` name shows whole.
#[test]
fn an_os_named_thread_shows_core_s_internal_name() {
    let (_dir, logger, path) = log_setup();
    let mut opts = logger.options();
    opts.log_threadnames = true;
    logger.set_options(opts);
    let names = ["d-scriptch.3", "d-other"].map(|os| {
        let logger = Arc::clone(&logger);
        std::thread::Builder::new()
            .name(os.into())
            .spawn(move || {
                logger.log_printf("checked\n");
                thread_name()
            })
            .unwrap()
            .join()
            .unwrap()
    });
    assert_eq!(names, ["scriptch.3", "d-other"]);
    assert_eq!(lines(&path), ["[scriptch.3] checked", "[   d-other] checked"]);
}

#[test]
fn the_mock_time_follows_the_stamp() {
    let (_dir, logger, path) = log_setup();
    let clock = Arc::new(dash_clock::MockClock::new());
    logger.set_clock(Arc::clone(&clock));
    let mut opts = logger.options();
    opts.log_timestamps = true;
    logger.set_options(opts);
    logger.log_printf("real\n");
    clock.set_mocktime(1_600_000_000);
    logger.log_printf("mocked\n");
    let lines = lines(&path);
    assert!(lines[0].ends_with("Z real"), "{}", lines[0]);
    assert!(lines[1].ends_with("Z (mocktime: 1600000000) mocked"), "{}", lines[1]);
}

#[test]
fn a_reopen_request_moves_to_the_new_file() {
    let (dir, logger, path) = log_setup();
    logger.log_printf("one\n");
    let rotated = dir.path().join("rotated.log");
    std::fs::rename(&path, &rotated).unwrap();
    logger.log_printf("two\n");
    logger.request_reopen();
    logger.log_printf("three\n");
    assert_eq!(lines(&rotated), ["one", "two"]);
    assert_eq!(lines(&path), ["three"]);
}

#[test]
fn shrinking_keeps_the_last_ten_megabytes_of_a_file_over_eleven() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("debug.log");
    let logger = Logger::new();
    logger.set_options(LogOptions {
        file_path: path.clone(),
        ..LogOptions::default()
    });

    let small = vec![b'a'; 11_000_000];
    std::fs::write(&path, &small).unwrap();
    logger.shrink_debug_file();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 11_000_000);

    let mut big = vec![b'a'; 1_000_001];
    big.extend(std::iter::repeat_n(b'b', 10_000_000));
    std::fs::write(&path, &big).unwrap();
    logger.shrink_debug_file();
    let kept = std::fs::read(&path).unwrap();
    assert_eq!(kept.len(), 10_000_000);
    assert!(kept.iter().all(|&b| b == b'b'));
}

#[test]
fn the_category_lists_are_core_s() {
    let logger = Logger::new();
    logger.enable_category(LogFlags::NET);
    logger.enable_category(LogFlags::CHAINLOCKS);
    assert_eq!(logger.log_categories_string(true), "chainlocks, net");
    assert_eq!(Logger::log_levels_string(), "info, debug, trace");
    let list = logger.log_categories_list(false);
    assert!(list.contains(&("net", true)) && list.contains(&("mempool", false)));
}

#[test]
fn without_debug_only_info_and_above_log() {
    let (_dir, logger, path) = log_setup();
    logger.disable_category(LogFlags::ALL);
    logger.set_log_level(Level::Trace);
    with_tracing(&logger, || {
        tracing::debug!(target: "net", "net debug");
        tracing::trace!("other trace");
        tracing::info!(target: "net", "net info");
        tracing::info!("other info");
    });
    assert_eq!(lines(&path), ["[net:info] net info", "other info"]);
}
