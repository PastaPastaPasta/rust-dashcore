; function 0x35c7a0-0x35d160 plt=['__assert_fail', '__stack_chk_fail', 'memmove', 'pow']
      ...
  35c964:  stp      xzr, xzr, [x26, #0x10]
  35c968:  cbz      w0, #0x35ce04
  35c96c:  adrp     x2, #0x115c000
  35c970:  scvtf    d0, d1
  35c974:  ldr      d2, [x2, #0x2c0]    ; -> 0x115c2c0 [.rodata] f64 0.025 (0x3f9999999999999a) hi64 0xbff3a5e353f7ced9
  35c978:  fmul     d0, d0, d2
  35c97c:  fcvtzu   x25, d0
  35c980:  udiv     x1, x25, x1
  35c984:  str      x1, [sp, #0x10]
  35c988:  cmp      x1, w0, sxtw
      ...
  35c9ec:  str      d11, [sp, #0x5b8]
  35c9f0:  str      x0, [sp, #0x28]
  35c9f4:  adrp     x0, #0x115c000
  35c9f8:  ldr      d10, [x0, #0x2c8]    ; -> 0x115c2c8 [.rodata] f64 -1.228 (0xbff3a5e353f7ced9) hi64 0x3fe6ab367a0f9097
  35c9fc:  adrp     x0, #0x115c000
  35ca00:  ldr      d11, [x0, #0x2d0]    ; -> 0x115c2d0 [.rodata] f64 0.7084 (0x3fe6ab367a0f9097) hi64 0x0003a98000000004
  35ca04:  b        #0x35caac
  35ca08:  ldr      x2, [x19, #0x230]
  35ca0c:  fmov     d8, #1.00000000
      ...
  35ca44:  mov      w22, w0
  35ca48:  b.eq     #0x35ca5c
  35ca4c:  fmov     d0, x2
  35ca50:  scvtf    d8, x28
  35ca54:  scvtf    d0, d0
  35ca58:  fdiv     d8, d8, d0
  35ca5c:  ucvtf    d0, x25
  35ca60:  fmov     d1, d10
  35ca64:  fdiv     d0, d0, d9
  35ca68:  bl       #0x91490    <pow@plt>
  35ca6c:  ldr      x0, [sp, #0x10]
  35ca70:  cmp      x0, x25
  35ca74:  b.hi     #0x35ca94
  35ca78:  fmov     d1, #1.00000000
  35ca7c:  fmadd    d0, d0, d11, d1
  35ca80:  fdiv     d1, d1, d0
  35ca84:  fcmpe    d1, d8
  35ca88:  b.ge     #0x35cfd0
  35ca8c:  fcmpe    d0, d8
  35ca90:  b.ls     #0x35cfd0
  35ca94:  ldr      x27, [x27, #8]
  35ca98:  cbz      x27, #0x35cfd0
