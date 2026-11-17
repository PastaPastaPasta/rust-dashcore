; function 0x4e6650-0x4e7334 plt=['__assert_fail', '__stack_chk_fail', 'memcmp', 'pthread_mutex_lock', 'pthread_mutex_unlock']
      ...
  4e685c:  fmov     d0, x1
  4e6860:  and      w1, w0, #0xffffff
  4e6864:  lsr      w0, w0, #0x18
  4e6868:  scvtf    d1, w1
  4e686c:  fdiv     d0, d0, d1
  4e6870:  cmp      w0, #0x1c
  4e6874:  b.hi     #0x4e6e9c
  4e6878:  mov      x1, #0x4070000000000000
  4e687c:  fmov     d1, x1
  4e6880:  add      w0, w0, #1
  4e6884:  fmul     d0, d0, d1
  4e6888:  cmp      w0, #0x1d
  4e688c:  b.ne     #0x4e6880
  4e6890:  and      w1, w6, #0xffffff
  4e6894:  mov      x0, #0xffe000000000
  4e6898:  movk     x0, #0x40ef, lsl #48
  4e689c:  fmov     d1, x0
  4e68a0:  scvtf    d2, w1
  4e68a4:  lsr      w6, w6, #0x18
  4e68a8:  str      d0, [sp, #8]
  4e68ac:  fdiv     d1, d1, d2
  4e68b0:  cmp      w6, #0x1c
  4e68b4:  b.hi     #0x4e6e5c
  4e68b8:  mov      x0, #0x4070000000000000
  4e68bc:  fmov     d2, x0
  4e68c0:  add      w6, w6, #1
  4e68c4:  fmul     d1, d1, d2
  4e68c8:  cmp      w6, #0x1d
  4e68cc:  b.ne     #0x4e68c0
  4e68d0:  fmov     d3, #0.50000000
  4e68d4:  fabd     d2, d0, d1
  4e68d8:  str      d1, [sp, #0x10]
  4e68dc:  fmul     d0, d0, d3
  4e68e0:  fcmpe    d0, d2
  4e68e4:  b.mi     #0x4e6a20
  4e68e8:  adrp     x0, #0x1421000
  4e68ec:  ldrb     w0, [x0, #0xdc0]    ; -> 0x1421dc0 [.tbss]
      ...
  4e69e4:  adrp     x0, #0xf8b000
  4e69e8:  add      x2, sp, #0x28
  4e69ec:  add      x1, sp, #0x20
  4e69f0:  add      x0, x0, #0xae0    ; -> 0xf8bae0 [.rodata] str "block's timestamp is too early %d %d" f64 2.285710829667507e-152 (0x2073276b636f6c62) hi64 0x6d617473656d6974
  4e69f4:  str      x3, [sp, #0x28]
  4e69f8:  bl       #0x513700    <fn 0x513700>
  4e69fc:  mov      w0, #4
      ...
  4e6a48:  ldp      d0, d1, [sp, #8]
  4e6a4c:  adrp     x19, #0x141d000
  4e6a50:  ldr      x19, [x19, #0xbe0]    ; -> 0x141dbe0 [.tbss]
  4e6a54:  fabd     d0, d0, d1
  4e6a58:  str      d0, [sp, #0x18]
  4e6a5c:  bl       #0xa5f460    <fn 0xa5f460>
  4e6a60:  adrp     x25, #0x141d000
      ...
  4e6ba8:  bl       #0x4d9004    <fn 0x4d9004>
  4e6bac:  adrp     x1, #0x1008000
  4e6bb0:  mov      x0, x23
  4e6bb4:  add      x1, x1, #0x358    ; -> 0x1008358 [.rodata] f64 0.0 (0x0000000000000000) hi64 0x00000000003e2d2d
  4e6bb8:  bl       #0x4d9004    <fn 0x4d9004>
  4e6bbc:  mov      x0, x22
  4e6bc0:  mov      x3, x23
      ...
  4e6cd4:  bl       #0x4d9004    <fn 0x4d9004>
  4e6cd8:  adrp     x1, #0x1008000
  4e6cdc:  mov      x0, x19
  4e6ce0:  add      x1, x1, #0x358    ; -> 0x1008358 [.rodata] f64 0.0 (0x0000000000000000) hi64 0x00000000003e2d2d
  4e6ce4:  bl       #0x4d9004    <fn 0x4d9004>
  4e6ce8:  mov      x0, x22
  4e6cec:  mov      x3, x19
      ...
  4e6e68:  fmov     d2, x0
  4e6e6c:  nop      
  4e6e70:  sub      w6, w6, #1
  4e6e74:  fmul     d1, d1, d2
  4e6e78:  cmp      w6, #0x1d
  4e6e7c:  b.ne     #0x4e6e70
  4e6e80:  fmov     d3, #0.50000000
  4e6e84:  fabd     d2, d0, d1
  4e6e88:  str      d1, [sp, #0x10]
  4e6e8c:  fmul     d0, d0, d3
  4e6e90:  fcmpe    d0, d2
  4e6e94:  b.mi     #0x4e6a20
  4e6e98:  b        #0x4e68e8
  4e6e9c:  cmp      w0, #0x1d
      ...
  4e6ea8:  fmov     d1, x1
  4e6eac:  nop      
  4e6eb0:  sub      w0, w0, #1
  4e6eb4:  fmul     d0, d0, d1
  4e6eb8:  cmp      w0, #0x1d
  4e6ebc:  b.ne     #0x4e6eb0
  4e6ec0:  b        #0x4e6890
