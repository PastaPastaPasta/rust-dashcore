//! Inputs (O2b §3.2, D10): a header byte, then at most [`MAX_INTENTS`] intents. Every byte
//! string decodes: a tag is taken modulo the tag count, an exhausted input reads zeros, and
//! every parameter is reduced where it is used. [`Program`]'s text form (`tests/chain_seeds`)
//! encodes to the bytes that decode to it, so seed programs, not bytes, are committed (§3.5).

use std::fmt::{self, Write as _};

use super::mutate::OPERATORS;

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
}

const TAGS: u8 = 17;

/// A decoded input.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Program {
    /// Start with the tip seven hours old (in IBD) instead of one.
    pub old: bool,
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
    /// Decode any bytes (D10).
    pub fn decode(data: &[u8]) -> Program {
        let data = &data[..data.len().min(MAX_INPUT)];
        let mut c = Cursor(data);
        let old = c.byte() & 1 == 1;
        let mut intents = Vec::new();
        while !c.0.is_empty() && intents.len() < MAX_INTENTS {
            let tag = c.byte() % TAGS;
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
                _ => Intent::Deliver {
                    branch: c.byte(),
                },
            });
        }
        Program {
            old,
            intents,
        }
    }

    /// The canonical bytes: `decode(encode(p)) == p` for every program whose `Raw` payloads
    /// are at most 255 bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![u8::from(self.old)];
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
            }
        }
        out
    }

    /// Parse the text form: one intent per line, `#` comment lines, an optional first line `start
    /// old` (the grammar is [`Intent`]'s `Display`).
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
        }
    }
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
        v => return Err(format!("unknown intent {v}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_bytes_and_programs_round_trip() {
        let text = "start old\nextend main 3 headers signal tx=fan\nfork 2 4 hold\nmutate cb_amount\n\
                    propose main@-0 again\npropose built#3 sig_bad\ninvalidate tip#1\n\
                    reconsider main@-2 ignore_cl\nprecious unsent#4\ncl random\ncl_enforce\n\
                    spork off\nflush periodic\nrestart kill\ntime 30\nwarp 2\nraw 00ff\nactivate\n\
                    deliver b2\ndeliver main rev\n";
        let p = Program::parse(text).expect("parses");
        assert_eq!(p.to_string(), text);
        assert_eq!(Program::decode(&p.encode()), p);
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
            let p = Program::decode(&bytes);
            assert!(p.intents.len() <= MAX_INTENTS);
            assert_eq!(Program::decode(&p.encode()), p, "len {len}");
            assert_eq!(Program::parse(&p.to_string()), Ok(p), "len {len}");
        }
    }
}
