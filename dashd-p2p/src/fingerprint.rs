//! Who the node is on the wire: the user agent, the advertised version and the local services
//! (P2a §3.3.1–3.3.4), built once at init into SB6's [`LocalIdentity`].
//!
//! rdashd identifies honestly as itself (DEC-83): the user agent is `/rdashd:<version>/`, with
//! Core's comment rules (`init.cpp:1827-1844`). The protocol version is v24's (DEC-82); every
//! send-side version gate follows from the negotiated common version.

use dash_p2p::LocalIdentity;
use dash_p2p_core::{ProtocolVersion, ServiceFlags, UserAgent};
use dash_params::{ChainSetup, Network};

/// Core's `PROTOCOL_VERSION` at v24 (`version.h:14`; DEC-82).
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::V24_0;

/// The client name in our user agent (DEC-83; DEC-48 Q7's form).
pub const CLIENT_NAME: &str = "rdashd";

/// Core's `MAX_SUBVERSION_LENGTH` (`net.h:86`).
pub const MAX_SUBVERSION_LENGTH: usize = 256;

/// The options our identity is built from (ND1 parses them).
#[derive(Clone, Debug, Default)]
pub struct IdentityOptions {
    /// `-uacomment`, in order.
    pub ua_comments: Vec<String>,
    /// `-pushversion`: the version we advertise, honoured off mainnet only.
    pub push_version: Option<i32>,
    /// Masternode mode (`m_nodeman != nullptr`).
    pub masternode: bool,
    /// `-v2transport` (default on).
    pub v2_transport: bool,
    /// `-peerbloomfilters` (default on).
    pub peer_bloom_filters: bool,
    /// `-peerblockfilters` with the basic filter index (default off).
    pub peer_block_filters: bool,
    /// `-prune`: no `NODE_NETWORK`.
    pub pruned: bool,
}

impl IdentityOptions {
    /// Core's defaults.
    pub fn new() -> Self {
        Self {
            v2_transport: true,
            peer_bloom_filters: true,
            ..Self::default()
        }
    }
}

/// Why the identity cannot be built: Core's `InitError`s, with Core's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdentityError {
    /// `"User Agent comment (%s) contains unsafe characters."`
    UnsafeComment(String),
    /// `"Total length of network version string (%i) exceeds maximum length (%i). Reduce the
    /// number or size of uacomments."`
    TooLong(usize),
}

impl core::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnsafeComment(c) => {
                write!(f, "User Agent comment ({c}) contains unsafe characters.")
            }
            Self::TooLong(n) => write!(
                f,
                "Total length of network version string ({n}) exceeds maximum length ({MAX_SUBVERSION_LENGTH}). Reduce the number or size of uacomments."
            ),
        }
    }
}

impl std::error::Error for IdentityError {}

/// The node's identity: the user agent with Core's comment rules, `-pushversion` off mainnet,
/// and the masternode flag.
// Core: init.cpp:1827-1844 (user agent); net_processing.cpp:1577-1580 (-pushversion)
pub fn local_identity(
    params: &ChainSetup,
    opts: &IdentityOptions,
) -> Result<LocalIdentity, IdentityError> {
    let mut comments = Vec::new();
    if params.network == Network::Devnet {
        comments.push(format!("devnet.{}", params.devnet_name.as_deref().unwrap_or_default()));
    }
    for c in &opts.ua_comments {
        if !c.chars().all(is_ua_comment_char) {
            return Err(IdentityError::UnsafeComment(c.clone()));
        }
        comments.push(c.clone());
    }
    let ua = format_sub_version(CLIENT_NAME, env!("CARGO_PKG_VERSION"), &comments);
    if ua.len() > MAX_SUBVERSION_LENGTH {
        return Err(IdentityError::TooLong(ua.len()));
    }
    let advertised_version = match opts.push_version {
        Some(v) if params.network != Network::Main => v,
        _ => PROTOCOL_VERSION.0,
    };
    Ok(LocalIdentity {
        protocol_version: PROTOCOL_VERSION,
        advertised_version,
        user_agent: UserAgent::new(ua.into_bytes()).map_err(|e| IdentityError::TooLong(e.len))?,
        masternode: opts.masternode,
    })
}

/// Core's `nLocalServices` after init (P2a §3.3.3): `NODE_NETWORK_LIMITED | NODE_HEADERS_COMPRESSED`
/// always, `NODE_P2P_V2` with `-v2transport`, `NODE_COMPACT_FILTERS` with `-peerblockfilters`,
/// `NODE_BLOOM` with `-peerbloomfilters`, and `NODE_NETWORK` unless pruned.
// Core: init.cpp:1125, 1235-1237, 1240-1246, 1370-1371, 2353-2354
pub fn local_services(opts: &IdentityOptions) -> ServiceFlags {
    let mut s = ServiceFlags::NODE_NETWORK_LIMITED | ServiceFlags::NODE_HEADERS_COMPRESSED;
    if opts.v2_transport {
        s |= ServiceFlags::NODE_P2P_V2;
    }
    if opts.peer_block_filters {
        s |= ServiceFlags::NODE_COMPACT_FILTERS;
    }
    if opts.peer_bloom_filters {
        s |= ServiceFlags::NODE_BLOOM;
    }
    if !opts.pruned {
        s |= ServiceFlags::NODE_NETWORK;
    }
    s
}

/// Core's `SAFE_CHARS_UA_COMMENT` (`util/strencodings.cpp:23`).
fn is_ua_comment_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || " .,;-_?@".contains(c)
}

/// Core's `FormatSubVersion`: `/name:version(c1; c2)/`.
// Core: clientversion.cpp:68-83 FormatSubVersion
fn format_sub_version(name: &str, version: &str, comments: &[String]) -> String {
    if comments.is_empty() {
        format!("/{name}:{version}/")
    } else {
        format!("/{name}:{version}({})/", comments.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dash_params::{ChainParamsBuilder, Release};

    use super::*;

    fn params(net: Network) -> Arc<ChainSetup> {
        let b = ChainParamsBuilder::new(net, Release::V24_0);
        let b = if net == Network::Devnet {
            b.devnet_name("alpha")
        } else {
            b
        };
        Arc::new(b.build().unwrap())
    }

    fn ua(id: &LocalIdentity) -> &str {
        id.user_agent.as_str().unwrap()
    }

    #[test]
    fn user_agent_follows_core_comment_rules() {
        let v = env!("CARGO_PKG_VERSION");
        let mut o = IdentityOptions::new();
        let id = local_identity(&params(Network::Regtest), &o).unwrap();
        assert_eq!(ua(&id), format!("/rdashd:{v}/"));
        assert_eq!(id.advertised_version, 70242);
        assert_eq!(id.protocol_version, ProtocolVersion(70242));

        o.ua_comments = vec!["a b".into(), "x@y".into()];
        let id = local_identity(&params(Network::Devnet), &o).unwrap();
        // Core: init.cpp:1832 `"devnet." + GetDevNetName()`, and GetDevNetName is "devnet-<name>".
        assert_eq!(ua(&id), format!("/rdashd:{v}(devnet.devnet-alpha; a b; x@y)/"));

        o.ua_comments = vec!["no/slash".into()];
        assert_eq!(
            local_identity(&params(Network::Regtest), &o).unwrap_err().to_string(),
            "User Agent comment (no/slash) contains unsafe characters."
        );

        o.ua_comments = vec!["x".repeat(300)];
        let err = local_identity(&params(Network::Regtest), &o).unwrap_err();
        assert!(matches!(err, IdentityError::TooLong(n) if n > MAX_SUBVERSION_LENGTH));
    }

    #[test]
    fn push_version_is_honoured_off_mainnet_only() {
        let mut o = IdentityOptions::new();
        o.push_version = Some(70100);
        assert_eq!(
            local_identity(&params(Network::Regtest), &o).unwrap().advertised_version,
            70100
        );
        assert_eq!(local_identity(&params(Network::Main), &o).unwrap().advertised_version, 70242);
    }

    #[test]
    fn local_services_defaults() {
        // P2a §3.3.3: 0x1C05 by default, 0x1C45 with block filters, 0x1C04 pruned, 0x0C05 without V2.
        let mut o = IdentityOptions::new();
        assert_eq!(local_services(&o).0, 0x1C05);
        o.peer_block_filters = true;
        assert_eq!(local_services(&o).0, 0x1C45);
        o.peer_block_filters = false;
        o.pruned = true;
        assert_eq!(local_services(&o).0, 0x1C04);
        o.pruned = false;
        o.v2_transport = false;
        assert_eq!(local_services(&o).0, 0x0C05);
    }
}
