; function 0x100470cb4-0x1004716f0 plt=['assert_rtn', 'pow', 'stack_chk_fail'] sym=__Z19GetNextWorkRequiredPK11CBlockIndexPK12CBlockHeaderRKN9Consensus6ParamsE
      ...
100470e30:  stp      q0, q0, [sp, #0x80]
100470e34:  cbz      w8, #0x10047141c
100470e38:  ldr      x9, [x19, #0x238]
100470e3c:  scvtf    d0, x9
100470e40:  mov      x10, #-0x6666666666666667
100470e44:  movk     x10, #0x999a
100470e48:  movk     x10, #0x3f99, lsl #48
100470e4c:  fmov     d1, x10
100470e50:  fmul     d0, d0, d1
100470e54:  fcvtzu   x10, d0
100470e58:  ldr      x11, [x19, #0x230]
100470e5c:  udiv     x10, x10, x11
100470e60:  stp      x10, x11, [sp, #0x30]
      ...
100471318:  cmp      x8, #1
10047131c:  b.lt     #0x100471330
100471320:  cbz      x21, #0x100471330
100471324:  scvtf    d0, x21
100471328:  ucvtf    d1, x23
10047132c:  fdiv     d9, d0, d1
100471330:  ldr      x8, [sp, #0x30]
100471334:  cmp      x20, x8
100471338:  b.lo     #0x1004713c0
10047133c:  ucvtf    d0, x20
100471340:  mov      x8, #0x3333333333333333
100471344:  movk     x8, #0x403c, lsl #48
100471348:  fmov     d1, x8
10047134c:  fdiv     d0, d0, d1
100471350:  mov      x8, #0xced9
100471354:  movk     x8, #0x53f7, lsl #16
100471358:  movk     x8, #0xa5e3, lsl #32
      ...
100471370:  str      x21, [sp, #0x20]
100471374:  mov      x21, x0
100471378:  mov      x19, x13
10047137c:  bl       #0x100d96284    <pow@plt>
100471380:  mov      x13, x19
100471384:  mov      x0, x21
100471388:  ldr      x21, [sp, #0x20]
      ...
1004713a0:  movk     x8, #0xab36, lsl #32
1004713a4:  movk     x8, #0x3fe6, lsl #48
1004713a8:  fmov     d1, x8
1004713ac:  fmadd    d0, d0, d1, d8
1004713b0:  fdiv     d1, d8, d0
1004713b4:  fcmp     d9, d1
1004713b8:  fccmp    d9, d0, #0, hi
1004713bc:  b.ge     #0x100471620
1004713c0:  ldr      x28, [x28, #8]
1004713c4:  cbz      x28, #0x100471620
