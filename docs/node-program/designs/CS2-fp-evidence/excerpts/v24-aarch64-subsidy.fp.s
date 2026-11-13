; function 0x587db0-0x5882e0 plt=['__assert_fail', '__stack_chk_fail', 'memcmp', 'memcpy']
      ...
  587e04:  mov      x0, #0xffe000000000
  587e08:  movk     x0, #0x40ef, lsl #48
  587e0c:  fmov     d8, x0
  587e10:  scvtf    d0, w1
  587e14:  lsr      w0, w20, #0x18
  587e18:  fdiv     d8, d8, d0
  587e1c:  cmp      w0, #0x1c
  587e20:  b.hi     #0x588070
  587e24:  mov      x1, #0x4070000000000000
  587e28:  fmov     d0, x1
  587e2c:  nop      
  587e30:  add      w0, w0, #1
  587e34:  fmul     d8, d8, d0
  587e38:  cmp      w0, #0x1d
  587e3c:  b.ne     #0x587e30
  587e40:  bl       #0xa87e20    <fn 0xa87e20>
      ...
  588078:  mov      x1, #0x3f70000000000000
  58807c:  fmov     d0, x1
  588080:  sub      w0, w0, #1
  588084:  fmul     d8, d8, d0
  588088:  cmp      w0, #0x1d
  58808c:  b.ne     #0x588080
  588090:  b        #0x587e40
      ...
  5880b8:  mov      x0, #0xc00000000000
  5880bc:  movk     x0, #0x4052, lsl #48
  5880c0:  fmov     d0, x0
  5880c4:  fccmpe   d8, d0, #2, le
  5880c8:  cset     w0, ls
  5880cc:  cbnz     w0, #0x588148
  5880d0:  mov      x0, #0x500000000000
      ...
  5880dc:  fmov     d0, x0
  5880e0:  mov      x0, #0xf44700000000
  5880e4:  mov      x3, #5
  5880e8:  fadd     d1, d8, d0
  5880ec:  movk     x0, #0x4140, lsl #48
  5880f0:  mov      x2, #0x19
  5880f4:  fmov     d0, x0
  5880f8:  fdiv     d1, d1, d2
  5880fc:  mov      x1, #0xe100
  588100:  movk     x1, #0x5f5, lsl #16
  588104:  fmul     d1, d1, d1
  588108:  fdiv     d0, d0, d1
  58810c:  fcvtzs   x0, d0
  588110:  cmp      x0, x3
  588114:  csel     x0, x0, x3, ge
  588118:  cmp      x0, x2
      ...
  588154:  fmov     d0, x0
  588158:  mov      x0, #0xb38000000000
  58815c:  mov      x3, #0x19
  588160:  fadd     d1, d8, d0
  588164:  movk     x0, #0x40c5, lsl #48
  588168:  mov      x2, #0x1f4
  58816c:  fmov     d0, x0
      ...
  588178:  mov      x0, #0x5c0000000000
  58817c:  movk     x0, #0x4091, lsl #48
  588180:  fmov     d0, x0
  588184:  fadd     d8, d8, d1
  588188:  mov      x2, #0x1f4
  58818c:  mov      x1, #0xe100
  588190:  movk     x1, #0x5f5, lsl #16
  588194:  fmul     d8, d8, d8
  588198:  fdiv     d0, d0, d8
  58819c:  fcvtzs   x0, d0
  5881a0:  cmp      x0, #0
  5881a4:  csinc    x0, x0, xzr, gt
  5881a8:  cmp      x0, x2
      ...
  5881ec:  mov      x2, #0xffe000000000
  5881f0:  movk     x2, #0x40ef, lsl #48
  5881f4:  fmov     d8, x2
  5881f8:  scvtf    d0, w0
  5881fc:  fdiv     d8, d8, d0
  588200:  b        #0x587e40
  588204:  cmp      x20, x23
  588208:  b.ne     #0x588010
