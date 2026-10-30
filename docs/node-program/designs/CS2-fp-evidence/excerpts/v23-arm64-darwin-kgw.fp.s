; function 0x10041783c-0x100418274 plt=['assert_rtn', 'pow', 'stack_chk_fail'] sym=__Z19GetNextWorkRequiredPK11CBlockIndexPK12CBlockHeaderRKN9Consensus6ParamsE
      ...
1004179b8:  stp      q0, q0, [sp, #0x80]
1004179bc:  cbz      w8, #0x100417fa0
1004179c0:  ldp      x11, x9, [x19, #0x1e8]
1004179c4:  scvtf    d0, x9
1004179c8:  mov      x10, #-0x6666666666666667
1004179cc:  movk     x10, #0x999a
1004179d0:  movk     x10, #0x3f99, lsl #48
1004179d4:  fmov     d1, x10
1004179d8:  fmul     d0, d0, d1
1004179dc:  fcvtzu   x10, d0
1004179e0:  udiv     x10, x10, x11
1004179e4:  stp      x10, x11, [sp, #0x30]
1004179e8:  cmp      x10, x8
      ...
100417e9c:  cmp      x8, #1
100417ea0:  b.lt     #0x100417eb4
100417ea4:  cbz      x21, #0x100417eb4
100417ea8:  scvtf    d0, x21
100417eac:  ucvtf    d1, x23
100417eb0:  fdiv     d9, d0, d1
100417eb4:  ldr      x8, [sp, #0x30]
100417eb8:  cmp      x20, x8
100417ebc:  b.lo     #0x100417f44
100417ec0:  ucvtf    d0, x20
100417ec4:  mov      x8, #0x3333333333333333
100417ec8:  movk     x8, #0x403c, lsl #48
100417ecc:  fmov     d1, x8
100417ed0:  fdiv     d0, d0, d1
100417ed4:  mov      x8, #0xced9
100417ed8:  movk     x8, #0x53f7, lsl #16
100417edc:  movk     x8, #0xa5e3, lsl #32
      ...
100417ef4:  str      x21, [sp, #0x20]
100417ef8:  mov      x21, x0
100417efc:  mov      x19, x13
100417f00:  bl       #0x100c8157c    <pow@plt>
100417f04:  mov      x13, x19
100417f08:  mov      x0, x21
100417f0c:  ldr      x21, [sp, #0x20]
      ...
100417f24:  movk     x8, #0xab36, lsl #32
100417f28:  movk     x8, #0x3fe6, lsl #48
100417f2c:  fmov     d1, x8
100417f30:  fmadd    d0, d0, d1, d8
100417f34:  fdiv     d1, d8, d0
100417f38:  fcmp     d9, d1
100417f3c:  fccmp    d9, d0, #0, hi
100417f40:  b.ge     #0x1004181a4
100417f44:  ldr      x28, [x28, #8]
100417f48:  cbz      x28, #0x1004181a4
