//! The process tests' dashd/rdashd node pair harness (CH3b b4b, D225): a node of either binary
//! on its own datadir and flags, launched at a `-dbcrashratio`, with its chain and UTXO set read
//! over RPC, and the test-built fork the crash tests feed it.

use std::path::{Path, PathBuf};

use dash_args::univalue::SettingsValue as UniValue;

use super::mine::*;
use super::*;

pub const CREDENTIALS: &str = "u:p";

/// The flags every node takes, dashd or rdashd, before the profile's own.
pub const FLAGS: [&str; 5] =
    ["-blocksonly=1", "-txindex=0", "-disablewallet", "-rpcuser=u", "-rpcpassword=p"];

/// The storage profile an rdashd node runs (BL-634). `Live` is regtest's default, CH5-5's
/// live-copy profile, for which nothing is passed; `Production` is `-storageprofile=production`,
/// the mainnet and testnet profile. Every launch checks the profile it really ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Storage {
    Live,
    Production,
}

impl Storage {
    /// The value in `B4B-SUMMARY` and in messages.
    pub fn name(self) -> &'static str {
        match self {
            Storage::Live => "livecopy",
            Storage::Production => "production",
        }
    }

    /// The chainstate `OPTIONS` values that tell the profiles apart: `allow_concurrent_memtable_write`
    /// (the live-copy profile requires it off) and `disable_auto_compactions` (its `quiet` mode).
    pub fn options(self) -> [(&'static str, &'static str); 2] {
        match self {
            Storage::Live => {
                [("allow_concurrent_memtable_write", "false"), ("disable_auto_compactions", "true")]
            }
            Storage::Production => {
                [("allow_concurrent_memtable_write", "true"), ("disable_auto_compactions", "false")]
            }
        }
    }
}

/// The number of a final RocksDB options file: `OPTIONS-<digits>`. `OPTIONS-<n>.dbtmp` is a
/// temp file a cut write leaves (RocksDB never purges it), and the width of the number is a
/// minimum, so files are compared by number, not by name.
pub fn options_number(name: &str) -> Option<u64> {
    let digits = name.strip_prefix("OPTIONS-")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// The final options file with the highest number in `dir`.
pub fn newest_options(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| {
            let path = e.unwrap().path();
            Some((options_number(path.file_name()?.to_str()?)?, path))
        })
        .max_by_key(|(number, _)| *number)
        .map(|(_, path)| path)
}

/// One node: its binary, datadir and flags, and its current launch.
pub struct Node {
    pub name: &'static str,
    pub binary: PathBuf,
    pub datadir: PathBuf,
    pub flags: Vec<String>,
    /// The profile an rdashd node must run; `None` for dashd, which has none.
    pub storage: Option<Storage>,
    pub run: Option<(Running, u16)>,
    /// The debug log's length when this launch began.
    pub offset: u64,
    /// The environment every launch sets beyond [`command`]'s, such as a test-hooks seam's.
    pub env: Vec<(&'static str, String)>,
}

impl Node {
    /// A node named `name` on `binary`, its datadir 0700 in `home`, with [`FLAGS`] and `flags`.
    pub fn new(name: &'static str, binary: &Path, home: &Path, flags: &[&str]) -> Node {
        let datadir = home.join(name);
        private_dir(&datadir);
        Node {
            name,
            binary: binary.to_owned(),
            datadir,
            flags: FLAGS.iter().chain(flags).map(|f| (*f).to_owned()).collect(),
            storage: None,
            run: None,
            offset: 0,
            env: Vec::new(),
        }
    }

    /// An rdashd node on `storage`. Only its own flags carry `-storageprofile`: `FLAGS` and the
    /// profile's go to dashd too, which would reject it.
    pub fn rdashd(home: &Path, flags: &[&str], storage: Storage) -> Node {
        let mut node = Node::new("rdashd", Path::new(RDASHD), home, flags);
        if storage == Storage::Production {
            node.flags.push("-storageprofile=production".to_owned());
        }
        node.storage = Some(storage);
        node
    }

    pub fn log(&self) -> PathBuf {
        debug_log(&self.datadir)
    }

    /// The debug log's length now.
    pub fn log_len(&self) -> u64 {
        std::fs::metadata(self.log()).map_or(0, |m| m.len())
    }

    /// What this launch appended to the debug log.
    pub fn new_log(&self) -> String {
        read_since(&self.log(), Some(self.offset))
    }

    /// Starts the node at `-dbcrashratio=<ratio>` and waits until init is done. An exit during
    /// init is returned with the launch's log, and must be a simulated crash; any other exit
    /// fails the test.
    pub fn launch(&mut self, ratio: i64) -> Result<(), String> {
        assert!(self.run.is_none(), "{} runs", self.name);
        self.offset = self.log_len();
        let ratio = format!("-dbcrashratio={ratio}");
        let (binary, datadir, env) = (&self.binary, &self.datadir, &self.env);
        let home = datadir.parent().unwrap();
        let flags: Vec<&str> = self.flags.iter().map(String::as_str).collect();
        let started = try_start_on_free_port(datadir, free_port, |port| {
            let rpcport = format!("-rpcport={port}");
            let args = [&["-printtoconsole=0", &rpcport, &ratio][..], &flags].concat();
            let mut cmd = regtest_on(binary, home, datadir, &args);
            cmd.envs(env.iter().map(|(k, v)| (k, v)));
            cmd
        });
        match started {
            Ok(run) => {
                self.run = Some(run);
                self.assert_storage(&self.new_log());
                Ok(())
            }
            Err((_, out)) => {
                let log = self.new_log();
                if let Err(e) = simulated_crash(&out, &log) {
                    panic!("{} exited during init: {e}\n{log}", self.name);
                }
                // A crash in recovery comes after the database opened: the line and the
                // `OPTIONS` file are there.
                self.assert_storage(&log);
                Err(log)
            }
        }
    }

    /// An rdashd launch ran the storage profile it was asked for, seen twice: the start-up line
    /// in this launch's `log` (once for production, none for the default), and the newest
    /// `OPTIONS-*` of the chainstate it opened, whose every value of the two keys that tell the
    /// profiles apart is the profile's.
    pub fn assert_storage(&self, log: &str) {
        let Some(storage) = self.storage else {
            return;
        };
        let said = log.matches("Chainstate storage profile:").count();
        let production = log.matches(PRODUCTION_PROFILE_LINE).count();
        let expected = usize::from(storage == Storage::Production);
        assert_eq!(
            (said, production),
            (expected, expected),
            "{} on {storage:?}:\n{log}",
            self.name
        );

        let dir = net(&self.datadir).join("chainstate");
        let newest = newest_options(&dir)
            .unwrap_or_else(|| panic!("{} has no OPTIONS file in {}", self.name, dir.display()));
        // The chainstate's column families: RocksDB's own `default` one holds nothing and keeps
        // its defaults on either profile.
        let mut section = String::new();
        let mut values: Vec<(&str, &str)> = Vec::new();
        let text = std::fs::read_to_string(&newest).unwrap();
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                section = line.to_owned();
            } else if let Some((key, value)) = line.split_once('=')
                && section != r#"[CFOptions "default"]"#
            {
                values.push((key, value));
            }
        }
        for (key, want) in storage.options() {
            let found: Vec<_> = values.iter().filter(|(k, _)| *k == key).map(|(_, v)| *v).collect();
            assert!(
                !found.is_empty() && found.iter().all(|v| *v == want),
                "{} on {storage:?}: {key} is {found:?}, not {want} ({})",
                self.name,
                newest.display()
            );
        }
    }

    /// [`launch`](Self::launch), which must succeed.
    pub fn start(&mut self, ratio: i64) {
        if let Err(log) = self.launch(ratio) {
            panic!("{} crashed during init at ratio {ratio}\n{log}", self.name);
        }
    }

    /// A call that may find the node dead: its reply, or the transport's error.
    pub fn try_call(&self, method: &str, params: &str) -> Result<Reply, String> {
        let (_, port) = self.run.as_ref().unwrap_or_else(|| panic!("{} is down", self.name));
        try_rpc(*port, CREDENTIALS, method, params)
    }

    /// A call's `result`, which must come.
    pub fn call(&self, method: &str, params: &str) -> String {
        let reply = self
            .try_call(method, params)
            .unwrap_or_else(|e| panic!("{} {method} {params}: {e}\n{}", self.name, self.new_log()));
        reply.result().to_owned()
    }

    /// The node died: waits for the exit, which must be a simulated crash. Returns this launch's
    /// log.
    pub fn crashed(&mut self) -> String {
        let (running, _) = self.run.take().expect("a running node");
        let out = running.wait();
        let log = self.new_log();
        if let Err(e) = simulated_crash(&out, &log) {
            panic!("{} died: {e}\n{log}", self.name);
        }
        log
    }

    /// `stop`, then a clean exit.
    pub fn stop(&mut self) {
        self.call("stop", "[]");
        let (running, _) = self.run.take().unwrap();
        let out = running.wait();
        assert_eq!(out.status.code(), Some(0), "{}: {}", self.name, stderr(&out));
    }

    pub fn submit(&self, block: &Mined) -> String {
        self.call("submitblock", &format!(r#"["{}"]"#, block.hex))
    }

    pub fn best(&self) -> String {
        self.call("getbestblockhash", "[]")
    }

    /// `getchaintips`, ordered by height and hash.
    pub fn tips(&self) -> Vec<Tip> {
        tips(&self.call("getchaintips", "[]"))
    }

    /// `gettxoutsetinfo "muhash"`, every field but `disk_size`, or the transport's error.
    pub fn try_utxo_set(&self) -> Result<Vec<(String, String)>, String> {
        Ok(utxo_set(self.try_call("gettxoutsetinfo", r#"["muhash"]"#)?.result()))
    }

    pub fn utxo_set(&self) -> Vec<(String, String)> {
        self.try_utxo_set()
            .unwrap_or_else(|e| panic!("{} gettxoutsetinfo: {e}\n{}", self.name, self.new_log()))
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("{}'s datadir: {}", self.name, self.datadir.display());
        }
    }
}

/// One `getchaintips` object: its height and hash, and every field as (key, JSON text), ordered
/// by key. Two tips are equal only when every field is.
#[derive(Debug, PartialEq)]
pub struct Tip {
    pub height: i64,
    pub hash: String,
    pub fields: Vec<(String, String)>,
}

impl Tip {
    pub fn status(&self) -> &str {
        value_of(&self.fields, "status").trim_matches('"')
    }
}

/// `getchaintips`' reply, every object whole, ordered by height and hash.
pub fn tips(result: &str) -> Vec<Tip> {
    let Some(UniValue::Arr(tips)) = UniValue::read(result.as_bytes()) else {
        panic!("getchaintips: {result}")
    };
    let mut tips: Vec<_> = tips
        .into_iter()
        .map(|tip| {
            let UniValue::Obj(fields) = tip else {
                panic!("getchaintips: {result}")
            };
            let mut fields: Vec<_> = fields.into_iter().map(|(k, v)| (k, v.write())).collect();
            fields.sort();
            let height = value_of(&fields, "height").parse().unwrap();
            let hash = value_of(&fields, "hash").trim_matches('"').to_owned();
            Tip {
                height,
                hash,
                fields,
            }
        })
        .collect();
    tips.sort_by(|a, b| (a.height, &a.hash).cmp(&(b.height, &b.hash)));
    tips
}

/// `hash`'s status in `tips`, if it is one.
pub fn status_of<'a>(tips: &'a [Tip], hash: &[u8; 32]) -> Option<&'a str> {
    let hex = rpc_hex(hash);
    tips.iter().find(|t| t.hash == hex).map(Tip::status)
}

/// `gettxoutsetinfo`'s object as (key, JSON text), `disk_size` left out: the database's size is
/// each store's own.
pub fn utxo_set(result: &str) -> Vec<(String, String)> {
    let Some(UniValue::Obj(fields)) = UniValue::read(result.as_bytes()) else {
        panic!("gettxoutsetinfo: {result}")
    };
    fields.into_iter().filter(|(k, _)| k != "disk_size").map(|(k, v)| (k, v.write())).collect()
}

pub fn value_of<'a>(set: &'a [(String, String)], key: &str) -> &'a str {
    set.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str()).expect(key)
}

/// `hash` (internal byte order) as RPC shows it.
pub fn rpc_hex(hash: &[u8; 32]) -> String {
    let mut h = *hash;
    h.reverse();
    hex(&h)
}

/// [`rpc_hex`], quoted: a JSON string.
pub fn shown(hash: &[u8; 32]) -> String {
    format!("\"{}\"", rpc_hex(hash))
}

/// The genesis block's hash, in internal byte order, from `node`.
pub fn genesis(node: &Node) -> [u8; 32] {
    let shown = node.call("getblockhash", "[0]");
    let mut hash: [u8; 32] = parse_hex(shown.trim_matches('"')).try_into().unwrap();
    hash.reverse();
    hash
}

/// The lines of `log` that start with one of `prefixes` once their time is cut.
pub fn lines_of(log: &str, prefixes: &[&str]) -> Vec<String> {
    log.lines()
        .map(|l| l.split_once(' ').map_or(l, |(_, text)| text))
        .filter(|l| prefixes.iter().any(|p| l.starts_with(p)))
        .map(str::to_owned)
        .collect()
}

/// Recovery's lines, Core's `ReplayBlocks`' (`validation.cpp:5243`, `:5280`, `:5299`).
pub fn replay_lines(log: &str) -> Vec<String> {
    lines_of(log, &["Replaying blocks", "Rolling back", "Rolling forward"])
}

/// One block on `prev` at `height`, a minute per height after genesis: a coinbase tagged `tag`
/// (each branch its own) with `outputs` zero-value outputs, then `spend` if any.
pub fn block(
    prev: &[u8; 32],
    height: u16,
    tag: &[u8],
    outputs: usize,
    spend: Option<Vec<u8>>,
) -> Mined {
    let txs = std::iter::once(coinbase_paying(height, tag, &vec![0; outputs])).chain(spend);
    mine_txs(prev, GENESIS_TIME + 60 * u32::from(height), &txs.collect::<Vec<_>>())
}

/// P1's and P2's blocks: F1..F101 coinbase only; A102..A104 of 40 outputs; B102..B105 of 40
/// outputs and a spend of F(h − 100)'s coinbase and of its parent's spend.
pub struct Fork {
    pub f: Vec<Mined>,
    pub a: Vec<Mined>,
    pub b: Vec<Mined>,
}

impl Fork {
    pub fn new(genesis: &[u8; 32]) -> Fork {
        let mut f: Vec<Mined> = Vec::new();
        let mut coinbases = Vec::new();
        for height in 1..=101 {
            let prev = f.last().map_or(genesis, |b| &b.hash);
            let cb = coinbase_paying(height, b"f", &[0]);
            coinbases.push(txid(&cb));
            f.push(mine_txs(prev, GENESIS_TIME + 60 * u32::from(height), &[cb]));
        }
        let mut a: Vec<Mined> = Vec::new();
        for height in 102..=104 {
            let prev = a.last().map_or(&f[100].hash, |b| &b.hash);
            a.push(block(prev, height, b"a", 40, None));
        }
        let (mut b, mut parent): (Vec<Mined>, Option<[u8; 32]>) = (Vec::new(), None);
        for height in 102..=105 {
            let prev = b.last().map_or(&f[100].hash, |b| &b.hash);
            let mut inputs = vec![(coinbases[usize::from(height) - 101], 0)];
            inputs.extend(parent.map(|txid| (txid, 0)));
            let spend = spend(&inputs, &[0, 0, 0]);
            parent = Some(txid(&spend));
            b.push(block(prev, height, b"b", 40, Some(spend)));
        }
        Fork {
            f,
            a,
            b,
        }
    }
}
