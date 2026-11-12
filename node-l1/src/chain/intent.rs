//! Inputs (O2b §3.2, D10): a header byte, then at most [`MAX_INTENTS`] intents. Every byte
//! string decodes: a tag is taken modulo the tag count, an exhausted input reads zeros, and
//! every parameter is reduced where it is used. [`Program`]'s text form (`tests/chain_seeds`)
//! encodes to the bytes that decode to it, so seed programs, not bytes, are committed (§3.5).
//!
//! Two encodings (O2b-4 §3a, D55), each a total decoder of its own, so a byte string means
//! something different under each and a corpus belongs to one of them:
//! - `chain-v1` ([`Program::decode_v1`]), `diff_chain`'s, kept unchanged for good: 17 tags.
//! - `chain-v2` ([`Program::decode_v2`]), the Diff targets': v1's 17 tags with the same layouts
//!   and meanings, then [`Intent::MutateX`] and [`Intent::ProposeX`], which name a slot of the
//!   append-only [`BANK`].

use std::fmt::{self, Write as _};
use std::path::Path;

use bitcoin_hashes::sha256;

use super::mutate::{BANK, BANK_CAPACITY, OPERATORS};

/// At most this many intents per input (O2b §6.3).
pub const MAX_INTENTS: usize = 48;
/// At most this many input bytes (O2b §6.3).
pub const MAX_INPUT: usize = 64 * 1024;

/// A block reference (O2b D26): the top two bits pick the list, the low six the index in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ref(pub u8);

/// What a [`Ref`] names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    /// The active chain, `k` blocks below the tip.
    Active(u8),
    /// The `k`-th block the input built (oldest first, modulo the count).
    Built(u8),
    /// The tip of fork branch `k`.
    Branch(u8),
    /// A hash no block has (63) or the `k`-th block the input built that the node never got.
    Unsent(u8),
    Random,
}

impl Ref {
    pub fn kind(self) -> RefKind {
        let k = self.0 & 63;
        match self.0 >> 6 {
            0 => RefKind::Active(k),
            1 => RefKind::Built(k),
            2 => RefKind::Branch(k),
            _ if k == 63 => RefKind::Random,
            _ => RefKind::Unsent(k),
        }
    }

    pub fn of(kind: RefKind) -> Ref {
        Ref(match kind {
            RefKind::Active(k) => k & 63,
            RefKind::Built(k) => 64 | (k & 63),
            RefKind::Branch(k) => 128 | (k & 63),
            RefKind::Unsent(k) => 192 | (k & 63).min(62),
            RefKind::Random => 255,
        })
    }
}

/// `EXTEND`'s flags.
pub const HEADERS_FIRST: u8 = 1;
/// Build and keep the blocks (with `HEADERS_FIRST`, send only their headers).
pub const HOLD: u8 = 2;
/// Signal testdummy (bit 28).
pub const SIGNAL: u8 = 4;
/// Bits 3–4: the transactions (none, spends, a fan-out, a chain).
pub const SHAPE_SHIFT: u8 = 3;
/// `Deliver`'s branch byte: the top bit sends the held blocks highest first (each block
/// before its parent: `prev-blk-not-found`), the rest names the branch.
pub const DELIVER_REVERSED: u8 = 0x80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxShape {
    None,
    Spends,
    FanOut,
    Chain,
}

pub fn shape(flags: u8) -> TxShape {
    match (flags >> SHAPE_SHIFT) & 3 {
        0 => TxShape::None,
        1 => TxShape::Spends,
        2 => TxShape::FanOut,
        _ => TxShape::Chain,
    }
}

/// One intent (O2b §3.2's table). Counts and depths are raw bytes, reduced on use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    /// `(n - 1) % 8 + 1` blocks (wrapping: 0 is 8) on branch `branch` (0: the active chain).
    Extend {
        branch: u8,
        n: u8,
        flags: u8,
    },
    /// A branch from the active chain `depth % 12` below its tip, `(n - 1) % 8 + 1` blocks
    /// long.
    Fork {
        depth: u8,
        n: u8,
        flags: u8,
    },
    /// Apply operator `op % OPERATORS.len()` to the next block built.
    Mutate {
        op: u8,
    },
    /// A proposal: of a new block on `on` (with operator `mutate - 1` if nonzero), or with
    /// `again`, of `on`'s own bytes again.
    Propose {
        on: Ref,
        mutate: u8,
        again: bool,
    },
    Invalidate(Ref),
    Reconsider(Ref, bool),
    Precious(Ref),
    /// Record a ChainLock on the referenced block.
    Cl(Ref),
    ClEnforce,
    /// Spork 19 on or off (a counted no-op until D34's experiment admits sporks, O2b-3 P5).
    Spork {
        on: bool,
    },
    /// `FLUSH` mode `% 3` (always, if needed, periodic).
    Flush(u8),
    /// `RESTART` clean (even) or kill (odd).
    Restart(u8),
    /// The clock forward by `(n + 1) × 2` minutes.
    Time(u8),
    /// The clock backward by `(n + 1) × 2` minutes.
    Warp(u8),
    /// A `BLOCK` of fuzz bytes (one per input).
    Raw(Vec<u8>),
    Activate,
    /// Send the held blocks of branch `branch & 0x7f` (0: the active chain's), lowest first,
    /// or highest first with [`DELIVER_REVERSED`] (`deliver b1 rev`).
    Deliver {
        branch: u8,
    },
    /// `chain-v2` only: apply the [`BANK`] builder in `slot` (below [`BANK_CAPACITY`]) to the
    /// next block built. A slot the bank does not fill yet is refused (`unknown_slot`).
    MutateX {
        slot: u8,
    },
    /// `chain-v2` only: [`Intent::Propose`] with the [`BANK`] builder in `slot`.
    ProposeX {
        on: Ref,
        slot: u8,
        again: bool,
    },
}

/// A corpus encoding (O2b-4 §3a.2): which decoder an input's bytes are read with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Encoding {
    /// `chain-v1`: [`Program::decode_v1`].
    V1,
    /// `chain-v2`: [`Program::decode_v2`].
    V2,
}

impl Encoding {
    /// The identity the registry, `node-l1/chain-encodings.toml` and the evidence name.
    pub fn name(self) -> &'static str {
        match self {
            Encoding::V1 => "chain-v1",
            Encoding::V2 => "chain-v2",
        }
    }

    pub fn parse(name: &str) -> Option<Encoding> {
        [Encoding::V1, Encoding::V2].into_iter().find(|e| e.name() == name)
    }

    fn tags(self) -> u8 {
        match self {
            Encoding::V1 => 17,
            Encoding::V2 => 19,
        }
    }
}

/// `node-l1/chain-encodings.toml`: each chain target's name and encoding, in file order.
///
/// # Panics
/// On a line that is not `name = "encoding"` with a known encoding: the file is the crate's own.
pub fn chain_encodings() -> Vec<(&'static str, Encoding)> {
    include_str!("../../chain-encodings.toml")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let (name, value) =
                l.split_once('=').unwrap_or_else(|| panic!("not `name = value`: {l}"));
            let value = value.trim().trim_matches('"');
            (name.trim(), Encoding::parse(value).unwrap_or_else(|| panic!("no encoding {value}")))
        })
        .collect()
}

/// Whether chain target `target` may start under `value`, its process's
/// `NODE_L1_CHAIN_ENCODING` (O2b-4 §3a.3 (1)): only if it names `target`'s encoding in
/// `chain-encodings.toml`. Unset, empty, another encoding, or a target the file does not list is
/// refused, naming what was expected.
pub fn check_env(target: &str, value: Option<&str>) -> Result<(), String> {
    let Some((_, encoding)) = chain_encodings().into_iter().find(|(n, _)| *n == target) else {
        return Err(format!("{target} is not in node-l1/chain-encodings.toml"));
    };
    let want = encoding.name();
    match value.filter(|v| !v.is_empty()) {
        Some(v) if v == want => Ok(()),
        Some(v) => Err(format!("NODE_L1_CHAIN_ENCODING is {v}, but {target}'s encoding is {want}")),
        None => Err(format!("NODE_L1_CHAIN_ENCODING is unset, but {target}'s encoding is {want}")),
    }
}

/// The planted searches' chain targets (O2b-4 4b), compiled only with `planted`; named here,
/// in every build, so that a build without them can tell their `chain-encodings.toml` rows apart.
pub const PLANTED_CHAIN_TARGETS: [&str; 2] = ["diff_chain_par_p3a", "diff_chain_p3b"];

/// The seed programs a planted search starts without (O2b-4 4b, Q1 (A)): the witnesses of its
/// bug, which would hand it the divergence it is to find.
pub const PLANT_SEARCH_EXCLUDES: [&str; 2] = ["w_p3a", "w_p3b"];

/// The seed programs `target`'s public seeds leave out, by file stem: [`PLANT_SEARCH_EXCLUDES`]
/// for a planted search, none for any other target.
pub fn seed_excludes(target: &str) -> &'static [&'static str] {
    if PLANTED_CHAIN_TARGETS.contains(&target) {
        &PLANT_SEARCH_EXCLUDES
    } else {
        &[]
    }
}

/// The public seed files the programs in `progs` (`*.prog`, but those whose stem `excludes`
/// names) compile to under `encoding` (O2b §3.5, O2b-4 §3a.2): each program's canonical bytes,
/// named by the first 8 bytes of their SHA-256 in hex; sorted by name.
pub fn compile_seeds(
    progs: &Path,
    encoding: Encoding,
    excludes: &[&str],
) -> std::io::Result<Vec<(String, Vec<u8>)>> {
    let mut seeds = Vec::new();
    for e in std::fs::read_dir(progs)? {
        let path = e?.path();
        if path.extension().is_none_or(|e| e != "prog") {
            continue;
        }
        if path.file_stem().and_then(|s| s.to_str()).is_some_and(|s| excludes.contains(&s)) {
            continue;
        }
        let program = Program::parse(&std::fs::read_to_string(&path)?)
            .map_err(|e| std::io::Error::other(format!("{}: {e}", path.display())))?;
        let bytes = program.encode(encoding);
        let digest = sha256::Hash::hash(&bytes).to_byte_array();
        seeds.push((digest[..8].iter().map(|b| format!("{b:02x}")).collect(), bytes));
    }
    seeds.sort();
    Ok(seeds)
}

/// Chain target `target`'s public seed files: [`compile_seeds`] of the shared programs in `progs`,
/// of its encoding's in `progs/<encoding>/` (O2b-4 4f2: `chain-v2`'s spork programs) and of its
/// own in `progs/<target>/`, each if there are any (OD-L3-96 Q2), but those [`seed_excludes`]
/// names, sorted by name. Another target's or encoding's programs never reach it. A program that
/// compiles to an earlier directory's bytes is an error.
pub fn target_seeds(
    progs: &Path,
    target: &str,
    encoding: Encoding,
) -> std::io::Result<Vec<(String, Vec<u8>)>> {
    let excludes = seed_excludes(target);
    let mut seeds = compile_seeds(progs, encoding, excludes)?;
    for own in [progs.join(encoding.name()), progs.join(target)] {
        if !own.is_dir() {
            continue;
        }
        for seed in compile_seeds(&own, encoding, excludes)? {
            if seeds.iter().any(|(name, _)| *name == seed.0) {
                return Err(std::io::Error::other(format!(
                    "{target}: {} is a shared seed",
                    seed.0
                )));
            }
            seeds.push(seed);
        }
        seeds.sort();
    }
    Ok(seeds)
}

/// A decoded input.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Program {
    /// Start with the tip seven hours old (in IBD) instead of one.
    pub old: bool,
    /// Leg B's W schedule (OD-L3-118): the node takes W's releases only when it waits for W
    /// (`hold`), not before each flush decision (`drain`, the default). `chain-v2` only: bit 1
    /// of the first byte, which `chain-v1` ignores. Off leg B nothing reads it.
    pub w_hold: bool,
    pub intents: Vec<Intent>,
}

struct Cursor<'a>(&'a [u8]);

impl Cursor<'_> {
    fn byte(&mut self) -> u8 {
        match self.0.split_first() {
            Some((b, rest)) => {
                self.0 = rest;
                *b
            }
            None => 0,
        }
    }
}

impl Program {
    /// Decode any bytes under `encoding`.
    pub fn decode(data: &[u8], encoding: Encoding) -> Program {
        match encoding {
            Encoding::V1 => Program::decode_v1(data),
            Encoding::V2 => Program::decode_v2(data),
        }
    }

    /// Decode any bytes as `chain-v1` (D10): `diff_chain`'s decoder, unchanged since O2b-3.
    pub fn decode_v1(data: &[u8]) -> Program {
        Program::decode_tags(data, Encoding::V1.tags())
    }

    /// Decode any bytes as `chain-v2` (O2b-4 D55): `chain-v1`'s tags 0–16 with their layouts and
    /// meanings, then tag 17 `MutateX { slot }` and tag 18 `ProposeX { on, slot, again }`, a
    /// tag being the byte modulo 19. A slot is its byte modulo [`BANK_CAPACITY`]; `ProposeX`'s
    /// top bit is `again`, as `Propose`'s. Bit 1 of the first byte is [`Program::w_hold`]
    /// (OD-L3-118), a bit `chain-v1` leaves unread.
    pub fn decode_v2(data: &[u8]) -> Program {
        Program {
            w_hold: data.first().is_some_and(|b| b & 2 != 0),
            ..Program::decode_tags(data, Encoding::V2.tags())
        }
    }

    fn decode_tags(data: &[u8], tags: u8) -> Program {
        let data = &data[..data.len().min(MAX_INPUT)];
        let mut c = Cursor(data);
        let old = c.byte() & 1 == 1;
        let mut intents = Vec::new();
        while !c.0.is_empty() && intents.len() < MAX_INTENTS {
            let tag = c.byte() % tags;
            intents.push(match tag {
                0 => Intent::Extend {
                    branch: c.byte(),
                    n: c.byte(),
                    flags: c.byte(),
                },
                1 => Intent::Fork {
                    depth: c.byte(),
                    n: c.byte(),
                    flags: c.byte(),
                },
                2 => Intent::Mutate {
                    op: c.byte(),
                },
                3 => {
                    let on = Ref(c.byte());
                    let m = c.byte();
                    Intent::Propose {
                        on,
                        mutate: m & 0x7f,
                        again: m & 0x80 != 0,
                    }
                }
                4 => Intent::Invalidate(Ref(c.byte())),
                5 => {
                    let r = Ref(c.byte());
                    Intent::Reconsider(r, c.byte() & 1 == 1)
                }
                6 => Intent::Precious(Ref(c.byte())),
                7 => Intent::Cl(Ref(c.byte())),
                8 => Intent::ClEnforce,
                9 => Intent::Spork {
                    on: c.byte() & 1 == 1,
                },
                10 => Intent::Flush(c.byte()),
                11 => Intent::Restart(c.byte()),
                12 => Intent::Time(c.byte()),
                13 => Intent::Warp(c.byte()),
                14 => {
                    let n = usize::from(c.byte());
                    let take = n.min(c.0.len());
                    let (raw, rest) = c.0.split_at(take);
                    c.0 = rest;
                    Intent::Raw(raw.to_vec())
                }
                15 => Intent::Activate,
                16 => Intent::Deliver {
                    branch: c.byte(),
                },
                17 => Intent::MutateX {
                    slot: c.byte() % BANK_CAPACITY,
                },
                _ => {
                    let on = Ref(c.byte());
                    let m = c.byte();
                    Intent::ProposeX {
                        on,
                        slot: m % BANK_CAPACITY,
                        again: m & 0x80 != 0,
                    }
                }
            });
        }
        Program {
            old,
            w_hold: false,
            intents,
        }
    }

    /// Whether `chain-v1` can say the program: no `chain-v2` bank intent and no
    /// [`w_hold`](Self::w_hold).
    pub fn is_v1(&self) -> bool {
        !self.w_hold
            && !self
                .intents
                .iter()
                .any(|i| matches!(i, Intent::MutateX { .. } | Intent::ProposeX { .. }))
    }

    /// The canonical bytes under `encoding`.
    ///
    /// # Panics
    /// For `chain-v1`, if the program has no `chain-v1` encoding ([`Program::is_v1`]).
    pub fn encode(&self, encoding: Encoding) -> Vec<u8> {
        match encoding {
            Encoding::V1 => self.encode_v1(),
            Encoding::V2 => self.encode_v2(),
        }
    }

    /// The canonical `chain-v1` bytes: `decode_v1(encode_v1(p)) == p` for every program whose
    /// `Raw` payloads are at most 255 bytes.
    ///
    /// # Panics
    /// If `self` holds a `chain-v2` bank intent or [`w_hold`](Self::w_hold): a harness bug, since
    /// `chain-v1` cannot say it.
    pub fn encode_v1(&self) -> Vec<u8> {
        assert!(self.is_v1(), "a chain-v2 bank intent or w_hold has no chain-v1 encoding");
        self.encode_any()
    }

    /// The canonical `chain-v2` bytes: `decode_v2(encode_v2(p)) == p` likewise. A program with no
    /// bank intent has the same bytes under both encodings.
    pub fn encode_v2(&self) -> Vec<u8> {
        self.encode_any()
    }

    fn encode_any(&self) -> Vec<u8> {
        let mut out = vec![u8::from(self.old) | u8::from(self.w_hold) << 1];
        for i in &self.intents {
            match i {
                Intent::Extend {
                    branch,
                    n,
                    flags,
                } => out.extend([0, *branch, *n, *flags]),
                Intent::Fork {
                    depth,
                    n,
                    flags,
                } => out.extend([1, *depth, *n, *flags]),
                Intent::Mutate {
                    op,
                } => out.extend([2, *op]),
                Intent::Propose {
                    on,
                    mutate,
                    again,
                } => out.extend([
                    3,
                    on.0,
                    (mutate & 0x7f)
                        | if *again {
                            0x80
                        } else {
                            0
                        },
                ]),
                Intent::Invalidate(r) => out.extend([4, r.0]),
                Intent::Reconsider(r, ignore) => out.extend([5, r.0, u8::from(*ignore)]),
                Intent::Precious(r) => out.extend([6, r.0]),
                Intent::Cl(r) => out.extend([7, r.0]),
                Intent::ClEnforce => out.push(8),
                Intent::Spork {
                    on,
                } => out.extend([9, u8::from(*on)]),
                Intent::Flush(m) => out.extend([10, *m]),
                Intent::Restart(k) => out.extend([11, *k]),
                Intent::Time(n) => out.extend([12, *n]),
                Intent::Warp(n) => out.extend([13, *n]),
                Intent::Raw(bytes) => {
                    let n = bytes.len().min(255);
                    out.extend([14, n as u8]);
                    out.extend(&bytes[..n]);
                }
                Intent::Activate => out.push(15),
                Intent::Deliver {
                    branch,
                } => out.extend([16, *branch]),
                Intent::MutateX {
                    slot,
                } => out.extend([17, slot % BANK_CAPACITY]),
                Intent::ProposeX {
                    on,
                    slot,
                    again,
                } => out.extend([18, on.0, (slot % BANK_CAPACITY) | (u8::from(*again) << 7)]),
            }
        }
        out
    }

    /// Parse the text form: one intent per line, `#` comment lines, optional first lines `start
    /// old` and `start hold` (the grammar is [`Intent`]'s `Display`).
    pub fn parse(text: &str) -> Result<Program, String> {
        let mut p = Program::default();
        for (n, line) in text.lines().enumerate() {
            // `#` also marks indexes (`tip#1`, `mutate #3`): a comment is a line that starts
            // with it, or the rest of a line from ` # `.
            let line = line.split(" # ").next().unwrap_or_default().trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line == "start old" {
                p.old = true;
                continue;
            }
            if line == "start recent" {
                continue;
            }
            if line == "start hold" {
                p.w_hold = true;
                continue;
            }
            p.intents.push(parse_intent(line).map_err(|e| format!("line {}: {e}: {line}", n + 1))?);
        }
        if p.intents.len() > MAX_INTENTS {
            return Err(format!("{} intents (at most {MAX_INTENTS})", p.intents.len()));
        }
        Ok(p)
    }
}

impl fmt::Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.old {
            writeln!(f, "start old")?;
        }
        if self.w_hold {
            writeln!(f, "start hold")?;
        }
        for i in &self.intents {
            writeln!(f, "{i}")?;
        }
        Ok(())
    }
}

fn branch_text(b: u8) -> String {
    if b == 0 {
        "main".into()
    } else {
        format!("b{b}")
    }
}

fn flags_text(flags: u8) -> String {
    let mut s = String::new();
    if flags & HEADERS_FIRST != 0 {
        s.push_str(" headers");
    }
    if flags & HOLD != 0 {
        s.push_str(" hold");
    }
    if flags & SIGNAL != 0 {
        s.push_str(" signal");
    }
    match shape(flags) {
        TxShape::None => {}
        TxShape::Spends => s.push_str(" tx=spends"),
        TxShape::FanOut => s.push_str(" tx=fan"),
        TxShape::Chain => s.push_str(" tx=chain"),
    }
    if flags >> 5 != 0 {
        let _ = write!(s, " flags+{}", flags >> 5);
    }
    s
}

impl fmt::Display for Ref {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind() {
            RefKind::Active(k) => write!(f, "main@-{k}"),
            RefKind::Built(k) => write!(f, "built#{k}"),
            RefKind::Branch(k) => write!(f, "tip#{k}"),
            RefKind::Unsent(k) => write!(f, "unsent#{k}"),
            RefKind::Random => f.write_str("random"),
        }
    }
}

impl fmt::Display for Intent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Intent::Extend {
                branch,
                n,
                flags,
            } => write!(f, "extend {} {}{}", branch_text(*branch), n, flags_text(*flags)),
            Intent::Fork {
                depth,
                n,
                flags,
            } => write!(f, "fork {depth} {n}{}", flags_text(*flags)),
            Intent::Mutate {
                op,
            } => match OPERATORS.get(usize::from(*op)) {
                Some(o) => write!(f, "mutate {}", o.name),
                None => write!(f, "mutate #{op}"),
            },
            Intent::Propose {
                on,
                mutate,
                again,
            } => {
                write!(f, "propose {on}")?;
                if *again {
                    f.write_str(" again")?;
                }
                if *mutate != 0 {
                    match OPERATORS.get(usize::from(*mutate) - 1) {
                        Some(o) => write!(f, " {}", o.name)?,
                        None => write!(f, " #{}", mutate - 1)?,
                    }
                }
                Ok(())
            }
            Intent::Invalidate(r) => write!(f, "invalidate {r}"),
            Intent::Reconsider(r, ignore) => {
                write!(
                    f,
                    "reconsider {r}{}",
                    if *ignore {
                        " ignore_cl"
                    } else {
                        ""
                    }
                )
            }
            Intent::Precious(r) => write!(f, "precious {r}"),
            Intent::Cl(r) => write!(f, "cl {r}"),
            Intent::ClEnforce => f.write_str("cl_enforce"),
            Intent::Spork {
                on,
            } => write!(
                f,
                "spork {}",
                if *on {
                    "on"
                } else {
                    "off"
                }
            ),
            Intent::Flush(m) => match m {
                0 => f.write_str("flush always"),
                1 => f.write_str("flush if_needed"),
                2 => f.write_str("flush periodic"),
                m => write!(f, "flush #{m}"),
            },
            Intent::Restart(k) => match k {
                0 => f.write_str("restart clean"),
                1 => f.write_str("restart kill"),
                k => write!(f, "restart #{k}"),
            },
            Intent::Time(n) => write!(f, "time {n}"),
            Intent::Warp(n) => write!(f, "warp {n}"),
            Intent::Raw(bytes) => {
                f.write_str("raw ")?;
                bytes.iter().try_for_each(|b| write!(f, "{b:02x}"))
            }
            Intent::Activate => f.write_str("activate"),
            Intent::Deliver {
                branch,
            } => {
                write!(f, "deliver {}", branch_text(branch & !DELIVER_REVERSED))?;
                if branch & DELIVER_REVERSED != 0 {
                    f.write_str(" rev")?;
                }
                Ok(())
            }
            Intent::MutateX {
                slot,
            } => write!(f, "mutatex {}", slot_text(*slot)),
            Intent::ProposeX {
                on,
                slot,
                again,
            } => {
                write!(f, "proposex {on}")?;
                if *again {
                    f.write_str(" again")?;
                }
                write!(f, " {}", slot_text(*slot))
            }
        }
    }
}

/// A bank slot's text: its builder's name, or `#slot` for a slot the bank does not fill.
fn slot_text(slot: u8) -> String {
    match BANK.get(usize::from(slot)) {
        Some(o) => o.name.to_owned(),
        None => format!("#{slot}"),
    }
}

/// A bank slot from its text: a builder's name, or `#slot` below [`BANK_CAPACITY`].
fn bank_slot(s: &str) -> Result<u8, String> {
    if let Some(n) = s.strip_prefix('#') {
        return n
            .parse()
            .ok()
            .filter(|&n| n < BANK_CAPACITY)
            .ok_or(format!("not a bank slot (0..{BANK_CAPACITY}): {s}"));
    }
    BANK.iter().position(|o| o.name == s).map(|i| i as u8).ok_or(format!("no bank builder {s}"))
}

fn num(s: Option<&str>) -> Result<u8, String> {
    let s = s.ok_or("a number is missing")?;
    let s = s.strip_prefix('#').unwrap_or(s);
    s.parse().map_err(|_| format!("not a byte: {s}"))
}

fn branch(s: Option<&str>) -> Result<u8, String> {
    match s.ok_or("a branch is missing")? {
        "main" => Ok(0),
        b => b.strip_prefix('b').ok_or(format!("not a branch: {b}"))?.parse().map_err(|_| b.into()),
    }
}

fn parse_ref(s: Option<&str>) -> Result<Ref, String> {
    let s = s.ok_or("a block reference is missing")?;
    let k = |p: &str| -> Result<u8, String> {
        s.strip_prefix(p).ok_or(String::new())?.parse().map_err(|_| format!("bad index: {s}"))
    };
    let kind = if s == "random" {
        RefKind::Random
    } else if s.starts_with("main@-") {
        RefKind::Active(k("main@-")?)
    } else if s.starts_with("built#") {
        RefKind::Built(k("built#")?)
    } else if s.starts_with("tip#") {
        RefKind::Branch(k("tip#")?)
    } else if s.starts_with("unsent#") {
        RefKind::Unsent(k("unsent#")?)
    } else {
        return Err(format!("not a block reference: {s}"));
    };
    let r = Ref::of(kind);
    if r.kind() != kind {
        return Err(format!("index out of range: {s}"));
    }
    Ok(r)
}

fn operator(s: &str) -> Result<u8, String> {
    if let Some(n) = s.strip_prefix('#') {
        return n.parse().map_err(|_| format!("not an operator: {s}"));
    }
    OPERATORS
        .iter()
        .position(|o| o.name == s)
        .map(|i| i as u8)
        .ok_or(format!("unknown operator {s}"))
}

fn parse_flags(words: &[&str]) -> Result<u8, String> {
    let mut f = 0u8;
    for w in words {
        match *w {
            "headers" => f |= HEADERS_FIRST,
            "hold" => f |= HOLD,
            "signal" => f |= SIGNAL,
            "tx=spends" => f |= 1 << SHAPE_SHIFT,
            "tx=fan" => f |= 2 << SHAPE_SHIFT,
            "tx=chain" => f |= 3 << SHAPE_SHIFT,
            w if w.starts_with("flags+") => {
                f |= w[6..].parse::<u8>().map_err(|_| w.to_string())? << 5;
            }
            w => return Err(format!("unknown flag {w}")),
        }
    }
    Ok(f)
}

fn parse_intent(line: &str) -> Result<Intent, String> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let mut it = words.iter().copied();
    let verb = it.next().unwrap_or_default();
    Ok(match verb {
        "extend" => Intent::Extend {
            branch: branch(it.next())?,
            n: num(it.next())?,
            flags: parse_flags(&words[3.min(words.len())..])?,
        },
        "fork" => Intent::Fork {
            depth: num(it.next())?,
            n: num(it.next())?,
            flags: parse_flags(&words[3.min(words.len())..])?,
        },
        "mutate" => Intent::Mutate {
            op: operator(it.next().ok_or("an operator is missing")?)?,
        },
        "propose" => {
            let on = parse_ref(it.next())?;
            let mut again = false;
            let mut mutate = 0;
            for w in it {
                if w == "again" {
                    again = true;
                } else {
                    mutate = operator(w)? + 1;
                }
            }
            Intent::Propose {
                on,
                mutate,
                again,
            }
        }
        "invalidate" => Intent::Invalidate(parse_ref(it.next())?),
        "reconsider" => {
            let r = parse_ref(it.next())?;
            Intent::Reconsider(r, it.next() == Some("ignore_cl"))
        }
        "precious" => Intent::Precious(parse_ref(it.next())?),
        "cl" => Intent::Cl(parse_ref(it.next())?),
        "cl_enforce" => Intent::ClEnforce,
        "spork" => Intent::Spork {
            on: it.next() == Some("on"),
        },
        "flush" => Intent::Flush(match it.next() {
            Some("always") => 0,
            Some("if_needed") => 1,
            Some("periodic") => 2,
            other => num(other)?,
        }),
        "restart" => Intent::Restart(match it.next() {
            Some("clean") => 0,
            Some("kill") => 1,
            other => num(other)?,
        }),
        "time" => Intent::Time(num(it.next())?),
        "warp" => Intent::Warp(num(it.next())?),
        "raw" => {
            let hex = it.next().unwrap_or_default();
            let bytes = hex
                .as_bytes()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| {
                    std::str::from_utf8(pair).ok().and_then(|d| u8::from_str_radix(d, 16).ok())
                })
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| "bad hex".to_string())?;
            if bytes.len() > 255 {
                return Err("a raw block above 255 bytes".into());
            }
            Intent::Raw(bytes)
        }
        "activate" => Intent::Activate,
        "deliver" => {
            let b = branch(it.next())?;
            if b & DELIVER_REVERSED != 0 {
                return Err(format!("branch out of range: b{b}"));
            }
            Intent::Deliver {
                branch: match it.next() {
                    None => b,
                    Some("rev") => b | DELIVER_REVERSED,
                    Some(w) => return Err(format!("unknown flag {w}")),
                },
            }
        }
        "mutatex" => Intent::MutateX {
            slot: bank_slot(it.next().ok_or("a bank slot is missing")?)?,
        },
        "proposex" => {
            let on = parse_ref(it.next())?;
            let mut again = false;
            let mut slot = None;
            for w in it {
                if w == "again" {
                    again = true;
                } else {
                    slot = Some(bank_slot(w)?);
                }
            }
            Intent::ProposeX {
                on,
                slot: slot.ok_or("a bank slot is missing")?,
                again,
            }
        }
        v => return Err(format!("unknown intent {v}")),
    })
}

/// The `chain-v1` golden (O2b-4 §3a.4, G-ENC-1): synthetic inputs and what the `chain-v1`
/// decoder makes of them, written once by `examples/chain_v1_golden.rs` before the decoder
/// changed, and checked against it by a unit test from then on.
pub mod golden {
    use serde_json::{Value, json};

    use super::{Intent, MAX_INPUT, MAX_INTENTS, Program};
    use crate::chain::generate::{mutate_operator, propose_operator};
    use crate::chain::mutate::OPERATORS;

    /// One golden input: literal bytes, or `prefix` then `byte` repeated up to `len` bytes.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Input {
        Bytes(Vec<u8>),
        Fill {
            prefix: Vec<u8>,
            byte: u8,
            len: usize,
        },
    }

    impl Input {
        pub fn bytes(&self) -> Vec<u8> {
            match self {
                Input::Bytes(b) => b.clone(),
                Input::Fill {
                    prefix,
                    byte,
                    len,
                } => {
                    let mut b = prefix.clone();
                    b.resize(*len, *byte);
                    b
                }
            }
        }

        fn to_json(&self) -> Value {
            match self {
                Input::Bytes(b) => json!({ "hex": hex(b) }),
                Input::Fill {
                    prefix,
                    byte,
                    len,
                } => json!({ "prefix_hex": hex(prefix), "fill": byte, "len": len }),
            }
        }

        fn from_json(v: &Value) -> Input {
            if let Some(h) = v.get("hex") {
                return Input::Bytes(unhex(h.as_str().expect("hex")));
            }
            Input::Fill {
                prefix: unhex(v["prefix_hex"].as_str().expect("prefix_hex")),
                byte: u8::try_from(v["fill"].as_u64().expect("fill")).expect("a byte"),
                len: usize::try_from(v["len"].as_u64().expect("len")).expect("a length"),
            }
        }
    }

    /// The operator each `Mutate` and `Propose` of `p` resolves to, by intent index; `None` for a
    /// `Propose` with no operator.
    pub fn operators(p: &Program) -> Vec<(usize, Option<&'static str>)> {
        p.intents
            .iter()
            .enumerate()
            .filter_map(|(i, intent)| match intent {
                Intent::Mutate {
                    op,
                } => Some((i, Some(OPERATORS[mutate_operator(*op)].name))),
                Intent::Propose {
                    mutate,
                    ..
                } => Some((i, propose_operator(*mutate).map(|m| OPERATORS[m].name))),
                _ => None,
            })
            .collect()
    }

    /// The golden's inputs (§3a.4), named; `seeds` are the public seed files, by file name.
    pub fn cases(seeds: &[(String, Vec<u8>)]) -> Vec<(String, Input)> {
        let mut out = Vec::new();
        let mut add = |name: String, bytes: Vec<u8>| out.push((name, Input::Bytes(bytes)));
        for op in [0u8, 1, 29, 30, 31, 59, 60, 255] {
            add(format!("mutate/op={op}"), vec![0x00, 2, op]);
        }
        for m in [0u8, 1, 30, 31, 32, 127] {
            for again in [0u8, 0x80] {
                add(format!("propose/m={:#04x}", m | again), vec![0x00, 3, 0x00, m | again]);
            }
        }
        for h in [0x00u8, 0x01, 0xfe, 0xff] {
            add(format!("header/{h:#04x}"), vec![h, 15]);
        }
        for t in 0..=255u8 {
            add(format!("tag/{t}"), vec![0x00, t]);
        }
        // Under chain-v1, 19 % 17 = 2: `Mutate(30)`, which resolves to `OPERATORS[0]`.
        add("alias/tag19-op30".into(), vec![0x00, 19, 30]);
        add("intents/max+1".into(), [vec![0x00], vec![15; MAX_INTENTS + 1]].concat());
        add("raw/past-end".into(), vec![0x00, 14, 200, 1, 2, 3]);
        for (name, bytes) in seeds {
            add(format!("seed/{name}"), bytes.clone());
        }
        out.push((
            "input/longer-than-max".into(),
            Input::Fill {
                prefix: vec![0x00],
                byte: 14,
                len: MAX_INPUT + 1,
            },
        ));
        out
    }

    /// The public seed files in `dir`, sorted by name.
    pub fn public_seeds(dir: &str) -> std::io::Result<Vec<(String, Vec<u8>)>> {
        let mut seeds = Vec::new();
        for e in std::fs::read_dir(dir)? {
            let path = e?.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
            if name.len() == 16 && name.bytes().all(|b| b.is_ascii_hexdigit()) {
                seeds.push((name, std::fs::read(&path)?));
            }
        }
        seeds.sort();
        Ok(seeds)
    }

    /// One case's golden entry, as `decode` gives it.
    pub fn entry(name: &str, input: &Input, decode: fn(&[u8]) -> Program) -> Value {
        let p = decode(&input.bytes());
        let ops: Vec<Value> = operators(&p).into_iter().map(|(i, o)| json!([i, o])).collect();
        json!({
            "name": name,
            "input": input.to_json(),
            "program": p.to_string(),
            "canonical_hex": hex(&p.encode_v1()),
            "operators": ops,
        })
    }

    /// The golden file's text.
    pub fn render(cases: &[(String, Input)]) -> String {
        let entries: Vec<Value> =
            cases.iter().map(|(n, i)| entry(n, i, Program::decode_v1)).collect();
        let doc = json!({
            "about": "O2b-4 G-ENC-1: synthetic inputs and the chain-v1 decoder's programs, \
                      canonical bytes and resolved operators, pinned before any decoder change \
                      (node-l1 examples/chain_v1_golden.rs)",
            "cases": entries,
        });
        let mut text = serde_json::to_string_pretty(&doc).expect("JSON");
        text.push('\n');
        text
    }

    /// The golden file's cases: name, input and expected entry.
    pub fn parse(text: &str) -> Vec<(String, Input, Value)> {
        let doc: Value = serde_json::from_str(text).expect("the golden is JSON");
        doc["cases"]
            .as_array()
            .expect("cases")
            .iter()
            .map(|c| {
                let name = c["name"].as_str().expect("name").to_owned();
                (name, Input::from_json(&c["input"]), c.clone())
            })
            .collect()
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::generate::{Selection, selection};
    use crate::chain::mutate::Operator;

    const GOLDEN: &str = include_str!("../../tests/data/chain-v1-golden.json");

    /// G-ENC-1/G-ENC-2: the `chain-v1` decoder still gives every golden input the program,
    /// canonical bytes and operators pinned before any decoder change.
    #[test]
    fn the_v1_decoder_reproduces_its_golden() {
        let cases = golden::parse(GOLDEN);
        let seeds = cases.iter().filter(|(n, ..)| n.starts_with("seed/")).count();
        assert_eq!(seeds, 15, "the 15 public seed files");
        assert!(cases.iter().any(|(n, ..)| n == "input/longer-than-max"));
        for (name, input, want) in &cases {
            assert_eq!(&golden::entry(name, input, Program::decode_v1), want, "{name}");
        }
    }

    #[test]
    fn text_bytes_and_programs_round_trip() {
        let text = "start old\nextend main 3 headers signal tx=fan\nfork 2 4 hold\nmutate cb_amount\n\
                    propose main@-0 again\npropose built#3 sig_bad\ninvalidate tip#1\n\
                    reconsider main@-2 ignore_cl\nprecious unsent#4\ncl random\ncl_enforce\n\
                    spork off\nflush periodic\nrestart kill\ntime 30\nwarp 2\nraw 00ff\nactivate\n\
                    deliver b2\ndeliver main rev\n";
        let p = Program::parse(text).expect("parses");
        assert_eq!(p.to_string(), text);
        assert_eq!(Program::decode_v1(&p.encode_v1()), p);
    }

    #[test]
    fn any_bytes_decode_and_reencode_to_the_same_program() {
        let mut x = 0x9e37_79b9_u32;
        for len in 0..400 {
            let bytes: Vec<u8> = (0..len)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    (x >> 7) as u8
                })
                .collect();
            for enc in [Encoding::V1, Encoding::V2] {
                let p = Program::decode(&bytes, enc);
                assert!(p.intents.len() <= MAX_INTENTS);
                assert_eq!(Program::decode(&p.encode(enc), enc), p, "{enc:?} len {len}");
                assert_eq!(Program::parse(&p.to_string()), Ok(p), "{enc:?} len {len}");
            }
        }
    }

    /// OD-L3-118: `w_hold` is bit 1 of `chain-v2`'s first byte, `start hold` in the text, and
    /// `chain-v1` neither reads nor says it; a program without it keeps its bytes.
    #[test]
    fn w_hold_is_chain_v2_s_second_header_bit() {
        let p = Program::parse("start old\nstart hold\nextend main 2\n").expect("parses");
        assert!(p.old && p.w_hold && !p.is_v1());
        let bytes = p.encode_v2();
        assert_eq!(bytes[0], 0b11);
        assert_eq!(Program::decode_v2(&bytes), p);
        assert_eq!(Program::parse(&p.to_string()), Ok(p.clone()));
        let v1 = Program::decode_v1(&bytes);
        assert!(v1.old && !v1.w_hold);
        let plain = Program {
            w_hold: false,
            ..p
        };
        assert_eq!(plain.encode_v2()[0], 1);
        assert_eq!(plain.encode_v2(), plain.encode_v1());
    }

    #[test]
    fn v2_bank_intents_round_trip() {
        let text = "mutatex #5\nproposex tip#1 again #63\nproposex main@-0 #0\nmutate cb_amount\n";
        let p = Program::parse(text).expect("parses");
        assert!(!p.is_v1());
        assert_eq!(p.to_string(), text);
        let bytes = p.encode_v2();
        let r = |t: &str| parse_ref(Some(t)).expect("a ref").0;
        assert_eq!(bytes, [0x00, 17, 5, 18, r("tip#1"), 63 | 0x80, 18, r("main@-0"), 0, 2, 0]);
        assert_eq!(Program::decode_v2(&bytes), p);
        assert_eq!(
            Program::parse("mutatex #64"),
            Err("line 1: not a bank slot (0..64): #64: mutatex #64".into())
        );
    }

    #[test]
    #[should_panic(expected = "no chain-v1 encoding")]
    fn a_bank_intent_has_no_v1_bytes() {
        Program::parse("mutatex #0").expect("parses").encode_v1();
    }

    /// The two decoders differ only in the tag modulus: tags 0–16 read alike, and 17 and 18 are
    /// v2's bank intents.
    #[test]
    fn v2_reads_v1_tags_alike_and_adds_two() {
        for t in 0..19u8 {
            let bytes = [0x00, t, 0x41, 0xc2, 3];
            let (v1, v2) = (Program::decode_v1(&bytes), Program::decode_v2(&bytes));
            match t {
                17 => assert_eq!(
                    v2.intents[0],
                    Intent::MutateX {
                        slot: 0x41 % 64
                    }
                ),
                18 => assert_eq!(
                    v2.intents[0],
                    Intent::ProposeX {
                        on: Ref(0x41),
                        slot: 0xc2 % 64,
                        again: true
                    }
                ),
                // Only the first intent: the bytes after it are tags, read modulo each count.
                _ => assert_eq!(v2.intents[0], v1.intents[0], "tag {t}"),
            }
        }
    }

    /// Each chain target's public seed directory is exactly its encoding's compile of the shared
    /// seed programs, its encoding's and its own (O2b-4 §3a.2, OD-L3-96 Q2, 4f2): no extra file,
    /// no other encoding's bytes, no other target's programs. The legacy target and S0/S1's have
    /// no programs of their own; `chain-v2` has its eight spork programs, `chain-v1` none. A planted
    /// search's leaves out its bugs' witnesses (O2b-4 4b): two fewer than S0/S1's.
    #[test]
    fn public_seeds_are_each_encoding_s_compile() {
        let root = env!("CARGO_MANIFEST_DIR");
        let progs = Path::new(root).join("tests/chain_seeds");
        assert_eq!(compile_seeds(&progs, Encoding::V1, &[]).expect("compiles").len(), 15);
        let mut own: Vec<String> = std::fs::read_dir(&progs)
            .expect("tests/chain_seeds")
            .map(|e| e.expect("entry").path())
            .filter(|p| p.is_dir())
            .map(|p| p.file_name().expect("a name").to_string_lossy().into_owned())
            .filter(|name| Encoding::parse(name).is_none())
            .collect();
        own.sort();
        // The encodings with programs of their own (O2b-4 4f2: `chain-v2`'s spork programs; none
        // for `chain-v1`, whose `diff_chain` seeds stay as they were), and how many each has.
        let by_encoding = |e: Encoding| match e {
            Encoding::V1 => 0,
            Encoding::V2 => 8,
        };
        assert!(!progs.join(Encoding::V1.name()).exists(), "no chain-v1 programs of its own");
        assert_eq!(
            compile_seeds(&progs.join(Encoding::V2.name()), Encoding::V2, &[])
                .expect("compiles")
                .len(),
            by_encoding(Encoding::V2)
        );
        // The targets with programs of their own, and how many each has.
        let pinned = [("diff_chain_av", 4), ("diff_chain_prodflush", 9)];
        let names: Vec<&str> = pinned.iter().map(|&(t, _)| t).collect();
        assert_eq!(own, names, "the targets with programs of their own");
        for (target, encoding) in chain_encodings() {
            let dir = format!("{root}/../node-fuzz/public-seeds/{target}");
            let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(&dir)
                .unwrap_or_else(|e| panic!("{dir}: {e}"))
                .map(|e| e.expect("entry").path())
                .map(|p| {
                    let name = p.file_name().expect("a name").to_string_lossy().into_owned();
                    (name, std::fs::read(&p).expect("read"))
                })
                .collect();
            files.sort();
            let excludes = seed_excludes(target);
            let want = target_seeds(&progs, target, encoding).expect("compiles");
            let extra = pinned.iter().find(|&&(t, _)| t == target).map_or(0, |&(_, n)| n);
            assert_eq!(
                want.len(),
                15 - excludes.len() + extra + by_encoding(encoding),
                "{target}: the seed programs"
            );
            assert!(files == want, "{target}: not exactly the {} compile", encoding.name());
            // The witnesses are left out by stem: each is a program, and its compile is in no
            // file of the directory, under whatever name.
            for stem in excludes {
                let prog = progs.join(format!("{stem}.prog"));
                let text = std::fs::read_to_string(&prog)
                    .unwrap_or_else(|e| panic!("{}: {e}", prog.display()));
                let witness = Program::parse(&text).expect("parses").encode(encoding);
                assert!(
                    files.iter().all(|(_, bytes)| *bytes != witness),
                    "{target}: holds {stem}'s compile"
                );
            }
        }
    }

    /// G-ENC-3: every golden input's `chain-v1` program keeps its meaning through `chain-v2`'s
    /// encoder and decoder: the same bytes, program, resolved operator names and selections.
    #[test]
    fn v1_programs_keep_their_meaning_under_v2() {
        let selections =
            |p: &Program| p.intents.iter().map(|i| selection(i, BANK)).collect::<Vec<_>>();
        for (name, input, _) in &golden::parse(GOLDEN) {
            let v1 = Program::decode_v1(&input.bytes());
            let bytes = v1.encode_v2();
            assert_eq!(bytes, v1.encode_v1(), "{name}");
            let v2 = Program::decode_v2(&bytes);
            assert_eq!(v2, v1, "{name}");
            assert_eq!(golden::operators(&v2), golden::operators(&v1), "{name}");
            assert_eq!(selections(&v2), selections(&v1), "{name}");
        }
    }

    /// G-ENC-4's append property: over 10,000 fixed-PRNG `chain-v2` inputs and the v2 compile of
    /// the seed programs, one more builder in the bank changes an intent's selection only where
    /// its slot was unknown before.
    #[test]
    fn appending_a_builder_changes_only_unknown_slots() {
        let copy = |o: &Operator| Operator {
            name: o.name,
            rejects: o.rejects,
            stage: o.stage,
            par: o.par,
            apply: o.apply,
            variants: o.variants,
        };
        let mut grown: Vec<Operator> = BANK.iter().map(copy).collect();
        grown.push(Operator {
            name: "test.appended",
            ..copy(&OPERATORS[0])
        });
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/chain_seeds");
        let mut inputs: Vec<Vec<u8>> = std::fs::read_dir(dir)
            .expect("tests/chain_seeds")
            .map(|e| e.expect("entry").path())
            .filter(|p| p.extension().is_some_and(|e| e == "prog"))
            .map(|p| Program::parse(&std::fs::read_to_string(p).expect("read")).expect("parses"))
            .map(|p| p.encode_v2())
            .collect();
        assert_eq!(inputs.len(), 15, "the seed programs");
        let mut x: u64 = 0x4f32_6234;
        let mut next = move || {
            x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = x;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        };
        for _ in 0..10_000 {
            let len = 2 + (next() % 399) as usize;
            inputs.push((0..len).map(|_| next() as u8).collect());
        }
        let (mut changed, mut unknown) = (0, 0);
        for bytes in &inputs {
            for intent in &Program::decode_v2(bytes).intents {
                let before = selection(intent, BANK);
                unknown += usize::from(before == Some(Selection::UnknownSlot));
                if before != selection(intent, &grown) {
                    assert_eq!(before, Some(Selection::UnknownSlot), "{intent:?}");
                    assert_eq!(
                        selection(intent, &grown),
                        Some(Selection::Operator(OPERATORS.len() + BANK.len())),
                        "{intent:?}"
                    );
                    changed += 1;
                }
            }
        }
        assert!(changed > 0 && unknown > changed, "changed {changed} of {unknown} unknown");
    }
}
