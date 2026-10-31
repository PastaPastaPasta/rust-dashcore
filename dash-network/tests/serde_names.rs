//! `Network`'s serialized forms are pinned (BS16): the serde names are the lowercase chain
//! names and the bincode encoding is the variant index, so adding the `NetworkKind`
//! conversions cannot move either.
#![cfg(all(feature = "serde", feature = "bincode"))]

use dash_network::Network;

const SNAPSHOT: [(Network, &str, u8); 4] = [
    (Network::Mainnet, "\"mainnet\"", 0),
    (Network::Testnet, "\"testnet\"", 1),
    (Network::Devnet, "\"devnet\"", 2),
    (Network::Regtest, "\"regtest\"", 3),
];

#[test]
fn serde_names_are_pinned() {
    for (network, json, _) in SNAPSHOT {
        assert_eq!(serde_json::to_string(&network).unwrap(), json);
        assert_eq!(serde_json::from_str::<Network>(json).unwrap(), network);
    }
}

#[test]
fn serde_rejects_aliases_and_other_cases() {
    // `FromStr` takes aliases and any case; serde takes exactly the lowercase names.
    for bad in ["\"main\"", "\"Mainnet\"", "\"MAINNET\"", "\"test\"", "\"dev\"", "\"\"", "0"] {
        assert!(serde_json::from_str::<Network>(bad).is_err(), "{bad}");
    }
}

#[test]
fn bincode_encoding_is_pinned() {
    let config = bincode::config::standard();
    for (network, _, byte) in SNAPSHOT {
        let bytes = bincode::encode_to_vec(network, config).unwrap();
        assert_eq!(bytes, [byte], "{network}");
        let (back, used): (Network, usize) = bincode::decode_from_slice(&bytes, config).unwrap();
        assert_eq!((back, used), (network, 1));
    }
    assert!(bincode::decode_from_slice::<Network, _>(&[4], config).is_err());
}

#[cfg(feature = "params")]
#[test]
fn round_trip_through_network_kind_keeps_the_serialized_form() {
    use dash_params::NetworkKind;

    for (network, json, _) in SNAPSHOT {
        let kind = NetworkKind::from(network);
        let back = Network::from(kind);
        assert_eq!(serde_json::to_string(&back).unwrap(), json);
    }
}
