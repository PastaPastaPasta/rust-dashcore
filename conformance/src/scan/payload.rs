//! Hand parsers for the three payloads the scans read (CbTx, QcTx, MnHf), written from Dash
//! Core's serialization code and independent of both libraries under test. BLS fields are
//! decoded with `dash-pkc`, which is Core's `CBLSWrapper::Unserialize` ported (quirks 22, 35).

use dash_pkc::bls::{BlsEagerPk, BlsEagerSig, BlsSchemeKind, BlsSchemeSource};

/// `MAX_SIZE` for CompactSize values (`src/serialize.h`).
const MAX_SIZE: u64 = 0x0200_0000;

/// What decoding a BLS field gives, as Core's `Unserialize` + `IsValid` do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlsState {
    /// A point of the scheme.
    Valid,
    /// All-zero bytes: the invalid object.
    Zero,
    /// Canonical only in the other scheme: the invalid object (quirk 22).
    OtherScheme,
    /// Canonical in neither scheme: the enclosing payload fails to parse.
    Malleable,
}

impl BlsState {
    /// Core's `IsValid()`.
    pub fn is_valid(self) -> bool {
        self == BlsState::Valid
    }

    pub fn name(self) -> &'static str {
        match self {
            BlsState::Valid => "valid",
            BlsState::Zero => "zero",
            BlsState::OtherScheme => "other-scheme",
            BlsState::Malleable => "malleable",
        }
    }
}

/// Classifies a decode of `bytes`: `decoded` is `Ok(is_valid())` or the decode error.
fn bls_state<E>(decoded: Result<bool, E>, bytes: &[u8]) -> BlsState {
    match decoded {
        Ok(true) => BlsState::Valid,
        Ok(false) if bytes.iter().all(|b| *b == 0) => BlsState::Zero,
        Ok(false) => BlsState::OtherScheme,
        Err(_) => BlsState::Malleable,
    }
}

/// Decodes a signature under `kind`.
pub fn sig_state(bytes: &[u8; 96], kind: BlsSchemeKind) -> BlsState {
    let decoded = BlsEagerSig::decode(bytes, BlsSchemeSource::Version(kind));
    bls_state(decoded.map(|s| s.is_valid()), bytes)
}

/// Decodes a public key under `kind`.
pub fn pk_state(bytes: &[u8; 48], kind: BlsSchemeKind) -> BlsState {
    let decoded = BlsEagerPk::decode(bytes, BlsSchemeSource::Version(kind));
    bls_state(decoded.map(|k| k.is_valid()), bytes)
}

/// A forward reader with Core's `ReadCompactSize` rules.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

type R<T> = Result<T, &'static str>;

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
        }
    }

    fn take(&mut self, n: usize) -> R<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or("length overflow")?;
        let out = self.data.get(self.pos..end).ok_or("unexpected end of data")?;
        self.pos = end;
        Ok(out)
    }

    fn array<const N: usize>(&mut self) -> R<[u8; N]> {
        Ok(self.take(N)?.try_into().expect("N bytes"))
    }

    fn u8(&mut self) -> R<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> R<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn u32(&mut self) -> R<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn compact_size(&mut self) -> R<u64> {
        let first = self.u8()?;
        // The value and the smallest one the width may encode.
        let (value, min) = match first {
            0..=0xfc => (u64::from(first), 0),
            0xfd => (u64::from(self.u16()?), 0xfd),
            0xfe => (u64::from(self.u32()?), 0x1_0000),
            0xff => (u64::from_le_bytes(self.array()?), 0x1_0000_0000),
        };
        if value < min {
            return Err("non-canonical ReadCompactSize()");
        }
        if value > MAX_SIZE {
            return Err("ReadCompactSize(): size too large");
        }
        Ok(value)
    }

    /// `DYNBITSET`: a CompactSize bit count, then packed bytes; bits past the count must be
    /// clear (`ReadFixedBitSet`, `src/serialize.h:452-476`). Returns the bit count and whether
    /// any bit is set.
    fn dyn_bitset(&mut self) -> R<(u64, bool)> {
        let n = self.compact_size()?;
        let nbytes = n.div_ceil(8) as usize;
        if nbytes > self.data.len() - self.pos {
            return Err("ReadFixedBitSet(): declared size exceeds remaining bytes");
        }
        let bytes = self.take(nbytes)?;
        if n % 8 != 0 {
            let rem = 8 - (n % 8) as u32;
            let mask = !(0xffu8 >> rem);
            if bytes[nbytes - 1] & mask != 0 {
                return Err("Out-of-range bits set");
            }
        }
        Ok((n, bytes.iter().any(|b| *b != 0)))
    }

    fn finish(&self) -> R<()> {
        if self.pos == self.data.len() {
            Ok(())
        } else {
            Err("trailing bytes in payload")
        }
    }
}

/// `CCbTx` (`src/evo/cbtx.h:28-69`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CbTx {
    pub version: u16,
    pub height: i32,
    /// `bestCLHeightDiff` (version >= 3).
    pub best_cl_height_diff: u32,
    /// `bestCLSignature` (version >= 3).
    pub best_cl_sig: [u8; 96],
    /// Whether the fields of version 3 were present.
    pub has_cl: bool,
}

pub fn parse_cbtx(payload: &[u8]) -> R<CbTx> {
    let mut r = Reader::new(payload);
    let version = r.u16()?;
    let height = i32::from_le_bytes(r.array()?);
    r.take(32)?; // merkleRootMNList
    let mut out = CbTx {
        version,
        height,
        best_cl_height_diff: 0,
        best_cl_sig: [0; 96],
        has_cl: false,
    };
    if version >= 2 {
        r.take(32)?; // merkleRootQuorums
        if version >= 3 {
            let diff = r.compact_size()?;
            out.best_cl_height_diff = u32::try_from(diff).map_err(|_| "bestCLHeightDiff > u32")?;
            out.best_cl_sig = r.array()?;
            r.take(8)?; // creditPoolBalance
            out.has_cl = true;
            if version >= 4 {
                r.take(32)?; // merkleRootAssetUnlocks
            }
        }
    }
    r.finish()?;
    Ok(out)
}

/// `MNHFTxPayload` (`src/evo/mnhftx.h:35-80`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MnHf {
    pub version: u8,
    pub bit: u8,
    pub quorum_hash: [u8; 32],
    pub sig: BlsState,
}

pub fn parse_mnhf(payload: &[u8]) -> R<MnHf> {
    let mut r = Reader::new(payload);
    let version = r.u8()?;
    let bit = r.u8()?;
    let quorum_hash = r.array()?;
    // `CBLSSignatureVersionWrapper(sig, /*legacy=*/false)`
    let sig = sig_state(&r.array()?, BlsSchemeKind::Basic);
    if sig == BlsState::Malleable {
        return Err("malleable BLS object");
    }
    r.finish()?;
    Ok(MnHf {
        version,
        bit,
        quorum_hash,
        sig,
    })
}

/// `CFinalCommitmentTxPayload` (`src/llmq/commitment.h:62-165`), reduced to what scan (f) needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Qc {
    pub qc_height: u32,
    pub version: u16,
    pub llmq_type: u8,
    pub quorum_hash: [u8; 32],
    pub quorum_index: Option<i16>,
    /// Core's `CFinalCommitment::IsNull()`.
    pub is_null: bool,
}

pub fn parse_qc(payload: &[u8]) -> R<Qc> {
    let mut r = Reader::new(payload);
    r.u16()?; // qcTx version
    let qc_height = r.u32()?;
    let version = r.u16()?;
    let llmq_type = r.u8()?;
    let quorum_hash = r.array()?;
    let quorum_index = if version == 2 || version == 4 {
        Some(i16::from_le_bytes(r.array()?))
    } else {
        None
    };
    let (_, signers_set) = r.dyn_bitset()?;
    let (_, valid_set) = r.dyn_bitset()?;
    let kind = BlsSchemeKind::from_legacy(version == 1 || version == 2);
    let pk = pk_state(&r.array()?, kind);
    let vvec: [u8; 32] = r.array()?;
    let quorum_sig = sig_state(&r.array()?, kind);
    let members_sig = sig_state(&r.array()?, kind);
    if [pk, quorum_sig, members_sig].contains(&BlsState::Malleable) {
        return Err("malleable BLS object");
    }
    r.finish()?;
    let is_null = !signers_set
        && !valid_set
        && !pk.is_valid()
        && vvec.iter().all(|b| *b == 0)
        && !members_sig.is_valid()
        && !quorum_sig.is_valid();
    Ok(Qc {
        qc_height,
        version,
        llmq_type,
        quorum_hash,
        quorum_index,
        is_null,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cbtx_v3_roundtrip_fields() {
        let mut p = Vec::new();
        p.extend_from_slice(&3u16.to_le_bytes());
        p.extend_from_slice(&1_987_776i32.to_le_bytes());
        p.extend_from_slice(&[7; 32]);
        p.extend_from_slice(&[8; 32]);
        p.push(5); // bestCLHeightDiff
        p.extend_from_slice(&[0; 96]);
        p.extend_from_slice(&0i64.to_le_bytes());
        let c = parse_cbtx(&p).unwrap();
        assert_eq!((c.version, c.height, c.best_cl_height_diff, c.has_cl), (3, 1_987_776, 5, true));
        // trailing byte
        p.push(0);
        assert_eq!(parse_cbtx(&p), Err("trailing bytes in payload"));
    }

    #[test]
    fn cbtx_rejects_non_canonical_diff() {
        let mut p = Vec::new();
        p.extend_from_slice(&3u16.to_le_bytes());
        p.extend_from_slice(&1i32.to_le_bytes());
        p.extend_from_slice(&[0; 64]);
        p.extend_from_slice(&[0xfd, 0x05, 0x00]); // 5 in three bytes
        p.extend_from_slice(&[0; 96 + 8]);
        assert_eq!(parse_cbtx(&p), Err("non-canonical ReadCompactSize()"));
    }

    #[test]
    fn bitset_padding_is_a_parse_error() {
        // 3 bits, byte 0b0000_1000 sets a padding bit
        let mut r = Reader::new(&[3, 0b0000_1000]);
        assert_eq!(r.dyn_bitset(), Err("Out-of-range bits set"));
        let mut r = Reader::new(&[3, 0b0000_0101]);
        assert_eq!(r.dyn_bitset(), Ok((3, true)));
        let mut r = Reader::new(&[3, 0]);
        assert_eq!(r.dyn_bitset(), Ok((3, false)));
        let mut r = Reader::new(&[9, 0, 0]);
        assert_eq!(r.dyn_bitset(), Ok((9, false)));
        let mut r = Reader::new(&[9, 0, 2]);
        assert_eq!(r.dyn_bitset(), Err("Out-of-range bits set"));
    }

    #[test]
    fn zero_sig_is_zero_state() {
        assert_eq!(sig_state(&[0; 96], BlsSchemeKind::Basic), BlsState::Zero);
    }
}
