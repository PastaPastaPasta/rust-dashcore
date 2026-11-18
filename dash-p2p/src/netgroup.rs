//! Core's `NetGroupManager` (`netgroup.cpp`) and the asmap interpreter (`util/asmap.cpp`): the
//! group an address is bucketed and diversified by.

use crate::netaddr::{NetAddr, Network};

/// Core's `NetGroupManager`: the group of an address, by AS number when an asmap is loaded.
#[derive(Clone, Debug, Default)]
pub struct NetGroupManager {
    /// The asmap as Core's `DecodeAsmap` returns it: bits, least significant first in each byte.
    asmap: Vec<bool>,
    allow_private_net: bool,
}

impl NetGroupManager {
    /// A manager over `asmap` bits (empty for none), as [`NetGroupManager::decode_asmap`]
    /// returns them. The host refuses to start on an empty decode of a given file, as Core's init
    /// does ("Could not parse asmap file"). Bits that never passed `SanityCheckASMap` are the
    /// caller's to answer for: Core asserts on them in `Interpret`, this maps them to AS 0.
    pub fn new(asmap: Vec<bool>, allow_private_net: bool) -> Self {
        Self {
            asmap,
            allow_private_net,
        }
    }

    /// Core's `DecodeAsmap` on file bytes: the bits, least significant first in each byte, or
    /// nothing when they fail `SanityCheckASMap(bits, 128)`.
    // Core: util/asmap.cpp:197-222 DecodeAsmap
    pub fn decode_asmap(bytes: &[u8]) -> Vec<bool> {
        let bits: Vec<bool> =
            bytes.iter().flat_map(|&b| (0..8).map(move |i| (b >> i) & 1 == 1)).collect();
        if !sanity_check_asmap(&bits, 128) {
            tracing::info!(target: "net", "Sanity check of asmap file failed");
            return Vec::new();
        }
        bits
    }

    /// Whether an asmap is loaded (`UsingASMap`).
    pub fn using_asmap(&self) -> bool {
        !self.asmap.is_empty()
    }

    /// The asmap bits, for the AddrMan serialization's asmap checksum.
    pub fn asmap(&self) -> &[bool] {
        &self.asmap
    }

    /// Core's `GetGroup`.
    // Core: netgroup.cpp:18-79 NetGroupManager::GetGroup
    pub fn group(&self, address: &NetAddr) -> Vec<u8> {
        let asn = self.mapped_as(address);
        if asn != 0 {
            let mut out = vec![Network::Ipv6 as u8];
            out.extend_from_slice(&asn.to_le_bytes());
            return out;
        }
        let mut out = vec![address.net_class(self.allow_private_net) as u8];
        let (mut start, mut bits) = (0usize, 0usize);
        if address.is_local() {
        } else if address.is_internal() {
            start = 6;
            bits = 10 * 8;
        } else if !address.is_routable(self.allow_private_net) {
        } else if address.has_linked_ipv4(self.allow_private_net) {
            let ipv4 = address.linked_ipv4();
            out.push((ipv4 >> 24) as u8);
            out.push((ipv4 >> 16) as u8);
            return out;
        } else if address.is_tor() || address.is_i2p() {
            bits = 4;
        } else if address.is_cjdns() {
            bits = 12;
        } else if address.is_he_net() {
            bits = 36;
        } else {
            bits = 32;
        }
        let bytes = address.addr_bytes();
        let num_bytes = bits / 8;
        out.extend_from_slice(&bytes[start..start + num_bytes]);
        let rem = bits % 8;
        if rem > 0 {
            out.push(bytes[num_bytes + start] | ((1u8 << (8 - rem)) - 1));
        }
        out
    }

    /// Core's `GetMappedAS`: 0 without an asmap or for a network an asmap does not cover.
    // Core: netgroup.cpp:81-111 NetGroupManager::GetMappedAS
    pub fn mapped_as(&self, address: &NetAddr) -> u32 {
        let class = address.net_class(self.allow_private_net);
        if self.asmap.is_empty() || !matches!(class, Network::Ipv4 | Network::Ipv6) {
            return 0;
        }
        let mut ip = [0u8; 16];
        if address.has_linked_ipv4(self.allow_private_net) {
            ip[10] = 0xFF;
            ip[11] = 0xFF;
            ip[12..].copy_from_slice(&address.linked_ipv4().to_be_bytes());
        } else {
            ip.copy_from_slice(&address.addr_bytes());
        }
        let bits: Vec<bool> =
            ip.iter().flat_map(|&b| (0..8).rev().map(move |i| (b >> i) & 1 == 1)).collect();
        interpret(&self.asmap, &bits)
    }
}

const INVALID: u32 = 0xFFFF_FFFF;

fn decode_bits(asmap: &[bool], pos: &mut usize, minval: u32, sizes: &[u8]) -> u32 {
    let mut val = minval;
    for (i, &size) in sizes.iter().enumerate() {
        let bit = if i + 1 != sizes.len() {
            let Some(&b) = asmap.get(*pos) else {
                break;
            };
            *pos += 1;
            b
        } else {
            false
        };
        if bit {
            val += 1 << size;
        } else {
            for b in 0..size {
                let Some(&bit) = asmap.get(*pos) else {
                    return INVALID;
                };
                *pos += 1;
                val += u32::from(bit) << (size - 1 - b);
            }
            return val;
        }
    }
    INVALID
}

const TYPE_BIT_SIZES: &[u8] = &[0, 0, 1];
const ASN_BIT_SIZES: &[u8] = &[15, 16, 17, 18, 19, 20, 21, 22, 23, 24];
const MATCH_BIT_SIZES: &[u8] = &[1, 2, 3, 4, 5, 6, 7, 8];
const JUMP_BIT_SIZES: &[u8] = &[
    5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29,
    30,
];

/// Core's `Interpret(asmap, ip)`. Core asserts on a map that runs out; this returns 0.
// Core: util/asmap.cpp Interpret
pub fn interpret(asmap: &[bool], ip: &[bool]) -> u32 {
    let mut pos = 0usize;
    let mut bits = ip.len();
    let mut default_asn = 0;
    while pos < asmap.len() {
        match decode_bits(asmap, &mut pos, 0, TYPE_BIT_SIZES) {
            0 => {
                let asn = decode_bits(asmap, &mut pos, 1, ASN_BIT_SIZES);
                return if asn == INVALID {
                    0
                } else {
                    asn
                };
            }
            1 => {
                let jump = decode_bits(asmap, &mut pos, 17, JUMP_BIT_SIZES);
                if jump == INVALID || bits == 0 || jump as usize >= asmap.len() - pos {
                    break;
                }
                if ip[ip.len() - bits] {
                    pos += jump as usize;
                }
                bits -= 1;
            }
            2 => {
                let m = decode_bits(asmap, &mut pos, 2, MATCH_BIT_SIZES);
                if m == INVALID {
                    break;
                }
                let matchlen = (u32::BITS - m.leading_zeros() - 1) as usize;
                if bits < matchlen {
                    break;
                }
                for bit in 0..matchlen {
                    if ip[ip.len() - bits] != ((m >> (matchlen - 1 - bit)) & 1 == 1) {
                        return default_asn;
                    }
                    bits -= 1;
                }
            }
            3 => {
                default_asn = decode_bits(asmap, &mut pos, 1, ASN_BIT_SIZES);
                if default_asn == INVALID {
                    break;
                }
            }
            _ => break,
        }
    }
    0
}

/// Core's `SanityCheckASMap(asmap, bits)`: whether `asmap` is a program `Interpret` runs to a
/// `RETURN` for every `bits`-bit input, with no jump into an instruction, no unreachable code,
/// no redundant `DEFAULT`, at most one short `MATCH` in a run, and at most 7 zero padding bits.
// Core: util/asmap.cpp:135-195 SanityCheckASMap
pub fn sanity_check_asmap(asmap: &[bool], mut bits: i32) -> bool {
    const RETURN: u32 = 0;
    const JUMP: u32 = 1;
    const MATCH: u32 = 2;
    const DEFAULT: u32 = 3;
    let endpos = asmap.len();
    let mut pos = 0usize;
    // All future positions we may jump to (bit offset in asmap -> bits to consume left).
    let mut jumps: Vec<(u32, i32)> = Vec::with_capacity(bits.max(0) as usize);
    let mut prevopcode = JUMP;
    let mut had_incomplete_match = false;
    while pos != endpos {
        let offset = pos as u32;
        if jumps.last().is_some_and(|&(to, _)| offset >= to) {
            // There was a jump into the middle of the previous instruction
            return false;
        }
        let opcode = decode_bits(asmap, &mut pos, 0, TYPE_BIT_SIZES);
        if opcode == RETURN {
            if prevopcode == DEFAULT {
                // There should not be any RETURN immediately after a DEFAULT (could be combined
                // into just RETURN)
                return false;
            }
            if decode_bits(asmap, &mut pos, 1, ASN_BIT_SIZES) == INVALID {
                // ASN straddles EOF
                return false;
            }
            let Some(&(to, left)) = jumps.last() else {
                // Nothing to execute anymore
                if endpos - pos > 7 {
                    // Excessive padding
                    return false;
                }
                // Nonzero padding bit, else sanely reached EOF
                return !asmap[pos..].contains(&true);
            };
            // Continue by pretending we jumped to the next instruction
            if pos as u32 != to {
                // Unreachable code
                return false;
            }
            // Restore the number of bits we would have had left after this jump
            bits = left;
            jumps.pop();
            prevopcode = JUMP;
        } else if opcode == JUMP {
            let jump = decode_bits(asmap, &mut pos, 17, JUMP_BIT_SIZES);
            if jump == INVALID {
                // Jump offset straddles EOF
                return false;
            }
            if jump as usize > endpos - pos {
                // Jump out of range
                return false;
            }
            if bits == 0 {
                // Consuming bits past the end of the input
                return false;
            }
            bits -= 1;
            let jump_offset = pos as u32 + jump;
            if jumps.last().is_some_and(|&(to, _)| jump_offset >= to) {
                // Intersecting jumps
                return false;
            }
            jumps.push((jump_offset, bits));
            prevopcode = JUMP;
        } else if opcode == MATCH {
            let m = decode_bits(asmap, &mut pos, 2, MATCH_BIT_SIZES);
            if m == INVALID {
                // Match bits straddle EOF
                return false;
            }
            let matchlen = (u32::BITS - m.leading_zeros() - 1) as i32;
            if prevopcode != MATCH {
                had_incomplete_match = false;
            }
            if matchlen < 8 && had_incomplete_match {
                // Within a sequence of matches only at most one should be incomplete
                return false;
            }
            had_incomplete_match = matchlen < 8;
            if bits < matchlen {
                // Consuming bits past the end of the input
                return false;
            }
            bits -= matchlen;
            prevopcode = MATCH;
        } else if opcode == DEFAULT {
            if prevopcode == DEFAULT {
                // There should not be two successive DEFAULTs (they could be combined into one)
                return false;
            }
            if decode_bits(asmap, &mut pos, 1, ASN_BIT_SIZES) == INVALID {
                // ASN straddles EOF
                return false;
            }
            prevopcode = DEFAULT;
        } else {
            // Instruction straddles EOF
            return false;
        }
    }
    // Reached EOF without RETURN instruction
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v6(s: &str) -> NetAddr {
        NetAddr::from_ip(s.parse().unwrap())
    }

    /// Core's `netbase_tests/netbase_getgroup` (no asmap).
    #[test]
    fn core_getgroup() {
        let ng = NetGroupManager::default();
        let ipv4 = Network::Ipv4 as u8;
        let ipv6 = Network::Ipv6 as u8;
        let unroutable = Network::Unroutable as u8;
        assert_eq!(ng.group(&v6("127.0.0.1")), vec![unroutable]);
        // "257.0.0.1" does not parse: Core's invalid address.
        assert_eq!(ng.group(&NetAddr::default()), vec![unroutable]);
        assert_eq!(ng.group(&v6("10.0.0.1")), vec![unroutable]);
        assert_eq!(ng.group(&v6("169.254.1.1")), vec![unroutable]);
        assert_eq!(ng.group(&v6("1.2.3.4")), vec![ipv4, 1, 2]);
        assert_eq!(ng.group(&v6("::FFFF:0:102:304")), vec![ipv4, 1, 2]);
        assert_eq!(ng.group(&v6("64:FF9B::102:304")), vec![ipv4, 1, 2]);
        assert_eq!(ng.group(&v6("2002:102:304:9999:9999:9999:9999:9999")), vec![ipv4, 1, 2]);
        assert_eq!(ng.group(&v6("2001:0:9999:9999:9999:9999:FEFD:FCFB")), vec![ipv4, 1, 2]);
        assert_eq!(
            ng.group(&v6("2001:470:abcd:9999:9999:9999:9999:9999")),
            vec![ipv6, 32, 1, 4, 112, 175]
        );
        assert_eq!(
            ng.group(&v6("2001:2001:9999:9999:9999:9999:9999:9999")),
            vec![ipv6, 32, 1, 32, 1]
        );
        let internal = NetAddr::internal("baz.net").unwrap();
        let mut expect = vec![Network::Internal as u8];
        expect.extend_from_slice(internal.bytes());
        assert_eq!(ng.group(&internal), expect);
    }
}
