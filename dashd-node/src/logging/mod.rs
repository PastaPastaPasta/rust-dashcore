//! Core-format logging: `debug.log` and the console with Core's line format, categories and
//! levels (`logging.{h,cpp}`, `init/common.cpp`, v24.0.0-rc.3).
//!
//! # How crates log (DEC-200)
//!
//! Every crate logs through `tracing`. [`CoreSubscriber`] renders each event as Core's
//! `LogPrintStr` would:
//!
//! - **The target is the Core category name** for Core's `LogPrint(cat)`/`LogPrintLevel(cat,
//!   level)` lines: `tracing::debug!(target: "validation", …)` is `LogPrint(BCLog::VALIDATION,
//!   …)`, and `tracing::info!(target: "net", …)` is `LogPrintLevel(BCLog::NET, Level::Info, …)`.
//!   The names are those of Core's line prefixes (`net`, `mempool`, `validation`, `chainlocks`,
//!   `llmq-dkg`, …, and `net|netconn`). Such a line is logged when Core would log it
//!   (`WillLogCategoryLevel`), with Core's `[category]` or `[category:level]` prefix.
//! - **Any other target at INFO and above is `LogPrintf`**: always logged, with no prefix at
//!   INFO and `[warning] `/`[error] ` above it (Core's `LogInfo`/`LogWarning`/`LogError`).
//!   Lines that Core prints with `LogPrintf` use INFO and keep Core's wording.
//! - **DEBUG and TRACE with any other target** are shown only while every category is enabled
//!   (`-debug=1`, `-debug=all`, or the `logging` RPC's `all`), at or above the global
//!   `-loglevel`, with Core's `[debug] `/`[trace] ` prefix.
//!
//! The message is the event's `message` field; other fields follow it as ` name=value`. A line
//! ends in a newline. Messages are never formatted when the line is not logged.
//!
//! Core prints the logging function's name with `-logsourcelocations`; `tracing` has none, so
//! the module path stands in for it.

mod categories;
mod options;
mod subscriber;
mod thread_name;
mod time;

use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use dash_clock::MockClock;

pub use categories::{Level, LogFlags, category_of_target, get_log_category, log_category_to_str};
pub use options::{
    DEFAULT_DEBUGLOGFILE, log_package_version, route_args_log, set_logging_categories,
    set_logging_level, set_logging_options, start_logging,
};
pub use subscriber::CoreSubscriber;
pub use thread_name::{set_thread_name, thread_name};
pub use time::format_iso8601_date_time;

/// The output switches Core sets from the arguments before logging starts (`SetLoggingOptions`).
#[derive(Clone, Debug)]
pub struct LogOptions {
    pub print_to_console: bool,
    pub print_to_file: bool,
    pub file_path: PathBuf,
    pub log_timestamps: bool,
    pub log_time_micros: bool,
    pub log_threadnames: bool,
    pub log_sourcelocations: bool,
    pub always_print_category_level: bool,
}

impl Default for LogOptions {
    // Core: logging.h:23-28, :138-147
    fn default() -> Self {
        LogOptions {
            print_to_console: false,
            print_to_file: false,
            file_path: PathBuf::new(),
            log_timestamps: true,
            log_time_micros: false,
            log_threadnames: false,
            log_sourcelocations: false,
            always_print_category_level: false,
        }
    }
}

/// A print callback's handle, for [`Logger::delete_callback`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallbackId(u64);

type Callback = Box<dyn Fn(&str) + Send + Sync>;

/// What Core guards with `m_cs`.
struct Inner {
    opts: LogOptions,
    fileout: Option<File>,
    msgs_before_open: VecDeque<String>,
    buffering: bool,
    category_log_levels: HashMap<LogFlags, Level>,
    callbacks: Vec<(CallbackId, Callback)>,
    next_callback: u64,
    /// The node clock: its mock time is stamped on each line (`GetMockTime()`).
    clock: Option<Arc<MockClock>>,
}

/// Core's `BCLog::Logger`. One per node; [`CoreSubscriber`] puts `tracing` in front of it.
pub struct Logger {
    inner: Mutex<Inner>,
    started_new_line: AtomicBool,
    log_level: AtomicU8,
    categories: AtomicU64,
    reopen_file: AtomicBool,
    log_ips: AtomicBool,
}

impl Default for Logger {
    fn default() -> Self {
        Logger::new()
    }
}

impl Logger {
    /// A logger that buffers until [`Logger::start_logging`], with no output selected.
    pub fn new() -> Logger {
        Logger {
            inner: Mutex::new(Inner {
                opts: LogOptions::default(),
                fileout: None,
                msgs_before_open: VecDeque::new(),
                buffering: true,
                category_log_levels: HashMap::new(),
                callbacks: Vec::new(),
                next_callback: 0,
                clock: None,
            }),
            started_new_line: AtomicBool::new(true),
            log_level: AtomicU8::new(Level::DEFAULT as u8),
            categories: AtomicU64::new(0),
            reopen_file: AtomicBool::new(false),
            log_ips: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn options(&self) -> LogOptions {
        self.lock().opts.clone()
    }

    pub fn set_options(&self, opts: LogOptions) {
        self.lock().opts = opts;
    }

    /// The node clock whose mock time each line shows.
    pub fn set_clock(&self, clock: Arc<MockClock>) {
        self.lock().clock = Some(clock);
    }

    /// Core's `fLogIPs` (`-logips`).
    pub fn log_ips(&self) -> bool {
        self.log_ips.load(Ordering::Relaxed)
    }

    pub fn set_log_ips(&self, on: bool) {
        self.log_ips.store(on, Ordering::Relaxed);
    }

    /// Core's `Enabled`: whether a line goes anywhere.
    pub fn enabled(&self) -> bool {
        let g = self.lock();
        g.buffering || g.opts.print_to_console || g.opts.print_to_file || !g.callbacks.is_empty()
    }

    /// Core's `PushBackCallback`: `f` gets every line, prefixed, after logging starts.
    pub fn push_back_callback(&self, f: impl Fn(&str) + Send + Sync + 'static) -> CallbackId {
        let mut g = self.lock();
        let id = CallbackId(g.next_callback);
        g.next_callback += 1;
        g.callbacks.push((id, Box::new(f)));
        id
    }

    /// Core's `DeleteCallback`.
    pub fn delete_callback(&self, id: CallbackId) {
        self.lock().callbacks.retain(|(i, _)| *i != id);
    }

    /// Core's `StartLogging`: opens the file (`"a"`, unbuffered), writes Core's five newlines,
    /// then flushes the lines buffered so far. `false` if the file cannot be opened; logging then
    /// stays buffered.
    // Core: logging.cpp:47-84
    pub fn start_logging(&self) -> bool {
        let mut g = self.lock();
        assert!(g.buffering && g.fileout.is_none(), "StartLogging called twice");
        if g.opts.print_to_file {
            assert!(!g.opts.file_path.as_os_str().is_empty());
            let Ok(mut file) = open_append(&g.opts.file_path) else {
                return false;
            };
            let _ = file.write_all(b"\n\n\n\n\n");
            g.fileout = Some(file);
        }
        g.buffering = false;
        let msgs = std::mem::take(&mut g.msgs_before_open);
        for s in &msgs {
            if let Some(f) = g.fileout.as_mut() {
                let _ = f.write_all(s.as_bytes());
            }
            if g.opts.print_to_console {
                let _ = std::io::stdout().lock().write_all(s.as_bytes());
            }
            for (_, cb) in &g.callbacks {
                cb(s);
            }
        }
        if g.opts.print_to_console {
            let _ = std::io::stdout().lock().flush();
        }
        true
    }

    /// Core's `DisconnectTestLogger`: back to buffering, the file closed, no callbacks.
    pub fn disconnect_test_logger(&self) {
        let mut g = self.lock();
        g.buffering = true;
        g.fileout = None;
        g.callbacks.clear();
    }

    /// Asks the next line to reopen the file (Core's SIGHUP handler sets `m_reopen_file`).
    pub fn request_reopen(&self) {
        self.reopen_file.store(true, Ordering::Relaxed);
    }

    /// Core's `ShrinkDebugFile`: keeps the last 10 MB of a file over 11 MB.
    // Core: logging.cpp:514-552
    pub fn shrink_debug_file(&self) {
        const RECENT_DEBUG_HISTORY_SIZE: u64 = 10 * 1_000_000;
        let path = self.lock().opts.file_path.clone();
        assert!(!path.as_os_str().is_empty());
        let Ok(mut file) = File::open(&path) else {
            return;
        };
        // Special files (device nodes) may have no size.
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if size <= 11 * (RECENT_DEBUG_HISTORY_SIZE / 10) {
            return;
        }
        if file.seek(SeekFrom::End(-(RECENT_DEBUG_HISTORY_SIZE as i64))).is_err() {
            drop(file);
            self.log_printf("Failed to shrink debug log file: fseek(...) failed\n");
            return;
        }
        let mut recent = Vec::with_capacity(RECENT_DEBUG_HISTORY_SIZE as usize);
        let _ = file.take(RECENT_DEBUG_HISTORY_SIZE).read_to_end(&mut recent);
        if let Ok(mut out) = File::create(&path) {
            let _ = out.write_all(&recent);
        }
    }

    /// Core's `LogPrintf`: an unconditional INFO line.
    pub fn log_printf(&self, s: &str) {
        self.log_print_str(s, "", "", 0, LogFlags::ALL, Level::Info);
    }

    /// Core's `LogPrintStr`: prefixes, escapes, stamps and writes `s`. The prefix and stamp go
    /// only on a line's first piece (`m_started_new_line`).
    // Core: logging.cpp:460-512
    pub fn log_print_str(
        &self,
        s: &str,
        function: &str,
        source_file: &str,
        source_line: u32,
        category: LogFlags,
        level: Level,
    ) {
        let mut g = self.lock();
        let new_line = self.started_new_line.load(Ordering::Relaxed);
        let mut out = String::with_capacity(s.len() + 64);
        if new_line {
            if g.opts.log_timestamps {
                time::push_timestamp(
                    &mut out,
                    g.opts.log_time_micros,
                    g.clock.as_ref().map_or(0, |c| c.mocktime()),
                );
            }
            if g.opts.log_threadnames {
                thread_name::with(|name| {
                    out.push_str(&format!(
                        "[{:>10}] ",
                        if name.is_empty() {
                            "unknown"
                        } else {
                            name
                        }
                    ))
                });
            }
            if g.opts.log_sourcelocations {
                let file = source_file.strip_prefix("./").unwrap_or(source_file);
                out.push_str(&format!("[{file}:{source_line}] [{function}] "));
            }
            push_prefix(&mut out, category, level, g.opts.always_print_category_level);
        }
        escape_into(&mut out, s);
        self.started_new_line.store(s.ends_with('\n'), Ordering::Relaxed);

        if g.buffering {
            g.msgs_before_open.push_back(out);
            return;
        }
        if g.opts.print_to_console {
            let mut stdout = std::io::stdout().lock();
            let _ = stdout.write_all(out.as_bytes());
            let _ = stdout.flush();
        }
        for (_, cb) in &g.callbacks {
            cb(&out);
        }
        if g.opts.print_to_file {
            if self.reopen_file.swap(false, Ordering::Relaxed)
                && let Ok(f) = open_append(&g.opts.file_path)
            {
                g.fileout = Some(f);
            }
            if let Some(f) = g.fileout.as_mut() {
                let _ = f.write_all(out.as_bytes());
            }
        }
    }

    /// Core's `GetCategoryMask`.
    pub fn category_mask(&self) -> LogFlags {
        LogFlags(self.categories.load(Ordering::Relaxed))
    }

    pub fn enable_category(&self, flag: LogFlags) {
        self.categories.fetch_or(flag.0, Ordering::Relaxed);
    }

    pub fn disable_category(&self, flag: LogFlags) {
        self.categories.fetch_and(!flag.0, Ordering::Relaxed);
    }

    /// Core's `EnableCategory(str)`: `false` for an unknown name.
    pub fn enable_category_str(&self, name: &str) -> bool {
        get_log_category(name).map(|f| self.enable_category(f)).is_some()
    }

    /// Core's `DisableCategory(str)`: `false` for an unknown name.
    pub fn disable_category_str(&self, name: &str) -> bool {
        get_log_category(name).map(|f| self.disable_category(f)).is_some()
    }

    /// Core's `WillLogCategory`.
    pub fn will_log_category(&self, category: LogFlags) -> bool {
        self.categories.load(Ordering::Relaxed) & category.0 != 0
    }

    /// Core's `WillLogCategoryLevel`: INFO and above always; below, the category must be on and
    /// the level at or above its own level, else the global one.
    // Core: logging.cpp:124-137
    pub fn will_log_category_level(&self, category: LogFlags, level: Level) -> bool {
        if level >= Level::Info {
            return true;
        }
        if !self.will_log_category(category) {
            return false;
        }
        let g = self.lock();
        level >= g.category_log_levels.get(&category).copied().unwrap_or_else(|| self.log_level())
    }

    /// Whether every category is on (`-debug=1`/`all`): DEC-200's gate for DEBUG and TRACE lines
    /// whose target is not a category.
    pub fn all_categories_enabled(&self) -> bool {
        self.category_mask() == LogFlags::ALL
    }

    /// Core's `LogLevel()`.
    pub fn log_level(&self) -> Level {
        Level::from_u8(self.log_level.load(Ordering::Relaxed))
    }

    pub fn set_log_level(&self, level: Level) {
        self.log_level.store(level as u8, Ordering::Relaxed);
    }

    /// Core's `SetLogLevel(str)`: `false` for an unknown level or one above INFO.
    pub fn set_log_level_str(&self, level: &str) -> bool {
        Level::parse_user(level).map(|l| self.set_log_level(l)).is_some()
    }

    /// Core's `SetCategoryLogLevel(category, level)`.
    pub fn set_category_log_level_str(&self, category: &str, level: &str) -> bool {
        let (Some(flag), Some(level)) = (get_log_category(category), Level::parse_user(level))
        else {
            return false;
        };
        self.lock().category_log_levels.insert(flag, level);
        true
    }

    /// Core's `SetCategoryLogLevel(levels)`: replaces every category's level.
    pub fn set_category_log_levels(&self, levels: HashMap<LogFlags, Level>) {
        self.lock().category_log_levels = levels;
    }

    /// Core's `CategoryLevels`.
    pub fn category_levels(&self) -> HashMap<LogFlags, Level> {
        self.lock().category_log_levels.clone()
    }

    /// Core's `LogCategoriesList`: `(name, active)` in Core's sorted order.
    pub fn log_categories_list(&self, enabled_only: bool) -> Vec<(&'static str, bool)> {
        categories::listed_categories()
            .into_iter()
            .map(|(flag, name)| (name, self.will_log_category(flag)))
            .filter(|(_, active)| !enabled_only || *active)
            .collect()
    }

    /// Core's `LogCategoriesString`.
    pub fn log_categories_string(&self, enabled_only: bool) -> String {
        let names: Vec<_> =
            self.log_categories_list(enabled_only).into_iter().map(|(n, _)| n).collect();
        names.join(", ")
    }

    /// Core's `LogLevelsString`.
    pub fn log_levels_string() -> String {
        let names: Vec<_> = Level::USER_LEVELS.iter().map(|l| l.as_str()).collect();
        names.join(", ")
    }

    /// Core's `DefaultShrinkDebugFile`: shrink unless a category is on.
    pub fn default_shrink_debug_file(&self) -> bool {
        self.category_mask() == LogFlags::NONE
    }
}

/// `fopen(path, "a")`.
fn open_append(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().append(true).create(true).open(path)
}

/// Core's `GetLogPrefix`.
// Core: logging.cpp:434-458
fn push_prefix(out: &mut String, category: LogFlags, level: Level, always: bool) {
    let category = if category == LogFlags::NONE {
        LogFlags::ALL
    } else {
        category
    };
    let has_category = always || category != LogFlags::ALL;
    if !has_category && level == Level::Info {
        return;
    }
    out.push('[');
    if has_category {
        out.push_str(log_category_to_str(category).unwrap_or(""));
    }
    if always || !has_category || level != Level::Debug {
        if has_category {
            out.push(':');
        }
        out.push_str(level.as_str());
    }
    out.push_str("] ");
}

/// Core's `LogEscapeMessage`: control characters other than newline, and DEL, as `\xNN`.
// Core: logging.cpp:419-432
fn escape_into(out: &mut String, s: &str) {
    for ch in s.chars() {
        if (ch < ' ' && ch != '\n') || ch == '\x7f' {
            out.push_str(&format!("\\x{:02x}", ch as u32));
        } else {
            out.push(ch);
        }
    }
}

#[cfg(test)]
mod tests;
