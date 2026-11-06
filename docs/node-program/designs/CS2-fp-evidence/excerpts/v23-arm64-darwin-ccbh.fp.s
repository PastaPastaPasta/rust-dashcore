; function 0x100672d44-0x100673c00 plt=['Unwind_Resume', 'ZNSt3__112basic_stringIcNS_11char_traitsIcEENS_9allocatorIcEEEaSERKS5_', 'ZNSt3__15mutex4lockEv', 'ZNSt3__15mutex6unlockEv', 'ZNSt3__16__sortIRNS_6__lessIxxEEPxEEvT0_S5_T_', 'ZdlPvm', 'Znwm', 'assert_rtn', 'cxa_begin_catch', 'cxa_end_catch', 'memcmp', 'memmove', 'stack_chk_fail'] sym=__ZL26ContextualCheckBlockHeaderRK12CBlockHeaderR20BlockValidationStateRN4node12BlockManagerERK17ChainstateManagerPK11CBlockIndexx
      ...
100672fd4:  ldr      w9, [x20, #0x48]
100672fd8:  lsr      w8, w9, #0x18
100672fdc:  and      w10, w9, #0xffffff
100672fe0:  ucvtf    d0, w10
100672fe4:  mov      x10, #0xffe000000000
100672fe8:  movk     x10, #0x40ef, lsl #48
100672fec:  fmov     d1, x10
100672ff0:  fdiv     d0, d1, d0
100672ff4:  cmp      w8, #0x1c
100672ff8:  b.hi     #0x100673018
100672ffc:  sub      w8, w8, #0x1d
100673000:  mov      x9, #0x4070000000000000
100673004:  fmov     d1, x9
100673008:  fmul     d0, d0, d1
10067300c:  adds     w8, w8, #1
100673010:  b.lo     #0x100673008
100673014:  b        #0x100673040
      ...
100673024:  add      w8, w8, #1
100673028:  mov      x9, #0x3f70000000000000
10067302c:  fmov     d1, x9
100673030:  fmul     d0, d0, d1
100673034:  sub      w8, w8, #1
100673038:  cmp      w8, #0x1e
10067303c:  b.hi     #0x100673030
100673040:  str      d0, [sp, #0x50]
100673044:  lsr      w8, w0, #0x18
100673048:  and      w9, w0, #0xffffff
10067304c:  ucvtf    d1, w9
100673050:  mov      x9, #0xffe000000000
100673054:  movk     x9, #0x40ef, lsl #48
100673058:  fmov     d2, x9
10067305c:  fdiv     d1, d2, d1
100673060:  cmp      w8, #0x1c
100673064:  b.hi     #0x100673084
100673068:  sub      w8, w8, #0x1d
10067306c:  mov      x9, #0x4070000000000000
100673070:  fmov     d2, x9
100673074:  fmul     d1, d1, d2
100673078:  adds     w8, w8, #1
10067307c:  b.lo     #0x100673074
100673080:  b        #0x1006730ac
      ...
100673090:  add      w8, w8, #1
100673094:  mov      x9, #0x3f70000000000000
100673098:  fmov     d2, x9
10067309c:  fmul     d1, d1, d2
1006730a0:  sub      w8, w8, #1
1006730a4:  cmp      w8, #0x1e
1006730a8:  b.hi     #0x10067309c
1006730ac:  str      d1, [sp, #0x48]
1006730b0:  fabd     d8, d0, d1
1006730b4:  fmov     d1, #0.50000000
1006730b8:  fmul     d0, d0, d1
1006730bc:  fcmp     d8, d0
1006730c0:  b.gt     #0x10067336c
1006730c4:  adrp     x8, #0x100f48000
1006730c8:  ldrb     w8, [x8, #0xd41]    ; -> 0x100f48d41 [__data]
      ...
1006730fc:  bl       #0x100c813c0    <Znwm@plt>
100673100:  str      x0, [sp, #0x30]
100673104:  adrp     x8, #0x100e55000
100673108:  ldr      q0, [x8, #0x920]    ; -> 0x100e55920 [__literals] f64 1.3e-322 (0x000000000000001a) hi64 0x8000000000000020
10067310c:  stur     q0, [sp, #0x38]
100673110:  adrp     x8, #0x100e9f000
100673114:  add      x8, x8, #0x875    ; -> 0x100e9f875 [__cstring] str 'ContextualCheckBlockHeader'
      ...
100673220:  bl       #0x100c813c0    <Znwm@plt>
100673224:  str      x0, [sp, #0xc0]
100673228:  adrp     x8, #0x100e55000
10067322c:  ldr      q0, [x8, #0x950]    ; -> 0x100e55950 [__literals] f64 1.4e-322 (0x000000000000001c) hi64 0x8000000000000020
100673230:  stur     q0, [x26, #0x20]
100673234:  adrp     x8, #0x100e9f000
100673238:  add      x8, x8, #0x940    ; -> 0x100e9f940 [__cstring] str 'bad-fork-prior-to-checkpoint'
      ...
100673370:  bl       #0x100c813c0    <Znwm@plt>
100673374:  str      x0, [sp, #0x30]
100673378:  adrp     x8, #0x100e55000
10067337c:  ldr      q0, [x8, #0x920]    ; -> 0x100e55920 [__literals] f64 1.3e-322 (0x000000000000001a) hi64 0x8000000000000020
100673380:  stur     q0, [sp, #0x38]
100673384:  adrp     x8, #0x100e9f000
100673388:  add      x8, x8, #0x875    ; -> 0x100e9f875 [__cstring] str 'ContextualCheckBlockHeader'
