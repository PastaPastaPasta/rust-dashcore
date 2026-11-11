//! A recording VFS over SQLite's `unix` VFS. While recording, it logs every write, truncate, sync
//! and delete of the files SQLite opens through it, with offsets and bytes. A sync (a delete's
//! directory sync too) is logged and not performed, recording or not: the image model decides what
//! it made durable, and the disk is not waited on.
//!
//! It copies the `unix` VFS and replaces `xOpen` (to wrap each file) and `xDelete` (to log it);
//! the copied methods see the same `pAppData`, so they behave as `unix`'s own. The `-shm` file is
//! opened by `unix` itself, not through `xOpen`, so it is never logged: an image never has one.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::{Mutex, MutexGuard, Once, OnceLock, PoisonError};

use rusqlite::ffi;

/// The name to open a database through ([`dash_kv_sqlite::SqliteConfig::vfs`]).
pub const NAME: &str = "dashkv-recording";

/// One logged operation. `file` is the file's name in its directory.
#[derive(Debug, Clone)]
pub enum Event {
    Write {
        file: String,
        offset: u64,
        data: Vec<u8>,
    },
    Truncate {
        file: String,
        size: u64,
    },
    Sync {
        file: String,
    },
    Delete {
        file: String,
    },
    /// The test's marker: the call that made commit `commit` returned; with `durable`, a crash
    /// from here on keeps every commit up to it.
    Acked {
        commit: usize,
        durable: bool,
    },
}

struct Recorder {
    on: bool,
    events: Vec<Event>,
}

static LOG: Mutex<Recorder> = Mutex::new(Recorder {
    on: false,
    events: Vec::new(),
});

/// One recording at a time: every test of the binary takes this first.
pub fn serial() -> MutexGuard<'static, ()> {
    static SERIAL: Mutex<()> = Mutex::new(());
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

fn recorder() -> MutexGuard<'static, Recorder> {
    LOG.lock().unwrap_or_else(PoisonError::into_inner)
}

fn log(event: impl FnOnce() -> Event) {
    let mut r = recorder();
    if r.on {
        let event = event();
        r.events.push(event);
    }
}

/// Starts a recording with an empty log.
pub fn start() {
    let mut r = recorder();
    r.on = true;
    r.events.clear();
}

/// Ends the recording and returns its log.
pub fn stop() -> Vec<Event> {
    let mut r = recorder();
    r.on = false;
    std::mem::take(&mut r.events)
}

pub fn acked(commit: usize, durable: bool) {
    log(|| Event::Acked {
        commit,
        durable,
    });
}

/// `unix`, which every file operation is forwarded to.
static INNER: OnceLock<usize> = OnceLock::new();

fn inner_vfs() -> *mut ffi::sqlite3_vfs {
    *INNER.get().expect("registered") as *mut ffi::sqlite3_vfs
}

/// Registers the VFS (once per process).
pub fn register() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: `unix` is a static VFS that lives for the process; the copy is leaked, as a
        // registered VFS must outlive every connection.
        unsafe {
            let unix = ffi::sqlite3_vfs_find(c"unix".as_ptr());
            assert!(!unix.is_null(), "SQLite has the unix VFS");
            INNER.set(unix as usize).expect("registered once");
            let mut vfs = *unix;
            vfs.zName = c"dashkv-recording".as_ptr();
            vfs.pNext = std::ptr::null_mut();
            vfs.szOsFile = (size_of::<File>() + (*unix).szOsFile as usize) as c_int;
            vfs.xOpen = Some(x_open);
            vfs.xDelete = Some(x_delete);
            let rc = ffi::sqlite3_vfs_register(Box::leak(Box::new(vfs)), 0);
            assert_eq!(rc, ffi::SQLITE_OK);
        }
    });
}

/// Our file: SQLite's base struct, then `unix`'s file, which lives right after it in the same
/// allocation (`szOsFile` covers both).
#[repr(C)]
struct File {
    base: ffi::sqlite3_file,
    inner: *mut ffi::sqlite3_file,
    /// The file's name in its directory; null for a temporary file, which is not logged.
    name: *mut String,
}

fn basename(path: &CStr) -> String {
    let path = path.to_string_lossy();
    path.rsplit('/').next().unwrap_or(&path).to_owned()
}

unsafe fn file<'a>(f: *mut ffi::sqlite3_file) -> &'a File {
    // SAFETY: SQLite passes back the file our `x_open` filled.
    unsafe { &*f.cast::<File>() }
}

unsafe fn inner(
    f: *mut ffi::sqlite3_file,
) -> (*mut ffi::sqlite3_file, &'static ffi::sqlite3_io_methods) {
    // SAFETY: an open file's inner file has its methods set by `unix`'s `xOpen`.
    unsafe {
        let inner = file(f).inner;
        (inner, &*(*inner).pMethods)
    }
}

unsafe fn name(f: *mut ffi::sqlite3_file) -> Option<String> {
    // SAFETY: `name` is null or a leaked box, freed only by `x_close`.
    unsafe { file(f).name.as_ref().cloned() }
}

unsafe extern "C" fn x_open(
    _vfs: *mut ffi::sqlite3_vfs,
    z_name: ffi::sqlite3_filename,
    f: *mut ffi::sqlite3_file,
    flags: c_int,
    out_flags: *mut c_int,
) -> c_int {
    // SAFETY: `f` has room for our `File` and `unix`'s file after it (`szOsFile`).
    unsafe {
        let inner = f.cast::<u8>().add(size_of::<File>()).cast::<ffi::sqlite3_file>();
        (*inner).pMethods = std::ptr::null();
        let vfs = inner_vfs();
        let rc = ((*vfs).xOpen.expect("unix opens"))(vfs, z_name, inner, flags, out_flags);
        if rc != ffi::SQLITE_OK {
            if !(*inner).pMethods.is_null() {
                ((*(*inner).pMethods).xClose.expect("closes"))(inner);
            }
            (*f).pMethods = std::ptr::null();
            return rc;
        }
        let name = if z_name.is_null() {
            std::ptr::null_mut()
        } else {
            Box::into_raw(Box::new(basename(CStr::from_ptr(z_name))))
        };
        f.cast::<File>().write(File {
            base: ffi::sqlite3_file {
                pMethods: &METHODS,
            },
            inner,
            name,
        });
        ffi::SQLITE_OK
    }
}

unsafe extern "C" fn x_delete(
    _vfs: *mut ffi::sqlite3_vfs,
    z_name: *const c_char,
    _sync_dir: c_int,
) -> c_int {
    // SAFETY: SQLite passes a path. Forwarded to `unix` without its directory sync, as a sync is
    // logged and not performed.
    unsafe {
        let file = basename(CStr::from_ptr(z_name));
        log(|| Event::Delete {
            file,
        });
        let vfs = inner_vfs();
        ((*vfs).xDelete.expect("unix deletes"))(vfs, z_name, 0)
    }
}

static METHODS: ffi::sqlite3_io_methods = ffi::sqlite3_io_methods {
    // Version 2: shared memory, but no memory-mapped I/O (`mmap_size` is 0 anyway).
    iVersion: 2,
    xClose: Some(x_close),
    xRead: Some(x_read),
    xWrite: Some(x_write),
    xTruncate: Some(x_truncate),
    xSync: Some(x_sync),
    xFileSize: Some(x_file_size),
    xLock: Some(x_lock),
    xUnlock: Some(x_unlock),
    xCheckReservedLock: Some(x_check_reserved_lock),
    xFileControl: Some(x_file_control),
    xSectorSize: Some(x_sector_size),
    xDeviceCharacteristics: Some(x_device_characteristics),
    xShmMap: Some(x_shm_map),
    xShmLock: Some(x_shm_lock),
    xShmBarrier: Some(x_shm_barrier),
    xShmUnmap: Some(x_shm_unmap),
    xFetch: None,
    xUnfetch: None,
};

unsafe extern "C" fn x_close(f: *mut ffi::sqlite3_file) -> c_int {
    // SAFETY: the last call on the file: close `unix`'s, then free the name.
    unsafe {
        let (inner, m) = inner(f);
        let rc = (m.xClose.expect("closes"))(inner);
        let name = file(f).name;
        if !name.is_null() {
            drop(Box::from_raw(name));
        }
        rc
    }
}

unsafe extern "C" fn x_read(
    f: *mut ffi::sqlite3_file,
    buf: *mut c_void,
    amt: c_int,
    off: i64,
) -> c_int {
    // SAFETY: forwarded unchanged.
    unsafe {
        let (inner, m) = inner(f);
        (m.xRead.expect("reads"))(inner, buf, amt, off)
    }
}

unsafe extern "C" fn x_write(
    f: *mut ffi::sqlite3_file,
    buf: *const c_void,
    amt: c_int,
    off: i64,
) -> c_int {
    // SAFETY: `buf` holds `amt` bytes; forwarded unchanged.
    unsafe {
        if let Some(file) = name(f) {
            log(|| Event::Write {
                file,
                offset: off as u64,
                data: std::slice::from_raw_parts(buf.cast::<u8>(), amt as usize).to_vec(),
            });
        }
        let (inner, m) = inner(f);
        (m.xWrite.expect("writes"))(inner, buf, amt, off)
    }
}

unsafe extern "C" fn x_truncate(f: *mut ffi::sqlite3_file, size: i64) -> c_int {
    // SAFETY: forwarded unchanged.
    unsafe {
        if let Some(file) = name(f) {
            log(|| Event::Truncate {
                file,
                size: size as u64,
            });
        }
        let (inner, m) = inner(f);
        (m.xTruncate.expect("truncates"))(inner, size)
    }
}

unsafe extern "C" fn x_sync(f: *mut ffi::sqlite3_file, _flags: c_int) -> c_int {
    // SAFETY: only reads our own fields.
    unsafe {
        if let Some(file) = name(f) {
            log(|| Event::Sync {
                file,
            });
        }
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_file_size(f: *mut ffi::sqlite3_file, size: *mut i64) -> c_int {
    // SAFETY: forwarded unchanged.
    unsafe {
        let (inner, m) = inner(f);
        (m.xFileSize.expect("sizes"))(inner, size)
    }
}

unsafe extern "C" fn x_lock(f: *mut ffi::sqlite3_file, level: c_int) -> c_int {
    // SAFETY: forwarded unchanged.
    unsafe {
        let (inner, m) = inner(f);
        (m.xLock.expect("locks"))(inner, level)
    }
}

unsafe extern "C" fn x_unlock(f: *mut ffi::sqlite3_file, level: c_int) -> c_int {
    // SAFETY: forwarded unchanged.
    unsafe {
        let (inner, m) = inner(f);
        (m.xUnlock.expect("unlocks"))(inner, level)
    }
}

unsafe extern "C" fn x_check_reserved_lock(f: *mut ffi::sqlite3_file, out: *mut c_int) -> c_int {
    // SAFETY: forwarded unchanged.
    unsafe {
        let (inner, m) = inner(f);
        (m.xCheckReservedLock.expect("checks"))(inner, out)
    }
}

unsafe extern "C" fn x_file_control(
    f: *mut ffi::sqlite3_file,
    op: c_int,
    arg: *mut c_void,
) -> c_int {
    // SAFETY: forwarded unchanged.
    unsafe {
        let (inner, m) = inner(f);
        (m.xFileControl.expect("controls"))(inner, op, arg)
    }
}

unsafe extern "C" fn x_sector_size(f: *mut ffi::sqlite3_file) -> c_int {
    // SAFETY: forwarded unchanged.
    unsafe {
        let (inner, m) = inner(f);
        (m.xSectorSize.expect("sector size"))(inner)
    }
}

unsafe extern "C" fn x_device_characteristics(f: *mut ffi::sqlite3_file) -> c_int {
    // SAFETY: forwarded unchanged.
    unsafe {
        let (inner, m) = inner(f);
        (m.xDeviceCharacteristics.expect("characteristics"))(inner)
    }
}

unsafe extern "C" fn x_shm_map(
    f: *mut ffi::sqlite3_file,
    page: c_int,
    size: c_int,
    extend: c_int,
    out: *mut *mut c_void,
) -> c_int {
    // SAFETY: forwarded unchanged.
    unsafe {
        let (inner, m) = inner(f);
        (m.xShmMap.expect("unix has shared memory"))(inner, page, size, extend, out)
    }
}

unsafe extern "C" fn x_shm_lock(
    f: *mut ffi::sqlite3_file,
    offset: c_int,
    n: c_int,
    flags: c_int,
) -> c_int {
    // SAFETY: forwarded unchanged.
    unsafe {
        let (inner, m) = inner(f);
        (m.xShmLock.expect("unix has shared memory"))(inner, offset, n, flags)
    }
}

unsafe extern "C" fn x_shm_barrier(f: *mut ffi::sqlite3_file) {
    // SAFETY: forwarded unchanged.
    unsafe {
        let (inner, m) = inner(f);
        (m.xShmBarrier.expect("unix has shared memory"))(inner);
    }
}

unsafe extern "C" fn x_shm_unmap(f: *mut ffi::sqlite3_file, delete: c_int) -> c_int {
    // SAFETY: forwarded unchanged.
    unsafe {
        let (inner, m) = inner(f);
        (m.xShmUnmap.expect("unix has shared memory"))(inner, delete)
    }
}
