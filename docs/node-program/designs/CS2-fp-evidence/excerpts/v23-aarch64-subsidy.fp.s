; function 0x4dc010-0x4dc500 plt=['__assert_fail', '__stack_chk_fail', 'memcmp', 'memcpy']
      ...
  4dc060:  mov      x0, #0xffe000000000
  4dc064:  movk     x0, #0x40ef, lsl #48
  4dc068:  fmov     d8, x0
  4dc06c:  scvtf    d0, w1
  4dc070:  lsr      w0, w20, #0x18
  4dc074:  fdiv     d8, d8, d0
  4dc078:  cmp      w0, #0x1c
  4dc07c:  b.hi     #0x4dc2c4
  4dc080:  mov      x1, #0x4070000000000000
  4dc084:  fmov     d0, x1
  4dc088:  add      w0, w0, #1
  4dc08c:  fmul     d8, d8, d0
  4dc090:  cmp      w0, #0x1d
  4dc094:  b.ne     #0x4dc088
  4dc098:  bl       #0x94fa10    <fn 0x94fa10>
      ...
  4dc2d0:  fmov     d0, x1
  4dc2d4:  nop      
  4dc2d8:  sub      w0, w0, #1
  4dc2dc:  fmul     d8, d8, d0
  4dc2e0:  cmp      w0, #0x1d
  4dc2e4:  b.ne     #0x4dc2d8
  4dc2e8:  b        #0x4dc098
      ...
  4dc310:  mov      x0, #0xc00000000000
  4dc314:  movk     x0, #0x4052, lsl #48
  4dc318:  fmov     d0, x0
  4dc31c:  fccmpe   d8, d0, #2, le
  4dc320:  cset     w0, ls
  4dc324:  cbnz     w0, #0x4dc3a0
  4dc328:  mov      x0, #0x500000000000
      ...
  4dc334:  fmov     d0, x0
  4dc338:  mov      x0, #0xf44700000000
  4dc33c:  mov      x3, #5
  4dc340:  fadd     d1, d8, d0
  4dc344:  movk     x0, #0x4140, lsl #48
  4dc348:  mov      x2, #0x19
  4dc34c:  fmov     d0, x0
  4dc350:  fdiv     d1, d1, d2
  4dc354:  mov      x1, #0xe100
  4dc358:  movk     x1, #0x5f5, lsl #16
  4dc35c:  fmul     d1, d1, d1
  4dc360:  fdiv     d0, d0, d1
  4dc364:  fcvtzs   x0, d0
  4dc368:  cmp      x0, x3
  4dc36c:  csel     x0, x0, x3, ge
  4dc370:  cmp      x0, x2
      ...
  4dc3ac:  fmov     d0, x0
  4dc3b0:  mov      x0, #0xb38000000000
  4dc3b4:  mov      x3, #0x19
  4dc3b8:  fadd     d1, d8, d0
  4dc3bc:  movk     x0, #0x40c5, lsl #48
  4dc3c0:  mov      x2, #0x1f4
  4dc3c4:  fmov     d0, x0
      ...
  4dc3d0:  mov      x0, #0x5c0000000000
  4dc3d4:  movk     x0, #0x4091, lsl #48
  4dc3d8:  fmov     d0, x0
  4dc3dc:  fadd     d8, d8, d1
  4dc3e0:  mov      x2, #0x1f4
  4dc3e4:  mov      x1, #0xe100
  4dc3e8:  movk     x1, #0x5f5, lsl #16
  4dc3ec:  fmul     d8, d8, d8
  4dc3f0:  fdiv     d0, d0, d8
  4dc3f4:  fcvtzs   x0, d0
  4dc3f8:  cmp      x0, #0
  4dc3fc:  csinc    x0, x0, xzr, gt
  4dc400:  cmp      x0, x2
      ...
  4dc430:  mov      x2, #0xffe000000000
  4dc434:  movk     x2, #0x40ef, lsl #48
  4dc438:  fmov     d8, x2
  4dc43c:  scvtf    d0, w0
  4dc440:  fdiv     d8, d8, d0
  4dc444:  b        #0x4dc098
  4dc448:  cmp      x20, x23
  4dc44c:  b.ne     #0x4dc264
