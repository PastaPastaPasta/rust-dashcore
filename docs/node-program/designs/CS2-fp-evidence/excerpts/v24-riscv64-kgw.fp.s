; function 0x304220-0x304cf8 plt=['__assert_fail', '__stack_chk_fail', 'memcpy', 'memmove', 'pow']
      ...
  3043d6:  addi     s10, s0, -0x508
  3043da:  addi     s6, s0, -0x4e8
  3043de:  beqz     a5, 0x50c
  3043e2:  fcvt.d.l fa5, a4
  3043e6:  auipc    a3, 0xca3
  3043ea:  fld      fa4, 0x2f2(a3)    ; -> 0xfa76d8 [.rodata] f64 0.025 (0x3f9999999999999a) hi64 0xbff3a5e353f7ced9
  3043ee:  fmul.d   fa5, fa5, fa4
  3043f2:  fcvt.lu.d a3, fa5, rtz
  3043f6:  divu     a3, a3, s4
  3043fa:  sd       a3, -0x5c0(s0)
  3043fe:  bltu     a5, a3, 0x4ec
      ...
  30444c:  addi     a5, s0, -0x228
  304450:  sd       a5, -0x5d0(s0)
  304454:  auipc    a5, 0xca3
  304458:  fld      fs1, 0x20c(a5)    ; -> 0xfa7660 [.rodata] f64 1.0 (0x3ff0000000000000) hi64 0x3fe0000000000000
  30445c:  auipc    a5, 0xca3
  304460:  fld      fs3, 0x284(a5)    ; -> 0xfa76e0 [.rodata] f64 -1.228 (0xbff3a5e353f7ced9) hi64 0x403c333333333333
  304464:  auipc    a5, 0xca3
  304468:  fld      fs2, 0x284(a5)    ; -> 0xfa76e8 [.rodata] f64 28.2 (0x403c333333333333) hi64 0x3fe6ab367a0f9097
  30446c:  auipc    a5, 0xca3
  304470:  fld      fs4, 0x284(a5)    ; -> 0xfa76f0 [.rodata] f64 0.7084 (0x3fe6ab367a0f9097) hi64 0x4024000000000000
  304474:  c.j      0x8a
  304476:  lwu      a5, 0x78(s7)
  30447a:  lwu      a1, 0x78(s2)
      ...
  3044ae:  sd       a5, -0x5b8(s0)
  3044b2:  bgtz     s9, 0x4b4
  3044b6:  c.li     s9, 0
  3044b8:  fcvt.d.lu fa0, s8
  3044bc:  fmv.d    fa1, fs3
  3044c0:  fdiv.d   fa0, fa0, fs2
  3044c4:  auipc    ra, 0xffd80
  3044c8:  jalr     ra, ra, -0x7d4    <pow@plt>
  3044cc:  ld       a5, -0x5c0(s0)
  3044d0:  bltu     s8, a5, 0x1c
  3044d4:  fmadd.d  fa5, fa0, fs4, fs1
  3044d8:  fdiv.d   fa4, fs1, fa5
  3044dc:  fle.d    a5, fs0, fa4
  3044e0:  bnez     a5, 0x62c
  3044e4:  fle.d    a5, fa5, fs0
  3044e8:  bnez     a5, 0x624
  3044ec:  ld       s7, 8(s7)
  3044f0:  beqz     s7, 0x61c
      ...
  304964:  c.j      -0x60c
  304966:  ld       a5, -0x5b8(s0)
  30496a:  beqz     a5, -0x4b2
  30496e:  fcvt.d.l fs0, a5
  304972:  fcvt.d.l fa5, s9
  304976:  fdiv.d   fs0, fs0, fa5
  30497a:  c.j      -0x4c2
  30497c:  ld       a5, 0(s6)
  304980:  ld       t5, 8(s6)
