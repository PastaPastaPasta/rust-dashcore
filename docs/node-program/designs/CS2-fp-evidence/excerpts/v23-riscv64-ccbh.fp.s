; function 0x49ae92-0x49ba78 plt=['__assert_fail', '__stack_chk_fail', 'memcmp', 'memcpy']
      ...
  49b064:  jalr     ra, ra, 0x2b0    <fn 0x2ad310>
  49b068:  lw       a7, 0x48(s2)
  49b06c:  auipc    a5, 0x9db
  49b070:  fld      fa4, 0x71c(a5)    ; -> 0xe76788 [.rodata] f64 65535.0 (0x40efffe000000000) hi64 0x4070000000000000
  49b074:  c.li     a4, 0x1c
  49b076:  slli     a5, a7, 0x28
  49b07a:  c.srli   a5, 0x28
  49b07c:  fcvt.d.w fa5, a5
  49b080:  srliw    a7, a7, 0x18
  49b084:  sext.w   a6, a0
  49b088:  fdiv.d   fa5, fa4, fa5
  49b08c:  bltu     a4, a7, 0x61e
  49b090:  auipc    a5, 0x9db
  49b094:  fld      fa3, 0x700(a5)    ; -> 0xe76790 [.rodata] f64 256.0 (0x4070000000000000) hi64 0x3f70000000000000
  49b098:  c.li     a5, 0x1d
  49b09a:  c.addiw  a7, 1
  49b09c:  fmul.d   fa5, fa5, fa3
  49b0a0:  bne      a7, a5, -6
  49b0a4:  slli     a5, a6, 0x28
  49b0a8:  c.srli   a5, 0x28
  49b0aa:  fcvt.d.w fa3, a5
  49b0ae:  fsd      fa5, -0x518(s0)
  49b0b2:  srliw    a6, a6, 0x18
  49b0b6:  c.li     a5, 0x1c
  49b0b8:  fdiv.d   fa4, fa4, fa3
  49b0bc:  bltu     a5, a6, 0x5d2
  49b0c0:  auipc    a5, 0x9db
  49b0c4:  fld      fa3, 0x6d0(a5)    ; -> 0xe76790 [.rodata] f64 256.0 (0x4070000000000000) hi64 0x3f70000000000000
  49b0c8:  c.li     a5, 0x1d
  49b0ca:  c.addiw  a6, 1
  49b0cc:  fmul.d   fa4, fa4, fa3
  49b0d0:  bne      a6, a5, -6
  49b0d4:  fsub.d   fa3, fa5, fa4
  49b0d8:  auipc    a5, 0x9db
  49b0dc:  fld      fa2, 0x5b8(a5)    ; -> 0xe76690 [.rodata] f64 0.5 (0x3fe0000000000000) hi64 0x408f400000000000
  49b0e0:  fmul.d   fa5, fa5, fa2
  49b0e4:  fsd      fa4, -0x510(s0)
  49b0e8:  fabs.d   fa4, fa3
  49b0ec:  flt.d    a5, fa5, fa4
  49b0f0:  bnez     a5, 0x35e
  49b0f4:  auipc    a5, 0xe3a
  49b0f8:  lbu      a5, -0x75c(a5)    ; -> 0x12d4998 [.tbss]
      ...
  49b3c2:  jal      -0xc9aa    <fn 0x48ea18>
  49b3c6:  addi     s1, s0, -0x358
  49b3ca:  auipc    a1, 0x9b6
  49b3ce:  addi     a1, a1, 0x306    ; -> 0xe516d0 [.rodata] f64 0.0 (0x0000000000000000) hi64 0x76656c2064696f76
  49b3d2:  c.mv     a0, s1
  49b3d4:  jal      -0xc9bc    <fn 0x48ea18>
  49b3d8:  c.mv     a3, s1
      ...
  49b410:  addi     a3, s0, -0x4f8
  49b414:  addi     a2, s0, -0x500
  49b418:  auipc    a1, 0x94f
  49b41c:  addi     a1, a1, 0x600    ; -> 0xdeaa18 [.rodata] str "block's timestamp is too early %d %d" f64 2.285710829667507e-152 (0x2073276b636f6c62) hi64 0x6d617473656d6974
  49b420:  c.mv     a0, s1
  49b422:  sd       a5, -0x4f8(s0)
  49b426:  jal      0x2a136    <fn 0x4c555c>
      ...
  49b46e:  jal      -0xca56    <fn 0x48ea18>
  49b472:  fld      fa5, -0x518(s0)
  49b476:  fld      fa4, -0x510(s0)
  49b47a:  fsub.d   fa5, fa5, fa4
  49b47e:  fabs.d   fa5, fa5
  49b482:  fsd      fa5, -0x508(s0)
  49b486:  auipc    ra, 0x511
  49b48a:  jalr     ra, ra, -0xbe    <fn 0x9ac3c8>
      ...
  49b624:  jal      -0xcc0c    <fn 0x48ea18>
  49b628:  addi     s3, s0, -0x1e0
  49b62c:  auipc    a1, 0x9b6
  49b630:  addi     a1, a1, 0xa4    ; -> 0xe516d0 [.rodata] f64 0.0 (0x0000000000000000) hi64 0x76656c2064696f76
  49b634:  c.mv     a0, s3
  49b636:  jal      -0xcc1e    <fn 0x48ea18>
  49b63a:  c.mv     a3, s3
      ...
  49b68e:  c.li     a5, 0x1d
  49b690:  beq      a6, a5, -0x5bc
  49b694:  auipc    a5, 0x9db
  49b698:  fld      fa3, 0x104(a5)    ; -> 0xe76798 [.rodata] f64 0.00390625 (0x3f70000000000000) hi64 0x40915c0000000000
  49b69c:  c.li     a5, 0x1d
  49b69e:  c.addiw  a6, -1
  49b6a0:  fmul.d   fa4, fa4, fa3
  49b6a4:  bne      a6, a5, -6
  49b6a8:  c.j      -0x5d4
  49b6aa:  c.li     a5, 0x1d
  49b6ac:  beq      a7, a5, -0x608
  49b6b0:  auipc    a5, 0x9db
  49b6b4:  fld      fa3, 0xe8(a5)    ; -> 0xe76798 [.rodata] f64 0.00390625 (0x3f70000000000000) hi64 0x40915c0000000000
  49b6b8:  c.li     a5, 0x1d
  49b6ba:  c.addiw  a7, -1
  49b6bc:  fmul.d   fa5, fa5, fa3
  49b6c0:  bne      a7, a5, -6
  49b6c4:  c.j      -0x620
  49b6c6:  lbu      a5, 0xb1(s1)
