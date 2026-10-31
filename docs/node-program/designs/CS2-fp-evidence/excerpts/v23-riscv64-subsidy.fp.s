; function 0x490fe4-0x49141a plt=['__assert_fail', '__stack_chk_fail', 'memcmp', 'memcpy']
      ...
  49101c:  bge      a5, a1, 0x110
  491020:  slli     a5, s1, 0x28
  491024:  c.srli   a5, 0x28
  491026:  fcvt.d.w fa5, a5
  49102a:  auipc    a5, 0x9e5
  49102e:  fld      fs0, 0x75e(a5)    ; -> 0xe76788 [.rodata] f64 65535.0 (0x40efffe000000000) hi64 0x4070000000000000
  491032:  srliw    a5, s1, 0x18
  491036:  c.li     a4, 0x1c
  491038:  fdiv.d   fs0, fs0, fa5
  49103c:  bltu     a4, a5, 0x248
  491040:  auipc    a4, 0x9e5
  491044:  fld      fa5, 0x750(a4)    ; -> 0xe76790 [.rodata] f64 256.0 (0x4070000000000000) hi64 0x3f70000000000000
  491048:  c.li     a4, 0x1d
  49104a:  c.addiw  a5, 1
  49104c:  fmul.d   fs0, fs0, fa5
  491050:  bne      a5, a4, -6
  491054:  auipc    ra, 0x428
  491058:  jalr     ra, ra, 0x6a0    <fn 0x8b96f4>
      ...
  4911f4:  addi     a5, a5, 0x267
  4911f8:  bge      a5, s5, 0xe4
  4911fc:  auipc    a5, 0x9e5
  491200:  fld      fa5, 0x5ac(a5)    ; -> 0xe767a8 [.rodata] f64 75.0 (0x4052c00000000000) hi64 0x4049800000000000
  491204:  fle.d    a5, fs0, fa5
  491208:  c.beqz   a5, 0xc
  49120a:  c.lui    a5, 6
  49120c:  addi     a5, a5, -0x241
  491210:  bge      a5, s5, 0xcc
  491214:  auipc    a5, 0x9e5
  491218:  fld      fa5, 0x5b4(a5)    ; -> 0xe767c8 [.rodata] f64 2600.0 (0x40a4500000000000) hi64 0x4140f44700000000
  49121c:  fadd.d   fa5, fs0, fa5
  491220:  auipc    a5, 0x9e5
  491224:  fld      fa3, 0x520(a5)    ; -> 0xe76740 [.rodata] f64 9.0 (0x4022000000000000) hi64 0x8003800280018000
  491228:  auipc    a5, 0x9e5
  49122c:  fld      fa4, 0x5a8(a5)    ; -> 0xe767d0 [.rodata] f64 2222222.0 (0x4140f44700000000) hi64 0x3fe62e42fefa39ef
  491230:  c.li     a5, 5
  491232:  fdiv.d   fa5, fa5, fa3
  491236:  fmul.d   fa5, fa5, fa5
  49123a:  fdiv.d   fa5, fa4, fa5
  49123e:  fcvt.l.d a0, fa5, rtz
  491242:  bge      a0, a5, 6
  491246:  c.li     a0, 5
  491248:  c.li     a5, 0x19
      ...
  491284:  c.li     a4, 0x1d
  491286:  beq      a5, a4, -0x232
  49128a:  auipc    a4, 0x9e5
  49128e:  fld      fa5, 0x50e(a4)    ; -> 0xe76798 [.rodata] f64 0.00390625 (0x3f70000000000000) hi64 0x40915c0000000000
  491292:  c.li     a4, 0x1d
  491294:  c.addiw  a5, -1
  491296:  fmul.d   fs0, fs0, fa5
  49129a:  bne      a5, a4, -6
  49129e:  c.j      -0x24a
  4912a0:  lw       a1, 0x244(s2)
      ...
  4912ae:  c.li     a6, 0
  4912b0:  blt      a5, s5, -0xbe
  4912b4:  auipc    a5, 0x9e5
  4912b8:  fld      fa5, 0x3d4(a5)    ; -> 0xe76688 [.rodata] f64 1.0 (0x3ff0000000000000) hi64 0x3fe0000000000000
  4912bc:  fadd.d   fs0, fs0, fa5
  4912c0:  auipc    a5, 0x9e5
  4912c4:  fld      fa5, 0x4e0(a5)    ; -> 0xe767a0 [.rodata] f64 1111.0 (0x40915c0000000000) hi64 0x4052c00000000000
  4912c8:  fmul.d   fs0, fs0, fs0
  4912cc:  fdiv.d   fa5, fa5, fs0
  4912d0:  fcvt.l.d a0, fa5, rtz
  4912d4:  bgtz     a0, 0x3c
  4912d8:  c.li     a0, 1
  4912da:  c.j      0x36
  4912dc:  auipc    a5, 0x9e5
  4912e0:  fld      fa5, 0x4d4(a5)    ; -> 0xe767b0 [.rodata] f64 51.0 (0x4049800000000000) hi64 0x4018000000000000
  4912e4:  fadd.d   fa5, fs0, fa5
  4912e8:  auipc    a5, 0x9e5
  4912ec:  fld      fa3, 0x4d0(a5)    ; -> 0xe767b8 [.rodata] f64 6.0 (0x4018000000000000) hi64 0x40c5b38000000000
  4912f0:  auipc    a5, 0x9e5
  4912f4:  fld      fa4, 0x4d0(a5)    ; -> 0xe767c0 [.rodata] f64 11111.0 (0x40c5b38000000000) hi64 0x40a4500000000000
  4912f8:  c.li     a5, 0x19
  4912fa:  fdiv.d   fa5, fa5, fa3
  4912fe:  fmul.d   fa5, fa5, fa5
  491302:  fdiv.d   fa5, fa4, fa5
  491306:  fcvt.l.d a0, fa5, rtz
  49130a:  bge      a0, a5, 6
  49130e:  c.li     a0, 0x19
  491310:  addi     a5, zero, 0x1f4
      ...
  49136c:  jalr     ra, ra, -0x562    <fn 0xade06>
  491370:  slli     a5, s1, 0x28
  491374:  c.srli   a5, 0x28
  491376:  fcvt.d.w fa5, a5
  49137a:  auipc    a5, 0x9e5
  49137e:  fld      fs0, 0x40e(a5)    ; -> 0xe76788 [.rodata] f64 65535.0 (0x40efffe000000000) hi64 0x4070000000000000
  491382:  fdiv.d   fs0, fs0, fa5
  491386:  c.j      -0x332
  491388:  lw       a1, 0x244(s2)
  49138c:  c.mv     a5, a1
