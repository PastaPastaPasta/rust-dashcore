//! A ProTx's network information (ABI 13; `src/test/oracle/oracle.h`, "NetInfo"): Core's
//! `Validate()` and `AddEntry()` on an `MnNetInfo` or `ExtNetInfo` as a payload carries it.
//! The chain's `-allowprivatenet` (ABI 16; see [`Chain`]) does not change their verdicts: on a
//! chain that requires a routable address (all but regtest) an RFC1918 address is `NotRoutable`
//! whatever it holds (Core `src/evo/netinfo.cpp:294, 456`).

use super::*;

/// The NetInfo implementation a ProTx version selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetInfoKind {
    /// `MnNetInfo` (ProTx versions below ExtAddr): one `CService`.
    Mn = ffi::ORACLE_NETINFO_MN as isize,
    /// `ExtNetInfo`: entries per purpose.
    Ext = ffi::ORACLE_NETINFO_EXT as isize,
}

/// Core's `NetInfoStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetInfoStatus {
    BadInput = 0,
    Duplicate = 1,
    MaxLimit = 2,
    BadAddress = 3,
    BadPort = 4,
    BadType = 5,
    NotRoutable = 6,
    Malformed = 7,
    Success = 8,
}

impl TryFrom<u8> for NetInfoStatus {
    type Error = u8;

    /// Core's enum value; the unknown value back as the error.
    fn try_from(status: u8) -> std::result::Result<Self, u8> {
        Ok(match status {
            0 => Self::BadInput,
            1 => Self::Duplicate,
            2 => Self::MaxLimit,
            3 => Self::BadAddress,
            4 => Self::BadPort,
            5 => Self::BadType,
            6 => Self::NotRoutable,
            7 => Self::Malformed,
            8 => Self::Success,
            other => return Err(other),
        })
    }
}

/// Result of [`Oracle::netinfo_add_entry`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetInfoAdd {
    pub status: NetInfoStatus,
    /// The `NetInfoEntry` Core added, serialized (on `Success` only).
    pub entry: Option<Vec<u8>>,
    /// The NetInfo after the call, serialized as it was read.
    pub netinfo: Vec<u8>,
}

/// Core's status, or an error for a byte that is not one.
fn status(byte: u8) -> Result<NetInfoStatus> {
    NetInfoStatus::try_from(byte).map_err(|byte| Error {
        kind: ErrorKind::Status(byte.into()),
        message: format!("{byte} is not a NetInfoStatus"),
    })
}

impl Oracle {
    /// `NetInfoInterface::Validate()` under `chain`'s rules.
    pub fn netinfo_validate(
        &self,
        chain: &Chain,
        kind: NetInfoKind,
        netinfo: &[u8],
    ) -> Result<NetInfoStatus> {
        self.on_chain(chain, || {
            let byte = self.with_out(|out| unsafe {
                (self.api.netinfo_validate)(kind as c_int, netinfo.as_ptr(), netinfo.len(), out)
            })?;
            status(byte)
        })
    }

    /// `NetInfoInterface::AddEntry(purpose, input)` under `chain`'s rules. `purpose` and
    /// `input` reach Core as they are (any `NetInfoPurpose` value, any bytes).
    pub fn netinfo_add_entry(
        &self,
        chain: &Chain,
        kind: NetInfoKind,
        netinfo: &[u8],
        purpose: u8,
        input: &[u8],
    ) -> Result<NetInfoAdd> {
        self.on_chain(chain, || {
            let mut byte = 0u8;
            let (entry, netinfo) =
                self.with_buffers(netinfo.len() + 512, |entry, entry_len, out, out_len| unsafe {
                    (self.api.netinfo_add_entry)(
                        kind as c_int,
                        netinfo.as_ptr(),
                        netinfo.len(),
                        purpose,
                        input.as_ptr(),
                        input.len(),
                        &mut byte,
                        entry,
                        entry_len,
                        out,
                        out_len,
                    )
                })?;
            let status = status(byte)?;
            Ok(NetInfoAdd {
                status,
                entry: (status == NetInfoStatus::Success).then_some(entry),
                netinfo,
            })
        })
    }
}
