//! Detection of the Core format of a file, and of the version it carries.

use dash_network::Network;

use super::envelope::network_of;
use super::error::{FormatError, FormatResult};
use super::flat::{self, Payload};
use super::governance::GovernanceDat;
use super::json::{MAX_JSON_FILE_SIZE, MAX_SETTINGS_FILE_SIZE, parse_settings_object};
use super::mncache::MncacheDat;
use super::netaddr::{DISK_VERSION_ADDRV2, DISK_VERSION_IGNORE_MASK};
use super::netfulfilled::NetFulfilledDat;
use super::peers::INCOMPATIBILITY_BASE;
use super::reader::Reader;
use super::sporks::SporksDat;
use super::{anchors, fee_estimates, governance, mempool, mncache, netfulfilled, peers, sporks};

/// The Core net-state formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    PeersDat,
    AnchorsDat,
    BanlistJson,
    MempoolDat,
    FeeEstimatesDat,
    SettingsJson,
    GovernanceDat,
    MncacheDat,
    NetfulfilledDat,
    SporksDat,
}

impl Format {
    pub const ALL: [Format; 10] = [
        Format::PeersDat,
        Format::AnchorsDat,
        Format::BanlistJson,
        Format::MempoolDat,
        Format::FeeEstimatesDat,
        Format::SettingsJson,
        Format::GovernanceDat,
        Format::MncacheDat,
        Format::NetfulfilledDat,
        Format::SporksDat,
    ];

    /// The file name Core gives it in the datadir (network subdirectory).
    pub fn file_name(self) -> &'static str {
        match self {
            Format::PeersDat => "peers.dat",
            Format::AnchorsDat => "anchors.dat",
            Format::BanlistJson => "banlist.json",
            Format::MempoolDat => "mempool.dat",
            Format::FeeEstimatesDat => "fee_estimates.dat",
            Format::SettingsJson => "settings.json",
            Format::GovernanceDat => "governance.dat",
            Format::MncacheDat => "mncache.dat",
            Format::NetfulfilledDat => "netfulfilled.dat",
            Format::SporksDat => "sporks.dat",
        }
    }

    pub fn from_file_name(name: &str) -> Option<Format> {
        Format::ALL.into_iter().find(|f| f.file_name() == name)
    }
}

/// The version information a format carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    /// `peers.dat`: the format byte and the oldest format that can read the file (the second
    /// byte minus 32).
    Peers {
        format: u8,
        lowest_compatible: u8,
    },
    /// `anchors.dat` has no file-level version; this says whether its first address (if any) is
    /// in the BIP155 form.
    Anchors {
        first_addrv2: Option<bool>,
    },
    /// `mempool.dat`'s `u64` version (1 in Core v24).
    Mempool {
        version: u64,
    },
    /// `fee_estimates.dat`'s version required to read and version of the writer.
    FeeEstimates {
        required: i32,
        wrote: i32,
    },
    /// The JSON files carry no version of their own (a ban entry has one each).
    Json,
    /// The `CFlatDB` files (`governance.dat`, `mncache.dat`, `netfulfilled.dat`, `sporks.dat`):
    /// whether the object's version string is one the reader loads (`governance.dat` loads the
    /// `-16` of Core v23 and v24.0.0-rc.3 as well as the `-17` of rc.4). When it is not, the file
    /// loads as empty, as in Core. `netfulfilled.dat` has no version string and is always `true`.
    Flat {
        current: bool,
    },
}

/// What a file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detected {
    pub format: Format,
    /// The network its magic names, for the formats that start with one.
    pub network: Option<Network>,
    pub version: Version,
}

fn magic_network(bytes: &[u8], what: &'static str) -> FormatResult<Network> {
    let found = Reader::new(bytes, what).array::<4>()?;
    network_of(found).ok_or(FormatError::WrongMagic {
        what,
        found,
    })
}

fn flat_header<T: Payload>(bytes: &[u8]) -> FormatResult<(Option<Network>, Version)> {
    let (network, current) = flat::peek::<T>(bytes)?;
    Ok((
        Some(network),
        Version::Flat {
            current,
        },
    ))
}

/// Detects a file the caller knows by name (`peers.dat`, ...): checks the header the format
/// begins with and reports its version. Does not read the whole file or check its checksum; use
/// the format's `parse` for that.
pub fn detect_named(file_name: &str, bytes: &[u8]) -> FormatResult<Detected> {
    let format = Format::from_file_name(file_name).ok_or(FormatError::Unrecognized)?;
    let what = format.file_name();
    let (network, version) = match format {
        Format::PeersDat => {
            let network = magic_network(bytes, what)?;
            let mut r = Reader::at(bytes, 4, what);
            let (format, compat) = (r.u8()?, r.u8()?);
            let lowest_compatible = compat
                .checked_sub(INCOMPATIBILITY_BASE)
                .ok_or_else(|| r.corrupt("compat value below the minimum"))?;
            (
                Some(network),
                Version::Peers {
                    format,
                    lowest_compatible,
                },
            )
        }
        Format::AnchorsDat => {
            let network = magic_network(bytes, what)?;
            let mut r = Reader::at(bytes, 4, what);
            let first_addrv2 = if r.compact_size(true)? == 0 {
                None
            } else {
                match r.u32_le()? & !DISK_VERSION_IGNORE_MASK {
                    0 => Some(false),
                    DISK_VERSION_ADDRV2 => Some(true),
                    _ => return Err(r.corrupt("unsupported CAddress disk format version")),
                }
            };
            (
                Some(network),
                Version::Anchors {
                    first_addrv2,
                },
            )
        }
        Format::MempoolDat => (
            None,
            Version::Mempool {
                version: mempool::version(bytes)?,
            },
        ),
        Format::FeeEstimatesDat => {
            let (required, wrote) = fee_estimates::versions(bytes)?;
            (
                None,
                Version::FeeEstimates {
                    required,
                    wrote,
                },
            )
        }
        // `ReadSettings` takes an empty settings file as no settings.
        Format::SettingsJson if bytes.is_empty() => (None, Version::Json),
        Format::SettingsJson => {
            parse_settings_object(bytes, what, MAX_SETTINGS_FILE_SIZE)?;
            (None, Version::Json)
        }
        Format::BanlistJson => {
            parse_settings_object(bytes, what, MAX_JSON_FILE_SIZE)?;
            (None, Version::Json)
        }
        Format::GovernanceDat => flat_header::<GovernanceDat>(bytes)?,
        Format::MncacheDat => flat_header::<MncacheDat>(bytes)?,
        Format::NetfulfilledDat => flat_header::<NetFulfilledDat>(bytes)?,
        Format::SporksDat => flat_header::<SporksDat>(bytes)?,
    };
    Ok(Detected {
        format,
        network,
        version,
    })
}

/// Detects a file from its bytes alone, trying the strictest formats first.
///
/// The binary formats are told apart by parsing them completely: a file that starts with a known
/// network magic is `anchors.dat` or `peers.dat` (the two share the envelope and are told apart
/// by which one parses), the rest are a version-1 mempool or a fee-estimates file when they parse
/// as one.
///
/// JSON is classified by **structure only**: a settings-style object (one JSON object, unique
/// top-level keys) with a `banned_nets` member is reported as a ban list, any other as settings.
/// That is a hint, not validation: `{"banned_nets":42}` is reported as a ban list although
/// [`banlist::parse`](super::banlist::parse) refuses it, and a settings file may in principle
/// hold that key. The caller still runs the reader of the format it got (or the one its file
/// name says) and takes its verdict. An empty file is no format.
///
/// An input that is a corrupt or truncated instance of a binary format is
/// [`FormatError::Unrecognized`] here; [`detect_named`] and the format's `parse` give the
/// specific error.
pub fn sniff(bytes: &[u8]) -> FormatResult<Detected> {
    // A binary file can begin with whitespace and `{` (a fee-estimates version word such as
    // 162592 is `20 7B 02 00`), so a failed JSON parse falls through to the binary formats.
    if let Ok(object) = parse_settings_object(bytes, "JSON file", MAX_JSON_FILE_SIZE) {
        let format = if object.get("banned_nets").is_some() {
            Format::BanlistJson
        } else if bytes.len() <= MAX_SETTINGS_FILE_SIZE {
            Format::SettingsJson
        } else {
            return Err(FormatError::Unrecognized);
        };
        return Ok(Detected {
            format,
            network: None,
            version: Version::Json,
        });
    }
    // The `CFlatDB` files begin with the compact-size length of their magic message (15 to 20), and
    // a network magic never does: the first byte of one is 0xbf, 0xce, 0xe2 or 0xfc.
    if let Some((format, parses)) = sniff_flat(bytes) {
        if parses {
            return detect_named(format.file_name(), bytes).map_err(|_| FormatError::Unrecognized);
        }
        return Err(FormatError::Unrecognized);
    }
    if let Ok(network) = magic_network(bytes, "file") {
        if anchors::parse(bytes, network).is_ok() {
            return detect_named("anchors.dat", bytes);
        }
        if peers::parse(bytes, network).is_ok() {
            return detect_named("peers.dat", bytes);
        }
        return Err(FormatError::Unrecognized);
    }
    // Version 1 is v24's mempool.dat. Its first eight bytes (`01 00 ..`) are no network magic and
    // no fee-estimate version word.
    if mempool::parse(bytes).is_ok() {
        return detect_named("mempool.dat", bytes);
    }
    if fee_estimates::parse(bytes).is_ok() {
        return detect_named("fee_estimates.dat", bytes);
    }
    Err(FormatError::Unrecognized)
}

/// Which flat file `bytes` begins as, and whether it parses completely.
fn sniff_flat(bytes: &[u8]) -> Option<(Format, bool)> {
    fn one<T: Payload>(
        format: Format,
        bytes: &[u8],
        parse: fn(&[u8], Network) -> FormatResult<flat::Loaded<T>>,
    ) -> Option<(Format, bool)> {
        let (network, _) = flat::peek::<T>(bytes).ok()?;
        Some((format, parse(bytes, network).is_ok_and(|l| l.discarded.is_none())))
    }
    one::<GovernanceDat>(Format::GovernanceDat, bytes, governance::parse)
        .or_else(|| one::<MncacheDat>(Format::MncacheDat, bytes, mncache::parse))
        .or_else(|| one::<NetFulfilledDat>(Format::NetfulfilledDat, bytes, netfulfilled::parse))
        .or_else(|| one::<SporksDat>(Format::SporksDat, bytes, sporks::parse))
}
