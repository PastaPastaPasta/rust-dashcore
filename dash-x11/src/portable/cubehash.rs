//! CubeHash-16/32-512 (Core: `src/crypto/x11/cubehash.c`), specialised to one 64-byte message:
//! two 32-byte blocks, the padding block `80 00 .. 00`, then sph's finalisation (`xv ^= 1`
//! after the padding block's 16 rounds, then 10 × 16 more rounds). Core builds
//! `SPH_SMALL_FOOTPRINT_CUBEHASH`, which computes the same function.
//!
//! The state is sph's `state[32]` in the specification's `x[00000] .. x[11111]` order, held as
//! eight 4-word rows: `a[0..4]` is `x[0..16]`, `b[0..4]` is `x[16..32]`. Each of the round's
//! swaps then exchanges whole rows (`j`, `k` bits) or words within rows (`l`, `m` bits). sph's
//! `ROUND_EVEN`/`ROUND_ODD` are this round with the swaps folded into variable renaming, which
//! is the identity after every even number of rounds.

/// sph's `IV512`, in `state` order.
#[rustfmt::skip]
pub(crate) const IV512: [u32; 32] = [
    0x2AEA2A61, 0x50F494D4, 0x2D538B8B, 0x4167D83E,
    0x3FEE2313, 0xC701CF8C, 0xCC39968E, 0x50AC5695,
    0x4D42C787, 0xA647A8B3, 0x97CF0BEF, 0x825B4537,
    0xEEF864D2, 0xF22090C4, 0xD0E5CD33, 0xA23911AE,
    0xFCD398D9, 0x148FE485, 0x1B017BEF, 0xB6444532,
    0x6A536159, 0x2FF5781C, 0x91FA7934, 0x0DBADEA9,
    0xD65C8A2B, 0xA5A70E75, 0xB1C62456, 0xBC796576,
    0x1921C8F7, 0xE7989AF1, 0x7795D246, 0xD43E3B44,
];

type Row = [u32; 4];

#[inline(always)]
fn add(x: Row, y: Row) -> Row {
    [
        x[0].wrapping_add(y[0]),
        x[1].wrapping_add(y[1]),
        x[2].wrapping_add(y[2]),
        x[3].wrapping_add(y[3]),
    ]
}

#[inline(always)]
fn xor(x: Row, y: Row) -> Row {
    [x[0] ^ y[0], x[1] ^ y[1], x[2] ^ y[2], x[3] ^ y[3]]
}

#[inline(always)]
fn rotl<const N: u32>(x: Row) -> Row {
    [x[0].rotate_left(N), x[1].rotate_left(N), x[2].rotate_left(N), x[3].rotate_left(N)]
}

/// The state: `x[0..16]` and `x[16..32]` as four rows each.
struct State {
    a: [Row; 4],
    b: [Row; 4],
}

impl State {
    /// One round of the CubeHash specification.
    #[inline(always)]
    fn round(&mut self) {
        let (a, b) = (&mut self.a, &mut self.b);
        for i in 0..4 {
            b[i] = add(b[i], a[i]);
            a[i] = rotl::<7>(a[i]);
        }
        // Swap x[00klm] with x[01klm], then xor.
        a.swap(0, 2);
        a.swap(1, 3);
        for i in 0..4 {
            a[i] = xor(a[i], b[i]);
            // Swap x[1jk0m] with x[1jk1m].
            b[i] = [b[i][2], b[i][3], b[i][0], b[i][1]];
            b[i] = add(b[i], a[i]);
            a[i] = rotl::<11>(a[i]);
        }
        // Swap x[0j0lm] with x[0j1lm], then xor.
        a.swap(0, 1);
        a.swap(2, 3);
        for i in 0..4 {
            a[i] = xor(a[i], b[i]);
            // Swap x[1jkl0] with x[1jkl1].
            b[i] = [b[i][1], b[i][0], b[i][3], b[i][2]];
        }
    }

    /// sph's `SIXTEEN_ROUNDS`, as two runs of eight unrolled rounds. The swaps are
    /// involutions, so an even unroll leaves them as register renaming; this shape measured
    /// fewest instructions.
    #[inline(always)]
    fn sixteen_rounds(&mut self) {
        for _ in 0..2 {
            for _ in 0..4 {
                self.round();
                self.round();
            }
        }
    }

    /// sph's `INPUT_BLOCK` (32 bytes, `dec32le`) and the block's 16 rounds.
    #[inline(always)]
    fn block(&mut self, data: &[u8]) {
        for i in 0..2 {
            for j in 0..4 {
                let o = 16 * i + 4 * j;
                let w = u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
                self.a[i][j] ^= w;
            }
        }
        self.sixteen_rounds();
    }
}

/// CubeHash-512 of a 64-byte message, as `sph_cubehash512_init`, `sph_cubehash512(.., 64)`,
/// `sph_cubehash512_close` compute it.
pub(crate) fn hash(input: &[u8; 64]) -> [u8; 64] {
    let mut s = State {
        a: [[0; 4]; 4],
        b: [[0; 4]; 4],
    };
    for r in 0..4 {
        for j in 0..4 {
            s.a[r][j] = IV512[4 * r + j];
            s.b[r][j] = IV512[16 + 4 * r + j];
        }
    }
    s.block(&input[..32]);
    s.block(&input[32..]);
    // `cubehash_close` with `ptr == 0`: the block `80 00 .. 00`, then 10 more times 16 rounds.
    s.a[0][0] ^= 0x80;
    s.sixteen_rounds();
    s.b[3][3] ^= 1;
    for _ in 0..10 {
        s.sixteen_rounds();
    }
    // The first 16 words, `enc32le`.
    let mut out = [0; 64];
    for (i, row) in s.a.iter().enumerate() {
        for (j, w) in row.iter().enumerate() {
            out[16 * i + 4 * j..][..4].copy_from_slice(&w.to_le_bytes());
        }
    }
    out
}
