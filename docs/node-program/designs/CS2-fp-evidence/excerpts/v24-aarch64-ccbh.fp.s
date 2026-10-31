; function 0x594ee0-0x595e18 plt=['__assert_fail', '__stack_chk_fail', 'memcmp', 'memcpy', 'pthread_mutex_lock', 'pthread_mutex_unlock']
      ...
  5950b0:  fmov     d2, x0
  5950b4:  nop      
  5950b8:  sub      w5, w5, #1
  5950bc:  fmul     d1, d1, d2
  5950c0:  cmp      w5, #0x1d
  5950c4:  b.ne     #0x5950b8
  5950c8:  fmov     d3, #0.50000000
  5950cc:  fabd     d2, d0, d1
  5950d0:  str      d1, [sp, #0x10]
  5950d4:  fmul     d0, d0, d3
  5950d8:  fcmpe    d0, d2
  5950dc:  b.mi     #0x59537c
  5950e0:  ldrb     w0, [x21, #0x59a]
  5950e4:  tbz      w0, #0, #0x595178
      ...
  5951bc:  adrp     x0, #0x10c8000
  5951c0:  mov      x8, x19
  5951c4:  add      x1, sp, #0x20
  5951c8:  add      x0, x0, #0x608    ; -> 0x10c8608 [.rodata] str "block's timestamp is too early %d %d" f64 2.285710829667507e-152 (0x2073276b636f6c62) hi64 0x6d617473656d6974
  5951cc:  str      x3, [sp, #0x28]
  5951d0:  bl       #0x49cd20    <fn 0x49cd20>
  5951d4:  mov      w0, #4
      ...
  595284:  fmov     d0, x0
  595288:  and      w0, w6, #0xffffff
  59528c:  lsr      w6, w6, #0x18
  595290:  scvtf    d1, w0
  595294:  fdiv     d0, d0, d1
  595298:  cmp      w6, #0x1c
  59529c:  b.hi     #0x595894
  5952a0:  mov      x0, #0x4070000000000000
  5952a4:  fmov     d1, x0
  5952a8:  add      w6, w6, #1
  5952ac:  fmul     d0, d0, d1
  5952b0:  cmp      w6, #0x1d
  5952b4:  b.ne     #0x5952a8
  5952b8:  and      w1, w5, #0xffffff
  5952bc:  mov      x0, #0xffe000000000
  5952c0:  movk     x0, #0x40ef, lsl #48
  5952c4:  fmov     d1, x0
  5952c8:  scvtf    d2, w1
  5952cc:  lsr      w5, w5, #0x18
  5952d0:  str      d0, [sp, #8]
  5952d4:  fdiv     d1, d1, d2
  5952d8:  cmp      w5, #0x1c
  5952dc:  b.hi     #0x5950a4
  5952e0:  mov      x0, #0x4070000000000000
  5952e4:  fmov     d2, x0
  5952e8:  add      w5, w5, #1
  5952ec:  fmul     d1, d1, d2
  5952f0:  cmp      w5, #0x1d
  5952f4:  b.ne     #0x5952e8
  5952f8:  fmov     d3, #0.50000000
  5952fc:  fabd     d2, d0, d1
  595300:  str      d1, [sp, #0x10]
  595304:  fmul     d0, d0, d3
  595308:  fcmpe    d0, d2
  59530c:  b.mi     #0x59537c
  595310:  b        #0x5950e0
  595314:  bl       #0xbf3184    <fn 0xbf3184>
      ...
  5953a4:  ldp      d0, d1, [sp, #8]
  5953a8:  adrp     x19, #0x158d000
  5953ac:  ldr      x19, [x19, #0xc70]    ; -> 0x158dc70 [.tbss]
  5953b0:  fabd     d0, d0, d1
  5953b4:  str      d0, [sp, #0x18]
  5953b8:  bl       #0xb90490    <fn 0xb90490>
  5953bc:  adrp     x21, #0x158d000
      ...
  5955ac:  bl       #0x585a30    <fn 0x585a30>
  5955b0:  adrp     x1, #0x114c000
  5955b4:  mov      x0, x21
  5955b8:  add      x1, x1, #0x340    ; -> 0x114c340 [.rodata] f64 0.0 (0x0000000000000000) hi64 0x6c61746f54746547
  5955bc:  bl       #0x585a30    <fn 0x585a30>
  5955c0:  mov      x0, x22
  5955c4:  mov      x3, x21
      ...
  5956d8:  bl       #0x585a30    <fn 0x585a30>
  5956dc:  adrp     x1, #0x114c000
  5956e0:  mov      x0, x19
  5956e4:  add      x1, x1, #0x340    ; -> 0x114c340 [.rodata] f64 0.0 (0x0000000000000000) hi64 0x6c61746f54746547
  5956e8:  bl       #0x585a30    <fn 0x585a30>
  5956ec:  mov      x0, x22
  5956f0:  mov      x3, x19
      ...
  5958a0:  fmov     d1, x0
  5958a4:  nop      
  5958a8:  sub      w6, w6, #1
  5958ac:  fmul     d0, d0, d1
  5958b0:  cmp      w6, #0x1d
  5958b4:  b.ne     #0x5958a8
  5958b8:  b        #0x5952b8
