//! Grøstl-512 (Core: `src/crypto/x11/groestl.c`, 64-bit table form), specialised to one 64-byte
//! message: `groestl_big_close` pads it to one 128-byte block (`80 00 .. 00 || BE64(1)`), so
//! the hash is one compression `P(H ^ m) ^ Q(m) ^ H` and the output transform `P(H) ^ H`.
//!
//! Words are little-endian as sph keeps them on little-endian CPUs (`C64e`, `dec64le`,
//! `enc64le`). The state is kept as bytes so each table index is one byte load; the `Q`
//! state is kept complemented, which turns its round constant into one byte like `P`'s.

/// sph's `T0`, as written in `groestl.c` before `C64e`. sph's `T1`..`T7` are `T0` rotated left
/// by 8..56 bits after `C64e` (checked when this table was extracted), which is how [`T`]
/// builds them.
#[rustfmt::skip]
const T0: [u64; 256] = [
    0xc632f4a5f497a5c6, 0xf86f978497eb84f8, 0xee5eb099b0c799ee, 0xf67a8c8d8cf78df6,
    0xffe8170d17e50dff, 0xd60adcbddcb7bdd6, 0xde16c8b1c8a7b1de, 0x916dfc54fc395491,
    0x6090f050f0c05060, 0x0207050305040302, 0xce2ee0a9e087a9ce, 0x56d1877d87ac7d56,
    0xe7cc2b192bd519e7, 0xb513a662a67162b5, 0x4d7c31e6319ae64d, 0xec59b59ab5c39aec,
    0x8f40cf45cf05458f, 0x1fa3bc9dbc3e9d1f, 0x8949c040c0094089, 0xfa68928792ef87fa,
    0xefd03f153fc515ef, 0xb29426eb267febb2, 0x8ece40c94007c98e, 0xfbe61d0b1ded0bfb,
    0x416e2fec2f82ec41, 0xb31aa967a97d67b3, 0x5f431cfd1cbefd5f, 0x456025ea258aea45,
    0x23f9dabfda46bf23, 0x535102f702a6f753, 0xe445a196a1d396e4, 0x9b76ed5bed2d5b9b,
    0x75285dc25deac275, 0xe1c5241c24d91ce1, 0x3dd4e9aee97aae3d, 0x4cf2be6abe986a4c,
    0x6c82ee5aeed85a6c, 0x7ebdc341c3fc417e, 0xf5f3060206f102f5, 0x8352d14fd11d4f83,
    0x688ce45ce4d05c68, 0x515607f407a2f451, 0xd18d5c345cb934d1, 0xf9e1180818e908f9,
    0xe24cae93aedf93e2, 0xab3e9573954d73ab, 0x6297f553f5c45362, 0x2a6b413f41543f2a,
    0x081c140c14100c08, 0x9563f652f6315295, 0x46e9af65af8c6546, 0x9d7fe25ee2215e9d,
    0x3048782878602830, 0x37cff8a1f86ea137, 0x0a1b110f11140f0a, 0x2febc4b5c45eb52f,
    0x0e151b091b1c090e, 0x247e5a365a483624, 0x1badb69bb6369b1b, 0xdf98473d47a53ddf,
    0xcda76a266a8126cd, 0x4ef5bb69bb9c694e, 0x7f334ccd4cfecd7f, 0xea50ba9fbacf9fea,
    0x123f2d1b2d241b12, 0x1da4b99eb93a9e1d, 0x58c49c749cb07458, 0x3446722e72682e34,
    0x3641772d776c2d36, 0xdc11cdb2cda3b2dc, 0xb49d29ee2973eeb4, 0x5b4d16fb16b6fb5b,
    0xa4a501f60153f6a4, 0x76a1d74dd7ec4d76, 0xb714a361a37561b7, 0x7d3449ce49face7d,
    0x52df8d7b8da47b52, 0xdd9f423e42a13edd, 0x5ecd937193bc715e, 0x13b1a297a2269713,
    0xa6a204f50457f5a6, 0xb901b868b86968b9, 0x0000000000000000, 0xc1b5742c74992cc1,
    0x40e0a060a0806040, 0xe3c2211f21dd1fe3, 0x793a43c843f2c879, 0xb69a2ced2c77edb6,
    0xd40dd9bed9b3bed4, 0x8d47ca46ca01468d, 0x671770d970ced967, 0x72afdd4bdde44b72,
    0x94ed79de7933de94, 0x98ff67d4672bd498, 0xb09323e8237be8b0, 0x855bde4ade114a85,
    0xbb06bd6bbd6d6bbb, 0xc5bb7e2a7e912ac5, 0x4f7b34e5349ee54f, 0xedd73a163ac116ed,
    0x86d254c55417c586, 0x9af862d7622fd79a, 0x6699ff55ffcc5566, 0x11b6a794a7229411,
    0x8ac04acf4a0fcf8a, 0xe9d9301030c910e9, 0x040e0a060a080604, 0xfe66988198e781fe,
    0xa0ab0bf00b5bf0a0, 0x78b4cc44ccf04478, 0x25f0d5bad54aba25, 0x4b753ee33e96e34b,
    0xa2ac0ef30e5ff3a2, 0x5d4419fe19bafe5d, 0x80db5bc05b1bc080, 0x0580858a850a8a05,
    0x3fd3ecadec7ead3f, 0x21fedfbcdf42bc21, 0x70a8d848d8e04870, 0xf1fd0c040cf904f1,
    0x63197adf7ac6df63, 0x772f58c158eec177, 0xaf309f759f4575af, 0x42e7a563a5846342,
    0x2070503050403020, 0xe5cb2e1a2ed11ae5, 0xfdef120e12e10efd, 0xbf08b76db7656dbf,
    0x8155d44cd4194c81, 0x18243c143c301418, 0x26795f355f4c3526, 0xc3b2712f719d2fc3,
    0xbe8638e13867e1be, 0x35c8fda2fd6aa235, 0x88c74fcc4f0bcc88, 0x2e654b394b5c392e,
    0x936af957f93d5793, 0x55580df20daaf255, 0xfc619d829de382fc, 0x7ab3c947c9f4477a,
    0xc827efacef8bacc8, 0xba8832e7326fe7ba, 0x324f7d2b7d642b32, 0xe642a495a4d795e6,
    0xc03bfba0fb9ba0c0, 0x19aab398b3329819, 0x9ef668d16827d19e, 0xa322817f815d7fa3,
    0x44eeaa66aa886644, 0x54d6827e82a87e54, 0x3bdde6abe676ab3b, 0x0b959e839e16830b,
    0x8cc945ca4503ca8c, 0xc7bc7b297b9529c7, 0x6b056ed36ed6d36b, 0x286c443c44503c28,
    0xa72c8b798b5579a7, 0xbc813de23d63e2bc, 0x1631271d272c1d16, 0xad379a769a4176ad,
    0xdb964d3b4dad3bdb, 0x649efa56fac85664, 0x74a6d24ed2e84e74, 0x1436221e22281e14,
    0x92e476db763fdb92, 0x0c121e0a1e180a0c, 0x48fcb46cb4906c48, 0xb88f37e4376be4b8,
    0x9f78e75de7255d9f, 0xbd0fb26eb2616ebd, 0x43692aef2a86ef43, 0xc435f1a6f193a6c4,
    0x39dae3a8e372a839, 0x31c6f7a4f762a431, 0xd38a593759bd37d3, 0xf274868b86ff8bf2,
    0xd583563256b132d5, 0x8b4ec543c50d438b, 0x6e85eb59ebdc596e, 0xda18c2b7c2afb7da,
    0x018e8f8c8f028c01, 0xb11dac64ac7964b1, 0x9cf16dd26d23d29c, 0x49723be03b92e049,
    0xd81fc7b4c7abb4d8, 0xacb915fa1543faac, 0xf3fa090709fd07f3, 0xcfa06f256f8525cf,
    0xca20eaafea8fafca, 0xf47d898e89f38ef4, 0x476720e9208ee947, 0x1038281828201810,
    0x6f0b64d564ded56f, 0xf073838883fb88f0, 0x4afbb16fb1946f4a, 0x5cca967296b8725c,
    0x38546c246c702438, 0x575f08f108aef157, 0x732152c752e6c773, 0x9764f351f3355197,
    0xcbae6523658d23cb, 0xa125847c84597ca1, 0xe857bf9cbfcb9ce8, 0x3e5d6321637c213e,
    0x96ea7cdd7c37dd96, 0x611e7fdc7fc2dc61, 0x0d9c9186911a860d, 0x0f9b9485941e850f,
    0xe04bab90abdb90e0, 0x7cbac642c6f8427c, 0x712657c457e2c471, 0xcc29e5aae583aacc,
    0x90e373d8733bd890, 0x06090f050f0c0506, 0xf7f4030103f501f7, 0x1c2a36123638121c,
    0xc23cfea3fe9fa3c2, 0x6a8be15fe1d45f6a, 0xaebe10f91047f9ae, 0x69026bd06bd2d069,
    0x17bfa891a82e9117, 0x9971e858e8295899, 0x3a5369276974273a, 0x27f7d0b9d04eb927,
    0xd991483848a938d9, 0xebde351335cd13eb, 0x2be5ceb3ce56b32b, 0x2277553355443322,
    0xd204d6bbd6bfbbd2, 0xa9399070904970a9, 0x07878089800e8907, 0x33c1f2a7f266a733,
    0x2decc1b6c15ab62d, 0x3c5a66226678223c, 0x15b8ad92ad2a9215, 0xc9a96020608920c9,
    0x875cdb49db154987, 0xaab01aff1a4fffaa, 0x50d8887888a07850, 0xa52b8e7a8e517aa5,
    0x03898a8f8a068f03, 0x594a13f813b2f859, 0x09929b809b128009, 0x1a2339173934171a,
    0x651075da75cada65, 0xd784533153b531d7, 0x84d551c65113c684, 0xd003d3b8d3bbb8d0,
    0x82dc5ec35e1fc382, 0x29e2cbb0cb52b029, 0x5ac3997799b4775a, 0x1e2d3311333c111e,
    0x7b3d46cb46f6cb7b, 0xa8b71ffc1f4bfca8, 0x6d0c61d661dad66d, 0x2c624e3a4e583a2c,
];

/// sph's `T0`..`T7` after `C64e`: `T[k][x]` is `T0[x]` rotated left by `8 * k` bits.
static T: [[u64; 256]; 8] = {
    let mut t = [[0; 256]; 8];
    let mut x = 0;
    while x < 256 {
        let mut k = 0;
        while k < 8 {
            t[k][x] = T0[x].swap_bytes().rotate_left(8 * k as u32);
            k += 1;
        }
        x += 1;
    }
    t
};

/// The 16 little-endian 64-bit words of a permutation state, as bytes, each stored twice (at
/// `j` and `j + 16`) so a round reads word `(d + k) mod 16` at `d + k`: every table index is
/// then one byte load at a fixed offset.
type State = [[u8; 8]; 32];

/// `H` after `groestl_big_init(512)`: zero but for word 15, the output size in bits.
const H_INIT: [u64; 16] = {
    let mut h = [0; 16];
    h[15] = 512u64.swap_bytes();
    h
};

/// Stores word `d` of a round's output.
#[inline(always)]
fn put(t: &mut State, d: usize, x: u64) {
    t[d] = x.to_le_bytes();
    t[d + 16] = t[d];
}

/// The word `j` of a state.
#[inline(always)]
fn get(a: &State, j: usize) -> u64 {
    u64::from_le_bytes(a[j])
}

/// One round of `P` (sph `ROUND_BIG_P`) from `a` into `t`, of which only words `FROM..16` are
/// computed. `PC64(j << 4, r)` XORs byte 0 of `a[j]` with `j << 4 | r`.
#[inline(always)]
fn round_p<const FROM: usize>(a: &State, t: &mut State, r: u8) {
    for d4 in (FROM..16).step_by(4) {
        for d in d4..d4 + 4 {
            let x = T[0][(a[d][0] ^ ((d as u8) << 4 | r)) as usize]
                ^ T[1][a[d + 1][1] as usize]
                ^ T[2][a[d + 2][2] as usize]
                ^ T[3][a[d + 3][3] as usize]
                ^ T[4][a[d + 4][4] as usize]
                ^ T[5][a[d + 5][5] as usize]
                ^ T[6][a[d + 6][6] as usize]
                ^ T[7][a[d + 11][7] as usize];
            put(t, d, x);
        }
    }
}

/// One round of `Q` (sph `ROUND_BIG_Q`) from the complemented state `!a` into `!t`.
/// `QC64(j << 4, r)` is `!((j << 4 | r) << 56)`, so `a[j] ^ QC64` is `!a[j]` with byte 7
/// XORed with `j << 4 | r`.
#[inline(always)]
fn round_q(a: &State, t: &mut State, r: u8) {
    for d4 in (0..16).step_by(4) {
        for d in d4..d4 + 4 {
            let x = T[0][a[d + 1][0] as usize]
                ^ T[1][a[d + 3][1] as usize]
                ^ T[2][a[d + 5][2] as usize]
                ^ T[3][a[d + 11][3] as usize]
                ^ T[4][a[d][4] as usize]
                ^ T[5][a[d + 2][5] as usize]
                ^ T[6][a[d + 4][6] as usize]
                ^ T[7][(a[d + 6][7] ^ (((d + 6) << 4) as u8 | r)) as usize];
            put(t, d, !x);
        }
    }
}

/// sph's `PERM_BIG_P` on `a`, of which only words `FROM..16` are computed.
#[inline(always)]
fn perm_p<const FROM: usize>(a: &mut State) {
    let mut t = [[0; 8]; 32];
    for r in (0..12).step_by(2) {
        round_p::<0>(a, &mut t, r);
        round_p::<0>(&t, a, r + 1);
    }
    round_p::<0>(a, &mut t, 12);
    round_p::<FROM>(&t, a, 13);
}

/// sph's `PERM_BIG_Q` on the complemented state.
#[inline(always)]
fn perm_q(a: &mut State) {
    let mut t = [[0; 8]; 32];
    for r in (0..14).step_by(2) {
        round_q(a, &mut t, r);
        round_q(&t, a, r + 1);
    }
}

/// `sph_groestl512_init`, `sph_groestl512(input, 64)`, `sph_groestl512_close`.
pub(crate) fn hash(input: &[u8; 64]) -> [u8; 64] {
    // The padded block m: the message, 0x80, zeros, the block count 1 big-endian.
    let mut m = [0u64; 16];
    for (w, b) in m.iter_mut().zip(input.as_chunks::<8>().0) {
        *w = u64::from_le_bytes(*b);
    }
    m[8] = 0x80;
    m[15] = 1 << 56;
    // COMPRESS_BIG: g = m ^ H; H ^= P(g) ^ Q(m).
    let mut g = [[0; 8]; 32];
    let mut q = [[0; 8]; 32];
    for j in 0..16 {
        put(&mut g, j, m[j] ^ H_INIT[j]);
        put(&mut q, j, !m[j]);
    }
    perm_p::<0>(&mut g);
    perm_q(&mut q);
    let mut h = [[0; 8]; 32];
    for (j, init) in H_INIT.into_iter().enumerate() {
        put(&mut h, j, init ^ get(&g, j) ^ !get(&q, j));
    }
    // FINAL_BIG: H ^= P(H); the output is words 8..16.
    let mut x = h;
    perm_p::<8>(&mut x);
    let mut out = [0; 64];
    for (j, o) in out.as_chunks_mut::<8>().0.iter_mut().enumerate() {
        *o = (get(&h, 8 + j) ^ get(&x, 8 + j)).to_le_bytes();
    }
    out
}
