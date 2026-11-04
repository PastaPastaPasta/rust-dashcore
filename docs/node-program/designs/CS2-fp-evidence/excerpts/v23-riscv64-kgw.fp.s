; function 0x2ad310-0x2adde8 plt=['__assert_fail', '__stack_chk_fail', 'memcpy', 'memmove', 'pow']
      ...
  2ad4c6:  addi     s10, s0, -0x508
  2ad4ca:  addi     s6, s0, -0x4e8
  2ad4ce:  beqz     a5, 0x50c
  2ad4d2:  fcvt.d.l fa5, a4
  2ad4d6:  auipc    a3, 0xbc9
  2ad4da:  fld      fa4, 0x232(a3)    ; -> 0xe76708 [.rodata] f64 0.025 (0x3f9999999999999a) hi64 0xbff3a5e353f7ced9
  2ad4de:  fmul.d   fa5, fa5, fa4
  2ad4e2:  fcvt.lu.d a3, fa5, rtz
  2ad4e6:  divu     a3, a3, s4
  2ad4ea:  sd       a3, -0x5c0(s0)
  2ad4ee:  bltu     a5, a3, 0x4ec
      ...
  2ad53c:  addi     a5, s0, -0x228
  2ad540:  sd       a5, -0x5d0(s0)
  2ad544:  auipc    a5, 0xbc9
  2ad548:  fld      fs1, 0x144(a5)    ; -> 0xe76688 [.rodata] f64 1.0 (0x3ff0000000000000) hi64 0x3fe0000000000000
  2ad54c:  auipc    a5, 0xbc9
  2ad550:  fld      fs3, 0x1c4(a5)    ; -> 0xe76710 [.rodata] f64 -1.228 (0xbff3a5e353f7ced9) hi64 0x403c333333333333
  2ad554:  auipc    a5, 0xbc9
  2ad558:  fld      fs2, 0x1c4(a5)    ; -> 0xe76718 [.rodata] f64 28.2 (0x403c333333333333) hi64 0x3fe6ab367a0f9097
  2ad55c:  auipc    a5, 0xbc9
  2ad560:  fld      fs4, 0x1c4(a5)    ; -> 0xe76720 [.rodata] f64 0.7084 (0x3fe6ab367a0f9097) hi64 0x4024000000000000
  2ad564:  c.j      0x8a
  2ad566:  lwu      a5, 0x78(s7)
  2ad56a:  lwu      a1, 0x78(s2)
      ...
  2ad59e:  sd       a5, -0x5b8(s0)
  2ad5a2:  bgtz     s9, 0x4b4
  2ad5a6:  c.li     s9, 0
  2ad5a8:  fcvt.d.lu fa0, s8
  2ad5ac:  fmv.d    fa1, fs3
  2ad5b0:  fdiv.d   fa0, fa0, fs2
  2ad5b4:  auipc    ra, 0xffdd4
  2ad5b8:  jalr     ra, ra, 0x73c    <pow@plt>
  2ad5bc:  ld       a5, -0x5c0(s0)
  2ad5c0:  bltu     s8, a5, 0x1c
  2ad5c4:  fmadd.d  fa5, fa0, fs4, fs1
  2ad5c8:  fdiv.d   fa4, fs1, fa5
  2ad5cc:  fle.d    a5, fs0, fa4
  2ad5d0:  bnez     a5, 0x62c
  2ad5d4:  fle.d    a5, fa5, fs0
  2ad5d8:  bnez     a5, 0x624
  2ad5dc:  ld       s7, 8(s7)
  2ad5e0:  beqz     s7, 0x61c
      ...
  2ada54:  c.j      -0x60c
  2ada56:  ld       a5, -0x5b8(s0)
  2ada5a:  beqz     a5, -0x4b2
  2ada5e:  fcvt.d.l fs0, a5
  2ada62:  fcvt.d.l fa5, s9
  2ada66:  fdiv.d   fs0, fs0, fa5
  2ada6a:  c.j      -0x4c2
  2ada6c:  ld       a5, 0(s6)
  2ada70:  ld       t5, 8(s6)
