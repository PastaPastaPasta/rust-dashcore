; function 0x100742c88-0x100743a40 plt=['Unwind_Resume', 'ZNSt3__15mutex4lockEv', 'ZNSt3__15mutex6unlockEv', 'ZNSt3__16__sortIRNS_6__lessIxxEEPxEEvT0_S5_T_', 'ZdlPvm', 'Znwm', 'assert_rtn', 'cxa_begin_catch', 'cxa_end_catch', 'memcmp', 'stack_chk_fail'] sym=__ZL26ContextualCheckBlockHeaderRK12CBlockHeaderR20BlockValidationStateRN4node12BlockManagerERK17ChainstateManagerPK11CBlockIndex
      ...
100742db0:  ldr      w9, [x20, #0x48]
100742db4:  lsr      w8, w9, #0x18
100742db8:  and      w10, w9, #0xffffff
100742dbc:  ucvtf    d0, w10
100742dc0:  mov      x10, #0xffe000000000
100742dc4:  movk     x10, #0x40ef, lsl #48
100742dc8:  fmov     d1, x10
100742dcc:  fdiv     d0, d1, d0
100742dd0:  cmp      w8, #0x1c
100742dd4:  b.hi     #0x100742e7c
100742dd8:  sub      w8, w8, #0x1d
100742ddc:  mov      x9, #0x4070000000000000
100742de0:  fmov     d1, x9
100742de4:  fmul     d0, d0, d1
100742de8:  adds     w8, w8, #1
100742dec:  b.lo     #0x100742de4
100742df0:  b        #0x100742ea4
      ...
100742e88:  add      w8, w8, #1
100742e8c:  mov      x9, #0x3f70000000000000
100742e90:  fmov     d1, x9
100742e94:  fmul     d0, d0, d1
100742e98:  sub      w8, w8, #1
100742e9c:  cmp      w8, #0x1e
100742ea0:  b.hi     #0x100742e94
100742ea4:  str      d0, [sp, #0x40]
100742ea8:  lsr      w8, w0, #0x18
100742eac:  and      w9, w0, #0xffffff
100742eb0:  ucvtf    d1, w9
100742eb4:  mov      x9, #0xffe000000000
100742eb8:  movk     x9, #0x40ef, lsl #48
100742ebc:  fmov     d2, x9
100742ec0:  fdiv     d1, d2, d1
100742ec4:  cmp      w8, #0x1c
100742ec8:  b.hi     #0x100742ee8
100742ecc:  sub      w8, w8, #0x1d
100742ed0:  mov      x9, #0x4070000000000000
100742ed4:  fmov     d2, x9
100742ed8:  fmul     d1, d1, d2
100742edc:  adds     w8, w8, #1
100742ee0:  b.lo     #0x100742ed8
100742ee4:  b        #0x100742f10
      ...
100742ef4:  add      w8, w8, #1
100742ef8:  mov      x9, #0x3f70000000000000
100742efc:  fmov     d2, x9
100742f00:  fmul     d1, d1, d2
100742f04:  sub      w8, w8, #1
100742f08:  cmp      w8, #0x1e
100742f0c:  b.hi     #0x100742f00
100742f10:  str      d1, [sp, #0x38]
100742f14:  fabd     d8, d0, d1
100742f18:  fmov     d1, #0.50000000
100742f1c:  fmul     d0, d0, d1
100742f20:  fcmp     d8, d0
100742f24:  b.gt     #0x1007431f8
100742f28:  ldrb     w8, [x22, #0x2e2]
100742f2c:  cmp      w8, #1
      ...
100742f5c:  bl       #0x100d960ec    <Znwm@plt>
100742f60:  str      x0, [sp, #0x20]
100742f64:  adrp     x8, #0x100f7d000
100742f68:  ldr      q0, [x8, #0x290]    ; -> 0x100f7d290 [__literals] f64 1.3e-322 (0x000000000000001a) hi64 0x8000000000000020
100742f6c:  stur     q0, [sp, #0x28]
100742f70:  adrp     x8, #0x100fd4000
100742f74:  add      x8, x8, #0x210    ; -> 0x100fd4210 [__cstring] str 'ContextualCheckBlockHeader'
      ...
100743080:  bl       #0x100d960ec    <Znwm@plt>
100743084:  str      x0, [sp, #0xb0]
100743088:  adrp     x8, #0x100f7d000
10074308c:  ldr      q0, [x8, #0x2c0]    ; -> 0x100f7d2c0 [__literals] f64 1.4e-322 (0x000000000000001c) hi64 0x8000000000000020
100743090:  stur     q0, [x24, #0x20]
100743094:  adrp     x8, #0x100fd4000
100743098:  add      x8, x8, #0x2db    ; -> 0x100fd42db [__cstring] str 'bad-fork-prior-to-checkpoint'
      ...
1007431fc:  bl       #0x100d960ec    <Znwm@plt>
100743200:  str      x0, [sp, #0x20]
100743204:  adrp     x8, #0x100f7d000
100743208:  ldr      q0, [x8, #0x290]    ; -> 0x100f7d290 [__literals] f64 1.3e-322 (0x000000000000001a) hi64 0x8000000000000020
10074320c:  stur     q0, [sp, #0x28]
100743210:  adrp     x8, #0x100fd4000
100743214:  add      x8, x8, #0x210    ; -> 0x100fd4210 [__cstring] str 'ContextualCheckBlockHeader'
