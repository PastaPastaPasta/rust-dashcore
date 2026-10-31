//! A peer running Dash Core v24's protocol version (70242) still gets the session dash-spv expects,
//! while dash-spv itself keeps advertising 70237 (BL-111).

use std::io::ErrorKind;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dash_spv::client::ClientConfig;
use dash_spv::network::handshake::SPV_ADVERTISED_PROTOCOL_VERSION;
use dash_spv::network::{NetworkEvent, NetworkManager, PeerNetworkManager};
use dashcore::consensus::encode::{self, deserialize_partial, serialize};
use dashcore::network::address::Address;
use dashcore::network::constants::{ServiceFlags, PROTOCOL_VERSION};
use dashcore::network::message::{NetworkMessage, RawNetworkMessage};
use dashcore::network::message_network::VersionMessage;
use dashcore::Network;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::time::timeout;

async fn send_message(stream: &mut TcpStream, message: NetworkMessage) {
    let raw = RawNetworkMessage {
        magic: Network::Regtest.magic(),
        payload: message,
    };
    stream.write_all(&serialize(&raw)).await.unwrap();
}

async fn read_message(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> NetworkMessage {
    loop {
        match deserialize_partial::<RawNetworkMessage>(buffer) {
            Ok((raw, consumed)) => {
                buffer.drain(..consumed);
                return raw.payload;
            }
            Err(encode::Error::Io(e)) if e.kind() == ErrorKind::UnexpectedEof => {}
            Err(e) => panic!("dash-spv sent a frame that does not decode: {e}"),
        }
        let mut chunk = [0u8; 4096];
        let read = stream.read(&mut chunk).await.unwrap();
        assert_ne!(read, 0, "dash-spv closed the connection mid-handshake");
        buffer.extend_from_slice(&chunk[..read]);
    }
}

fn core_v24_version_message(peer: SocketAddr) -> VersionMessage {
    let local = "127.0.0.1:0".parse().unwrap();
    VersionMessage {
        version: PROTOCOL_VERSION,
        services: ServiceFlags::NETWORK | ServiceFlags::NODE_HEADERS_COMPRESSED,
        timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64,
        receiver: Address::new(&peer, ServiceFlags::NETWORK),
        sender: Address::new(&local, ServiceFlags::NETWORK),
        nonce: 1,
        user_agent: "/Dash Core:24.0.0/".to_owned(),
        start_height: 2,
        relay: true,
        mn_auth_challenge: [0; 32],
        masternode_connection: false,
    }
}

#[tokio::test]
async fn core_v24_peer_completes_handshake_and_gets_sendheaders2() {
    assert_eq!(PROTOCOL_VERSION, 70242, "the mock peer must play Dash Core v24");

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer_addr = listener.local_addr().unwrap();
    let (seen_tx, seen_rx) = oneshot::channel();
    let (close_tx, close_rx) = oneshot::channel::<()>();

    let server = tokio::spawn(async move {
        let (mut stream, client_addr) = listener.accept().await.unwrap();
        send_message(&mut stream, NetworkMessage::Version(core_v24_version_message(client_addr)))
            .await;
        send_message(&mut stream, NetworkMessage::Verack).await;

        // Collect what dash-spv sends until the post-handshake header preference shows up.
        let mut buffer = Vec::new();
        let mut received = Vec::new();
        loop {
            let message = read_message(&mut stream, &mut buffer).await;
            let done =
                matches!(message, NetworkMessage::SendHeaders2 | NetworkMessage::SendHeaders);
            received.push(message);
            if done {
                break;
            }
        }
        seen_tx.send(received).unwrap();
        let _ = close_rx.await;
    });

    let storage = TempDir::new().unwrap();
    let mut config = ClientConfig::new(Network::Regtest);
    config.storage_path = storage.path().to_path_buf();
    config.max_peers = 1;
    config.peers = vec![peer_addr];
    config.restrict_to_configured_peers = true;
    config.enable_filters = false;
    config.enable_masternodes = false;

    let mut manager = PeerNetworkManager::new(&config).await.unwrap();
    let mut events = manager.subscribe_network_events();
    manager.start().await.unwrap();

    let received = timeout(Duration::from_secs(10), seen_rx)
        .await
        .expect("dash-spv never finished the handshake with a 70242 peer")
        .unwrap();
    // The peer's height only reaches the pool once `Peer::update_peer_info` accepted its version
    // message, so this also shows that 70242 is inside the bounds dash-spv validates.
    let best_height = timeout(Duration::from_secs(5), async {
        loop {
            if let NetworkEvent::PeersUpdated {
                connected_count: 1,
                best_height,
                ..
            } = events.recv().await.unwrap()
            {
                return best_height;
            }
        }
    })
    .await
    .expect("a 70242 peer was not kept in the pool");
    assert_eq!(best_height, Some(2), "dash-spv rejected the 70242 peer's version message");

    let version = received
        .iter()
        .find_map(|message| match message {
            NetworkMessage::Version(version) => Some(version),
            _ => None,
        })
        .expect("dash-spv sent no version message");
    assert_eq!(version.version, 70237);
    assert_eq!(version.version, SPV_ADVERTISED_PROTOCOL_VERSION);
    assert!(received.contains(&NetworkMessage::Verack), "dash-spv did not verack the peer");
    assert_eq!(
        received.last(),
        Some(&NetworkMessage::SendHeaders2),
        "a peer with NODE_HEADERS_COMPRESSED must get sendheaders2"
    );

    manager.stop().await;
    close_tx.send(()).unwrap();
    server.await.unwrap();
}
