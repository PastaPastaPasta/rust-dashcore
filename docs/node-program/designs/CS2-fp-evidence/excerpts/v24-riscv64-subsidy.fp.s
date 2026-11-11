; function 0x529d20-0x52a180 plt=['__assert_fail', '__stack_chk_fail', 'memcmp', 'memcpy']
      ...
  529d5c:  bge      a5, a1, 0x10c
  529d60:  slli     a5, s1, 0x28
  529d64:  c.srli   a5, 0x28
  529d66:  fcvt.d.w fa5, a5
  529d6a:  auipc    a5, 0xa7e
  529d6e:  fld      fs0, -0x612(a5)    ; -> 0xfa7758 [.rodata] f64 65535.0 (0x40efffe000000000) hi64 0x4070000000000000
  529d72:  srliw    a5, s1, 0x18
  529d76:  c.li     a4, 0x1c
  529d78:  fdiv.d   fs0, fs0, fa5
  529d7c:  bltu     a4, a5, 0x24e
  529d80:  auipc    a4, 0xa7e
  529d84:  fld      fa5, -0x620(a4)    ; -> 0xfa7760 [.rodata] f64 256.0 (0x4070000000000000) hi64 0x3f70000000000000
  529d88:  c.li     a4, 0x1d
  529d8a:  c.addiw  a5, 1
  529d8c:  fmul.d   fs0, fs0, fa5
  529d90:  bne      a5, a4, -6
  529d94:  auipc    ra, 0x4af
  529d98:  jalr     ra, ra, -0x5b8    <fn 0x9d87dc>
      ...
  529f3a:  addi     a5, a5, 0x267
  529f3e:  bge      a5, s5, 0xe4
  529f42:  auipc    a5, 0xa7e
  529f46:  fld      fa5, -0x7ca(a5)    ; -> 0xfa7778 [.rodata] f64 75.0 (0x4052c00000000000) hi64 0x4049800000000000
  529f4a:  fle.d    a5, fs0, fa5
  529f4e:  c.beqz   a5, 0xc
  529f50:  c.lui    a5, 6
  529f52:  addi     a5, a5, -0x241
  529f56:  bge      a5, s5, 0xcc
  529f5a:  auipc    a5, 0xa7e
  529f5e:  fld      fa5, -0x7c2(a5)    ; -> 0xfa7798 [.rodata] f64 2600.0 (0x40a4500000000000) hi64 0x4140f44700000000
  529f62:  fadd.d   fa5, fs0, fa5
  529f66:  auipc    a5, 0xa7d
  529f6a:  fld      fa3, 0x7aa(a5)    ; -> 0xfa7710 [.rodata] f64 9.0 (0x4022000000000000) hi64 0x8003800280018000
  529f6e:  auipc    a5, 0xa7e
  529f72:  fld      fa4, -0x7ce(a5)    ; -> 0xfa77a0 [.rodata] f64 2222222.0 (0x4140f44700000000) hi64 0x3fe62e42fefa39ef
  529f76:  c.li     a5, 5
  529f78:  fdiv.d   fa5, fa5, fa3
  529f7c:  fmul.d   fa5, fa5, fa5
  529f80:  fdiv.d   fa5, fa4, fa5
  529f84:  fcvt.l.d a0, fa5, rtz
  529f88:  bge      a0, a5, 6
  529f8c:  c.li     a0, 5
  529f8e:  c.li     a5, 0x19
      ...
  529fca:  c.li     a4, 0x1d
  529fcc:  beq      a5, a4, -0x238
  529fd0:  auipc    a4, 0xa7d
  529fd4:  fld      fa5, 0x798(a4)    ; -> 0xfa7768 [.rodata] f64 0.00390625 (0x3f70000000000000) hi64 0x40915c0000000000
  529fd8:  c.li     a4, 0x1d
  529fda:  c.addiw  a5, -1
  529fdc:  fmul.d   fs0, fs0, fa5
  529fe0:  bne      a5, a4, -6
  529fe4:  c.j      -0x250
  529fe6:  lw       a1, 0x28c(s2)
      ...
  529ff4:  c.li     a6, 0
  529ff6:  blt      a5, s5, -0xbe
  529ffa:  auipc    a5, 0xa7d
  529ffe:  fld      fa5, 0x666(a5)    ; -> 0xfa7660 [.rodata] f64 1.0 (0x3ff0000000000000) hi64 0x3fe0000000000000
  52a002:  fadd.d   fs0, fs0, fa5
  52a006:  auipc    a5, 0xa7d
  52a00a:  fld      fa5, 0x76a(a5)    ; -> 0xfa7770 [.rodata] f64 1111.0 (0x40915c0000000000) hi64 0x4052c00000000000
  52a00e:  fmul.d   fs0, fs0, fs0
  52a012:  fdiv.d   fa5, fa5, fs0
  52a016:  fcvt.l.d a0, fa5, rtz
  52a01a:  bgtz     a0, 0x3c
  52a01e:  c.li     a0, 1
  52a020:  c.j      0x36
  52a022:  auipc    a5, 0xa7d
  52a026:  fld      fa5, 0x75e(a5)    ; -> 0xfa7780 [.rodata] f64 51.0 (0x4049800000000000) hi64 0x4018000000000000
  52a02a:  fadd.d   fa5, fs0, fa5
  52a02e:  auipc    a5, 0xa7d
  52a032:  fld      fa3, 0x75a(a5)    ; -> 0xfa7788 [.rodata] f64 6.0 (0x4018000000000000) hi64 0x40c5b38000000000
  52a036:  auipc    a5, 0xa7d
  52a03a:  fld      fa4, 0x75a(a5)    ; -> 0xfa7790 [.rodata] f64 11111.0 (0x40c5b38000000000) hi64 0x40a4500000000000
  52a03e:  c.li     a5, 0x19
  52a040:  fdiv.d   fa5, fa5, fa3
  52a044:  fmul.d   fa5, fa5, fa5
  52a048:  fdiv.d   fa5, fa4, fa5
  52a04c:  fcvt.l.d a0, fa5, rtz
  52a050:  bge      a0, a5, 6
  52a054:  c.li     a0, 0x19
  52a056:  addi     a5, zero, 0x1f4
      ...
  52a0bc:  jalr     ra, ra, -0x788    <fn 0xe12930>
  52a0c0:  slli     a5, s1, 0x28
  52a0c4:  c.srli   a5, 0x28
  52a0c6:  fcvt.d.w fa5, a5
  52a0ca:  auipc    a5, 0xa7d
  52a0ce:  fld      fs0, 0x68e(a5)    ; -> 0xfa7758 [.rodata] f64 65535.0 (0x40efffe000000000) hi64 0x4070000000000000
  52a0d2:  fdiv.d   fs0, fs0, fa5
  52a0d6:  c.j      -0x342
  52a0d8:  lw       a1, 0x28c(s2)
  52a0dc:  c.mv     a5, a1
