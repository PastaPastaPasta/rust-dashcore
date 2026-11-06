//! The VM-level power-loss harness (ST3b S7, PLAN-ST3b-S7): rdashd in a KVM guest on a
//! `cache=none` virtio disk, QEMU SIGKILLed at random or log-triggered points, and the guest booted
//! again on the same disk, so its own filesystem journal recovers before rdashd does.
//!
//! QEMU runs in a container of the `st3b-qemu` image (`contrib/node/crash/vm/Dockerfile`, tagged
//! by its sha256) as this user, labelled `st3b.run=<run>` and named `st3b-vm-<run>-<boot>`, under
//! a shell that SIGKILLs it once the run's lock is free (a harness killed without unwinding leaves
//! no guest running). The power cut is a SIGKILL straight to QEMU's host process, pinned by a
//! pidfd, no docker round trip on the way. The guest is the host's kernel (`st3b-priv kernel`) and
//! the initramfs `mkguest.sh` builds. Its RPC is forwarded to a loopback port; the checks reach it
//! through [`Guest::rpc`], a [`NodeRpc`] (dashcore-rpc's client), as they reach a node on the host.
//!
//! The pieces here are the [`Disk`], one boot ([`Guest`], whose `Drop` kills and removes its
//! container), the console [`Tail`] and trigger [`Matcher`], the kill [`Plan`] and the
//! [`Killer`] that carries it out, and the floor bookkeeping; [`scenario`] runs them.

pub mod scenario;

use std::fs::{self, File};
use std::io::{self, Read};
use std::net::{Ipv4Addr, SocketAddr};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dashcore::hashes::{Hash, HashEngine, Hmac, HmacEngine, sha256};
use dashcore_rpc::Client;
use dashcore_rpc::jsonrpc::{self, simple_http::SimpleHttpTransport};

use super::baseline::node_args;
use super::checks::{Floor, RPC_PATIENCE, c1_lines};
use super::devices::{Fs, ROOT, Stale, remove_own_file, sparse_file, try_lock};
use super::fixture::{Fixture, hex};
use super::image::reflink;
use super::logwrites::Rng;
use crate::{NodeRpc, RpcError};

/// The label every guest container carries, its value the run id.
pub const LABEL: &str = "st3b.run";
const NAME_PREFIX: &str = "st3b-vm-";
/// The data disk: sparse, so a run writes far less.
pub const DISK_LEN: u64 = 2 << 30;
/// User-mode networking's fixed addresses: the guest, and the host as the guest sees it.
pub const GUEST_IP: &str = "10.0.2.15";
pub const HOST_IP: [u8; 4] = [10, 0, 2, 2];
/// rdashd's RPC port inside the guest (forwarded to a free loopback port on the host).
pub const GUEST_RPC_PORT: u16 = 19998;
const RPC_USER: &str = "st3b";
/// x86's `COMMAND_LINE_SIZE` (2048), its terminating NUL included.
pub const CMDLINE_MAX: usize = 2047;
/// What the guest's init prints when rdashd exits, before the code.
pub const EXITED: &str = "ST3B-GUEST: rdashd exited ";
/// QEMU's disk I/O: `aio=io_uring` is refused inside Docker (seccomp, PLAN-ST3b-S7 §7), so
/// `native`; with `cache=none` both are O_DIRECT.
pub const AIO: &str = "native";
/// The image's Dockerfile, which names its tag.
const DOCKERFILE: &[u8] = include_bytes!("../../../../contrib/node/crash/vm/Dockerfile");
/// `docker run`'s resource limits for one guest: the guest's 1 GiB and 2 vCPUs with room for
/// QEMU, at the default CPU weight (a low one starved boots past any patience on a loaded box, S7
/// r1 m4), and a low I/O weight, as the weekly unit's `IOSchedulingClass=idle` never reaches it.
const CONTAINER_LIMITS: [&str; 3] = ["--cpus=2", "--memory=2g", "--blkio-weight=100"];
/// The container's command: QEMU (its arguments after the lock's path) under a shell that
/// SIGKILLs it once it can take the run's lock, that is once the harness is gone however it died
/// (S7 r1 m6). The container ends with QEMU.
const QEMU_WRAPPER: &str =
    r#"l=$1; shift; "$@" & q=$!; (flock -x "$l" true; kill -9 $q) & wait $q"#;
/// QEMU's `comm`: the kernel keeps 15 bytes of `qemu-system-x86_64`.
const QEMU_COMM: &str = "qemu-system-x86";
/// How long `docker run`'s shell may take to start QEMU.
const QEMU_START: Duration = Duration::from_secs(30);
/// How often the killer reads the console: the poll adds 0-5 ms to a triggered kill's latency.
const TAIL_POLL: Duration = Duration::from_millis(5);
/// How long a boot may take to answer RPC: no less than the checks' own patience, as a recovery
/// boot replays and loads a W-40k block index under `-checkblockindex` on a loaded box (S7 r1 m4).
pub const VM_BOOT_PATIENCE: Duration = RPC_PATIENCE;
/// Boots QEMU could not start because another process took its forwarded port first: tried again
/// on a new port, up to this many tries in all (S7 r1 m8).
const BOOT_TRIES: usize = 3;

/// The `st3b-qemu` image this build runs: tagged by the first 12 hex of its Dockerfile's sha256, so
/// a changed Dockerfile is a new image and a stale tag never runs.
pub fn image_tag() -> String {
    image_tag_of(DOCKERFILE)
}

fn image_tag_of(dockerfile: &[u8]) -> String {
    format!("st3b-qemu:{}", &sha256_hex(dockerfile)[..12])
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&sha256::Hash::hash(bytes).to_byte_array())
}

fn sha256_file(path: &Path) -> io::Result<String> {
    Ok(sha256_hex(&fs::read(path)?))
}

/// Standard base64 with padding (the guest's busybox `base64 -d` reads it).
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// rdashd's arguments as `st3b.args=` carries them: one per line, base64. An empty argument or one
/// with a newline would not survive the guest's split.
pub fn encode_args(args: &[String]) -> Result<String, String> {
    if let Some(bad) = args.iter().find(|a| a.is_empty() || a.contains('\n')) {
        return Err(format!("an argument the guest cannot pass: {bad:?}"));
    }
    Ok(base64(args.join("\n").as_bytes()))
}

/// The guest kernel's command line. `quiet` keeps kernel lines off the console, where they would
/// land inside rdashd's (the init prints the filesystem's own).
pub fn cmdline(fs: Fs, args: &[String]) -> Result<String, String> {
    let line = format!(
        "console=ttyS0 quiet panic=-1 st3b.fs={} st3b.args={}",
        fs.name(),
        encode_args(args)?
    );
    if line.len() > CMDLINE_MAX {
        return Err(format!(
            "a kernel command line of {} bytes (at most {CMDLINE_MAX})",
            line.len()
        ));
    }
    Ok(line)
}

/// `-rpcauth`'s value for [`RPC_USER`], `password` and `salt`, as Core's `share/rpcauth` makes it:
/// `<user>:<salt>$<hex HMAC-SHA256 of the password, keyed with the salt>`. The guest's command line
/// (in QEMU's argv, so in `ps` and `docker inspect`) carries this, never the password (S7 r1 n1).
pub fn rpcauth(salt: &str, password: &str) -> String {
    let mut engine = HmacEngine::<sha256::HashEngine>::new(salt.as_bytes());
    engine.input(password.as_bytes());
    let mac: Hmac<sha256::Hash> = engine.finalize();
    format!("{RPC_USER}:{salt}${}", hex(&mac.to_byte_array()))
}

/// rdashd's arguments in the guest (its init adds `-datadir` and `-printtoconsole`): regtest, RPC
/// for the forward with `-rpcauth` (see [`rpcauth`]), no peers (the harness adds the fixture's
/// with `addnode` when the workload needs it), the fixture's mock time, then [`node_args`]. `extra`
/// must not set what the harness owns.
pub fn guest_args(
    fixture: &Fixture,
    extra: &[String],
    rpcauth: &str,
) -> Result<Vec<String>, String> {
    crate::rust_node::check_args(extra).map_err(|e| e.to_string())?;
    let mut args: Vec<String> =
        ["-regtest", "-daemon=0", "-shrinkdebugfile=0", "-server=1", "-listen=0", "-connect=0"]
            .map(String::from)
            .to_vec();
    args.extend([
        format!("-rpcbind={GUEST_IP}"),
        format!("-rpcallowip={}", Ipv4Addr::from(HOST_IP)),
        format!("-rpcport={GUEST_RPC_PORT}"),
        format!("-rpcauth={rpcauth}"),
        format!("-mocktime={}", fixture.mocktime),
    ]);
    args.extend(node_args(fixture, extra));
    Ok(args)
}

/// The container of boot `boot` of run `run`.
pub fn container_name(run: &str, boot: usize) -> String {
    format!("{NAME_PREFIX}{run}-{boot}")
}

/// The run a container name of ours belongs to.
pub fn container_run(name: &str) -> Option<&str> {
    let (run, boot) = name.strip_prefix(NAME_PREFIX)?.rsplit_once('-')?;
    (!run.is_empty() && boot.parse::<usize>().is_ok()).then_some(run)
}

fn docker(args: &[&str]) -> io::Result<Output> {
    Command::new("docker").args(args).output()
}

fn checked(args: &[&str], out: Output) -> io::Result<String> {
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    Err(io::Error::other(format!(
        "docker {}: {}: {}",
        args.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    )))
}

/// A process's `comm` and state letter from `/proc/<pid>/stat`, if it exists.
fn proc_stat(pid: i32) -> Option<(String, char)> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `<pid> (<comm>) <state> ...`: the comm may hold spaces and parentheses, the last `)` ends it.
    let (open, close) = (stat.find('(')?, stat.rfind(')')?);
    let comm = stat.get(open + 1..close)?.to_owned();
    let state = stat.get(close + 1..)?.trim_start().chars().next()?;
    Some((comm, state))
}

/// Whether `pid` is a live (not zombie) QEMU process.
fn is_qemu(pid: i32) -> bool {
    proc_stat(pid).is_some_and(|(comm, state)| comm == QEMU_COMM && !matches!(state, 'Z' | 'X'))
}

/// QEMU among the children of the container's shell `shell` (host pids).
fn qemu_child(shell: i32) -> Option<i32> {
    let children = fs::read_to_string(format!("/proc/{shell}/task/{shell}/children")).ok()?;
    children.split_whitespace().filter_map(|c| c.parse().ok()).find(|&c| is_qemu(c))
}

/// QEMU's host process, pinned by a pidfd (S7 r2 N-a): a signal through it reaches that process
/// or none, never one that took its pid after it exited.
#[derive(Debug)]
pub struct Qemu {
    pid: i32,
    fd: OwnedFd,
}

/// `Ok(false)` for ESRCH (no such process), the error otherwise.
fn esrch_is_false(r: libc::c_long) -> io::Result<bool> {
    if r >= 0 {
        return Ok(true);
    }
    let e = io::Error::last_os_error();
    if e.raw_os_error() == Some(libc::ESRCH) {
        Ok(false)
    } else {
        Err(e)
    }
}

impl Qemu {
    /// Pins process `pid`; `None` unless it is a live QEMU once pinned (checked after the open, so
    /// the check is of the process the pidfd holds) or when it is gone.
    fn open(pid: i32) -> io::Result<Option<Qemu>> {
        // SAFETY: pidfd_open(2) takes a pid and flags, and returns a new fd or -1.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if !esrch_is_false(fd)? {
            return Ok(None);
        }
        // SAFETY: the fd is new and ours alone.
        let fd = unsafe { OwnedFd::from_raw_fd(fd as RawFd) };
        Ok(is_qemu(pid).then_some(Qemu {
            pid,
            fd,
        }))
    }

    /// SIGKILLs it, the guest's power cut, with no docker round trip on the way (S7 r1 M1): it
    /// runs as this user, in no user namespace. False when QEMU had exited (a zombie included: the
    /// guest stopped first).
    pub fn kill(&self) -> io::Result<bool> {
        if !is_qemu(self.pid) {
            return Ok(false);
        }
        let null = std::ptr::null::<libc::siginfo_t>();
        // SAFETY: pidfd_send_signal(2) takes our pidfd, a signal, no siginfo and no flags.
        let raw = self.fd.as_raw_fd();
        esrch_is_false(unsafe {
            libc::syscall(libc::SYS_pidfd_send_signal, raw, libc::SIGKILL, null, 0)
        })
    }
}

/// `docker rm -f <name>`; one already gone is no error.
fn remove_container(name: &str) -> io::Result<()> {
    let args = ["rm", "-f", name];
    let out = docker(&args)?;
    if !out.status.success() && String::from_utf8_lossy(&out.stderr).contains("No such container") {
        return Ok(());
    }
    checked(&args, out).map(drop)
}

/// One of our containers, as `docker ps -a` lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Container {
    pub name: String,
    /// Its run label.
    pub run: String,
    /// Not exited, created-only or dead: QEMU may run in it (an unknown state counts as live).
    pub live: bool,
}

/// `docker ps` lines `<name>\t<run label>\t<state>`, as [`containers`] asks for them.
fn parse_ps(out: &str) -> Vec<Container> {
    out.lines()
        .filter_map(|l| {
            let mut fields = l.split('\t');
            let (name, run) = (fields.next()?, fields.next()?);
            Some(Container {
                name: name.to_owned(),
                run: run.to_owned(),
                live: !matches!(fields.next(), Some("exited" | "created" | "dead")),
            })
        })
        .collect()
}

/// Our containers, running or not: all of them, or one run's.
pub fn containers(run: Option<&str>) -> io::Result<Vec<Container>> {
    let filter = match run {
        Some(r) => format!("label={LABEL}={r}"),
        None => format!("label={LABEL}"),
    };
    let format = format!("{{{{.Names}}}}\t{{{{.Label \"{LABEL}\"}}}}\t{{{{.State}}}}");
    let args = ["ps", "-a", "--filter", &filter, "--format", &format];
    Ok(parse_ps(&checked(&args, docker(&args)?)?))
}

/// The leak lines of our containers (`container <name> <run>`), as `st3b-crash leaks` prints
/// them beside the devices and mounts.
pub fn container_leaks(run: Option<&str>) -> io::Result<Vec<String>> {
    Ok(containers(run)?.into_iter().map(|c| format!("container {} {}", c.name, c.run)).collect())
}

/// What a container's run label says about its run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RunState {
    /// Its lock is held: a live run.
    Alive,
    /// Its lock is free, or the label is no run id (names no lock): stale.
    Dead,
    /// A run id whose lock is gone: a dead run's directory removed, or a live run's removed under
    /// it. They look the same, so a person decides (as `st3b-priv sweep`'s exit 4, S7 r1 n6),
    /// while its container runs; an exited one is stale (S7 r2 N-e).
    Orphan,
}

/// The state of run `run` from its lock under the harness root.
fn run_state(run: &str) -> io::Result<RunState> {
    let is_id = run.split_once('-').is_some_and(|(a, b)| {
        [a, b].iter().all(|p| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit()))
    });
    if !is_id {
        return Ok(RunState::Dead);
    }
    match File::open(Path::new(ROOT).join("run").join(run).join("lock")) {
        Ok(lock) if try_lock(&lock)? => Ok(RunState::Dead),
        Ok(_) => Ok(RunState::Alive),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(RunState::Orphan),
        Err(e) => Err(e),
    }
}

/// The containers of `all` a stale sweep removes (their run is dead, or they are an orphan's and
/// have exited: the wrapper leaves no QEMU once the lock was free, and a live run's container
/// would be running), and the running orphans it leaves (`container <name> <run>: ...`).
fn stale_containers(
    all: Vec<Container>,
    state: impl Fn(&str) -> io::Result<RunState>,
) -> io::Result<(Vec<String>, Vec<String>)> {
    let (mut stale, mut orphans) = (Vec::new(), Vec::new());
    for c in all {
        match state(&c.run)? {
            RunState::Alive => {}
            RunState::Orphan if c.live => orphans.push(format!(
                "container {} {}: the run's lock is gone and it runs; `docker rm -f {}` if it is \
                 dead",
                c.name, c.run, c.name
            )),
            RunState::Dead | RunState::Orphan => stale.push(c.name),
        }
    }
    Ok((stale, orphans))
}

/// Removes every container labelled [`LABEL`] whose run's lock nobody holds (a killed harness's
/// guest); a live run's are left alone, and a running orphan's (see `RunState::Orphan`) too. What it
/// removed is `swept` (`container <name>`); each orphan and each removal that failed is in
/// `failed`, and the rest are still tried.
pub fn sweep_stale_containers() -> io::Result<Stale> {
    let (stale, orphans) = stale_containers(containers(None)?, run_state)?;
    let mut out = Stale {
        swept: Vec::new(),
        failed: orphans,
    };
    for name in stale {
        match remove_container(&name) {
            Ok(()) => out.swept.push(format!("container {name}")),
            Err(e) => out.failed.push(format!("container {name}: {e}")),
        }
    }
    Ok(out)
}

/// A guest's data disk: a sparse raw file with a filesystem made straight on it (no sudo, no loop
/// device).
#[derive(Debug)]
pub struct Disk {
    path: PathBuf,
}

impl Disk {
    /// A new [`DISK_LEN`] file at `path`, formatted `fs` (inode tables and journal written up
    /// front, so no lazy init writes in the guest).
    pub fn create(path: &Path, fs: Fs) -> io::Result<Disk> {
        sparse_file(path, DISK_LEN)?;
        let (mkfs, opts): (&str, &[&str]) = match fs {
            Fs::Ext4 => (
                "/usr/sbin/mkfs.ext4",
                &["-q", "-F", "-E", "lazy_itable_init=0,lazy_journal_init=0"],
            ),
            Fs::Xfs => ("/usr/sbin/mkfs.xfs", &["-q", "-f"]),
        };
        let out = Command::new(mkfs).args(opts).arg(path).output()?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "{mkfs} {}: {}",
                path.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(Disk {
            path: path.to_owned(),
        })
    }

    /// A reflink copy at `path` (a data copy where the filesystem can't reflink, BL-609).
    pub fn copy_to(&self, path: &Path) -> io::Result<Disk> {
        reflink(&File::open(&self.path)?, path)?.sync_all()?;
        Ok(Disk {
            path: path.to_owned(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A read-only check of the filesystem on the disk, which no guest may have open and which
    /// must be cleanly unmounted (the init unmounts it when rdashd exits): `e2fsck -fn` or
    /// `xfs_repair -n -f`. The report of a check that found damage, or `None` (S7 r1 m2).
    pub fn fsck(&self, fs: Fs) -> io::Result<Option<String>> {
        let (tool, opts): (&str, &[&str]) = match fs {
            Fs::Ext4 => ("/usr/sbin/e2fsck", &["-f", "-n"]),
            Fs::Xfs => ("/usr/sbin/xfs_repair", &["-n", "-f"]),
        };
        let out = Command::new(tool).args(opts).arg(&self.path).output()?;
        if out.status.success() {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);
        let last: Vec<&str> = text.lines().rev().take(5).collect();
        Ok(Some(format!(
            "{tool} {}: {}: {}",
            self.path.display(),
            out.status,
            last.into_iter().rev().collect::<Vec<_>>().join(" | ")
        )))
    }

    pub fn remove(self) -> io::Result<()> {
        remove_own_file(&self.path)
    }
}

/// One run's guest setup: the image, the kernel and initramfs a guest directory names (checked
/// against its manifest), the run's containers and lock, and its RPC password.
pub struct Vm {
    pub image: String,
    pub kernel: PathBuf,
    pub initramfs: PathBuf,
    /// `mkguest.sh`'s manifest.
    pub manifest: String,
    run: String,
    /// The run's lock, which the container's shell waits on (see [`QEMU_WRAPPER`]).
    lock: PathBuf,
    kvm_gid: u32,
    user: String,
    password: String,
    salt: String,
}

/// Without the password.
impl std::fmt::Debug for Vm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vm")
            .field("image", &self.image)
            .field("kernel", &self.kernel)
            .field("initramfs", &self.initramfs)
            .field("run", &self.run)
            .finish_non_exhaustive()
    }
}

/// The `<kind> <sha256> <path>` line of a manifest.
fn manifest_entry<'a>(manifest: &'a str, kind: &str) -> Option<(&'a str, &'a str)> {
    manifest.lines().find_map(|l| {
        let mut f = l.splitn(3, ' ');
        (f.next() == Some(kind)).then_some(())?;
        Some((f.next()?, f.next()?))
    })
}

impl Vm {
    /// The setup of run `run` from guest directory `guest` (`mkguest.sh`'s out dir): the kernel
    /// and initramfs must match its manifest, and the image must exist.
    pub fn new(guest: &Path, run: &str) -> io::Result<Vm> {
        let manifest = fs::read_to_string(guest.join("manifest.txt"))?;
        let bad = |what: &str| io::Error::other(format!("{}: {what}", guest.display()));
        let (kernel_sha, kernel) =
            manifest_entry(&manifest, "kernel").ok_or_else(|| bad("no kernel in the manifest"))?;
        let (initramfs_sha, initramfs) = manifest_entry(&manifest, "initramfs")
            .ok_or_else(|| bad("no initramfs in the manifest"))?;
        let (kernel, initramfs) = (PathBuf::from(kernel), guest.join(initramfs));
        for (path, sha) in [(&kernel, kernel_sha), (&initramfs, initramfs_sha)] {
            if sha256_file(path)? != sha {
                return Err(bad(&format!("{} does not match the manifest", path.display())));
            }
        }
        let image = image_tag();
        if !docker(&["image", "inspect", &image])?.status.success() {
            return Err(io::Error::other(format!(
                "no image {image}: build it with `docker build -t {image} contrib/node/crash/vm`"
            )));
        }
        let lock = Path::new(ROOT).join("run").join(run).join("lock");
        if !lock.is_file() {
            return Err(io::Error::other(format!("no run lock {}", lock.display())));
        }
        let mut random = [0u8; 32];
        File::open("/dev/urandom")?.read_exact(&mut random)?;
        // SAFETY: getuid and getgid cannot fail.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        Ok(Vm {
            image,
            kernel,
            initramfs,
            manifest,
            run: run.to_owned(),
            lock,
            kvm_gid: fs::metadata("/dev/kvm")?.gid(),
            user: format!("{uid}:{gid}"),
            password: hex(&random[..16]),
            salt: hex(&random[16..]),
        })
    }

    /// The run's `-rpcauth` value (see [`rpcauth`]); the password itself stays here.
    pub fn rpcauth(&self) -> String {
        rpcauth(&self.salt, &self.password)
    }

    /// The `docker run` arguments of boot `boot`: `disk` attached `cache=none` (O_DIRECT), the
    /// console to `console`, guest RPC forwarded to 127.0.0.1:`rpc_port`. `dir` (holding the disk
    /// and console) is bind-mounted, the kernel, initramfs and run lock read-only.
    pub fn docker_args(
        &self,
        boot: usize,
        dir: &Path,
        disk: &Disk,
        console: &Path,
        cmdline: &str,
        rpc_port: u16,
    ) -> Vec<String> {
        let path = |p: &Path| p.display().to_string();
        let mut args: Vec<String> = ["run", "-d", "--name"].map(String::from).to_vec();
        args.push(container_name(&self.run, boot));
        args.extend(["--label".into(), format!("{LABEL}={}", self.run)]);
        args.extend(["--device", "/dev/kvm", "--group-add"].map(String::from));
        args.push(self.kvm_gid.to_string());
        args.extend(["--user".into(), self.user.clone(), "--network".into(), "host".into()]);
        // The container is outside the harness's unit, whose limits never reach it: its own.
        args.extend(CONTAINER_LIMITS.map(String::from));
        args.extend(["-v".into(), format!("{0}:{0}", path(dir))]);
        for ro in [&self.kernel, &self.initramfs, &self.lock] {
            args.extend(["-v".into(), format!("{0}:{0}:ro", path(ro))]);
        }
        args.push(self.image.clone());
        args.extend(["sh".into(), "-c".into(), QEMU_WRAPPER.into(), "sh".into(), path(&self.lock)]);
        args.extend(
            [
                "qemu-system-x86_64",
                "-enable-kvm",
                "-m",
                "1024",
                "-smp",
                "2",
                "-cpu",
                "host",
                "-no-reboot",
                "-display",
                "none",
                "-monitor",
                "none",
            ]
            .map(String::from),
        );
        args.extend([
            "-kernel".into(),
            path(&self.kernel),
            "-initrd".into(),
            path(&self.initramfs),
        ]);
        args.extend(["-append".into(), cmdline.to_owned()]);
        args.extend([
            "-drive".into(),
            format!("file={},format=raw,if=virtio,cache=none,aio={AIO}", path(disk.path())),
            "-netdev".into(),
            format!("user,id=n0,hostfwd=tcp:127.0.0.1:{rpc_port}-{GUEST_IP}:{GUEST_RPC_PORT}"),
            "-device".into(),
            "virtio-net-pci,netdev=n0".into(),
            "-serial".into(),
            format!("file:{}", path(console)),
        ]);
        args
    }

    /// Boots the guest on `disk` with rdashd's `args` (see [`guest_args`]), its console at
    /// `dir/console-<boot>.log`.
    pub fn boot(
        &self,
        boot: usize,
        dir: &Path,
        disk: &Disk,
        fs: Fs,
        args: &[String],
    ) -> io::Result<Guest> {
        let cmdline = cmdline(fs, args).map_err(io::Error::other)?;
        let console = dir.join(format!("console-{boot}.log"));
        File::create(&console)?;
        let rpc_port = crate::find_available_port();
        let docker_args = self.docker_args(boot, dir, disk, &console, &cmdline, rpc_port);
        let refs: Vec<&str> = docker_args.iter().map(String::as_str).collect();
        // A container left by a failed `docker run` goes with the guest's Drop.
        let mut guest = Guest {
            name: container_name(&self.run, boot),
            console,
            rpc_port,
            password: self.password.clone(),
            qemu: None,
            removed: false,
            started: Instant::now(),
            args: args.to_vec(),
        };
        let out = docker(&refs)?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "docker run {}: {}: {}",
                guest.name,
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        guest.qemu = guest.find_qemu()?;
        Ok(guest)
    }

    /// [`Vm::boot`], then waits up to [`VM_BOOT_PATIENCE`] for RPC, with a killer carrying out
    /// `init` (if any) on the boot's console from its start meanwhile. A boot whose QEMU could not
    /// forward its port (another process took it first) is tried again on a new one, the same plan
    /// again, up to [`BOOT_TRIES`] tries (S7 r1 m8).
    pub fn boot_ready(
        &self,
        boot: usize,
        dir: &Path,
        disk: &Disk,
        fs: Fs,
        args: &[String],
        init: Option<Plan>,
    ) -> io::Result<Booted> {
        let mut tries = 1;
        loop {
            let mut guest = self.boot(boot, dir, disk, fs, args)?;
            let killer = init.map(|p| {
                Killer::spawn(&guest, Tail::new(guest.console_path()), p, None, Arc::default())
            });
            let ready = guest.wait_rpc(VM_BOOT_PATIENCE);
            let kill = killer.map(Killer::finish).transpose()?.flatten();
            if ready.is_err() && kill.is_none() && tries < BOOT_TRIES && guest.hostfwd_failed() {
                println!("boot {boot}: QEMU could not forward port {}; again", guest.rpc_port);
                guest.kill()?;
                tries += 1;
                continue;
            }
            return Ok(Booted {
                guest,
                kill,
                ready,
            });
        }
    }
}

/// What [`Vm::boot_ready`] got to.
#[derive(Debug)]
pub struct Booted {
    pub guest: Guest,
    /// The init killer's kill, if it made one (then `ready` is moot).
    pub kill: Option<Kill>,
    /// How long the boot took to answer RPC, or why it did not.
    pub ready: Result<Duration, String>,
}

/// One boot: a container running QEMU. `Drop` kills and removes it, so no container outlives its
/// `Guest`, panics included.
pub struct Guest {
    name: String,
    console: PathBuf,
    rpc_port: u16,
    password: String,
    /// QEMU's host process; `None` if it was gone before it was found.
    qemu: Option<Arc<Qemu>>,
    removed: bool,
    started: Instant,
    /// rdashd's arguments in this boot, for the storage-profile label (BL-634).
    args: Vec<String>,
}

/// Without the password.
impl std::fmt::Debug for Guest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Guest")
            .field("name", &self.name)
            .field("console", &self.console)
            .field("rpc_port", &self.rpc_port)
            .field("qemu", &self.qemu)
            .field("removed", &self.removed)
            .finish_non_exhaustive()
    }
}

impl Guest {
    /// QEMU's host process: the container's shell (`State.Pid`, as the container runs QEMU under
    /// [`QEMU_WRAPPER`]) has it as a child. One `docker inspect` for the shell, then only `/proc`
    /// (S7 r2 N-d); docker again only once the shell is gone or the time is up. Waits up to
    /// [`QEMU_START`]; `None` if the container stopped first (QEMU failed to start, say).
    fn find_qemu(&self) -> io::Result<Option<Arc<Qemu>>> {
        let args = ["inspect", "-f", "{{.State.Running}} {{.State.Pid}}", &self.name];
        let state = checked(&args, docker(&args)?)?;
        let shell = match state.trim().split_once(' ') {
            Some(("true", pid)) => pid.parse().unwrap_or(0),
            _ => 0,
        };
        if shell <= 0 {
            return Ok(None);
        }
        let deadline = Instant::now() + QEMU_START;
        loop {
            if let Some(pid) = qemu_child(shell)
                && let Some(qemu) = Qemu::open(pid)?
                // Still the shell's child once pinned: the pidfd holds that QEMU.
                && qemu_child(shell) == Some(pid)
            {
                return Ok(Some(Arc::new(qemu)));
            }
            let late = Instant::now() > deadline;
            if (late || proc_stat(shell).is_none()) && !self.running()? {
                return Ok(None);
            }
            if late {
                return Err(io::Error::other(format!(
                    "{}: no QEMU under the container's shell (pid {shell}) within {} s",
                    self.name,
                    QEMU_START.as_secs()
                )));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Whether QEMU stopped because it could not set up the RPC forward.
    fn hostfwd_failed(&self) -> bool {
        docker(&["logs", &self.name]).is_ok_and(|out| {
            String::from_utf8_lossy(&out.stderr).contains("host forwarding rule")
                || String::from_utf8_lossy(&out.stdout).contains("host forwarding rule")
        })
    }

    /// Cuts the power: SIGKILL to QEMU (see [`Qemu::kill`]). False when QEMU was gone already.
    pub fn power_cut(&self) -> io::Result<bool> {
        self.qemu.as_deref().map_or(Ok(false), Qemu::kill)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn console_path(&self) -> &Path {
        &self.console
    }

    /// rdashd's arguments in this boot.
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// When `docker run` was called.
    pub fn started(&self) -> Instant {
        self.started
    }

    /// The console so far (a line cut by the kill is kept as it is).
    pub fn console(&self) -> String {
        fs::read(&self.console)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default()
    }

    /// The exit code the guest's init printed, once rdashd has exited.
    pub fn exit_code(&self) -> Option<i32> {
        exit_code(&self.console())
    }

    /// The forwarded RPC, waiting up to `timeout` for each reply.
    pub fn rpc_with_timeout(&self, timeout: Duration) -> Result<Client, RpcError> {
        let transport = SimpleHttpTransport::builder()
            .url(&format!("http://127.0.0.1:{}", self.rpc_port))?
            .timeout(timeout)
            .auth(RPC_USER, Some(&self.password))
            .build();
        Ok(Client::from_jsonrpc(jsonrpc::Client::with_transport(transport)))
    }

    /// The forwarded RPC, with the checks' patience ([`RPC_PATIENCE`]).
    pub fn rpc(&self) -> Result<Client, RpcError> {
        self.rpc_with_timeout(RPC_PATIENCE)
    }

    fn running(&self) -> io::Result<bool> {
        let args = ["inspect", "-f", "{{.State.Running}}", &self.name];
        Ok(checked(&args, docker(&args)?)?.trim() == "true")
    }

    /// Waits up to `patience` for RPC to answer (rdashd has loaded); returns how long the boot
    /// took from `docker run`. Fails at once if rdashd exits or QEMU stops.
    pub fn wait_rpc(&self, patience: Duration) -> Result<Duration, String> {
        let deadline = Instant::now() + patience;
        let rpc = self.rpc_with_timeout(Duration::from_secs(5)).map_err(|e| e.to_string())?;
        loop {
            if rpc.get_block_count().is_ok() {
                return Ok(self.started.elapsed());
            }
            if let Some(code) = self.exit_code() {
                return Err(format!("rdashd exited {code} before RPC answered"));
            }
            if !self.running().map_err(|e| e.to_string())? {
                return Err("the guest stopped before RPC answered".to_owned());
            }
            if Instant::now() > deadline {
                return Err(format!("no RPC within {} s", patience.as_secs()));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// Cuts the power if QEMU still runs, then removes the container.
    pub fn kill(&mut self) -> io::Result<()> {
        if self.removed {
            return Ok(());
        }
        // `docker rm -f` kills whatever runs; the direct kill only makes the cut prompt.
        let _ = self.power_cut();
        remove_container(&self.name)?;
        self.removed = true;
        Ok(())
    }

    /// Waits up to `patience` for the guest to power off by itself (after `stop`), removes the
    /// container and returns the code the init printed.
    pub fn wait_exit(&mut self, patience: Duration) -> io::Result<Option<i32>> {
        let deadline = Instant::now() + patience;
        while self.running()? {
            if Instant::now() > deadline {
                self.kill()?;
                return Err(io::Error::other(format!(
                    "{} did not power off within {} s",
                    self.name,
                    patience.as_secs()
                )));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        remove_container(&self.name)?;
        self.removed = true;
        Ok(self.exit_code())
    }
}

impl Drop for Guest {
    fn drop(&mut self) {
        if let Err(e) = self.kill() {
            eprintln!("st3b: removing {}: {e}", self.name);
        }
    }
}

/// Console text that fails C1 at the VM level beside rdashd's own ([`c1_lines`]): the guest
/// kernel's filesystem and block errors and warnings, and a panic. The init copies the kernel's
/// filesystem lines of every level from `/dev/kmsg` (`quiet` keeps warnings off the console). A
/// recovery rdashd accepts on a damaged filesystem is no recovery (S7 r1 #4, m2).
pub const VM_FORBIDDEN: [&str; 13] = [
    "EXT4-fs error",
    "EXT4-fs warning",
    // ext4's mount of a filesystem an earlier boot found damaged.
    "mounting fs with errors",
    "Remounting filesystem read-only",
    "Aborting journal",
    "Metadata corruption detected",
    "Corruption of in-memory data",
    "Corruption warning",
    "Shutting down filesystem",
    "XFS (vda): Internal error",
    "Buffer I/O error",
    "I/O error, dev vda",
    "Kernel panic",
];
/// The init's line with ext4's `errors_count` after the mount: anything but 0 fails C1.
const ERRORS_COUNT: &str = "ST3B-GUEST: ext4 errors_count ";

/// Whether a console line is a guest kernel failure: one of [`VM_FORBIDDEN`], a JBD2 error, or
/// a non-zero ext4 `errors_count`.
fn vm_forbidden(line: &str) -> bool {
    VM_FORBIDDEN.iter().any(|f| line.contains(f))
        || (line.contains("JBD2") && line.to_ascii_lowercase().contains("error"))
        || line.strip_prefix(ERRORS_COUNT).is_some_and(|n| n.trim() != "0")
}

/// The lines of a guest's console that fail C1: rdashd's ([`c1_lines`]) and the kernel's
/// (see `vm_forbidden`).
pub fn vm_c1_lines(console: &str) -> Vec<String> {
    let mut lines = c1_lines(console);
    lines.extend(console.lines().filter(|l| vm_forbidden(l)).map(|l| format!("C1: {l}")));
    lines
}

/// The code of the console's `ST3B-GUEST: rdashd exited <code>` line.
pub fn exit_code(console: &str) -> Option<i32> {
    console.lines().find_map(|l| l.strip_prefix(EXITED)?.trim().parse().ok())
}

/// A growing file read as whole lines: the console. A line still being written (or cut by the
/// kill) is never returned.
#[derive(Debug)]
pub struct Tail {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
}

impl Tail {
    /// From the file's start.
    pub fn new(path: &Path) -> Tail {
        Tail {
            path: path.to_owned(),
            offset: 0,
            partial: Vec::new(),
        }
    }

    /// From the file's current end: lines written before are not new.
    pub fn at_end(path: &Path) -> io::Result<Tail> {
        let mut tail = Tail::new(path);
        tail.offset = fs::metadata(path)?.len();
        Ok(tail)
    }

    /// The complete lines written since the last call.
    pub fn lines(&mut self) -> io::Result<Vec<String>> {
        use std::io::{Seek, SeekFrom};
        let mut f = File::open(&self.path)?;
        f.seek(SeekFrom::Start(self.offset))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        self.offset += buf.len() as u64;
        self.partial.extend_from_slice(&buf);
        let Some(end) = self.partial.iter().rposition(|&b| b == b'\n') else {
            return Ok(Vec::new());
        };
        let rest = self.partial.split_off(end + 1);
        let done = std::mem::replace(&mut self.partial, rest);
        Ok(String::from_utf8_lossy(&done).lines().map(str::to_owned).collect())
    }
}

/// The console lines a kill is triggered on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// A split flush's partial batches, "Writing partial batch", spread over up to four (V-FLUSH).
    /// Not the final batch: a kill after it has nothing to replay, and random kills cover it
    /// (S7 r1 ruling 2).
    Flush,
    /// A split flush's first batches only: "Writing partial batch" (V-RECOVERY's first crash,
    /// which leaves blocks to replay).
    Partial,
    /// A recovery replaying blocks: "Replaying blocks" and "Rolling forward" (V-RECOVERY).
    Recovery,
    /// The `UpdateTip` lines of one reorg, the one whose old tip is at height `top` (S7 r2 N6): rdashd
    /// logs no line of its own for `DisconnectTip`, so a tip lower than the last one, from `top`,
    /// starts it, and it lasts while the tip is below `top` (its disconnects, then its connects up
    /// to the old height): `lines` lines (see [`reorg_trigger`]). Drops from any other height are
    /// another reorg's, and never match.
    Reorg {
        top: u64,
        lines: u64,
    },
}

impl Trigger {
    /// A plan skips up to this many matches less one before it fires, so kills spread over a
    /// trigger's lines rather than all hitting the first.
    /// A reorg's skip spans all its lines, so kills spread over its disconnects and its connects
    /// (S7 r2 N6).
    fn skips(self) -> u64 {
        match self {
            Trigger::Flush => 4,
            Trigger::Partial | Trigger::Recovery => 2,
            Trigger::Reorg {
                lines,
                ..
            } => lines.max(1),
        }
    }

    /// A triggered kill's jitter after its line is read: up to this many ms. A split flush's
    /// window at `-dbbatchsize=2000` (the first partial batch to the final one) is 3.5-7 ms in
    /// about 40% of W-40k's flushes at heights 12-13k (plain disk, S7 r4), so V-FLUSH's trigger
    /// and V-RECOVERY's first-crash trigger jitter by at most [`FLUSH_JITTER_MS`]. Their skip
    /// covers only a flush's first partial batches (up to 4 for V-FLUSH, 2 for V-RECOVERY); the
    /// kill latency places the rest, so kills cluster early in a long flush. A replay and a reorg
    /// last longer and keep [`JITTER_MS`].
    pub fn jitter_ms(self) -> u64 {
        match self {
            Trigger::Flush | Trigger::Partial => FLUSH_JITTER_MS,
            Trigger::Recovery
            | Trigger::Reorg {
                ..
            } => JITTER_MS,
        }
    }
}

/// The height of an `UpdateTip` line.
fn update_tip_height(line: &str) -> Option<u64> {
    let rest = &line[line.find("UpdateTip: new best=")?..];
    let h = &rest[rest.find(" height=")? + " height=".len()..];
    h.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// Matches console lines against a [`Trigger`].
#[derive(Debug)]
pub struct Matcher {
    trigger: Trigger,
    /// The last `UpdateTip` height, or the boot's starting height before the first.
    last: Option<u64>,
    /// In the target reorg: the tip's height before it.
    reorg_top: Option<u64>,
}

impl Matcher {
    pub fn new(trigger: Trigger) -> Matcher {
        Matcher::new_at(trigger, None)
    }

    /// A matcher on a boot that starts with its tip at height `start`: rdashd logs no `UpdateTip`
    /// for the tip it loads, so a reorg that is the boot's first tip change drops from `start`
    /// (S7 r2 N2).
    pub fn new_at(trigger: Trigger, start: Option<u64>) -> Matcher {
        Matcher {
            trigger,
            last: start,
            reorg_top: None,
        }
    }

    /// Whether `line` (a whole line) is one the trigger fires on.
    pub fn matches(&mut self, line: &str) -> bool {
        let has = |s: &str| line.contains(s);
        match self.trigger {
            Trigger::Flush | Trigger::Partial => has("Writing partial batch"),
            Trigger::Recovery => has("Replaying blocks") || has("Rolling forward"),
            Trigger::Reorg {
                top: target,
                ..
            } => {
                let Some(h) = update_tip_height(line) else {
                    return false;
                };
                let last = self.last.replace(h);
                if let Some(top) = self.reorg_top {
                    // A tip back at the old height ends it, as in [`cut_mid_reorg`].
                    if h < top {
                        return true;
                    }
                    self.reorg_top = None;
                    return false;
                }
                match last {
                    Some(l) if h < l && l == target => {
                        self.reorg_top = Some(l);
                        true
                    }
                    _ => false,
                }
            }
        }
    }
}

/// The random kill time, from the workload's start.
pub const RANDOM_MS: std::ops::RangeInclusive<u64> = 1_000..=60_000;
/// A triggered kill's jitter after its line is read, for a replay or a reorg: up to this many ms
/// (see [`Trigger::jitter_ms`]).
pub const JITTER_MS: u64 = 50;
/// The same for a split flush's partial batches (V-FLUSH, V-RECOVERY's first crash), S7 r4.
pub const FLUSH_JITTER_MS: u64 = 5;

/// When a kill comes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// At this time after the workload starts.
    At(Duration),
    /// After `skip` matches of the trigger, on the next, `jitter` after it is read.
    OnMatch {
        trigger: Trigger,
        skip: u64,
        jitter: Duration,
    },
}

impl Plan {
    /// A time in [`RANDOM_MS`].
    pub fn random(rng: &mut Rng) -> Plan {
        let (lo, hi) = (*RANDOM_MS.start(), *RANDOM_MS.end());
        Plan::At(Duration::from_millis(lo + rng.below(hi - lo + 1)))
    }

    /// A match of `trigger`, its skip and jitter drawn: one draw each, whatever the trigger, so
    /// the seed's later plans do not depend on the bounds ([`Rng::below`]'s rejection sampling
    /// aside, which redraws with odds of at most n/2^64).
    pub fn on(trigger: Trigger, rng: &mut Rng) -> Plan {
        Plan::OnMatch {
            trigger,
            skip: rng.below(trigger.skips()),
            jitter: Duration::from_millis(rng.below(trigger.jitter_ms() + 1)),
        }
    }
}

/// A kill that happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kill {
    /// From the killer's start to the kill.
    pub after: Duration,
    /// The line that triggered it.
    pub line: Option<String>,
    /// When `armed` was set, just before the signal.
    pub armed_at: Instant,
    /// Whether the signal reached a running QEMU: false if the guest had stopped by itself first
    /// (S7 r1 m3).
    pub landed: bool,
}

/// A thread carrying out one [`Plan`] on a guest: it watches the console (or the clock), sets
/// `armed`, then cuts the power ([`Qemu::kill`]). Dropped without [`Killer::finish`] (a panic), it
/// is told to stop.
pub struct Killer {
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<io::Result<Option<Kill>>>>,
}

impl Drop for Killer {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

impl Killer {
    /// Watches `guest`'s console from `tail` (lines already read are not triggers); the boot's tip
    /// started at height `height` (see [`Matcher::new_at`]).
    pub fn spawn(
        guest: &Guest,
        mut tail: Tail,
        plan: Plan,
        height: Option<u64>,
        armed: Arc<AtomicBool>,
    ) -> Killer {
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&cancel);
        let qemu = guest.qemu.clone();
        let handle = std::thread::spawn(move || {
            let start = Instant::now();
            let mut seen = 0;
            let mut matcher = match plan {
                Plan::OnMatch {
                    trigger,
                    ..
                } => Some(Matcher::new_at(trigger, height)),
                Plan::At(_) => None,
            };
            let line = loop {
                if stop.load(Ordering::SeqCst) {
                    return Ok(None);
                }
                match (plan, &mut matcher) {
                    (Plan::At(at), _) if start.elapsed() >= at => break None,
                    (
                        Plan::OnMatch {
                            skip,
                            jitter,
                            ..
                        },
                        Some(m),
                    ) => {
                        let mut hit = None;
                        for l in tail.lines()? {
                            if m.matches(&l) {
                                if seen == skip {
                                    hit = Some(l);
                                    break;
                                }
                                seen += 1;
                            }
                        }
                        if let Some(l) = hit {
                            std::thread::sleep(jitter);
                            break Some(l);
                        }
                    }
                    _ => {}
                }
                std::thread::sleep(TAIL_POLL);
            };
            // Finished during the jitter: no kill after `finish` (S7 r1 #8).
            if stop.load(Ordering::SeqCst) {
                return Ok(None);
            }
            armed.store(true, Ordering::SeqCst);
            let armed_at = Instant::now();
            let landed = qemu.as_deref().map_or(Ok(false), Qemu::kill)?;
            Ok(Some(Kill {
                after: start.elapsed(),
                line,
                armed_at,
                landed,
            }))
        });
        Killer {
            cancel,
            handle: Some(handle),
        }
    }

    /// Stops watching and returns the kill, if one happened. A kill that came before this call
    /// stands, whatever the workload did after it.
    pub fn finish(mut self) -> io::Result<Option<Kill>> {
        self.cancel.store(true, Ordering::SeqCst);
        let handle = self.handle.take().expect("finish runs once");
        handle.join().map_err(|_| io::Error::other("the killer panicked"))?
    }
}

/// The floor after a workload: its last acknowledgement's trajectory position, or the floor before
/// it when it made none. Every acknowledgement that returned is durable, the kill's timing
/// regardless: `gettxoutsetinfo` answers after its flush, the guest's flush reaches QEMU as a
/// virtio flush that QEMU completes only after `fdatasync` on the host file, and a SIGKILL to QEMU
/// leaves the host standing (its submitted writes land). The cache mode is not what makes it hold
/// (`writeback` would too); the flip side is that the VM level cannot model a volatile disk cache
/// that drops unflushed writes: the device level does (S7 r1 n2).
pub fn floor_after(before: Floor, acks: &[usize]) -> Floor {
    acks.last().map_or(before, |&p| Floor::Ack(p))
}

/// The tip and every ancestor the fixture knows: the blocks the active chain has the data of (the
/// VM level has no C0 to list them, so C4 samples these).
pub fn active_chain(fixture: &Fixture, tip: &str) -> Vec<String> {
    let mut chain = vec![tip.to_owned()];
    let mut at = tip;
    while let Some(prev) = fixture.rows.get(at).and_then(|r| r.prev.as_deref()) {
        chain.push(prev.to_owned());
        at = prev;
    }
    chain
}

/// rdashd's line when it starts replaying blocks a crash left between a flush's batches.
pub const REPLAYING: &str = "Replaying blocks";

/// The trajectory's reorgs as `(top, end)`: `top` the last position before the tip's height
/// drops, `end` the first after it whose height is above `top`'s (the block whose arrival set off
/// the reorg). The positions strictly between are the reorg's: its disconnects, then its connects
/// up to the old height.
pub fn reorg_windows(fixture: &Fixture) -> Vec<(usize, usize)> {
    let height = |i: usize| height_at(fixture, i);
    let mut windows = Vec::new();
    let mut i = 1;
    while i < fixture.trajectory.len() {
        match (height(i - 1), height(i)) {
            (Some(top), Some(h)) if h < top => {
                let end = (i..fixture.trajectory.len())
                    .find(|&j| height(j).is_some_and(|h| h > top))
                    .unwrap_or(fixture.trajectory.len());
                windows.push((i - 1, end));
                i = end + 1;
            }
            _ => i += 1,
        }
    }
    windows
}

/// The reorg (see [`reorg_windows`]) whose positions hold `pos`, if any.
pub fn reorg_at(windows: &[(usize, usize)], pos: usize) -> Option<(usize, usize)> {
    windows.iter().copied().find(|&(top, end)| top < pos && pos < end)
}

/// The height of the trajectory's block at `pos`.
pub fn height_at(fixture: &Fixture, pos: usize) -> Option<u64> {
    fixture.trajectory.get(pos).and_then(|h| fixture.rows.get(h)).map(|r| r.height)
}

/// The trigger of a kill in reorg `window` (see [`reorg_windows`]): its old tip's height, and the
/// `UpdateTip` lines it logs below that height (its disconnects, then its connects up to the old
/// height), the trajectory's positions after the top up to the first back at the top's height.
pub fn reorg_trigger(fixture: &Fixture, (top, end): (usize, usize)) -> Option<Trigger> {
    let top_height = height_at(fixture, top)?;
    let lines =
        (top + 1..end).take_while(|&i| height_at(fixture, i).is_some_and(|h| h < top_height));
    Some(Trigger::Reorg {
        top: top_height,
        lines: lines.count() as u64,
    })
}

/// Whether a killed boot's console shows a reorg cut mid-way: a tip height drop was seen and no
/// `UpdateTip` at or above the height before the drop came after it. The boot's tip started at
/// height `start`: rdashd logs no `UpdateTip` for the tip it loads, so a reboot whose first tip
/// change is the reorg's first disconnect drops from there (S7 r2 N2).
///
/// That is V-REORG's window (S7 r1 ruling 1) with the host's bound (S7 r2 N3, see `scenario`):
/// rdashd, like Core, flushes nothing inside a reorg at W-small's heights and `-dbcache=4`, so the
/// node always comes back at the last flushed state before it, and what the VM level tests is that
/// a power cut mid-reorg recovers there with no half-applied reorg. A durable state inside a reorg
/// is the device level's (D-REORG-S checks every flush and FUA point); C3 finding the node strictly
/// inside a reorg is reported apart, the stronger case, and holds only with a flush inside the
/// reorg on the killed boot's console (see [`flushed_inside_reorg`]).
pub fn cut_mid_reorg(console: &str, start: Option<u64>) -> bool {
    let (mut last, mut top) = (start, None);
    for h in console.lines().filter_map(update_tip_height) {
        match (top, last) {
            (Some(t), _) if h >= t => top = None,
            (None, Some(l)) if h < l => top = Some(l),
            _ => {}
        }
        last = Some(h);
    }
    top.is_some()
}

/// rdashd's coins flush lines (`-debug=coindb`): the final batch, and the commit after it.
pub const FLUSHED: [&str; 2] = ["Writing final batch", "to coin database"];

/// Whether the killed boot's console shows a coins flush inside the reorg whose old tip is at
/// height `top`: after its drop and before a tip back at `top` (S7 r2 N4). The boot's tip started
/// at height `start` (see [`cut_mid_reorg`]); `started_inside`: the boot started inside that reorg
/// (its drop came on an earlier boot), so the reorg runs from the console's start.
///
/// Nothing flushes inside a W-small reorg at `-dbcache=4`, so a node that comes back strictly inside
/// one with no such flush lost acknowledged state (its tip fell back to a disconnected block whose
/// hash the trajectory repeats) or made a state durable without a flush (S7 r1 m1).
pub fn flushed_inside_reorg(
    console: &str,
    start: Option<u64>,
    top: u64,
    started_inside: bool,
) -> bool {
    let (mut last, mut inside) = (start, started_inside);
    for line in console.lines() {
        if let Some(h) = update_tip_height(line) {
            if inside && h >= top {
                inside = false;
            } else if !inside && last == Some(top) && h < top {
                inside = true;
            }
            last = Some(h);
        } else if inside && FLUSHED.iter().any(|f| line.contains(f)) {
            return true;
        }
    }
    false
}

/// `addnode <host's 10.0.2.2>:<port> add` on the guest: the fixture's peer, as the guest reaches
/// the host's loopback through user-mode networking.
pub fn add_peer(rpc: &dyn NodeRpc, port: u16) -> Result<(), RpcError> {
    rpc.add_node(SocketAddr::from((HOST_IP, port)), "add")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unbase64(s: &str) -> Vec<u8> {
        let val = |c: u8| {
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
                .iter()
                .position(|&x| x == c)
                .expect("a base64 digit") as u32
        };
        let mut out = Vec::new();
        for chunk in s.as_bytes().chunks(4) {
            let digits: Vec<u8> = chunk.iter().copied().filter(|&c| c != b'=').collect();
            let n = digits.iter().enumerate().fold(0u32, |n, (i, &c)| n | val(c) << (18 - 6 * i));
            for i in 0..digits.len() - 1 {
                out.push((n >> (16 - 8 * i)) as u8);
            }
        }
        out
    }

    #[test]
    fn base64_matches_rfc_4648() {
        let vectors =
            [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foobar", "Zm9vYmFy")];
        for (plain, coded) in vectors {
            assert_eq!(base64(plain.as_bytes()), coded);
            assert_eq!(unbase64(coded), plain.as_bytes());
        }
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(unbase64(&base64(&all)), all);
    }

    fn fixture() -> Fixture {
        Fixture {
            mocktime: 1_700_048_600,
            chain_args: vec!["-dip3params=1000000:1000000".into()],
            ..Fixture::default()
        }
    }

    #[test]
    fn args_reach_the_guest_one_per_line() {
        let extra = ["-dbcache=4".to_owned(), "-dbbatchsize=20000".to_owned()];
        let auth = rpcauth("00112233445566778899aabbccddeeff", "pw");
        let args = guest_args(&fixture(), &extra, &auth).expect("allowed");
        for want in [
            "-regtest",
            "-connect=0",
            "-rpcbind=10.0.2.15",
            "-rpcallowip=10.0.2.2",
            "-rpcport=19998",
            "-rpcauth=st3b:00112233445566778899aabbccddeeff$7b8f189f40d30ff97690c4908b4197901df9ea1d38a3efd045054da8288c4b14",
            "-mocktime=1700048600",
            "-dip3params=1000000:1000000",
            "-debug=coindb",
        ] {
            assert!(args.iter().any(|a| a == want), "{want} in {args:?}");
        }
        // The fixture's chain, then -debug=coindb, then the caller's, last (they win).
        assert_eq!(args[args.len() - 2..], extra);
        let line = cmdline(Fs::Xfs, &args).expect("fits");
        assert!(line.starts_with("console=ttyS0 quiet panic=-1 st3b.fs=xfs st3b.args="));
        let coded = line.rsplit_once("st3b.args=").expect("args").1;
        assert_eq!(String::from_utf8(unbase64(coded)).unwrap(), args.join("\n"));
        // No password on the command line: only its HMAC (Python's hmac module agrees).
        assert!(!args.iter().any(|a| a.starts_with("-rpcpassword") || a.starts_with("-rpcuser")));
    }

    #[test]
    fn args_the_guest_cannot_take_are_refused() {
        assert!(guest_args(&fixture(), &["-rpcport=1".to_owned()], "a").is_err());
        assert!(guest_args(&fixture(), &["-datadir=/x".to_owned()], "a").is_err());
        assert!(guest_args(&fixture(), &["-rpcpassword=x".to_owned()], "a").is_err());
        assert!(encode_args(&["a\nb".to_owned()]).is_err());
        assert!(encode_args(&[String::new()]).is_err());
        let long = vec!["-x".repeat(800)];
        assert!(cmdline(Fs::Ext4, &long).is_err());
    }

    #[test]
    fn containers_are_named_for_their_run_and_boot() {
        let name = container_name("1793520000-42", 7);
        assert_eq!(name, "st3b-vm-1793520000-42-7");
        assert_eq!(container_run(&name), Some("1793520000-42"));
        assert_eq!(container_run("st3b-vm-x"), None);
        assert_eq!(container_run("st3b-vm-1-2-y"), None);
        assert_eq!(container_run("other-1-2"), None);
        let ps = "st3b-vm-1-2-0\t1-2\trunning\nst3b-vm-9-9-3\t9-9\texited\nmanual\tmanual\t\
                  running\nst3b-vm-5-5-0\t5-5\trunning\nst3b-vm-5-5-1\t5-5\texited\n\
                  st3b-vm-5-5-2\t5-5\n";
        let all = parse_ps(ps);
        assert_eq!(all.len(), 6);
        assert!(all[0].live && !all[1].live && all[5].live);
        // A live run's containers stay; a dead one's and a label that is no run id go; a running
        // orphan's stays and is reported (an unknown state counts as running), an exited one's
        // goes (S7 r2 N-e).
        let state = |run: &str| {
            Ok(match run {
                "1-2" => RunState::Alive,
                "5-5" => RunState::Orphan,
                _ => RunState::Dead,
            })
        };
        let (stale, orphans) = stale_containers(all, state).unwrap();
        assert_eq!(stale, ["st3b-vm-9-9-3", "manual", "st3b-vm-5-5-1"]);
        assert_eq!(orphans.len(), 2);
        assert!(orphans[0].starts_with("container st3b-vm-5-5-0 5-5: "), "{orphans:?}");
        assert!(orphans[1].starts_with("container st3b-vm-5-5-2 5-5: "), "{orphans:?}");
        assert_eq!(run_state("manual").unwrap(), RunState::Dead);
        assert_eq!(run_state("../x").unwrap(), RunState::Dead);
        assert_eq!(run_state("1-999999999").unwrap(), RunState::Orphan);
    }

    #[test]
    fn a_boot_is_a_labelled_capped_container_on_an_odirect_disk() {
        let vm = Vm {
            image: "st3b-qemu:abc".into(),
            kernel: "/work/scratch/st3b/vm/vmlinuz-7".into(),
            initramfs: "/work/scratch/st3b/vm/g/initramfs.cpio.gz".into(),
            manifest: String::new(),
            run: "9-1".into(),
            lock: "/work/scratch/st3b/run/9-1/lock".into(),
            kvm_gid: 991,
            user: "1001:1001".into(),
            password: "pw".into(),
            salt: "salt".into(),
        };
        assert!(!format!("{vm:?}").contains("pw"));
        let dir = Path::new("/work/scratch/st3b/run/9-1/lap0");
        let disk = Disk {
            path: dir.join("data.raw"),
        };
        let args = vm.docker_args(3, dir, &disk, &dir.join("console-3.log"), "CMDLINE", 40000);
        let line = args.join(" ");
        for want in [
            "run -d --name st3b-vm-9-1-3 --label st3b.run=9-1",
            "--device /dev/kvm --group-add 991 --user 1001:1001 --network host",
            "--cpus=2 --memory=2g --blkio-weight=100 -v",
            "-v /work/scratch/st3b/run/9-1/lap0:/work/scratch/st3b/run/9-1/lap0 ",
            "-v /work/scratch/st3b/vm/vmlinuz-7:/work/scratch/st3b/vm/vmlinuz-7:ro",
            "-v /work/scratch/st3b/run/9-1/lock:/work/scratch/st3b/run/9-1/lock:ro",
            "/work/scratch/st3b/run/9-1/lock qemu-system-x86_64 -enable-kvm -m 1024 -smp 2 -cpu host -no-reboot",
            "-append CMDLINE",
            "file=/work/scratch/st3b/run/9-1/lap0/data.raw,format=raw,if=virtio,cache=none,aio=native",
            "hostfwd=tcp:127.0.0.1:40000-10.0.2.15:19998",
            "-serial file:/work/scratch/st3b/run/9-1/lap0/console-3.log",
        ] {
            assert!(line.contains(want), "{want:?} in {line}");
        }
        // The kernel command line is one argument; QEMU runs under the lock watcher.
        assert_eq!(args[args.iter().position(|a| a == "-append").unwrap() + 1], "CMDLINE");
        let image = args.iter().position(|a| a == "st3b-qemu:abc").unwrap();
        assert_eq!(args[image + 1..image + 4], ["sh", "-c", QEMU_WRAPPER]);
        assert!(!args.iter().any(|a| a.starts_with("--cpu-shares")));
    }

    #[test]
    fn rpcauth_is_the_salted_hmac_of_the_password() {
        // Python: hmac.new(b"<salt>", b"pw", hashlib.sha256).hexdigest()
        assert_eq!(
            rpcauth("00112233445566778899aabbccddeeff", "pw"),
            "st3b:00112233445566778899aabbccddeeff$7b8f189f40d30ff97690c4908b4197901df9ea1d38a3efd045054da8288c4b14"
        );
    }

    #[test]
    fn proc_stat_reads_comm_and_state() {
        let (comm, state) = proc_stat(std::process::id() as i32).expect("our own stat");
        assert!(!comm.is_empty() && comm.len() <= 15, "{comm:?}");
        // Running, sleeping or in a disk wait, whatever the box's load: not a zombie.
        assert!(state.is_ascii_alphabetic() && !matches!(state, 'Z' | 'X'), "{state}");
        assert!(!is_qemu(std::process::id() as i32));
        // No such pid (above pid_max): nothing to pin; and no process but QEMU is pinned.
        assert!(Qemu::open(i32::MAX).unwrap().is_none());
        assert!(Qemu::open(std::process::id() as i32).unwrap().is_none());
        assert_eq!(qemu_child(i32::MAX), None);
    }

    #[test]
    fn the_image_tag_follows_the_dockerfile() {
        let tag = image_tag();
        assert!(tag.starts_with("st3b-qemu:") && tag.len() == "st3b-qemu:".len() + 12);
        assert_ne!(image_tag_of(b"FROM x\n"), tag);
        assert_eq!(image_tag_of(b""), "st3b-qemu:e3b0c44298fc");
    }

    #[test]
    fn the_manifest_names_the_kernel_and_initramfs() {
        let m = "abc  ./init\nkernel 9f55 /work/scratch/st3b/vm/vmlinuz-7\ninitramfs 1cd9 initramfs.cpio.gz\n";
        assert_eq!(manifest_entry(m, "kernel"), Some(("9f55", "/work/scratch/st3b/vm/vmlinuz-7")));
        assert_eq!(manifest_entry(m, "initramfs"), Some(("1cd9", "initramfs.cpio.gz")));
        assert_eq!(manifest_entry(m, "rdashd"), None);
    }

    #[test]
    fn c1_fails_on_the_guest_kernels_filesystem_errors() {
        let console = "ST3B-GUEST: kmsg EXT4-fs (vda): recovery complete\n\
                       ST3B-GUEST: ext4 errors_count 0\n\
                       ST3B-GUEST: kmsg EXT4-fs error (device vda): ext4_lookup:1: inode #2\n\
                       ST3B-GUEST: kmsg XFS (vda): Metadata corruption detected at xfs_buf_ioend\n\
                       ST3B-GUEST: kmsg Buffer I/O error on dev vda, logical block 7\n\
                       ST3B-GUEST: kmsg EXT4-fs (vda): warning: mounting fs with errors, running e2fsck\n\
                       ST3B-GUEST: kmsg JBD2: Error -5 detected when updating journal superblock\n\
                       ST3B-GUEST: ext4 errors_count 3\n\
                       t UpdateTip: new best=00ab height=7\n\
                       t Error opening block database\n";
        let lines = vm_c1_lines(console);
        assert_eq!(lines.len(), 7, "{lines:?}");
        assert!(lines.iter().all(|l| l.starts_with("C1: ")));
        for fine in [
            "ST3B-GUEST: kmsg XFS (vda): Ending recovery (logdev: internal)\n",
            "ST3B-GUEST: kmsg jbd2_journal_init: JBD2 journal started\n",
            "ST3B-GUEST: ext4 errors_count 0\n",
        ] {
            assert!(vm_c1_lines(fine).is_empty(), "{fine}");
        }
    }

    #[test]
    fn the_exit_code_comes_from_the_init_line() {
        assert_eq!(exit_code("x\nST3B-GUEST: rdashd exited 0\n"), Some(0));
        assert_eq!(exit_code("ST3B-GUEST: rdashd exited 1"), Some(1));
        assert_eq!(exit_code("ST3B-GUEST: booted\n"), None);
    }

    #[test]
    fn the_tail_returns_whole_lines_only() {
        use std::io::Write;
        let dir = tempfile::tempdir().expect("a directory");
        let path = dir.path().join("console.log");
        let mut f = File::create(&path).unwrap();
        f.write_all(b"before\n").unwrap();
        let mut tail = Tail::at_end(&path).unwrap();
        let mut all = Tail::new(&path);
        assert_eq!(all.lines().unwrap(), ["before"]);
        f.write_all(b"one\ntw").unwrap();
        assert_eq!(tail.lines().unwrap(), ["one"]);
        assert!(tail.lines().unwrap().is_empty());
        f.write_all(b"o\nthree\nfou").unwrap();
        assert_eq!(tail.lines().unwrap(), ["two", "three"]);
        // A line the kill cut stays unread.
        assert!(tail.lines().unwrap().is_empty());
    }

    fn tip(h: u64) -> String {
        format!(
            "2026-11-01T00:00:00Z (mocktime: 1) UpdateTip: new best=ab{h} height={h} version=0x20000000 log2_work=2.0"
        )
    }

    #[test]
    fn triggers_match_their_lines() {
        let mut flush = Matcher::new(Trigger::Flush);
        let mut partial = Matcher::new(Trigger::Partial);
        let mut recovery = Matcher::new(Trigger::Recovery);
        let lines = [
            "t [coindb] Writing partial batch of 0.00 MiB",
            "t [coindb] Writing final batch of 0.00 MiB",
            "t Replaying blocks",
            "t Rolling forward 00ab (7)",
            "t Rolling back 00ab (7)",
        ];
        let hits = |m: &mut Matcher| lines.iter().map(|l| m.matches(l)).collect::<Vec<_>>();
        assert_eq!(hits(&mut flush), [true, false, false, false, false]);
        assert_eq!(hits(&mut partial), [true, false, false, false, false]);
        assert_eq!(hits(&mut recovery), [false, false, true, true, false]);
    }

    const REORG_1099: Trigger = Trigger::Reorg {
        top: 1099,
        lines: 5,
    };

    fn console(hs: &[u64]) -> String {
        hs.iter().map(|&h| tip(h) + "\n").collect()
    }

    #[test]
    fn the_reorg_trigger_spans_the_disconnects_and_connects() {
        assert_eq!(update_tip_height(&tip(1098)), Some(1098));
        assert_eq!(update_tip_height("Writing final batch"), None);
        let mut m = Matcher::new(REORG_1099);
        // 1096..1099, a 3-deep reorg back to 1096, its branch to 1100, then on: the reorg's lines
        // are those below the old top, up to the first back at it.
        let heights = [1096, 1097, 1098, 1099, 1098, 1097, 1096, 1097, 1098, 1099, 1100, 1101];
        let hits: Vec<bool> = heights.iter().map(|&h| m.matches(&tip(h))).collect();
        let want = [false, false, false, false, true, true, true, true, true, false, false, false];
        assert_eq!(hits, want);
        // Other lines leave it alone.
        assert!(!m.matches("t Writing final batch"));
        // Another reorg (a drop from 1101) is not the target's.
        assert!(!m.matches(&tip(1100)));
        assert!(!m.matches(&tip(1101)));
    }

    #[test]
    fn a_reboot_seeds_the_reorg_trigger_with_its_starting_height() {
        // The node came back at 1099 (S7 r2 N2): the boot's first UpdateTip is the first
        // disconnect, and the reorg lasts up to the first tip back at 1099.
        let heights = [1098, 1097, 1096, 1097, 1098, 1099, 1100];
        let want = [true, true, true, true, true, false, false];
        let mut m = Matcher::new_at(REORG_1099, Some(1099));
        assert_eq!(heights.iter().map(|&h| m.matches(&tip(h))).collect::<Vec<_>>(), want);
        // Unseeded, the drop is not seen at all.
        let mut m = Matcher::new(REORG_1099);
        assert!(heights.iter().all(|&h| !m.matches(&tip(h))));
    }

    #[test]
    fn only_the_target_reorgs_drop_arms_the_trigger() {
        let mut m = Matcher::new_at(
            Trigger::Reorg {
                top: 1350,
                lines: 15,
            },
            Some(1100),
        );
        // Reorg 1 (from 1100) passes unmatched; reorg 2 (from 1350) matches.
        let first = [1099, 1098, 1097, 1098, 1099, 1100, 1101];
        assert!(first.iter().all(|&h| !m.matches(&tip(h))));
        assert!(!m.matches(&tip(1349)) && !m.matches(&tip(1350)));
        assert!(m.matches(&tip(1349)) && m.matches(&tip(1342)) && m.matches(&tip(1349)));
        assert!(!m.matches(&tip(1350)));
    }

    #[test]
    fn a_reorg_is_cut_mid_way_until_a_tip_reaches_its_old_top() {
        // 1097..1100, then the reorg's disconnects and connects back toward 1100.
        assert!(!cut_mid_reorg(&console(&[1097, 1098, 1099, 1100]), None));
        assert!(cut_mid_reorg(&console(&[1099, 1100, 1099]), None));
        assert!(cut_mid_reorg(&console(&[1099, 1100, 1099, 1098, 1099]), None));
        // A tip at the old top's height ends it, as one above does.
        assert!(!cut_mid_reorg(&console(&[1099, 1100, 1099, 1098, 1099, 1100]), None));
        assert!(!cut_mid_reorg(&console(&[1100, 1098, 1101, 1102]), None));
        // A second reorg after a finished one.
        assert!(cut_mid_reorg(&console(&[1100, 1099, 1101, 1360, 1355]), None));
        assert!(!cut_mid_reorg("t Writing partial batch\n", None));
    }

    #[test]
    fn a_console_that_starts_at_the_drop_is_cut_from_the_boots_height() {
        // A reboot at 1100 whose first UpdateTip is the reorg's first disconnect (S7 r2 N2): the
        // old top is 1100, so 1099b is still mid-way and 1100b ends it.
        let killed = [1099, 1098, 1097, 1098, 1099];
        assert!(cut_mid_reorg(&console(&killed), Some(1100)));
        assert!(!cut_mid_reorg(&console(&[1099, 1098, 1097, 1098, 1099, 1100]), Some(1100)));
        // Unseeded, it took 1099 for the old top and ended the reorg early.
        assert!(!cut_mid_reorg(&console(&killed), None));
        // No tip change at all: nothing cut.
        assert!(!cut_mid_reorg("", Some(1100)));
    }

    #[test]
    fn a_flush_counts_only_inside_the_reorg() {
        let lines = |items: &[&str]| items.iter().map(|l| format!("{l}\n")).collect::<String>();
        let (t1100, t1099, t1098, t1097) = (tip(1100), tip(1099), tip(1098), tip(1097));
        let final_batch = "t [coindb] Writing final batch of 0.01 MiB";
        let committed =
            "t [coindb] Committed 9 changed transaction outputs (out of 9) to coin database...";
        // A flush before the drop (an acknowledgement's) or after the reorg is not inside it.
        let before = lines(&[committed, &t1099, &t1098, &t1099, &t1100, final_batch]);
        assert!(!flushed_inside_reorg(&before, Some(1100), 1100, false));
        for flush in [final_batch, committed] {
            let inside = lines(&[&t1099, &t1098, flush, &t1097]);
            assert!(flushed_inside_reorg(&inside, Some(1100), 1100, false));
            // Another reorg's drop (from 1098) is not this one's.
            assert!(!flushed_inside_reorg(&inside, Some(1098), 1100, false));
            // A boot that started inside it: from the console's start.
            let started = lines(&[flush, &t1098, &t1099, &t1100]);
            assert!(flushed_inside_reorg(&started, Some(1097), 1100, true));
            assert!(!flushed_inside_reorg(&started, Some(1097), 1100, false));
        }
        assert!(!flushed_inside_reorg(
            &lines(&["t Writing partial batch", &t1099]),
            Some(1100),
            1100,
            false
        ));
    }

    #[test]
    fn plans_are_drawn_from_the_seed_within_their_bounds() {
        let mut rng = Rng::new(7);
        let mut again = Rng::new(7);
        for _ in 0..1000 {
            let p = Plan::random(&mut rng);
            assert_eq!(p, Plan::random(&mut again));
            let Plan::At(t) = p else {
                panic!("a time")
            };
            assert!(RANDOM_MS.contains(&(t.as_millis() as u64)));
            for trigger in [Trigger::Flush, Trigger::Partial, Trigger::Recovery, REORG_1099] {
                let p = Plan::on(trigger, &mut rng);
                assert_eq!(p, Plan::on(trigger, &mut again));
                let Plan::OnMatch {
                    skip,
                    jitter,
                    ..
                } = p
                else {
                    panic!("a match")
                };
                assert!(skip < trigger.skips());
                assert!(jitter <= Duration::from_millis(trigger.jitter_ms()));
            }
        }
        assert_ne!(Plan::random(&mut Rng::new(1)), Plan::random(&mut Rng::new(2)));
    }

    #[test]
    fn a_flush_trigger_jitters_by_at_most_5_ms_a_replay_or_reorg_by_50_with_one_draw_each() {
        assert_eq!(Trigger::Flush.jitter_ms(), 5);
        assert_eq!(Trigger::Partial.jitter_ms(), 5);
        assert_eq!(Trigger::Recovery.jitter_ms(), 50);
        assert_eq!(REORG_1099.jitter_ms(), 50);
        let triggers = [Trigger::Flush, Trigger::Partial, Trigger::Recovery, REORG_1099];
        let mut max = [0u64; 4];
        for seed in 0..2000 {
            // The draw that follows a plan is the same whatever the plan's trigger: each plan
            // takes one draw for its skip and one for its jitter (rejection aside, ≤ n/2^64).
            let mut by_hand = Rng::new(seed);
            by_hand.next_u64();
            by_hand.next_u64();
            let next = by_hand.next_u64();
            for (i, &trigger) in triggers.iter().enumerate() {
                let mut rng = Rng::new(seed);
                let Plan::OnMatch {
                    jitter,
                    ..
                } = Plan::on(trigger, &mut rng)
                else {
                    panic!("a match")
                };
                assert_eq!(rng.next_u64(), next, "{trigger:?}");
                max[i] = max[i].max(jitter.as_millis() as u64);
            }
        }
        // The bounds are reached, not only respected.
        assert_eq!(max, [5, 5, 50, 50]);
    }

    #[test]
    fn the_floor_is_the_last_acknowledgement_that_returned() {
        assert_eq!(floor_after(Floor::Start, &[]), Floor::Start);
        assert_eq!(floor_after(Floor::Start, &[3, 9]), Floor::Ack(9));
        // A workload killed before its first acknowledgement keeps the floor it started at.
        assert_eq!(floor_after(Floor::Ack(9), &[]), Floor::Ack(9));
        assert_eq!(floor_after(Floor::Ack(9), &[9, 12]), Floor::Ack(12));
    }

    #[test]
    fn the_active_chain_walks_back_to_genesis() {
        use crate::crash::fixture::Row;
        let mut f = fixture();
        for i in 0..4u64 {
            let row = Row {
                hash: format!("b{i}"),
                height: i,
                prev: i.checked_sub(1).map(|p| format!("b{p}")),
                fields: serde_json::Map::new(),
            };
            f.rows.insert(row.hash.clone(), row);
        }
        assert_eq!(active_chain(&f, "b2"), ["b2", "b1", "b0"]);
        assert_eq!(active_chain(&f, "x"), ["x"]);
    }

    #[test]
    fn reorg_windows_span_the_disconnects_and_connects() {
        use crate::crash::fixture::Row;
        let mut f = fixture();
        // Heights by position: 0 1 2 3 | 2 1 2 3 | 4 5 | 4 5 | 6: a 2-deep reorg at 3 that ends
        // when height 4 arrives (position 8), then a 1-deep one at 9 that ends at 12.
        let heights = [0u64, 1, 2, 3, 2, 1, 2, 3, 4, 5, 4, 5, 6];
        for (i, &h) in heights.iter().enumerate() {
            let hash = format!("p{i}");
            f.rows.insert(
                hash.clone(),
                Row {
                    hash: hash.clone(),
                    height: h,
                    prev: None,
                    fields: serde_json::Map::new(),
                },
            );
            f.trajectory.push(hash);
        }
        let w = reorg_windows(&f);
        assert_eq!(w, [(3, 8), (9, 12)]);
        // Reorg 1's lines below its top (height 3): positions 4..=6; reorg 2's (height 5): 10.
        let lines = |w| match reorg_trigger(&f, w) {
            Some(Trigger::Reorg {
                top,
                lines,
            }) => (top, lines),
            t => panic!("{t:?}"),
        };
        assert_eq!(lines(w[0]), (3, 3));
        assert_eq!(lines(w[1]), (5, 1));
        assert_eq!(reorg_at(&w, 3), None);
        assert_eq!(reorg_at(&w, 4), Some((3, 8)));
        assert_eq!(reorg_at(&w, 7), Some((3, 8)));
        assert_eq!(reorg_at(&w, 8), None);
        assert_eq!(reorg_at(&w, 11), Some((9, 12)));
        assert_eq!(reorg_at(&w, 12), None);
    }
}
