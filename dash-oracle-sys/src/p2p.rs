//! The P2P overlay messages (ABI 7): Core's handler reads of BS8's message types.

use std::ffi::CString;
use std::os::raw::c_int;

use crate::{Error, ErrorKind, Oracle, Result, c_string, ffi};

/// The commands `oracle_p2p_decode` reads: the overlay messages, plus `smlentry` (one
/// simplified masternode list entry) and `qsnapshot` (one quorum snapshot), which are no
/// messages of their own.
pub const P2P_COMMANDS: [&str; 14] = [
    "clsig",
    "isdlock",
    "qsigrec",
    "qfcommit",
    "mnauth",
    "spork",
    "platformban",
    "qgetdata",
    "qdata",
    "getqrinfo",
    "qrinfo",
    "mnlistdiff",
    "smlentry",
    "qsnapshot",
];

/// The stream a P2P message is read from (`oracle_p2p_ctx`), every field explicit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct P2pStream {
    /// The stream's version: the peer's common protocol version.
    pub version: i32,
    /// `bls::bls_legacy_scheme` during the read and the write-back.
    pub bls_legacy: bool,
    /// qdata: the quorum's threshold (the verification vector's expected size).
    pub qdata_max_vvec: u32,
    /// qdata: the quorum's valid member count (the contributions' maximum).
    pub qdata_max_contrib: u32,
}

/// What Core's qdata handler read beyond the bytes. For qdata, [`P2pVerdict::Read`] means the
/// modeled codec reads succeeded (the request, then the vectors the data mask names, each within
/// its limit), not that NetQuorum accepts the message. Outside the verdict, as `oracle.h` lists:
/// its exact counts (it refuses a verification vector under the threshold and fewer
/// contributions than the valid member count; compare these counts), its stop after a refused
/// vector (the contributions are still read here), the state that decides whether it reads
/// contributions at all, and its validation of what it read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QDataRead {
    /// The request's `nError` as read (`UNDEFINED`, 0xff, when it has none).
    pub error: u8,
    /// The verification vector's size, when the handler read one.
    pub vvec_size: Option<usize>,
    /// The number of encrypted contributions, when the handler read them.
    pub contrib_count: Option<usize>,
}

/// A message Core read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct P2pDecoded {
    /// The bytes read (the handler ignores what follows).
    pub consumed: usize,
    /// The object written back at the same version and scheme.
    pub reencoded: Vec<u8>,
    /// For qdata, what the handler read.
    pub qdata: Option<QDataRead>,
}

/// Core's verdict on a P2P message: read, or refused by a handler check before or during the
/// read. Core's deserializer refusing the bytes is `ErrorKind::Decode`; any other exception
/// (`std::bad_alloc` among them), and any in the write-back, is `ErrorKind::Exception`, no
/// verdict (BL-61).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum P2pVerdict {
    Read(P2pDecoded),
    /// The handler check that refused it (qgetdata's length, qdata's vector bounds).
    Refused(String),
}

impl Oracle {
    /// Reads the payload of `command` as Core's handler does on `stream`.
    pub fn p2p_decode(
        &self,
        command: &str,
        payload: &[u8],
        stream: &P2pStream,
    ) -> Result<P2pVerdict> {
        self.shared(|| {
            let command = CString::new(command).map_err(|_| Error {
                kind: ErrorKind::Argument,
                message: "NUL in the command".into(),
            })?;
            let ctx = ffi::OracleP2pCtx {
                format: ffi::ORACLE_P2P_FORMAT,
                stream_version: stream.version,
                bls_legacy: c_int::from(stream.bls_legacy),
                qdata_max_vvec: stream.qdata_max_vvec,
                qdata_max_contrib: stream.qdata_max_contrib,
            };
            let mut consumed = 0;
            let mut fields = ffi::OracleP2pFields::default();
            let mut refused = false;
            let reencoded = self.with_buffer(payload.len() + 64, |out, len| {
                let status = unsafe {
                    (self.api.p2p_decode)(
                        command.as_ptr(),
                        payload.as_ptr(),
                        payload.len(),
                        &ctx,
                        &mut consumed,
                        out,
                        len,
                        &mut fields,
                    )
                };
                refused = status == ffi::ORACLE_REJECT;
                // A refusal writes no buffer: report it as a success with an empty one.
                if refused {
                    unsafe { *len = 0 };
                    ffi::ORACLE_OK
                } else {
                    status
                }
            })?;
            if refused {
                return Ok(P2pVerdict::Refused(c_string(&fields.reject_reason)));
            }
            let size = |n: i64| usize::try_from(n).ok();
            let qdata = (fields.qdata_error >= 0).then(|| QDataRead {
                error: fields.qdata_error as u8,
                vvec_size: size(fields.qdata_vvec_size),
                contrib_count: size(fields.qdata_contrib_count),
            });
            Ok(P2pVerdict::Read(P2pDecoded {
                consumed,
                reencoded,
                qdata,
            }))
        })
    }
}
