//! [`LongCoins`]: the bytes of coins longer than [`INLINE_MAX`], each in its own exact-size box
//! (CH2 §2.3: "long coins are boxed"), with no byte cap. Compressed P2PK, bare multisig and
//! non-standard scripts land here (≈ 0.2 % of mainnet's coins up to height 453,371).
//!
//! The slot keeps a [`LongRef`]: the box's index in a slab and the coin's length. A freed index is
//! reused first.

use super::INLINE_MAX;

/// Where a long coin lies: its slab index and length, kept in the slot's coin field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LongRef {
    index: u32,
    len: u32,
}

impl LongRef {
    pub(super) fn write(self, field: &mut [u8; INLINE_MAX]) {
        field[..4].copy_from_slice(&self.index.to_le_bytes());
        field[4..8].copy_from_slice(&self.len.to_le_bytes());
    }

    pub(super) fn read(field: &[u8; INLINE_MAX]) -> Self {
        Self {
            index: u32::from_le_bytes(field[..4].try_into().unwrap()),
            len: u32::from_le_bytes(field[4..8].try_into().unwrap()),
        }
    }

    pub(super) fn len(self) -> usize {
        self.len as usize
    }
}

/// The long coins' boxes.
#[derive(Default)]
pub(super) struct LongCoins {
    boxes: Vec<Option<Box<[u8]>>>,
    /// The indexes of freed boxes.
    free: Vec<u32>,
    /// The boxes' bytes.
    bytes: u64,
}

impl LongCoins {
    pub(super) fn put(&mut self, coin: &[u8]) -> LongRef {
        self.put_box(coin.into())
    }

    /// Keeps a box that moved in, without a copy.
    pub(super) fn put_box(&mut self, coin: Box<[u8]>) -> LongRef {
        let len = u32::try_from(coin.len()).expect("a coin under 4 GiB");
        self.bytes += u64::from(len);
        let index = match self.free.pop() {
            Some(i) => {
                self.boxes[i as usize] = Some(coin);
                i
            }
            None => {
                self.boxes.push(Some(coin));
                u32::try_from(self.boxes.len() - 1).expect("under 2^32 long coins")
            }
        };
        LongRef {
            index,
            len,
        }
    }

    pub(super) fn get(&self, r: LongRef) -> &[u8] {
        self.boxes[r.index as usize].as_deref().expect("a live long coin")
    }

    pub(super) fn free(&mut self, r: LongRef) {
        drop(self.take(r));
    }

    /// Moves a long coin's box out, freeing its place.
    pub(super) fn take(&mut self, r: LongRef) -> Box<[u8]> {
        let coin = self.boxes[r.index as usize].take().expect("a live long coin");
        self.bytes -= coin.len() as u64;
        self.free.push(r.index);
        coin
    }

    /// The boxes and both vectors' capacity.
    pub(super) fn allocated(&self) -> u64 {
        self.bytes
            + (self.boxes.capacity() * size_of::<Option<Box<[u8]>>>()
                + self.free.capacity() * size_of::<u32>()) as u64
    }
}
