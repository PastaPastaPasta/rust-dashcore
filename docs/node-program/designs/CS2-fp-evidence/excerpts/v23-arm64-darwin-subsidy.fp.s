; function 0x10065b7d8-0x10065bd2c plt=['ZdlPvm', 'Znwm', 'assert_rtn', 'memcmp', 'memmove', 'stack_chk_fail'] sym=__ZL21GetBlockSubsidyHelperiiRKN9Consensus6ParamsEb
      ...
10065b950:  tbnz     w23, #0, #0x10065b9d0
10065b954:  lsr      w8, w22, #0x18
10065b958:  and      w9, w22, #0xffffff
10065b95c:  ucvtf    d0, w9
10065b960:  mov      x9, #0xffe000000000
10065b964:  movk     x9, #0x40ef, lsl #48
10065b968:  fmov     d1, x9
10065b96c:  fdiv     d8, d1, d0
10065b970:  cmp      w8, #0x1c
10065b974:  b.hi     #0x10065b994
10065b978:  sub      w8, w8, #0x1d
10065b97c:  mov      x9, #0x4070000000000000
10065b980:  fmov     d0, x9
10065b984:  fmul     d8, d8, d0
10065b988:  adds     w8, w8, #1
10065b98c:  b.lo     #0x10065b984
10065b990:  b        #0x10065b9e8
      ...
10065b9a0:  add      w8, w8, #1
10065b9a4:  mov      x9, #0x3f70000000000000
10065b9a8:  fmov     d0, x9
10065b9ac:  fmul     d8, d8, d0
10065b9b0:  sub      w8, w8, #1
10065b9b4:  cmp      w8, #0x1e
10065b9b8:  b.hi     #0x10065b9ac
      ...
10065b9c8:  and      x1, x8, #0x7fffffffffffffff
10065b9cc:  bl       #0x100c81360    <ZdlPvm@plt>
10065b9d0:  and      w8, w22, #0xffffff
10065b9d4:  ucvtf    d0, w8
10065b9d8:  mov      x8, #0xffe000000000
10065b9dc:  movk     x8, #0x40ef, lsl #48
10065b9e0:  fmov     d1, x8
10065b9e4:  fdiv     d8, d1, d0
10065b9e8:  bl       #0x1008910a0    <fn 0x1008910a0 __Z6Paramsv>
10065b9ec:  ldrsb    w8, [x0, #0x33f]
10065b9f0:  tbnz     w8, #0x1f, #0x10065ba0c
      ...
10065bb4c:  cmp      w20, w8
10065bb50:  b.gt     #0x10065bb80
10065bb54:  fmov     d0, #1.00000000
10065bb58:  fadd     d0, d8, d0
10065bb5c:  fmul     d0, d0, d0
10065bb60:  mov      x8, #0x5c0000000000
10065bb64:  movk     x8, #0x4091, lsl #48
10065bb68:  fmov     d1, x8
10065bb6c:  fdiv     d0, d1, d0
10065bb70:  fcvtzs   x8, d0
10065bb74:  cmp      x8, #1
10065bb78:  csinc    x8, x8, xzr, gt
10065bb7c:  b        #0x10065bbe8
      ...
10065bb98:  mov      x8, #0xc00000000000
10065bb9c:  movk     x8, #0x4052, lsl #48
10065bba0:  fmov     d0, x8
10065bba4:  fcmp     d8, d0
10065bba8:  b.hi     #0x10065bcb8
10065bbac:  mov      x8, #0x800000000000
10065bbb0:  movk     x8, #0x4049, lsl #48
10065bbb4:  fmov     d0, x8
10065bbb8:  fadd     d0, d8, d0
10065bbbc:  fmov     d1, #6.00000000
10065bbc0:  fdiv     d0, d0, d1
10065bbc4:  fmul     d0, d0, d0
10065bbc8:  mov      x8, #0xb38000000000
10065bbcc:  movk     x8, #0x40c5, lsl #48
10065bbd0:  fmov     d1, x8
10065bbd4:  fdiv     d0, d1, d0
10065bbd8:  fcvtzs   x8, d0
10065bbdc:  mov      w9, #0x19
10065bbe0:  cmp      x8, #0x19
10065bbe4:  csel     x8, x8, x9, gt
      ...
10065bcb8:  mov      x8, #0x500000000000
10065bcbc:  movk     x8, #0x40a4, lsl #48
10065bcc0:  fmov     d0, x8
10065bcc4:  fadd     d0, d8, d0
10065bcc8:  fmov     d1, #9.00000000
10065bccc:  fdiv     d0, d0, d1
10065bcd0:  fmul     d0, d0, d0
10065bcd4:  mov      x8, #0xf44700000000
10065bcd8:  movk     x8, #0x4140, lsl #48
10065bcdc:  fmov     d1, x8
10065bce0:  fdiv     d0, d1, d0
10065bce4:  fcvtzs   x8, d0
10065bce8:  mov      w9, #5
10065bcec:  cmp      x8, #5
10065bcf0:  csel     x8, x8, x9, gt
