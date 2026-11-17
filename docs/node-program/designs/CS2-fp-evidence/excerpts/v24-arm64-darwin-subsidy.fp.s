; function 0x10072b334-0x10072b768 plt=['ZdlPvm', 'assert_rtn', 'memcmp', 'stack_chk_fail'] sym=__ZL21GetBlockSubsidyHelperiiRKN9Consensus6ParamsEb
      ...
10072b414:  tbnz     w23, #0x1f, #0x10072b440
10072b418:  cbz      w24, #0x10072b454
10072b41c:  and      w8, w22, #0xffffff
10072b420:  ucvtf    d0, w8
10072b424:  mov      x8, #0xffe000000000
10072b428:  movk     x8, #0x40ef, lsl #48
10072b42c:  fmov     d1, x8
10072b430:  fdiv     d8, d1, d0
10072b434:  b        #0x10072b4bc
10072b438:  mov      w24, #0
10072b43c:  tbz      w23, #0x1f, #0x10072b418
      ...
10072b450:  cbnz     w24, #0x10072b41c
10072b454:  lsr      w8, w22, #0x18
10072b458:  and      w9, w22, #0xffffff
10072b45c:  ucvtf    d0, w9
10072b460:  mov      x9, #0xffe000000000
10072b464:  movk     x9, #0x40ef, lsl #48
10072b468:  fmov     d1, x9
10072b46c:  fdiv     d8, d1, d0
10072b470:  cmp      w8, #0x1c
10072b474:  b.hi     #0x10072b494
10072b478:  sub      w8, w8, #0x1d
10072b47c:  mov      x9, #0x4070000000000000
10072b480:  fmov     d0, x9
10072b484:  fmul     d8, d8, d0
10072b488:  adds     w8, w8, #1
10072b48c:  b.lo     #0x10072b484
10072b490:  b        #0x10072b4bc
      ...
10072b4a0:  add      w8, w8, #1
10072b4a4:  mov      x9, #0x3f70000000000000
10072b4a8:  fmov     d0, x9
10072b4ac:  fmul     d8, d8, d0
10072b4b0:  sub      w8, w8, #1
10072b4b4:  cmp      w8, #0x1e
10072b4b8:  b.hi     #0x10072b4ac
      ...
10072b500:  csel     x2, x9, x8, lt
10072b504:  adrp     x8, #0x100e5b000
10072b508:  add      x8, x8, #8    ; -> 0x100e5b008 [__const] str 'devnet' f64 6.3230024212015e-310 (0x000074656e766564) hi64 0x0000000000000000
10072b50c:  ldrb     w10, [x8, #0x17]    ; -> 0x100e5b017 [__const] f64 0.0 (0x0000000000000000) hi64 0x7473657467657206
10072b510:  sxtb     w9, w10
10072b514:  ldr      x11, [x8, #8]    ; -> 0x100e5b008 [__const] str 'devnet' f64 6.3230024212015e-310 (0x000074656e766564) hi64 0x0000000000000000
10072b518:  cmp      w9, #0
      ...
10072b52c:  cmp      w23, #0
10072b530:  mov      x11, sp
10072b534:  csel     x0, x10, x11, lt
10072b538:  ldr      x10, [x8]    ; -> 0x100e5b000 [__const] f64 2.0522684006491881e-289 (0x0400000000000000) hi64 0x000074656e766564
10072b53c:  cmp      w9, #0
10072b540:  csel     x1, x10, x8, lt
10072b544:  bl       #0x100d9635c    <memcmp@plt>
      ...
10072b594:  cmp      w20, w8
10072b598:  b.gt     #0x10072b5c8
10072b59c:  fmov     d0, #1.00000000
10072b5a0:  fadd     d0, d8, d0
10072b5a4:  fmul     d0, d0, d0
10072b5a8:  mov      x8, #0x5c0000000000
10072b5ac:  movk     x8, #0x4091, lsl #48
10072b5b0:  fmov     d1, x8
10072b5b4:  fdiv     d0, d1, d0
10072b5b8:  fcvtzs   x8, d0
10072b5bc:  cmp      x8, #1
10072b5c0:  csinc    x8, x8, xzr, gt
10072b5c4:  b        #0x10072b630
      ...
10072b5e0:  mov      x8, #0xc00000000000
10072b5e4:  movk     x8, #0x4052, lsl #48
10072b5e8:  fmov     d0, x8
10072b5ec:  fcmp     d8, d0
10072b5f0:  b.hi     #0x10072b6fc
10072b5f4:  mov      x8, #0x800000000000
10072b5f8:  movk     x8, #0x4049, lsl #48
10072b5fc:  fmov     d0, x8
10072b600:  fadd     d0, d8, d0
10072b604:  fmov     d1, #6.00000000
10072b608:  fdiv     d0, d0, d1
10072b60c:  fmul     d0, d0, d0
10072b610:  mov      x8, #0xb38000000000
10072b614:  movk     x8, #0x40c5, lsl #48
10072b618:  fmov     d1, x8
10072b61c:  fdiv     d0, d1, d0
10072b620:  fcvtzs   x8, d0
10072b624:  mov      w9, #0x19
10072b628:  cmp      x8, #0x19
10072b62c:  csel     x8, x8, x9, gt
      ...
10072b6fc:  mov      x8, #0x500000000000
10072b700:  movk     x8, #0x40a4, lsl #48
10072b704:  fmov     d0, x8
10072b708:  fadd     d0, d8, d0
10072b70c:  fmov     d1, #9.00000000
10072b710:  fdiv     d0, d0, d1
10072b714:  fmul     d0, d0, d0
10072b718:  mov      x8, #0xf44700000000
10072b71c:  movk     x8, #0x4140, lsl #48
10072b720:  fmov     d1, x8
10072b724:  fdiv     d0, d1, d0
10072b728:  fcvtzs   x8, d0
10072b72c:  mov      w9, #5
10072b730:  cmp      x8, #5
10072b734:  csel     x8, x8, x9, gt
