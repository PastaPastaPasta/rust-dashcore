; function 0x5363c6-0x5371d4 plt=['__assert_fail', '__stack_chk_fail', 'memcmp', 'memcpy']
      ...
  536590:  jalr     ra, ra, -0x36c    <fn 0x304220>
  536594:  lw       a6, 0x48(s2)
  536598:  auipc    a5, 0xa71
  53659c:  fld      fa4, 0x1c0(a5)    ; -> 0xfa7758 [.rodata] f64 65535.0 (0x40efffe000000000) hi64 0x4070000000000000
  5365a0:  c.li     a4, 0x1c
  5365a2:  slli     a5, a6, 0x28
  5365a6:  c.srli   a5, 0x28
  5365a8:  fcvt.d.w fa5, a5
  5365ac:  srliw    a6, a6, 0x18
  5365b0:  sext.w   a5, a0
  5365b4:  fdiv.d   fa5, fa4, fa5
  5365b8:  bltu     a4, a6, 0x7a6
  5365bc:  auipc    a4, 0xa71
  5365c0:  fld      fa3, 0x1a4(a4)    ; -> 0xfa7760 [.rodata] f64 256.0 (0x4070000000000000) hi64 0x3f70000000000000
  5365c4:  c.li     a4, 0x1d
  5365c6:  c.addiw  a6, 1
  5365c8:  fmul.d   fa5, fa5, fa3
  5365cc:  bne      a6, a4, -6
  5365d0:  slli     a4, a5, 0x28
  5365d4:  c.srli   a4, 0x28
  5365d6:  fcvt.d.w fa3, a4
  5365da:  fsd      fa5, -0x528(s0)
  5365de:  srliw    a5, a5, 0x18
  5365e2:  c.li     a4, 0x1c
  5365e4:  fdiv.d   fa4, fa4, fa3
  5365e8:  bltu     a4, a5, 0x75a
  5365ec:  auipc    a4, 0xa71
  5365f0:  fld      fa3, 0x174(a4)    ; -> 0xfa7760 [.rodata] f64 256.0 (0x4070000000000000) hi64 0x3f70000000000000
  5365f4:  c.li     a4, 0x1d
  5365f6:  c.addiw  a5, 1
  5365f8:  fmul.d   fa4, fa4, fa3
  5365fc:  bne      a5, a4, -6
  536600:  fsub.d   fa3, fa5, fa4
  536604:  auipc    a5, 0xa71
  536608:  fld      fa2, 0x64(a5)    ; -> 0xfa7668 [.rodata] f64 0.5 (0x3fe0000000000000) hi64 0x408f400000000000
  53660c:  fmul.d   fa5, fa5, fa2
  536610:  fsd      fa4, -0x520(s0)
  536614:  fabs.d   fa4, fa3
  536618:  flt.d    a5, fa5, fa4
  53661c:  bnez     a5, 0x436
  536620:  lbu      a5, 0x59a(s3)
  536624:  c.beqz   a5, 0x88
      ...
  5366e8:  addi     a3, s0, -0x508
  5366ec:  addi     a2, s0, -0x510
  5366f0:  auipc    a1, 0x9de
  5366f4:  addi     a1, a1, -0x1a0    ; -> 0xf14550 [.rodata] str "block's timestamp is too early %d %d" f64 2.285710829667507e-152 (0x2073276b636f6c62) hi64 0x6d617473656d6974
  5366f8:  c.mv     a0, s1
  5366fa:  sd       a5, -0x508(s0)
  5366fe:  jal      -0xeef14    <fn 0x4477ea>
      ...
  536862:  jal      -0xe7b2    <fn 0x5280b0>
  536866:  addi     s1, s0, -0x368
  53686a:  auipc    a1, 0xa4c
  53686e:  addi     a1, a1, 0x526    ; -> 0xf82d90 [.rodata] f64 0.0 (0x0000000000000000) hi64 0x76656c2064696f76
  536872:  c.mv     a0, s1
  536874:  jal      -0xe7c4    <fn 0x5280b0>
  536878:  c.mv     a3, s1
      ...
  536a72:  jal      -0xe9c2    <fn 0x5280b0>
  536a76:  fld      fa5, -0x528(s0)
  536a7a:  fld      fa4, -0x520(s0)
  536a7e:  fsub.d   fa5, fa5, fa4
  536a82:  fabs.d   fa5, fa5
  536a86:  fsd      fa5, -0x518(s0)
  536a8a:  auipc    ra, 0x58c
  536a8e:  jalr     ra, ra, 0x37e    <fn 0xac2e08>
      ...
  536afc:  jal      -0xea4c    <fn 0x5280b0>
  536b00:  addi     s3, s0, -0x1f0
  536b04:  auipc    a1, 0xa4c
  536b08:  addi     a1, a1, 0x28c    ; -> 0xf82d90 [.rodata] f64 0.0 (0x0000000000000000) hi64 0x76656c2064696f76
  536b0c:  c.mv     a0, s3
  536b0e:  jal      -0xea5e    <fn 0x5280b0>
  536b12:  c.mv     a3, s3
      ...
  536d42:  c.li     a4, 0x1d
  536d44:  beq      a5, a4, -0x744
  536d48:  auipc    a4, 0xa71
  536d4c:  fld      fa3, -0x5e0(a4)    ; -> 0xfa7768 [.rodata] f64 0.00390625 (0x3f70000000000000) hi64 0x40915c0000000000
  536d50:  c.li     a4, 0x1d
  536d52:  c.addiw  a5, -1
  536d54:  fmul.d   fa4, fa4, fa3
  536d58:  bne      a5, a4, -6
  536d5c:  c.j      -0x75c
  536d5e:  c.li     a4, 0x1d
  536d60:  beq      a6, a4, -0x790
  536d64:  auipc    a4, 0xa71
  536d68:  fld      fa3, -0x5fc(a4)    ; -> 0xfa7768 [.rodata] f64 0.00390625 (0x3f70000000000000) hi64 0x40915c0000000000
  536d6c:  c.li     a4, 0x1d
  536d6e:  c.addiw  a6, -1
  536d70:  fmul.d   fa5, fa5, fa3
  536d74:  bne      a6, a4, -6
  536d78:  c.j      -0x7a8
  536d7a:  addi     a1, s0, -0x318
