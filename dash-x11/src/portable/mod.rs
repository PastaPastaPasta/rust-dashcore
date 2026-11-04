//! Portable ports of Core's sph functions (`src/crypto/x11/*.c`, the soft paths of `echo.cpp`
//! and `shavite.cpp`), specialised to how `HashX11` calls them: BLAKE-512 over the whole input,
//! every other function over one 64-byte digest. Every target runs these; the SIMD backends are
//! checked against them.

pub(crate) mod aes;
pub(crate) mod blake;
pub(crate) mod bmw;
pub(crate) mod cubehash;
pub(crate) mod echo;
pub(crate) mod groestl;
pub(crate) mod jh;
pub(crate) mod keccak;
pub(crate) mod luffa;
pub(crate) mod shavite;
pub(crate) mod simd;
pub(crate) mod skein;
