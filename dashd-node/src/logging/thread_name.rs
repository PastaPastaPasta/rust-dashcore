//! The thread name a line shows with `-logthreadnames` (`util/threadnames.cpp`).

use std::cell::RefCell;

thread_local! {
    static NAME: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Core's `ThreadSetInternalName`: the name this thread's lines show (`init` for the main
/// thread, `shutoff` once shutdown starts, `scheduler`, …).
pub fn set_thread_name(name: &str) {
    NAME.with(|n| {
        let mut n = n.borrow_mut();
        n.clear();
        n.push_str(name);
    });
}

/// Core's `ThreadGetInternalName`. A thread never named here shows its `std::thread` name, so
/// threads other crates spawn with a name show it. The script-check pool's, which rayon names
/// `d-scriptch.N` (`CheckQueue::new`), show `scriptch.N`: Core's `ThreadRename` puts the `d-`
/// only on the OS name.
// Core: util/threadnames.cpp:59-63
pub fn thread_name() -> String {
    with(str::to_owned)
}

pub(super) fn with<R>(f: impl FnOnce(&str) -> R) -> R {
    NAME.with(|n| {
        let n = n.borrow();
        if n.is_empty() {
            let current = std::thread::current();
            let os = current.name().unwrap_or("");
            f(os.strip_prefix("d-").filter(|rest| rest.starts_with("scriptch.")).unwrap_or(os))
        } else {
            f(&n)
        }
    })
}
