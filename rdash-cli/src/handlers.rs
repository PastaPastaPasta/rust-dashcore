//! The request handlers of src/bitcoin-cli.cpp at v24.0.0-rc.3: how a command line becomes a
//! JSON-RPC request, and how the reply becomes the value dash-cli prints.
//!
//! Core: `BaseRequestHandler` and its subclasses. A handler fails with the text of the
//! `std::runtime_error` Core throws.

use std::time::{SystemTime, UNIX_EPOCH};

use dash_args::ArgsManager;
use dash_args::chain::{DEVNET, REGTEST, TESTNET};

use crate::convert::{rpc_convert_named_values, rpc_convert_values};
use crate::json::{
    UniValue, UniValueExt, jsonrpc_process_batch_reply, jsonrpc_reply_obj, jsonrpc_request_obj,
};
use crate::{CLIENT_VERSION, PACKAGE_NAME};

pub const DEFAULT_NAMED: bool = false;

/// See `GetNetworkName()` in netbase.cpp.
const NETWORKS: [&str; 7] =
    ["not_publicly_routable", "ipv4", "ipv6", "onion", "i2p", "cjdns", "internal"];
const NETWORK_SHORT_NAMES: [&str; 7] = ["npr", "ipv4", "ipv6", "onion", "i2p", "cjdns", "int"];
const UNREACHABLE_NETWORK_IDS: [usize; 2] = [0, 6];

/// Core: `DEFAULT_NBLOCKS`.
pub const DEFAULT_NBLOCKS: &str = "1";

/// Core: `BaseRequestHandler`.
pub trait RequestHandler {
    fn prepare_request(&mut self, method: &str, args: &[String]) -> Result<UniValue, String>;
    fn process_reply(&mut self, reply: &UniValue) -> Result<UniValue, String>;
}

fn network_string_to_id(name: &str) -> Option<usize> {
    NETWORKS.iter().position(|n| *n == name)
}

/// `strprintf("%*s", width, s)`: right-aligned, counting bytes as `std::ostream` does.
fn pad_left(s: &str, width: usize) -> String {
    format!("{}{s}", " ".repeat(width.saturating_sub(s.len())))
}

/// `strprintf("%-*s", width, s)`.
fn pad_right(s: &str, width: usize) -> String {
    format!("{s}{}", " ".repeat(width.saturating_sub(s.len())))
}

fn err<T>(r: Result<T, dash_args::univalue::RuntimeError>) -> Result<T, String> {
    r.map_err(|e| e.0)
}

/// Core: `AddrinfoRequestHandler`.
pub struct AddrinfoRequestHandler;

impl RequestHandler for AddrinfoRequestHandler {
    fn prepare_request(&mut self, _method: &str, args: &[String]) -> Result<UniValue, String> {
        if !args.is_empty() {
            return Err("-addrinfo takes no arguments".to_string());
        }
        let params = rpc_convert_values("getnodeaddresses", &["0".to_string()])?;
        Ok(jsonrpc_request_obj("getnodeaddresses", params, 1))
    }

    fn process_reply(&mut self, reply: &UniValue) -> Result<UniValue, String> {
        if !reply.key("error").is_null() {
            return Ok(reply.clone());
        }
        let nodes = err(reply.key("result").get_values())?;
        if !nodes.is_empty() && nodes[0].key("network").is_null() {
            return Err("-addrinfo requires dashd server to be running v21.0 and up".to_string());
        }
        // Count the number of peers known to our node, by network.
        let mut counts = [0u64; NETWORKS.len()];
        for node in &nodes {
            let network_name = err(node.key("network").get_str())?;
            let Some(id) = network_string_to_id(network_name) else {
                continue;
            };
            counts[id] += 1;
        }
        // Prepare result to return to user.
        let mut result = UniValue::new_object();
        let mut addresses = UniValue::new_object();
        let mut total = 0u64;
        for i in 1..NETWORKS.len() - 1 {
            addresses.push_kv(NETWORKS[i], counts[i].into());
            total += counts[i];
        }
        addresses.push_kv("total", total.into());
        result.push_kv("addresses_known", addresses);
        Ok(jsonrpc_reply_obj(result, 1))
    }
}

/// Core: `GetinfoRequestHandler`.
pub struct GetinfoRequestHandler;

const ID_NETWORKINFO: usize = 0;
const ID_BLOCKCHAININFO: usize = 1;
const ID_WALLETINFO: usize = 2;
const ID_BALANCES: usize = 3;

/// `batch[id]`. Core indexes a `std::vector` sized by the reply, which reads out of bounds when
/// a server answers a batch with fewer members than it was sent; here a missing member is null.
fn slot(batch: &[UniValue], id: usize) -> &UniValue {
    batch.get(id).unwrap_or(&crate::json::NULL)
}

impl RequestHandler for GetinfoRequestHandler {
    /// Create a simulated `getinfo` request.
    fn prepare_request(&mut self, _method: &str, args: &[String]) -> Result<UniValue, String> {
        if !args.is_empty() {
            return Err("-getinfo takes no arguments".to_string());
        }
        let mut result = UniValue::new_array();
        result.push_back(jsonrpc_request_obj(
            "getnetworkinfo",
            UniValue::Null,
            ID_NETWORKINFO as i32,
        ));
        result.push_back(jsonrpc_request_obj(
            "getblockchaininfo",
            UniValue::Null,
            ID_BLOCKCHAININFO as i32,
        ));
        result.push_back(jsonrpc_request_obj(
            "getwalletinfo",
            UniValue::Null,
            ID_WALLETINFO as i32,
        ));
        result.push_back(jsonrpc_request_obj("getbalances", UniValue::Null, ID_BALANCES as i32));
        Ok(result)
    }

    /// Collect values from the batch and form a simulated `getinfo` reply.
    fn process_reply(&mut self, batch_in: &UniValue) -> Result<UniValue, String> {
        let batch = err(jsonrpc_process_batch_reply(batch_in))?;
        // Errors in getnetworkinfo() and getblockchaininfo() are fatal, pass them on;
        // getwalletinfo() and getbalances() are allowed to fail if there is no wallet.
        if !slot(&batch, ID_NETWORKINFO).key("error").is_null() {
            return Ok(slot(&batch, ID_NETWORKINFO).clone());
        }
        if !slot(&batch, ID_BLOCKCHAININFO).key("error").is_null() {
            return Ok(slot(&batch, ID_BLOCKCHAININFO).clone());
        }
        let net = slot(&batch, ID_NETWORKINFO).key("result");
        let chain = slot(&batch, ID_BLOCKCHAININFO).key("result");
        let wallet = slot(&batch, ID_WALLETINFO).key("result");
        let balances = slot(&batch, ID_BALANCES).key("result");

        let mut result = UniValue::new_object();
        result.push_kv("version", net.key("version").clone());
        result.push_kv("blocks", chain.key("blocks").clone());
        result.push_kv("headers", chain.key("headers").clone());
        result.push_kv("verificationprogress", chain.key("verificationprogress").clone());
        result.push_kv("timeoffset", net.key("timeoffset").clone());

        let mut connections = UniValue::new_object();
        connections.push_kv("in", net.key("connections_in").clone());
        connections.push_kv("out", net.key("connections_out").clone());
        connections.push_kv("total", net.key("connections").clone());
        connections.push_kv("mn_in", net.key("connections_mn_in").clone());
        connections.push_kv("mn_out", net.key("connections_mn_out").clone());
        connections.push_kv("mn_total", net.key("connections_mn").clone());
        result.push_kv("connections", connections);

        result.push_kv("networks", net.key("networks").clone());
        result.push_kv("difficulty", chain.key("difficulty").clone());
        result.push_kv("chain", chain.key("chain").clone());
        if !wallet.is_null() {
            result.push_kv("coinjoin_balance", wallet.key("coinjoin_balance").clone());
            result.push_kv("has_wallet", true.into());
            result.push_kv("keypoolsize", wallet.key("keypoolsize").clone());
            result.push_kv("walletname", wallet.key("walletname").clone());
            if !wallet.key("unlocked_until").is_null() {
                result.push_kv("unlocked_until", wallet.key("unlocked_until").clone());
            }
            result.push_kv("paytxfee", wallet.key("paytxfee").clone());
        }
        if !balances.is_null() {
            result.push_kv("balance", balances.key("mine").key("trusted").clone());
        }
        result.push_kv("relayfee", net.key("relayfee").clone());
        result.push_kv("warnings", net.key("warnings").clone());
        Ok(jsonrpc_reply_obj(result, 1))
    }
}

/// Core: one peer of `NetinfoRequestHandler::m_peers`.
struct Peer {
    addr: String,
    sub_version: String,
    conn_type: String,
    network: &'static str,
    age: String,
    transport_protocol_type: String,
    min_ping: f64,
    ping: f64,
    addr_processed: i64,
    addr_rate_limited: i64,
    last_blck: i64,
    last_recv: i64,
    last_send: i64,
    last_trxn: i64,
    id: i32,
    mapped_as: i32,
    version: i32,
    is_addr_relay_enabled: bool,
    is_bip152_hb_from: bool,
    is_bip152_hb_to: bool,
    is_outbound: bool,
    is_tx_relay: bool,
}

/// Core: `NetinfoRequestHandler`.
pub struct NetinfoRequestHandler<'a> {
    args: &'a ArgsManager,
    /// Peer counts by (in/out/total, networks/total).
    counts: [[u16; NETWORKS.len() + 1]; 3],
    block_relay_peers_count: u8,
    manual_peers_count: u8,
    details_level: u8,
    is_asmap_on: bool,
    max_addr_length: usize,
    max_addr_processed_length: usize,
    max_addr_rate_limited_length: usize,
    max_age_length: usize,
    max_id_length: usize,
    peers: Vec<Peer>,
}

const MAX_DETAIL_LEVEL: u8 = 4;
const ID_PEERINFO: usize = 0;
const ID_NETINFO_NETWORKINFO: usize = 1;

impl<'a> NetinfoRequestHandler<'a> {
    pub fn new(args: &'a ArgsManager) -> Self {
        NetinfoRequestHandler {
            args,
            counts: [[0; NETWORKS.len() + 1]; 3],
            block_relay_peers_count: 0,
            manual_peers_count: 0,
            details_level: 0,
            is_asmap_on: false,
            max_addr_length: 0,
            max_addr_processed_length: 5,
            max_addr_rate_limited_length: 6,
            max_age_length: 5,
            max_id_length: 2,
            peers: Vec::new(),
        }
    }

    fn details_requested(&self) -> bool {
        self.details_level > 0 && self.details_level < 5
    }
    fn is_address_selected(&self) -> bool {
        self.details_level == 2 || self.details_level == 4
    }
    fn is_version_selected(&self) -> bool {
        self.details_level == 3 || self.details_level == 4
    }

    fn chain_to_string(&self) -> Result<&'static str, String> {
        let chain = err(self.args.get_chain_name())?;
        Ok(match chain.as_str() {
            TESTNET => " testnet",
            DEVNET => " devnet",
            REGTEST => " regtest",
            _ => "",
        })
    }

    fn ping_time_to_string(seconds: f64) -> String {
        if seconds < 0.0 {
            return String::new();
        }
        let milliseconds = (1000.0 * seconds).round();
        if milliseconds > 999_999.0 {
            "-".to_string()
        } else {
            // Printed as the double it is, so that a rounded -0 stays "-0".
            format!("{milliseconds}")
        }
    }

    fn connection_type_for_netinfo(conn_type: &str) -> &'static str {
        match conn_type {
            "outbound-full-relay" => "full",
            "block-relay-only" => "block",
            "manual" => "manual",
            "feeler" => "feeler",
            "addr-fetch" => "addr",
            _ => "",
        }
    }

    /// Core: `m_help_doc`.
    pub fn help_doc() -> String {
        let max = MAX_DETAIL_LEVEL;
        let mut s = String::new();
        s.push_str("-netinfo level|\"help\" \n\n");
        s.push_str("Returns a network peer connections dashboard with information from the remote server.\n");
        s.push_str("This human-readable interface will change regularly and is not intended to be a stable API.\n");
        s.push_str("Under the hood, -netinfo fetches the data by calling getpeerinfo and getnetworkinfo.\n");
        s.push_str(&format!(
            "An optional integer argument from 0 to {max} can be passed for different peers listings; {max} to 255 are parsed as {max}.\n"
        ));
        s.push_str("Pass \"help\" to see this detailed help documentation.\n");
        s.push_str("If more than one argument is passed, only the first one is read and parsed.\n");
        s.push_str("Suggestion: use with the Linux watch(1) command for a live dashboard; see example below.\n\n");
        s.push_str("Arguments:\n");
        s.push_str(&format!(
            "1. level (integer 0-{max}, optional)  Specify the info level of the peers dashboard (default 0):\n"
        ));
        s.push_str(concat!(
            "                                  0 - Peer counts for each reachable network as well as for block relay peers\n",
            "                                      and manual peers, and the list of local addresses and ports\n",
            "                                  1 - Like 0 but preceded by a peers listing (without address and version columns)\n",
            "                                  2 - Like 1 but with an address column\n",
            "                                  3 - Like 1 but with a version column\n",
            "                                  4 - Like 1 but with both address and version columns\n",
            "2. help (string \"help\", optional) Print this help documentation instead of the dashboard.\n\n",
            "Result:\n\n",
        ));
        s.push_str(&format!(
            "* The peers listing in levels 1-{max} displays all of the peers sorted by direction and minimum ping time:\n\n"
        ));
        s.push_str(concat!(
            "  Column   Description\n",
            "  ------   -----------\n",
            "  <->      Direction\n",
            "           \"in\"  - inbound connections are those initiated by the peer\n",
            "           \"out\" - outbound connections are those initiated by us\n",
            "  type     Type of peer connection\n",
            "           \"full\"   - full relay, the default\n",
            "           \"block\"  - block relay; like full relay but does not relay transactions or addresses\n",
            "           \"manual\" - peer we manually added using RPC addnode or the -addnode/-connect config options\n",
            "           \"feeler\" - short-lived connection for testing addresses\n",
            "           \"addr\"   - address fetch; short-lived connection for requesting addresses\n",
            "  net      Network the peer connected through (\"ipv4\", \"ipv6\", \"onion\", \"i2p\", \"cjdns\", or \"npr\" (not publicly routable))\n",
            "  v        Version of transport protocol used for the connection\n",
            "  mping    Minimum observed ping time, in milliseconds (ms)\n",
            "  ping     Last observed ping time, in milliseconds (ms)\n",
            "  send     Time since last message sent to the peer, in seconds\n",
            "  recv     Time since last message received from the peer, in seconds\n",
            "  txn      Time since last novel transaction received from the peer and accepted into our mempool, in minutes\n",
            "           \"*\" - we do not relay transactions to this peer (relaytxes is false)\n",
            "  blk      Time since last novel block passing initial validity checks received from the peer, in minutes\n",
            "  hb       High-bandwidth BIP152 compact block relay\n",
            "           \".\" (to)   - we selected the peer as a high-bandwidth peer\n",
            "           \"*\" (from) - the peer selected us as a high-bandwidth peer\n",
            "  addrp    Total number of addresses processed, excluding those dropped due to rate limiting\n",
            "           \".\" - we do not relay addresses to this peer (addr_relay_enabled is false)\n",
            "  addrl    Total number of addresses dropped due to rate limiting\n",
            "  age      Duration of connection to the peer, in minutes\n",
            "  asmap    Mapped AS (Autonomous System) number in the BGP route to the peer, used for diversifying\n",
            "           peer selection (only displayed if the -asmap config option is set)\n",
            "  id       Peer index, in increasing order of peer connections since node startup\n",
            "  address  IP address and port of the peer\n",
            "  version  Peer version and subversion concatenated, e.g. \"70016/Satoshi:21.0.0/\"\n\n",
            "* The peer counts table displays the number of peers for each reachable network as well as\n",
            "  the number of block relay peers and manual peers.\n\n",
            "* The local addresses table lists each local address broadcast by the node, the port, and the score.\n\n",
            "Examples:\n\n",
            "Peer counts table of reachable networks and list of local addresses\n",
            "> dash-cli -netinfo\n\n",
            "The same, preceded by a peers listing without address and version columns\n",
            "> dash-cli -netinfo 1\n\n",
            "Full dashboard\n",
        ));
        s.push_str(&format!("> dash-cli -netinfo {max}\n\n"));
        s.push_str("Full live dashboard, adjust --interval or --no-title as needed (Linux)\n");
        s.push_str(&format!("> watch --interval 1 --no-title dash-cli -netinfo {max}\n\n"));
        s.push_str("See this help\n");
        s.push_str("> dash-cli -netinfo help\n");
        s
    }
}

/// Core: `ParseUInt8`: `std::from_chars` over the whole string, after one leading `+`.
fn parse_uint8(s: &str) -> Option<u8> {
    if s.starts_with("+-") {
        return None;
    }
    let digits = s.strip_prefix('+').unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u8>().ok()
}

impl RequestHandler for NetinfoRequestHandler<'_> {
    fn prepare_request(&mut self, _method: &str, args: &[String]) -> Result<UniValue, String> {
        if let Some(first) = args.first() {
            match parse_uint8(first) {
                Some(n) => self.details_level = n.min(MAX_DETAIL_LEVEL),
                None => {
                    return Err(format!(
                        "invalid -netinfo argument: {first}\nFor more information, run: dash-cli -netinfo help"
                    ));
                }
            }
        }
        let mut result = UniValue::new_array();
        result.push_back(jsonrpc_request_obj("getpeerinfo", UniValue::Null, ID_PEERINFO as i32));
        result.push_back(jsonrpc_request_obj(
            "getnetworkinfo",
            UniValue::Null,
            ID_NETINFO_NETWORKINFO as i32,
        ));
        Ok(result)
    }

    fn process_reply(&mut self, batch_in: &UniValue) -> Result<UniValue, String> {
        let batch = err(jsonrpc_process_batch_reply(batch_in))?;
        if !slot(&batch, ID_PEERINFO).key("error").is_null() {
            return Ok(slot(&batch, ID_PEERINFO).clone());
        }
        if !slot(&batch, ID_NETINFO_NETWORKINFO).key("error").is_null() {
            return Ok(slot(&batch, ID_NETINFO_NETWORKINFO).clone());
        }

        let networkinfo = slot(&batch, ID_NETINFO_NETWORKINFO).key("result");
        if err(networkinfo.key("version").get_int())? < 200_000 {
            return Err("-netinfo requires dashd server to be running v20.0 and up".to_string());
        }
        let time_now =
            SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);

        // Count peer connection totals, and if details are requested, store peer data in a vector.
        for peer in err(slot(&batch, ID_PEERINFO).key("result").get_values())? {
            let network = err(peer.key("network").get_str())?;
            let Some(network_id) = network_string_to_id(network) else {
                continue;
            };
            let is_outbound = !err(peer.key("inbound").get_bool())?;
            let is_tx_relay = if peer.key("relaytxes").is_null() {
                true
            } else {
                err(peer.key("relaytxes").get_bool())?
            };
            let conn_type = err(peer.key("connection_type").get_str())?.to_string();
            let out = usize::from(is_outbound);
            self.counts[out][network_id] = self.counts[out][network_id].wrapping_add(1); // in/out by network
            self.counts[out][NETWORKS.len()] = self.counts[out][NETWORKS.len()].wrapping_add(1); // in/out overall
            self.counts[2][network_id] = self.counts[2][network_id].wrapping_add(1); // total by network
            self.counts[2][NETWORKS.len()] = self.counts[2][NETWORKS.len()].wrapping_add(1); // total overall
            if conn_type == "block-relay-only" {
                self.block_relay_peers_count = self.block_relay_peers_count.wrapping_add(1);
            }
            if conn_type == "manual" {
                self.manual_peers_count = self.manual_peers_count.wrapping_add(1);
            }
            if self.details_requested() {
                // Push data for this peer to the peers vector.
                let peer_id = err(peer.key("id").get_int())?;
                let mapped_as = if peer.key("mapped_as").is_null() {
                    0
                } else {
                    err(peer.key("mapped_as").get_int())?
                };
                let version = err(peer.key("version").get_int())?;
                let addr_processed = if peer.key("addr_processed").is_null() {
                    0
                } else {
                    err(peer.key("addr_processed").get_int64())?
                };
                let addr_rate_limited = if peer.key("addr_rate_limited").is_null() {
                    0
                } else {
                    err(peer.key("addr_rate_limited").get_int64())?
                };
                let conn_time = err(peer.key("conntime").get_int64())?;
                let last_blck = err(peer.key("last_block").get_int64())?;
                let last_recv = err(peer.key("lastrecv").get_int64())?;
                let last_send = err(peer.key("lastsend").get_int64())?;
                let last_trxn = err(peer.key("last_transaction").get_int64())?;
                let min_ping = if peer.key("minping").is_null() {
                    -1.0
                } else {
                    err(peer.key("minping").get_real())?
                };
                let ping = if peer.key("pingtime").is_null() {
                    -1.0
                } else {
                    err(peer.key("pingtime").get_real())?
                };
                let addr = err(peer.key("addr").get_str())?.to_string();
                let age = if conn_time == 0 {
                    String::new()
                } else {
                    (time_now.wrapping_sub(conn_time) / 60).to_string()
                };
                let sub_version = err(peer.key("subver").get_str())?.to_string();
                let transport = if peer.key("transport_protocol_type").is_null() {
                    "v1".to_string()
                } else {
                    err(peer.key("transport_protocol_type").get_str())?.to_string()
                };
                let is_addr_relay_enabled = if peer.key("addr_relay_enabled").is_null() {
                    false
                } else {
                    err(peer.key("addr_relay_enabled").get_bool())?
                };
                let is_bip152_hb_from = err(peer.key("bip152_hb_from").get_bool())?;
                let is_bip152_hb_to = err(peer.key("bip152_hb_to").get_bool())?;
                self.max_addr_length = (addr.len() + 1).max(self.max_addr_length);
                self.max_addr_processed_length =
                    addr_processed.to_string().len().max(self.max_addr_processed_length);
                self.max_addr_rate_limited_length =
                    addr_rate_limited.to_string().len().max(self.max_addr_rate_limited_length);
                self.max_age_length = age.len().max(self.max_age_length);
                self.max_id_length = peer_id.to_string().len().max(self.max_id_length);
                self.is_asmap_on |= mapped_as != 0;
                self.peers.push(Peer {
                    addr,
                    sub_version,
                    conn_type,
                    network: NETWORK_SHORT_NAMES[network_id],
                    age,
                    transport_protocol_type: transport,
                    min_ping,
                    ping,
                    addr_processed,
                    addr_rate_limited,
                    last_blck,
                    last_recv,
                    last_send,
                    last_trxn,
                    id: peer_id,
                    mapped_as,
                    version,
                    is_addr_relay_enabled,
                    is_bip152_hb_from,
                    is_bip152_hb_to,
                    is_outbound,
                    is_tx_relay,
                });
            }
        }

        // Generate report header.
        // The arguments of the C++ `strprintf` are evaluated right to left, which decides which
        // error is reported first.
        let subversion = err(networkinfo.key("subversion").get_str())?;
        let protocol_version = err(networkinfo.key("protocolversion").get_int())?;
        let mut result = format!(
            "{PACKAGE_NAME} client {CLIENT_VERSION}{} - server {protocol_version}{subversion}\n\n",
            self.chain_to_string()?,
        );

        // Report detailed peer connections list sorted by direction and minimum ping time.
        if self.details_requested() && !self.peers.is_empty() {
            // `std::sort(m_peers)` with `Peer::operator<`: `std::tie(is_outbound, min_ping)`.
            // The sort is unstable, so ties come out in libstdc++'s order.
            let mut order: Vec<usize> = (0..self.peers.len()).collect();
            let peers = &self.peers;
            crate::introsort::std_sort(&mut order, &|&a: &usize, &b: &usize| {
                let (a, b) = (&peers[a], &peers[b]);
                (!a.is_outbound && b.is_outbound)
                    || (a.is_outbound == b.is_outbound && a.min_ping < b.min_ping)
            });
            let mut taken: Vec<Option<Peer>> =
                std::mem::take(&mut self.peers).into_iter().map(Some).collect();
            self.peers = order.into_iter().map(|i| taken[i].take().unwrap()).collect();
            result.push_str(&format!(
                "<->   type   net  v  mping   ping send recv  txn  blk  hb {}{}{} ",
                pad_left("addrp", self.max_addr_processed_length),
                pad_left("addrl", self.max_addr_rate_limited_length),
                pad_left("age", self.max_age_length),
            ));
            if self.is_asmap_on {
                result.push_str(" asmap ");
            }
            result.push_str(&format!(
                "{} {}{}\n",
                pad_left("id", self.max_id_length),
                pad_right(
                    if self.is_address_selected() {
                        "address"
                    } else {
                        ""
                    },
                    if self.is_address_selected() {
                        self.max_addr_length
                    } else {
                        0
                    }
                ),
                if self.is_version_selected() {
                    "version"
                } else {
                    ""
                },
            ));
            for peer in &self.peers {
                let version = format!("{}{}", peer.version, peer.sub_version);
                let transport = peer.transport_protocol_type.as_bytes();
                let transport_char = if transport.len() == 2 && transport[0] == b'v' {
                    char::from(transport[1])
                } else {
                    ' '
                };
                let hb = format!(
                    "{}{}",
                    if peer.is_bip152_hb_to {
                        "."
                    } else {
                        " "
                    },
                    if peer.is_bip152_hb_from {
                        "*"
                    } else {
                        " "
                    }
                );
                let send = if peer.last_send != 0 {
                    time_now.wrapping_sub(peer.last_send).to_string()
                } else {
                    String::new()
                };
                let recv = if peer.last_recv != 0 {
                    time_now.wrapping_sub(peer.last_recv).to_string()
                } else {
                    String::new()
                };
                let txn = if peer.last_trxn != 0 {
                    (time_now.wrapping_sub(peer.last_trxn) / 60).to_string()
                } else if peer.is_tx_relay {
                    String::new()
                } else {
                    "*".to_string()
                };
                let blk = if peer.last_blck != 0 {
                    (time_now.wrapping_sub(peer.last_blck) / 60).to_string()
                } else {
                    String::new()
                };
                let addrp = if peer.addr_processed != 0 {
                    peer.addr_processed.to_string()
                } else if peer.is_addr_relay_enabled {
                    String::new()
                } else {
                    ".".to_string()
                };
                let addrl = if peer.addr_rate_limited != 0 {
                    peer.addr_rate_limited.to_string()
                } else {
                    String::new()
                };
                let asmap = if self.is_asmap_on && peer.mapped_as != 0 {
                    peer.mapped_as.to_string()
                } else {
                    String::new()
                };
                result.push_str(&format!(
                    "{} {} {} {}{}{}{}{}{}{}  {} {}{}{}{} {} {}{}\n",
                    pad_left(
                        if peer.is_outbound {
                            "out"
                        } else {
                            "in"
                        },
                        3
                    ),
                    pad_left(Self::connection_type_for_netinfo(&peer.conn_type), 6),
                    pad_left(peer.network, 5),
                    pad_left(&transport_char.to_string(), 2),
                    pad_left(&Self::ping_time_to_string(peer.min_ping), 7),
                    pad_left(&Self::ping_time_to_string(peer.ping), 7),
                    pad_left(&send, 5),
                    pad_left(&recv, 5),
                    pad_left(&txn, 5),
                    pad_left(&blk, 5),
                    pad_left(&hb, 2),
                    pad_left(&addrp, self.max_addr_processed_length),
                    pad_left(&addrl, self.max_addr_rate_limited_length),
                    pad_left(&peer.age, self.max_age_length),
                    pad_left(
                        &asmap,
                        if self.is_asmap_on {
                            7
                        } else {
                            0
                        }
                    ),
                    pad_left(&peer.id.to_string(), self.max_id_length),
                    pad_right(
                        if self.is_address_selected() {
                            &peer.addr
                        } else {
                            ""
                        },
                        if self.is_address_selected() {
                            self.max_addr_length
                        } else {
                            0
                        }
                    ),
                    if self.is_version_selected() && version != "0" {
                        version.as_str()
                    } else {
                        ""
                    },
                ));
            }
            result.push_str(&format!(
                "                        ms     ms  sec  sec  min  min                {}\n\n",
                pad_left("min", self.max_age_length)
            ));
        }

        // Report peer connection totals by type.
        result.push_str("     ");
        let mut reachable_networks: Vec<usize> = Vec::new();
        for network in err(networkinfo.key("networks").get_values())? {
            if err(network.key("reachable").get_bool())? {
                let network_name = err(network.key("name").get_str())?;
                let Some(network_id) = network_string_to_id(network_name) else {
                    continue;
                };
                result.push_str(&pad_left(network_name, 8)); // column header
                reachable_networks.push(network_id);
            }
        }

        for network_id in UNREACHABLE_NETWORK_IDS {
            if self.counts[2][network_id] == 0 {
                continue;
            }
            result.push_str(&pad_left(NETWORK_SHORT_NAMES[network_id], 8)); // column header
            reachable_networks.push(network_id);
        }

        result.push_str("   total   block");
        if self.manual_peers_count != 0 {
            result.push_str("  manual");
        }

        for (i, row) in ["in", "out", "total"].iter().enumerate() {
            result.push_str(&format!("\n{}", pad_right(row, 5))); // row header
            for &n in &reachable_networks {
                result.push_str(&pad_left(&self.counts[i][n].to_string(), 8)); // network peers count
            }
            result.push_str(&format!(
                "   {}",
                pad_left(&self.counts[i][NETWORKS.len()].to_string(), 5)
            )); // total peers count
            if i == 1 {
                // The outbound row has two extra columns for block relay and manual peer counts.
                result.push_str(&format!(
                    "   {}",
                    pad_left(&self.block_relay_peers_count.to_string(), 5)
                ));
                if self.manual_peers_count != 0 {
                    result.push_str(&format!(
                        "   {}",
                        pad_left(&self.manual_peers_count.to_string(), 5)
                    ));
                }
            }
        }

        // Report local addresses, ports, and scores.
        result.push_str("\n\nLocal addresses");
        let local_addrs = err(networkinfo.key("localaddresses").get_values())?;
        if local_addrs.is_empty() {
            result.push_str(": n/a\n");
        } else {
            let mut max_addr_size = 0usize;
            for addr in &local_addrs {
                max_addr_size = max_addr_size.max(err(addr.key("address").get_str())?.len() + 1);
            }
            for addr in &local_addrs {
                result.push_str(&format!(
                    "\n{}    port {}    score {}",
                    pad_right(err(addr.key("address").get_str())?, max_addr_size),
                    pad_left(&err(addr.key("port").get_int())?.to_string(), 6),
                    pad_left(&err(addr.key("score").get_int())?.to_string(), 6),
                ));
            }
        }

        Ok(jsonrpc_reply_obj(UniValue::Str(result), 1))
    }
}

/// Core: `GenerateToAddressRequestHandler`.
#[derive(Default)]
pub struct GenerateToAddressRequestHandler {
    address_str: String,
}

impl RequestHandler for GenerateToAddressRequestHandler {
    fn prepare_request(&mut self, _method: &str, args: &[String]) -> Result<UniValue, String> {
        self.address_str = args.get(1).ok_or_else(|| "vector::_M_range_check".to_string())?.clone();
        let params = rpc_convert_values("generatetoaddress", args)?;
        Ok(jsonrpc_request_obj("generatetoaddress", params, 1))
    }

    fn process_reply(&mut self, reply: &UniValue) -> Result<UniValue, String> {
        let mut result = UniValue::new_object();
        result.push_kv("address", self.address_str.as_str().into());
        result.push_kv("blocks", err(reply.get_obj())?.key("result").clone());
        Ok(jsonrpc_reply_obj(result, 1))
    }
}

/// Core: `DefaultRequestHandler`.
pub struct DefaultRequestHandler<'a> {
    args: &'a ArgsManager,
}

impl<'a> DefaultRequestHandler<'a> {
    pub fn new(args: &'a ArgsManager) -> Self {
        DefaultRequestHandler {
            args,
        }
    }
}

impl RequestHandler for DefaultRequestHandler<'_> {
    fn prepare_request(&mut self, method: &str, args: &[String]) -> Result<UniValue, String> {
        let params = if err(self.args.get_bool_arg("-named", DEFAULT_NAMED))? {
            rpc_convert_named_values(method, args)?
        } else {
            rpc_convert_values(method, args)?
        };
        Ok(jsonrpc_request_obj(method, params, 1))
    }

    fn process_reply(&mut self, reply: &UniValue) -> Result<UniValue, String> {
        Ok(err(reply.get_obj())?.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uint8_parsing_follows_core() {
        assert_eq!(parse_uint8("4"), Some(4));
        assert_eq!(parse_uint8("+4"), Some(4));
        assert_eq!(parse_uint8("255"), Some(255));
        assert_eq!(parse_uint8("256"), None);
        assert_eq!(parse_uint8("-1"), None);
        assert_eq!(parse_uint8("+-1"), None);
        assert_eq!(parse_uint8(""), None);
        assert_eq!(parse_uint8("1x"), None);
        assert_eq!(parse_uint8(" 1"), None);
    }

    #[test]
    fn padding_counts_bytes() {
        assert_eq!(pad_left("ab", 4), "  ab");
        assert_eq!(pad_right("ab", 4), "ab  ");
        assert_eq!(pad_left("abc", 2), "abc");
    }

    #[test]
    fn ping_times() {
        assert_eq!(NetinfoRequestHandler::ping_time_to_string(-1.0), "");
        assert_eq!(NetinfoRequestHandler::ping_time_to_string(0.0004), "0");
        assert_eq!(NetinfoRequestHandler::ping_time_to_string(0.1236), "124");
        assert_eq!(NetinfoRequestHandler::ping_time_to_string(1000.0), "-");
    }
}
