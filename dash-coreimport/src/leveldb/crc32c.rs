//! Castagnoli CRC and LevelDB's checksum mask.

const fn table() -> [u32; 256] {
    let mut out = [0; 256];
    let mut n = 0;
    while n < out.len() {
        let mut crc = n as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = (crc >> 1) ^ (0x82f63b78 & (0u32.wrapping_sub(crc & 1)));
            bit += 1;
        }
        out[n] = crc;
        n += 1;
    }
    out
}

const TABLE: [u32; 256] = table();

pub(crate) fn extend(initial: u32, bytes: &[u8]) -> u32 {
    let mut crc = !initial;
    for &byte in bytes {
        crc = TABLE[((crc as u8) ^ byte) as usize] ^ (crc >> 8);
    }
    !crc
}

pub(crate) fn mask(crc: u32) -> u32 {
    crc.rotate_right(15).wrapping_add(0xa282ead8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leveldb_vectors() {
        assert_eq!(extend(0, b"123456789"), 0xe3069283);
        assert_eq!(extend(0, &[0; 32]), 0x8a9136aa);
        assert_eq!(extend(0, &[255; 32]), 0x62a8ab43);
        assert_eq!(extend(0, &std::array::from_fn::<_, 32, _>(|n| n as u8)), 0x46dd794e);
        assert_eq!(extend(0, &std::array::from_fn::<_, 32, _>(|n| 31 - n as u8)), 0x113fdb5c);
        let mut data = [0u8; 48];
        data[0] = 1;
        data[1] = 0xc0;
        data[16] = 0x14;
        data[22] = 4;
        data[27] = 0x14;
        data[31] = 0x18;
        data[32] = 0x28;
        data[40] = 2;
        assert_eq!(extend(0, &data), 0xd9963a56);
        assert_ne!(extend(0, b"a"), extend(0, b"foo"));
        assert_eq!(extend(extend(0, b"hello "), b"world"), extend(0, b"hello world"));
        let crc = extend(0, b"foo");
        assert_ne!(crc, mask(crc));
        assert_ne!(crc, mask(mask(crc)));
        let unmask = |n: u32| n.wrapping_sub(0xa282ead8).rotate_left(15);
        assert_eq!(unmask(mask(crc)), crc);
        assert_eq!(unmask(unmask(mask(mask(crc)))), crc);
    }
}
