//! [`CoinScript`]: a coin's `scriptPubKey`, inline when short; and [`ScriptRef`], a script
//! borrowed from serialized coin bytes, still compressed.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;

use super::compress::{
    MAX_SPECIAL_SCRIPT_LEN, decompress_script_into, is_unspendable, write_compressed_script,
};
use crate::primitives::CoreWrite;

/// The longest script kept inline: compressed P2PK (35 bytes), so P2PKH (25), P2SH (23) and
/// compressed P2PK decode without allocating. Core's `CScript` keeps 28 bytes inline.
const INLINE: usize = 35;

/// A coin's `scriptPubKey` (Core's `CScript`), inline up to 35 bytes and on the heap beyond.
/// It dereferences to the script's bytes; equality and hashing are the bytes'.
#[derive(Clone)]
pub struct CoinScript(Repr);

#[derive(Clone)]
enum Repr {
    Inline {
        len: u8,
        bytes: [u8; INLINE],
    },
    Heap(Box<[u8]>),
}

impl CoinScript {
    pub fn as_slice(&self) -> &[u8] {
        match &self.0 {
            Repr::Inline {
                len,
                bytes,
            } => &bytes[..usize::from(*len)],
            Repr::Heap(bytes) => bytes,
        }
    }
}

impl Default for CoinScript {
    fn default() -> Self {
        Self(Repr::Inline {
            len: 0,
            bytes: [0; INLINE],
        })
    }
}

impl From<&[u8]> for CoinScript {
    fn from(script: &[u8]) -> Self {
        if script.len() <= INLINE {
            let mut bytes = [0; INLINE];
            bytes[..script.len()].copy_from_slice(script);
            Self(Repr::Inline {
                len: script.len() as u8,
                bytes,
            })
        } else {
            Self(Repr::Heap(script.into()))
        }
    }
}

impl Deref for CoinScript {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl AsRef<[u8]> for CoinScript {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl PartialEq for CoinScript {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for CoinScript {}

impl Hash for CoinScript {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_slice().hash(state);
    }
}

impl fmt::Debug for CoinScript {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CoinScript(")?;
        for byte in self.as_slice() {
            write!(f, "{byte:02x}")?;
        }
        f.write_str(")")
    }
}

/// A coin's script as `ScriptCompression` stores it, borrowed from the serialized bytes. A special
/// form is decompressed only on demand ([`ScriptRef::expand`]), into a caller's buffer, so reading
/// a coin never allocates.
///
/// Only the decoder makes a special form, so its code and bytes are always ones Core reads.
///
/// Equality is that of the scripts Core holds after reading: a special form equals the raw
/// script it decompresses to. Comparing two special forms of codes 4 and 5, or one with a raw
/// script, decompresses them.
#[derive(Clone, Copy, Debug)]
pub struct ScriptRef<'a>(Form<'a>);

impl PartialEq for ScriptRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self.0, other.0) {
            (Form::Script(a), Form::Script(b)) => a == b,
            // Codes 0–3 decompress without a check, so equal forms are equal scripts and different
            // codes or bytes are different scripts.
            (
                Form::Special {
                    code: a,
                    data: x,
                },
                Form::Special {
                    code: b,
                    data: y,
                },
            ) if a < 4 && b < 4 => a == b && x == y,
            _ => {
                let (mut a, mut b) = ([0u8; MAX_SPECIAL_SCRIPT_LEN], [0u8; MAX_SPECIAL_SCRIPT_LEN]);
                self.expand(&mut a) == other.expand(&mut b)
            }
        }
    }
}

impl Eq for ScriptRef<'_> {}

#[derive(Clone, Copy, Debug)]
enum Form<'a> {
    /// The script itself.
    Script(&'a [u8]),
    /// Special form `code` (0–5) and its `special_script_size(code)` bytes.
    Special {
        code: u8,
        data: &'a [u8],
    },
}

impl<'a> ScriptRef<'a> {
    /// A script given in full (an output's `scriptPubKey`).
    pub fn from_script(script: &'a [u8]) -> ScriptRef<'a> {
        ScriptRef(Form::Script(script))
    }

    /// Special form `code` and its bytes, as the decoder read them.
    pub(crate) fn special(code: u8, data: &'a [u8]) -> ScriptRef<'a> {
        ScriptRef(Form::Special {
            code,
            data,
        })
    }

    /// The script's bytes when it is not a special form: a raw form, or `OP_RETURN` for a raw form
    /// longer than `MAX_SCRIPT_SIZE`, which Core reads as `OP_RETURN`.
    pub fn as_script(&self) -> Option<&'a [u8]> {
        match self.0 {
            Form::Script(script) => Some(script),
            Form::Special {
                ..
            } => None,
        }
    }

    /// The special form's code (0–5) and its 20 or 32 bytes.
    pub fn as_special(&self) -> Option<(u8, &'a [u8])> {
        match self.0 {
            Form::Script(_) => None,
            Form::Special {
                code,
                data,
            } => Some((code, data)),
        }
    }

    /// The script as Core holds it after reading: a special form decompressed into `buf`, and an
    /// empty script where Core's `DecompressScript` fails (an x coordinate of codes 4 and 5 off the
    /// curve, which Core leaves its freshly constructed script empty for).
    pub fn expand<'b>(&'b self, buf: &'b mut [u8; MAX_SPECIAL_SCRIPT_LEN]) -> &'b [u8] {
        match self.0 {
            Form::Script(script) => script,
            Form::Special {
                code,
                data,
            } => match decompress_script_into(u32::from(code), data, buf) {
                Some(len) => &buf[..len],
                None => &[],
            },
        }
    }

    /// [`ScriptRef::expand`] as an owned script.
    pub fn to_script(&self) -> CoinScript {
        let mut buf = [0u8; MAX_SPECIAL_SCRIPT_LEN];
        CoinScript::from(self.expand(&mut buf))
    }

    /// `CScript::IsUnspendable` of the script, without decompressing it: no special form is
    /// unspendable (each decompresses to at most 67 bytes that do not start with `OP_RETURN`, or
    /// to nothing).
    pub fn is_unspendable(&self) -> bool {
        self.as_script().is_some_and(is_unspendable)
    }

    /// `ScriptCompression::Ser` of the script Core read: the bytes Core writes when it serializes
    /// the coin again. A special form decompresses to a script `CompressScript` gives back as the
    /// same code and bytes, except an x coordinate of codes 4 and 5 off the curve, which Core read
    /// as an empty script.
    pub fn write_compressed(&self, w: &mut impl CoreWrite) {
        match self.0 {
            Form::Script(script) => write_compressed_script(script, w),
            Form::Special {
                code,
                data,
            } => {
                let mut buf = [0u8; MAX_SPECIAL_SCRIPT_LEN];
                if code < 4 || decompress_script_into(u32::from(code), data, &mut buf).is_some() {
                    w.write(&[code]);
                    w.write(data);
                } else {
                    write_compressed_script(&[], w);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_and_long_scripts_compare_by_their_bytes() {
        for len in [0, 1, 25, INLINE, INLINE + 1, 67, 10_000] {
            let bytes: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let script = CoinScript::from(&bytes[..]);
            assert_eq!(&*script, &bytes[..]);
            assert_eq!(script, CoinScript::from(&bytes[..]));
            assert_eq!(matches!(script.0, Repr::Inline { .. }), len <= INLINE);
        }
        assert!(CoinScript::default().is_empty());
    }
}
