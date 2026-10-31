use honggfuzz::fuzz;

fn do_test(data: &[u8]) {
    let tx_result: Result<dashcore::blockdata::transaction::Transaction, _> =
        dashcore::consensus::encode::deserialize(data);
    match tx_result {
        Err(_) => {}
        Ok(tx) => {
            let ser = dashcore::consensus::encode::serialize(&tx);
            if (tx.version as i16) < 0 {
                // Core writes `nVersion | (nType << 16)` (`primitives/transaction.h`): a negative
                // version sign-extends over the type slot, so those two bytes do not round-trip.
                assert_eq!(ser.len(), data.len());
                assert_eq!(&ser[..2], &data[..2]);
                assert_eq!(&ser[4..], &data[4..]);
            } else {
                assert_eq!(&ser[..], data);
            }
            // Dash serializes no witnesses (no BIP144), with or without inputs: the size is the
            // serialized length and the weight is four times that.
            assert_eq!(tx.size(), ser.len());
            assert_eq!(tx.weight().to_wu() as usize, 4 * ser.len());
        }
    }
}

fn main() {
    loop {
        fuzz!(|data| {
            do_test(data);
        });
    }
}

#[cfg(all(test, fuzzing))]
mod tests {
    fn extend_vec_from_hex(hex: &str, out: &mut Vec<u8>) {
        let mut b = 0;
        for (idx, c) in hex.as_bytes().iter().enumerate() {
            b <<= 4;
            match *c {
                b'A'..=b'F' => b |= c - b'A' + 10,
                b'a'..=b'f' => b |= c - b'a' + 10,
                b'0'..=b'9' => b |= c - b'0',
                _ => panic!("Bad hex"),
            }
            if (idx & 1) == 1 {
                out.push(b);
                b = 0;
            }
        }
    }

    #[test]
    fn zero_input_transaction_has_no_marker_or_flag() {
        // Version 1, no inputs, no outputs, lock time 0: ten bytes, weight forty.
        let mut a = Vec::new();
        extend_vec_from_hex("01000000000000000000", &mut a);
        super::do_test(&a);
    }

    #[test]
    fn duplicate_crash() {
        let mut a = Vec::new();
        extend_vec_from_hex("000700000001000000010000", &mut a);
        super::do_test(&a);
    }
}
