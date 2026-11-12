//! JH-512 (Core: `src/crypto/x11/jh.c`), specialised to one 64-byte message: two
//! compressions, of the message and of the padding block `80 00 .. 00 || BE128(512)`
//! (`jh_close` with `ptr == 0` pads 64 bytes). E8 is sph's 64-bit round `SLu` with the round
//! order regrouped (see [`e8`]); Core builds the `SPH_SMALL_FOOTPRINT_JH` loop, which computes
//! the same function.
//!
//! The state is sph's `H.wide`: 16 words `h0h, h0l, .., h7h, h7l`, decoded little-endian as sph
//! does on little-endian CPUs (`C64e` byte-swaps the constants, `dec64le`/`enc64le` the data).

/// sph's `C`, the 42 round constants (`hi`, `lo` for even words, then for odd words), as
/// written in `jh.c` before `C64e`.
#[rustfmt::skip]
pub(crate) const C: [u64; 168] = [
    0x72d5dea2df15f867, 0x7b84150ab7231557, 0x81abd6904d5a87f6, 0x4e9f4fc5c3d12b40,
    0xea983ae05c45fa9c, 0x03c5d29966b2999a, 0x660296b4f2bb538a, 0xb556141a88dba231,
    0x03a35a5c9a190edb, 0x403fb20a87c14410, 0x1c051980849e951d, 0x6f33ebad5ee7cddc,
    0x10ba139202bf6b41, 0xdc786515f7bb27d0, 0x0a2c813937aa7850, 0x3f1abfd2410091d3,
    0x422d5a0df6cc7e90, 0xdd629f9c92c097ce, 0x185ca70bc72b44ac, 0xd1df65d663c6fc23,
    0x976e6c039ee0b81a, 0x2105457e446ceca8, 0xeef103bb5d8e61fa, 0xfd9697b294838197,
    0x4a8e8537db03302f, 0x2a678d2dfb9f6a95, 0x8afe7381f8b8696c, 0x8ac77246c07f4214,
    0xc5f4158fbdc75ec4, 0x75446fa78f11bb80, 0x52de75b7aee488bc, 0x82b8001e98a6a3f4,
    0x8ef48f33a9a36315, 0xaa5f5624d5b7f989, 0xb6f1ed207c5ae0fd, 0x36cae95a06422c36,
    0xce2935434efe983d, 0x533af974739a4ba7, 0xd0f51f596f4e8186, 0x0e9dad81afd85a9f,
    0xa7050667ee34626a, 0x8b0b28be6eb91727, 0x47740726c680103f, 0xe0a07e6fc67e487b,
    0x0d550aa54af8a4c0, 0x91e3e79f978ef19e, 0x8676728150608dd4, 0x7e9e5a41f3e5b062,
    0xfc9f1fec4054207a, 0xe3e41a00cef4c984, 0x4fd794f59dfa95d8, 0x552e7e1124c354a5,
    0x5bdf7228bdfe6e28, 0x78f57fe20fa5c4b2, 0x05897cefee49d32e, 0x447e9385eb28597f,
    0x705f6937b324314a, 0x5e8628f11dd6e465, 0xc71b770451b920e7, 0x74fe43e823d4878a,
    0x7d29e8a3927694f2, 0xddcb7a099b30d9c1, 0x1d1b30fb5bdc1be0, 0xda24494ff29c82bf,
    0xa4e7ba31b470bfff, 0x0d324405def8bc48, 0x3baefc3253bbd339, 0x459fc3c1e0298ba0,
    0xe5c905fdf7ae090f, 0x947034124290f134, 0xa271b701e344ed95, 0xe93b8e364f2f984a,
    0x88401d63a06cf615, 0x47c1444b8752afff, 0x7ebb4af1e20ac630, 0x4670b6c5cc6e8ce6,
    0xa4d5a456bd4fca00, 0xda9d844bc83e18ae, 0x7357ce453064d1ad, 0xe8a6ce68145c2567,
    0xa3da8cf2cb0ee116, 0x33e906589a94999a, 0x1f60b220c26f847b, 0xd1ceac7fa0d18518,
    0x32595ba18ddd19d3, 0x509a1cc0aaa5b446, 0x9f3d6367e4046bba, 0xf6ca19ab0b56ee7e,
    0x1fb179eaa9282174, 0xe9bdf7353b3651ee, 0x1d57ac5a7550d376, 0x3a46c2fea37d7001,
    0xf735c1af98a4d842, 0x78edec209e6b6779, 0x41836315ea3adba8, 0xfac33b4d32832c83,
    0xa7403b1f1c2747f3, 0x5940f034b72d769a, 0xe73e4e6cd2214ffd, 0xb8fd8d39dc5759ef,
    0x8d9b0c492b49ebda, 0x5ba2d74968f3700d, 0x7d3baed07a8d5584, 0xf5a5e9f0e4f88e65,
    0xa0b8a2f436103b53, 0x0ca8079e753eec5a, 0x9168949256e8884f, 0x5bb05c55f8babc4c,
    0xe3bb3b99f387947b, 0x75daf4d6726b1c5d, 0x64aeac28dc34b36d, 0x6c34a550b828db71,
    0xf861e2f2108d512a, 0xe3db643359dd75fc, 0x1cacbcf143ce3fa2, 0x67bbd13c02e843b0,
    0x330a5bca8829a175, 0x7f34194db416535c, 0x923b94c30e794d1e, 0x797475d7b6eeaf3f,
    0xeaa8d4f7be1a3921, 0x5cf47e094c232751, 0x26a32453ba323cd2, 0x44a3174a6da6d5ad,
    0xb51d3ea6aff2c908, 0x83593d98916b3c56, 0x4cf87ca17286604d, 0x46e23ecc086ec7f6,
    0x2f9833b3b1bc765e, 0x2bd666a5efc4e62a, 0x06f4b6e8bec1d436, 0x74ee8215bcef2163,
    0xfdc14e0df453c969, 0xa77d5ac406585826, 0x7ec1141606e0fa16, 0x7e90af3d28639d3f,
    0xd2c9f2e3009bd20c, 0x5faace30b7d40c30, 0x742a5116f2e03298, 0x0deb30d8e3cef89a,
    0x4bc59e7bb5f17992, 0xff51e66e048668d3, 0x9b234d57e6966731, 0xcce6a6f3170a7505,
    0xb17681d913326cce, 0x3c175284f805a262, 0xf42bcbb378471547, 0xff46548223936a48,
    0x38df58074e5e6565, 0xf2fc7c89fc86508e, 0x31702e44d00bca86, 0xf04009a23078474e,
    0x65a0ee39d1f73883, 0xf75ee937e42c3abd, 0x2197b2260113f86f, 0xa344edd1ef9fdee7,
    0x8ba0df15762592d9, 0x3c85f7f612dc42be, 0xd8a7ec7cab27b07e, 0x538d7ddaaa3ea8de,
    0xaa25ce93bd0269d8, 0x5af643fd1a7308f9, 0xc05fefda174a19a5, 0x974d66334cfd216a,
    0x35b49831db411570, 0xea1e0fbbedcd549b, 0x9ad063a151974072, 0xf6759dbf91476fe2,
];

/// sph's `IV512`, as written in `jh.c` before `C64e`.
#[rustfmt::skip]
pub(crate) const IV512: [u64; 16] = [
    0x6fd14b963e00aa17, 0x636a2e057a15d543, 0x8a225e8d0c97ef0b, 0xe9341259f2b3c361,
    0x891da0c1536f801e, 0x2aa9056bea2b6d80, 0x588eccdb2075baa6, 0xa90f3a76baf83bf7,
    0x0169e60541e34a69, 0x46b58a8e2e6fe65a, 0x1047a7d0c1843c24, 0x3b6e71b12d5ac199,
    0xcf57f6ec9db1f856, 0xa706887c5716b156, 0xe3c2fcdfe68517fb, 0x545a4678cc8cdd4b,
];

/// `C` after `C64e`.
static C_LE: [u64; 168] = {
    let mut c = C;
    let mut i = 0;
    while i < c.len() {
        c[i] = c[i].swap_bytes();
        i += 1;
    }
    c
};

/// The padding block: `0x80`, zeros, the bit length 512 as a big-endian 128-bit number.
pub(crate) const PAD: [u8; 64] = {
    let mut b = [0; 64];
    b[0] = 0x80;
    b[62] = 0x02;
    b
};

/// sph's `Sb` S-box layer on one bit-slice of four words.
#[inline(always)]
fn sb(mut x0: u64, mut x1: u64, mut x2: u64, mut x3: u64, c: u64) -> [u64; 4] {
    x3 = !x3;
    x0 ^= c & !x2;
    let tmp = c ^ (x0 & x1);
    x0 ^= x2 & x3;
    x3 ^= !x1 & x2;
    x1 ^= x0 & x2;
    x2 ^= x0 & !x3;
    x0 ^= x1 | x3;
    x3 ^= x1 & x2;
    x1 ^= tmp & x0;
    x2 ^= tmp;
    [x0, x1, x2, x3]
}

/// sph's `Wz`: swaps adjacent `n`-bit groups selected by `c`.
#[inline(always)]
fn wz(x: u64, c: u64, n: u32) -> u64 {
    ((x >> n) & c) | ((x & c) << n)
}

/// One round (sph `SLu(r, RO)`, `RO = r % 7`) on one 64-bit half of the state (`l` = 0 for
/// the `h` words, 1 for the `l` words): `S` on even and odd words, `L`, then `W_RO` on the odd
/// words. `W5` (`Wz` by 32 bits) is a rotation; `W6` swaps halves and is done by the caller.
#[inline(always)]
fn round<const RO: u32>(h: &mut [u64; 16], r: usize, l: usize) {
    let [e0, e1, e2, e3] = sb(h[l], h[4 + l], h[8 + l], h[12 + l], C_LE[4 * r + l]);
    let [mut o0, mut o1, mut o2, mut o3] =
        sb(h[2 + l], h[6 + l], h[10 + l], h[14 + l], C_LE[4 * r + 2 + l]);
    // Lb(e0, e1, e2, e3, o0, o1, o2, o3).
    o0 ^= e1;
    o1 ^= e2;
    o2 ^= e3 ^ e0;
    o3 ^= e0;
    h[l] = e0 ^ o1;
    h[4 + l] = e1 ^ o2;
    h[8 + l] = e2 ^ o3 ^ o0;
    h[12 + l] = e3 ^ o0;
    let w = |x| match RO {
        0 => wz(x, 0x5555_5555_5555_5555, 1),
        1 => wz(x, 0x3333_3333_3333_3333, 2),
        2 => wz(x, 0x0F0F_0F0F_0F0F_0F0F, 4),
        3 => wz(x, 0x00FF_00FF_00FF_00FF, 8),
        4 => wz(x, 0x0000_FFFF_0000_FFFF, 16),
        5 => x.rotate_left(32),
        _ => x,
    };
    h[2 + l] = w(o0);
    h[6 + l] = w(o1);
    h[10 + l] = w(o2);
    h[14 + l] = w(o3);
}

/// sph's `E8`: 42 rounds. `W6`, every seventh round, swaps the halves of the odd words; between
/// those swaps the two halves never mix, so each runs its seven rounds alone (in registers).
#[inline(always)]
fn e8(h: &mut [u64; 16]) {
    for r in (0..42).step_by(7) {
        for l in 0..2 {
            round::<0>(h, r, l);
            round::<1>(h, r + 1, l);
            round::<2>(h, r + 2, l);
            round::<3>(h, r + 3, l);
            round::<4>(h, r + 4, l);
            round::<5>(h, r + 5, l);
            round::<6>(h, r + 6, l);
        }
        // W6 on the odd words.
        h.swap(2, 3);
        h.swap(6, 7);
        h.swap(10, 11);
        h.swap(14, 15);
    }
}

/// One block (sph `INPUT_BUF1`, `E8`, `INPUT_BUF2`).
#[inline(always)]
fn compress(h: &mut [u64; 16], block: &[u8; 64]) {
    let mut m = [0u64; 8];
    for (w, b) in m.iter_mut().zip(block.as_chunks::<8>().0) {
        *w = u64::from_le_bytes(*b);
    }
    for i in 0..8 {
        h[i] ^= m[i];
    }
    e8(h);
    for i in 0..8 {
        h[8 + i] ^= m[i];
    }
}

/// `sph_jh512_init`, `sph_jh512(input, 64)`, `sph_jh512_close`.
pub(crate) fn hash(input: &[u8; 64]) -> [u8; 64] {
    let mut h = IV512.map(u64::swap_bytes);
    compress(&mut h, input);
    compress(&mut h, &PAD);
    let mut out = [0; 64];
    for (o, w) in out.as_chunks_mut::<8>().0.iter_mut().zip(&h[8..]) {
        *o = w.to_le_bytes();
    }
    out
}
