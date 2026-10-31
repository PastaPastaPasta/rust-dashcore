; function 0x2f0270-0x2f0c30 plt=['__assert_fail', '__stack_chk_fail', 'memmove', 'pow']
      ...
  2f0434:  stp      xzr, xzr, [x26, #0x10]
  2f0438:  cbz      w0, #0x2f08d4
  2f043c:  adrp     x2, #0x1017000
  2f0440:  scvtf    d0, d1
  2f0444:  ldr      d2, [x2, #0x390]    ; -> 0x1017390 [.rodata] f64 0.025 (0x3f9999999999999a) hi64 0xbff3a5e353f7ced9
  2f0448:  fmul     d0, d0, d2
  2f044c:  fcvtzu   x25, d0
  2f0450:  udiv     x1, x25, x1
  2f0454:  str      x1, [sp, #0x10]
  2f0458:  cmp      x1, w0, sxtw
      ...
  2f04bc:  str      d11, [sp, #0x5b8]
  2f04c0:  str      x0, [sp, #0x28]
  2f04c4:  adrp     x0, #0x1017000
  2f04c8:  ldr      d10, [x0, #0x398]    ; -> 0x1017398 [.rodata] f64 -1.228 (0xbff3a5e353f7ced9) hi64 0x3fe6ab367a0f9097
  2f04cc:  adrp     x0, #0x1017000
  2f04d0:  ldr      d11, [x0, #0x3a0]    ; -> 0x10173a0 [.rodata] f64 0.7084 (0x3fe6ab367a0f9097) hi64 0x0000000400000003
  2f04d4:  b        #0x2f057c
  2f04d8:  ldr      x2, [x19, #0x1e8]
  2f04dc:  fmov     d8, #1.00000000
      ...
  2f0514:  mov      w22, w0
  2f0518:  b.eq     #0x2f052c
  2f051c:  fmov     d0, x2
  2f0520:  scvtf    d8, x28
  2f0524:  scvtf    d0, d0
  2f0528:  fdiv     d8, d8, d0
  2f052c:  ucvtf    d0, x25
  2f0530:  fmov     d1, d10
  2f0534:  fdiv     d0, d0, d9
  2f0538:  bl       #0x81490    <pow@plt>
  2f053c:  ldr      x0, [sp, #0x10]
  2f0540:  cmp      x0, x25
  2f0544:  b.hi     #0x2f0564
  2f0548:  fmov     d1, #1.00000000
  2f054c:  fmadd    d0, d0, d11, d1
  2f0550:  fdiv     d1, d1, d0
  2f0554:  fcmpe    d1, d8
  2f0558:  b.ge     #0x2f0aa0
  2f055c:  fcmpe    d0, d8
  2f0560:  b.ls     #0x2f0aa0
  2f0564:  ldr      x27, [x27, #8]
  2f0568:  cbz      x27, #0x2f0aa0
