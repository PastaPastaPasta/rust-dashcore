//! The device stack: loop devices on image files, device-mapper targets on them, and mounts, all
//! made through the privileged wrapper `contrib/node/crash/st3b-priv` (the harness's only `sudo`).
//!
//! A [`Run`] owns `<root>/run/<id>/` and `<root>/mnt/<id>/` and holds `<root>/run/<id>/lock` while
//! it lives; device-mapper names are `st3b-<id>-<role>`. A [`Stack`] tears down what it built in
//! reverse order on every exit path, `Drop` (a panic) included. A harness killed outright leaves
//! its devices behind with its lock released, and [`sweep_stale`] (run at every start) removes
//! those.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Where every image, device backing file and mountpoint lives; the wrapper refuses anything else.
pub const ROOT: &str = "/work/scratch/st3b";

/// A filesystem the harness formats and mounts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fs {
    Ext4,
    Xfs,
}

impl Fs {
    pub fn name(self) -> &'static str {
        match self {
            Fs::Ext4 => "ext4",
            Fs::Xfs => "xfs",
        }
    }

    pub fn parse(s: &str) -> Option<Fs> {
        match s {
            "ext4" => Some(Fs::Ext4),
            "xfs" => Some(Fs::Xfs),
            _ => None,
        }
    }
}

/// The privileged wrapper.
#[derive(Clone, Debug)]
pub struct Priv {
    path: PathBuf,
}

impl Priv {
    /// `$ST3B_PRIV`, or the wrapper in this source tree.
    pub fn locate() -> Priv {
        let path = std::env::var_os("ST3B_PRIV").map(PathBuf::from).unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../contrib/node/crash/st3b-priv")
        });
        Priv {
            path,
        }
    }

    /// Runs `sudo -n st3b-priv <args>` and returns its stdout.
    pub fn run(&self, args: &[&str]) -> io::Result<String> {
        let out = self.output(args)?;
        if !out.status.success() {
            return Err(Self::failed(args, &out));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Sweeps run `id`: `Some(what the wrapper swept)`, or `None` when its lock is held (it is
    /// alive, and the wrapper touched nothing). An orphan, with devices or mounts but no run
    /// directory (wrapper exit 4), is an error, so [`sweep_stale`] lists it (ST3b r2 m-1).
    pub fn sweep(&self, id: &str) -> io::Result<Option<String>> {
        let args = ["sweep", id];
        let out = self.output(&args)?;
        match out.status.code() {
            Some(0) => Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())),
            Some(3) => Ok(None),
            _ => Err(Self::failed(&args, &out)),
        }
    }

    fn output(&self, args: &[&str]) -> io::Result<std::process::Output> {
        Command::new("sudo").arg("-n").arg(&self.path).args(args).output()
    }

    fn failed(args: &[&str], out: &std::process::Output) -> io::Error {
        io::Error::other(format!(
            "st3b-priv {}: {}: {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Creates `path` and its missing parents with mode 0700.
pub fn private_dir(path: &Path) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(path)
}

fn try_lock(file: &File) -> io::Result<bool> {
    // SAFETY: an open descriptor.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let e = io::Error::last_os_error();
    if e.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(false)
    } else {
        Err(e)
    }
}

/// One harness run's directories and lock.
#[derive(Debug)]
pub struct Run {
    id: String,
    dir: PathBuf,
    mnt: PathBuf,
    _lock: File,
}

impl Run {
    /// Sweeps stale runs, then takes a new run id (`<utc seconds>-<pid>`), its directories and its
    /// lock.
    pub fn start(p: &Priv) -> io::Result<Run> {
        let root = Path::new(ROOT);
        private_dir(&root.join("run"))?;
        private_dir(&root.join("mnt"))?;
        // A stale run that does not sweep is reported, not fatal: it cannot share the new run's
        // names, and the leak check of whoever owns it reports it.
        let stale = sweep_stale(p)?;
        for swept in &stale.swept {
            eprintln!("st3b: stale run: {swept}");
        }
        for failed in &stale.failed {
            eprintln!("st3b: stale run not swept: {failed}");
        }
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let id = format!("{secs}-{}", std::process::id());
        let (dir, mnt) = (root.join("run").join(&id), root.join("mnt").join(&id));
        // Not recursive: an existing directory means the id is taken.
        DirBuilder::new().mode(0o700).create(&dir)?;
        let lock = OpenOptions::new().write(true).create_new(true).open(dir.join("lock"))?;
        if !try_lock(&lock)? {
            return Err(io::Error::other(format!("run lock {id} is held")));
        }
        private_dir(&mnt)?;
        Ok(Run {
            id,
            dir,
            mnt,
            _lock: lock,
        })
    }

    /// Ends a clean run: removes its lock and its (by now empty) directories. Anything a check kept
    /// as a repro keeps its directory too.
    pub fn finish(self) -> io::Result<()> {
        for m in fs::read_dir(&self.mnt)? {
            fs::remove_dir(m?.path())?;
        }
        fs::remove_dir(&self.mnt)?;
        remove_own_file(&self.dir.join("lock"))?;
        drop(self._lock);
        match fs::remove_dir(&self.dir) {
            Err(e) if e.kind() == io::ErrorKind::DirectoryNotEmpty => Ok(()),
            r => r,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// `<root>/run/<id>/`: images and logs.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// A mountpoint `<root>/mnt/<id>/<name>`, created.
    pub fn mountpoint(&self, name: &str) -> io::Result<PathBuf> {
        let m = self.mnt.join(name);
        private_dir(&m)?;
        Ok(m)
    }

    /// The device-mapper name for `role` (no `-` in it, so the run id parses back out).
    pub fn dm(&self, role: &str) -> String {
        assert!(!role.is_empty() && !role.contains('-'), "role {role:?}");
        format!("st3b-{}-{role}", self.id)
    }
}

/// Creates a sparse file of `len` bytes.
pub fn sparse_file(path: &Path, len: u64) -> io::Result<File> {
    let f = OpenOptions::new().read(true).write(true).create_new(true).open(path)?;
    f.set_len(len)?;
    Ok(f)
}

/// The size of a block device in 512-byte sectors, as device-mapper tables count.
pub fn sectors(file_len: u64) -> u64 {
    file_len / 512
}

#[derive(Debug)]
enum Layer {
    Loop(String),
    Dm(String),
    Mount(PathBuf),
}

/// Devices and mounts built in order and torn down in reverse.
pub struct Stack<'p> {
    p: &'p Priv,
    layers: Vec<Layer>,
}

impl<'p> Stack<'p> {
    pub fn new(p: &'p Priv) -> Self {
        Stack {
            p,
            layers: Vec::new(),
        }
    }

    /// Attaches a loop device to `file` and returns it (`/dev/loopN`).
    pub fn attach(&mut self, file: &Path) -> io::Result<String> {
        let dev = self.p.run(&["losetup-attach", &file.to_string_lossy()])?.trim().to_owned();
        if !dev.starts_with("/dev/loop") {
            return Err(io::Error::other(format!("losetup-attach printed {dev:?}")));
        }
        self.layers.push(Layer::Loop(dev.clone()));
        Ok(dev)
    }

    /// Creates the device-mapper device `name` from a one-line `table`; returns its path.
    pub fn dm_create(&mut self, name: &str, table: &str) -> io::Result<PathBuf> {
        self.p.run(&["dm-create", name, table])?;
        self.layers.push(Layer::Dm(name.to_owned()));
        Ok(Path::new("/dev/mapper").join(name))
    }

    /// Formats `dev` (no discard; inode tables written up front).
    pub fn mkfs(&self, dev: &Path, fs: Fs) -> io::Result<()> {
        self.p.run(&["mkfs", &dev.to_string_lossy(), fs.name()]).map(drop)
    }

    /// Mounts `dev` on `mnt` (xfs with `nouuid`).
    pub fn mount(&mut self, dev: &Path, mnt: &Path, fs: Fs, read_only: bool) -> io::Result<()> {
        let mode = if read_only {
            "ro"
        } else {
            "rw"
        };
        self.p.run(&["mount", &dev.to_string_lossy(), &mnt.to_string_lossy(), fs.name(), mode])?;
        self.layers.push(Layer::Mount(mnt.to_owned()));
        Ok(())
    }

    /// Gives a mounted filesystem's root to this user.
    pub fn chown_mnt(&self, mnt: &Path) -> io::Result<()> {
        self.p.run(&["chown-mnt", &mnt.to_string_lossy()]).map(drop)
    }

    /// Sends a device-mapper message (`mark <name>` to a log-writes device).
    pub fn message(&self, name: &str, msg: &[&str]) -> io::Result<()> {
        let mut args = vec!["dm-message", name];
        args.extend_from_slice(msg);
        self.p.run(&args).map(drop)
    }

    /// Tears down everything, newest first; keeps going past failures and reports them all.
    pub fn teardown(&mut self) -> Result<(), String> {
        let mut failed = Vec::new();
        while let Some(layer) = self.layers.pop() {
            let r = match &layer {
                Layer::Mount(m) => self.p.run(&["umount", &m.to_string_lossy()]),
                Layer::Dm(name) => self.p.run(&["dm-remove", name]),
                Layer::Loop(dev) => self.p.run(&["losetup-detach", dev]),
            };
            if let Err(e) = r {
                failed.push(format!("{layer:?}: {e}"));
            }
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(failed.join("; "))
        }
    }
}

impl Drop for Stack<'_> {
    fn drop(&mut self) {
        if let Err(e) = self.teardown() {
            eprintln!("st3b: teardown failed: {e}");
        }
    }
}

/// Our devices and mounts still present (`mount <path>`, `dm <name>`, `loop <dev> <file>`), for
/// one run or all.
pub fn leaks(p: &Priv, run: Option<&str>) -> io::Result<Vec<String>> {
    let mut args = vec!["leaks"];
    args.extend(run);
    let out = p.run(&args)?;
    Ok(out.lines().filter(|l| !l.is_empty()).map(str::to_owned).collect())
}

/// The run id a leak line belongs to.
pub(crate) fn leak_run(line: &str) -> Option<&str> {
    let (kind, rest) = line.split_once(' ')?;
    match kind {
        "dm" => rest.strip_prefix("st3b-")?.rsplit_once('-').map(|(id, _)| id),
        "mount" => rest.strip_prefix(ROOT)?.strip_prefix("/mnt/")?.split('/').next(),
        "loop" => {
            rest.split_once(' ')?.1.strip_prefix(ROOT)?.strip_prefix("/run/")?.split('/').next()
        }
        _ => None,
    }
}

/// What [`sweep_stale`] did: the wrapper's report for each run it swept, and why each run it tried
/// was not swept.
#[derive(Debug, Default)]
pub struct Stale {
    pub swept: Vec<String>,
    pub failed: Vec<String>,
}

/// Sweeps every run that has devices or mounts but whose lock nobody holds. The wrapper holds a
/// run's lock while it sweeps it, so a live run, or one another sweep is clearing, is left alone.
/// A run that fails to sweep is listed and the others are still tried (ST3b r1 m-2).
pub fn sweep_stale(p: &Priv) -> io::Result<Stale> {
    let mut runs: Vec<String> =
        leaks(p, None)?.iter().filter_map(|l| leak_run(l)).map(str::to_owned).collect();
    runs.sort();
    runs.dedup();
    let mut stale = Stale::default();
    for id in runs {
        match p.sweep(&id) {
            Ok(swept) => stale.swept.extend(swept),
            Err(e) => stale.failed.push(format!("{id}: {e}")),
        }
    }
    Ok(stale)
}

/// Removes a file the harness made under its root (an image, a lock).
pub fn remove_own_file(path: &Path) -> io::Result<()> {
    let real = fs::canonicalize(path)?;
    if !real.starts_with(ROOT) || !real.is_file() {
        return Err(io::Error::other(format!("not a harness file: {}", real.display())));
    }
    fs::remove_file(real)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leak_lines_name_their_run() {
        let run = |l: &str| leak_run(l).map(str::to_owned);
        assert_eq!(run("dm st3b-1761700000-42-lw").as_deref(), Some("1761700000-42"));
        assert_eq!(run(&format!("mount {ROOT}/mnt/9-1/rec")).as_deref(), Some("9-1"));
        assert_eq!(
            run(&format!("loop /dev/loop3 {ROOT}/run/9-1/data.img")).as_deref(),
            Some("9-1")
        );
        assert_eq!(run("dm other-thing"), None);
        assert_eq!(run("loop /dev/loop3 /elsewhere/x.img"), None);
    }
}
