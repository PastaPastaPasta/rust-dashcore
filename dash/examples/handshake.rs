extern crate dashcore;

use dashcore::consensus::{encode, Decodable};
use dashcore::constants::{genesis_block, ChainHash};
use dashcore::network::{address, constants, message, message_network};
use dashcore::secp256k1::rand::Rng;
use dashcore::{block, secp256k1, BlockHash, Network};
use dashcore_hashes::Hash;
use secp256k1::rand;
use std::cmp::PartialEq;
use std::io::{BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpStream};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, process};

#[derive(PartialEq)]
enum HandshakeState {
    INIT,
    VERSION_SENT,
    VERSION_RECEIVED,
    VERACK_SENT,
    VERACK_RECEIVED,
    HANDSHAKE_COMPLETE,
}

struct PingInfo {
    sent_nonce: u64,
    sent_time: SystemTime,
}

struct Node {
    address: SocketAddr,
    stream: Option<TcpStream>,
    handshake_complete: HandshakeState,
    ping_info: PingInfo,
    sent_desires: bool,
    init_block_count: Option<i32>,
    // New: store headers that have been received
    headers: Vec<block::Header>,
    headers_in_flight_to: i32,
}

impl Node {
    fn connect(&mut self) {
        if let Ok(stream) = TcpStream::connect(self.address) {
            self.stream = Some(stream);
        }
    }
}

fn main() {
    // This example establishes a connection to a Bitcoin node, sends the intial
    // "version" message, waits for the reply, and finally closes the connection.
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("not enough arguments");
        process::exit(1);
    }

    let str_address = &args[1];

    let address: SocketAddr = str_address.parse().unwrap_or_else(|error| {
        eprintln!("Error parsing address: {:?}", error);
        process::exit(1);
    });

    let mut node = Node {
        address,
        stream: None,
        handshake_complete: HandshakeState::INIT,
        ping_info: PingInfo {
            sent_nonce: 0,
            sent_time: SystemTime::UNIX_EPOCH,
        },
        sent_desires: false,
        init_block_count: None,
        headers: Vec::new(),
        headers_in_flight_to: 0,
    };

    node.connect();

    let mut need_more_headers = true;

    // Setup StreamReader
    let read_stream = node.stream.as_ref().unwrap().try_clone().unwrap();
    let mut stream_reader = BufReader::new(read_stream);
    loop {
        handle_handshake(&mut node);
        if node.handshake_complete != HandshakeState::HANDSHAKE_COMPLETE {
            continue;
        }
        send_ping_message(&mut node);
        maybe_send_desires(&mut node);

        if need_more_headers {
            sync_headers(&mut node);
        }

        // Loop an retrieve new messages
        let reply = message::RawNetworkMessage::consensus_decode(&mut stream_reader).unwrap();
        match reply.payload {
            message::NetworkMessage::Headers(headers) => {
                println!("Received {} headers:", headers.len());
                for header in headers {
                    // println!("have so far {:?}; processing {:?}", node.headers.len(), header);
                    let last_hash = if let Some(last_header) = node.headers.last() {
                        last_header.block_hash()
                    } else {
                        Network::Dash.known_genesis_block_hash().unwrap()
                    };
                    if last_hash == header.prev_blockhash {
                        // Add header to node.headers
                        node.headers.push(header);
                    } else {
                        assert!(
                            false,
                            "Received header {:?} with incorrect prev_blockhash {:?}",
                            header.block_hash(),
                            header.prev_blockhash
                        );
                    }
                }
                if let Some(init_block_count) = node.init_block_count {
                    if node.headers.len() >= init_block_count as usize {
                        need_more_headers = false;
                    }
                }
            }
            message::NetworkMessage::Version(_) => {
                println!("Received version message: {:?}", reply.payload);

                let second_message = message::RawNetworkMessage {
                    magic: constants::Network::Dash.magic(),
                    payload: message::NetworkMessage::Verack,
                };

                let _ = node
                    .stream
                    .as_ref()
                    .unwrap()
                    .write_all(encode::serialize(&second_message).as_slice());
                println!("Sent verack message");
            }
            message::NetworkMessage::Verack => {
                println!("Received verack message: {:?}", reply.payload);
                // break;
            }
            message::NetworkMessage::Ping(nonce) => {
                println!("Received ping message: {:?}", reply.payload);
                let pong_message = message::RawNetworkMessage {
                    magic: constants::Network::Dash.magic(),
                    payload: message::NetworkMessage::Pong(nonce),
                };
                let _ = node
                    .stream
                    .as_ref()
                    .unwrap()
                    .write_all(encode::serialize(&pong_message).as_slice());
                println!("Sent pong message: {:?}", nonce);
            }
            message::NetworkMessage::Pong(nonce) => {
                println!("Received pong message: {:?}", reply.payload);
                if nonce != node.ping_info.sent_nonce {
                    println!("ERROR: Received pong message with incorrect nonce: {:?}", nonce);
                }
            }
            _ => {
                println!("Received unknown message: {:?}", reply.payload);
                // break;
            }
        }
    }
    let _ = node.stream.as_ref().unwrap().shutdown(Shutdown::Both);
}

fn handle_handshake(node: &mut Node) {
    if node.handshake_complete == HandshakeState::HANDSHAKE_COMPLETE {
        return;
    }

    if node.handshake_complete == HandshakeState::INIT {
        let version_message = build_version_message(node.address);
        let raw_version_message = message::RawNetworkMessage {
            magic: constants::Network::Dash.magic(),
            payload: version_message,
        };
        let _ = node
            .stream
            .as_mut()
            .unwrap()
            .write_all(encode::serialize(&raw_version_message).as_slice());
        println!("Sent version message");
        node.handshake_complete = HandshakeState::VERSION_SENT;
    }

    let stream = node.stream.as_mut().unwrap();
    let reply = message::RawNetworkMessage::consensus_decode(stream).unwrap();
    match reply.payload {
        message::NetworkMessage::Version(version) => {
            println!("Received version message: {:?}", version);

            node.init_block_count = Some(version.start_height);

            let second_message = message::RawNetworkMessage {
                magic: constants::Network::Dash.magic(),
                payload: message::NetworkMessage::Verack,
            };

            let _ = stream.write_all(encode::serialize(&second_message).as_slice());
            println!("Sent verack message");
        }
        message::NetworkMessage::Verack => {
            println!("Received verack message: {:?}", reply.payload);
            node.handshake_complete = HandshakeState::HANDSHAKE_COMPLETE;
        }
        _ => {
            println!("Received unknown message: {:?}", reply.payload);
        }
    }
}

// A function which will send `sendheaders` and `sendcmpct` messages if they have not been sent yet.
fn maybe_send_desires(node: &mut Node) {
    if node.sent_desires {
        return;
    }
    let sendheaders_message = message::RawNetworkMessage {
        magic: constants::Network::Dash.magic(),
        payload: message::NetworkMessage::SendHeaders,
    };
    let _ =
        node.stream.as_mut().unwrap().write_all(encode::serialize(&sendheaders_message).as_slice());
    println!("Sent sendheaders message");
    node.sent_desires = true;
}

// A function which takes in the last ping time; and if that time was more than a minute ago, sends a ping message. Returns the nonce of the ping message.
fn send_ping_message(node: &mut Node) {
    let duration = SystemTime::now().duration_since(node.ping_info.sent_time).unwrap().as_secs();
    if duration < 10 {
        return;
    }
    let nonce: u64 = secp256k1::rand::thread_rng().gen();
    println!("Sending ping message, nonce: {:?} duration: {:?}", nonce, duration);
    let ping_message = message::RawNetworkMessage {
        magic: constants::Network::Dash.magic(),
        payload: message::NetworkMessage::Ping(nonce),
    };
    let _ = node.stream.as_mut().unwrap().write_all(encode::serialize(&ping_message).as_slice());
    println!("Sent ping message: {:?}", nonce);
    let duration = SystemTime::now().duration_since(node.ping_info.sent_time).unwrap().as_secs();
    println!("Sending ping message, duration: {:?}", duration);
    node.ping_info.sent_nonce = nonce;
    node.ping_info.sent_time = SystemTime::now();
}

fn sync_headers(node: &mut Node) {
    // Get a mutable reference to the stream.
    let stream = node.stream.as_mut().unwrap();

    if node.headers.len() < node.headers_in_flight_to as usize {
        // We shouldn't request more yet. They are still in flight.
        return;
    }

    // Step 1: Build the block locator.
    // For a simple first sync, we use the genesis block hash.
    let genesis_hash = Network::Dash.known_genesis_block_hash().unwrap();
    let current_hash = if node.headers.is_empty() {
        genesis_hash
    } else {
        node.headers.last().unwrap().block_hash()
    };

    // let genesis_hash = ChainHash::using_genesis_block(constants::Network::Dash);
    let block_locator = vec![current_hash];

    // Step 2: Define the stop hash.
    // An all-zero hash signals no specific stop condition.
    let stop_hash: BlockHash = BlockHash::from_byte_array([0; 32]);

    // Step 3: Construct the GetHeaders message.
    // Assuming dashcore provides a GetHeadersMessage similar to Bitcoin's.
    let getheaders_msg =
        dashcore::network::message_blockdata::GetHeadersMessage::new(block_locator, stop_hash);

    println!("Going to send getheaders {:?}", getheaders_msg);

    // Wrap the getheaders message in a RawNetworkMessage.
    let raw_getheaders = message::RawNetworkMessage {
        magic: constants::Network::Dash.magic(),
        payload: message::NetworkMessage::GetHeaders(getheaders_msg),
    };

    // Serialize and send the message.
    let serialized = encode::serialize(&raw_getheaders);
    stream.write_all(&serialized).expect("Failed to send getheaders message");

    // This will request up to 2000 hashes. Make sure we wait for the response before sending another getheaders message.
    node.headers_in_flight_to = node.headers_in_flight_to + 2000;

    println!("Sent getheaders message");
}

fn build_version_message(address: SocketAddr) -> message::NetworkMessage {
    // Building version message, see https://en.bitcoin.it/wiki/Protocol_documentation#version
    let my_address = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 0);

    // "bitfield of features to be enabled for this connection"
    let services = constants::ServiceFlags::NETWORK;

    // "standard UNIX timestamp in seconds"
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).expect("Time error").as_secs();

    // "The network address of the node receiving this message"
    let addr_recv = address::Address::new(&address, constants::ServiceFlags::NONE);

    // "The network address of the node emitting this message"
    let addr_from = address::Address::new(&my_address, constants::ServiceFlags::NONE);

    // "Node random nonce, randomly generated every time a version packet is sent. This nonce is used to detect connections to self."
    let nonce: u64 = secp256k1::rand::thread_rng().gen();

    // "User Agent (0x00 if string is 0 bytes long)"
    let user_agent = String::from("rust-dashcore");

    // "The last block received by the emitting node"
    let start_height: i32 = 0;

    // Generate challenge for masternode authentication
    let mn_auth_challenge = rand::random();

    // Construct the message
    message::NetworkMessage::Version(message_network::VersionMessage::new(
        services,
        timestamp as i64,
        addr_recv,
        addr_from,
        nonce,
        user_agent,
        start_height,
        mn_auth_challenge,
    ))
}
