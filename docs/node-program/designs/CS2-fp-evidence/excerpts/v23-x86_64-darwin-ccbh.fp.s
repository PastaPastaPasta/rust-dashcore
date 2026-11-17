; function 0x100765ba0-0x100766ea0 plt=['Unwind_Resume', 'ZNSt3__112basic_stringIcNS_11char_traitsIcEENS_9allocatorIcEEE6appendEPKc', 'ZNSt3__112basic_stringIcNS_11char_traitsIcEENS_9allocatorIcEEEaSERKS5_', 'ZNSt3__15mutex4lockEv', 'ZNSt3__15mutex6unlockEv', 'ZNSt3__16__sortIRNS_6__lessIxxEEPxEEvT0_S5_T_', 'ZdlPvm', 'Znwm', 'assert_rtn', 'cxa_begin_catch', 'cxa_end_catch', 'memcmp', 'memmove', 'stack_chk_fail'] sym=__ZL26ContextualCheckBlockHeaderRK12CBlockHeaderR20BlockValidationStateRN4node12BlockManagerERK17ChainstateManagerPK11CBlockIndexx
      ...
100765e82:  shr      ecx, 0x18
100765e85:  mov      esi, edx
100765e87:  and      esi, 0xffffff
100765e8d:  cvtsi2sd xmm2, esi
100765e91:  movsd    xmm1, qword ptr [rip + 0x73ef67]    ; -> 0x100ea4e00 [__literals] f64 65535.0 (0x40efffe000000000) hi64 0x3f70000000000000
100765e99:  movapd   xmm0, xmm1
100765e9d:  divsd    xmm0, xmm2
100765ea1:  cmp      edx, 0x1cffffff
100765ea7:  ja       0x100765ee0
100765ea9:  mov      edx, 5
      ...
100765eb3:  je       0x100765f0d
100765eb5:  neg      edx
100765eb7:  xor      esi, esi
100765eb9:  movsd    xmm2, qword ptr [rip + 0x73ef4f]    ; -> 0x100ea4e10 [__literals] f64 256.0 (0x4070000000000000) hi64 0x4052c00000000000
100765ec1:  nop      word ptr cs:[rax + rax]
100765ed0:  mulsd    xmm0, xmm2
100765ed4:  dec      esi
100765ed6:  cmp      edx, esi
100765ed8:  jne      0x100765ed0
      ...
100765ee0:  cmp      edx, 0x1e000000
100765ee6:  jb       0x100765f55
100765ee8:  inc      ecx
100765eea:  movsd    xmm2, qword ptr [rip + 0x73ef16]    ; -> 0x100ea4e08 [__literals] f64 0.00390625 (0x3f70000000000000) hi64 0x4070000000000000
100765ef2:  nop      word ptr cs:[rax + rax]
100765f00:  mulsd    xmm0, xmm2
100765f04:  dec      ecx
100765f06:  cmp      ecx, 0x1e
100765f09:  ja       0x100765f00
      ...
100765f12:  cmp      ecx, 7
100765f15:  jb       0x100765f55
100765f17:  add      edx, -0x1d
100765f1a:  movsd    xmm2, qword ptr [rip + 0x73eeee]    ; -> 0x100ea4e10 [__literals] f64 256.0 (0x4070000000000000) hi64 0x4052c00000000000
100765f22:  nop      word ptr cs:[rax + rax]
100765f30:  mulsd    xmm0, xmm2
100765f34:  mulsd    xmm0, xmm2
100765f38:  mulsd    xmm0, xmm2
100765f3c:  mulsd    xmm0, xmm2
100765f40:  mulsd    xmm0, xmm2
100765f44:  mulsd    xmm0, xmm2
100765f48:  mulsd    xmm0, xmm2
100765f4c:  mulsd    xmm0, xmm2
100765f50:  add      edx, 8
100765f53:  jne      0x100765f30
100765f55:  movsd    qword ptr [rbp - 0xf0], xmm0
      ...
100765f62:  mov      edx, eax
100765f64:  and      edx, 0xffffff
100765f6a:  xorps    xmm2, xmm2
100765f6d:  cvtsi2sd xmm2, edx
100765f71:  divsd    xmm1, xmm2
100765f75:  cmp      eax, 0x1cffffff
100765f7a:  ja       0x100765fb0
100765f7c:  mov      eax, 5
      ...
100765f86:  je       0x100765fdd
100765f88:  neg      eax
100765f8a:  xor      edx, edx
100765f8c:  movsd    xmm2, qword ptr [rip + 0x73ee7c]    ; -> 0x100ea4e10 [__literals] f64 256.0 (0x4070000000000000) hi64 0x4052c00000000000
100765f94:  nop      word ptr cs:[rax + rax]
100765fa0:  mulsd    xmm1, xmm2
100765fa4:  dec      edx
100765fa6:  cmp      eax, edx
100765fa8:  jne      0x100765fa0
      ...
100765fb0:  cmp      eax, 0x1e000000
100765fb5:  jb       0x100766025
100765fb7:  inc      ecx
100765fb9:  movsd    xmm2, qword ptr [rip + 0x73ee47]    ; -> 0x100ea4e08 [__literals] f64 0.00390625 (0x3f70000000000000) hi64 0x4070000000000000
100765fc1:  nop      word ptr cs:[rax + rax]
100765fd0:  mulsd    xmm1, xmm2
100765fd4:  dec      ecx
100765fd6:  cmp      ecx, 0x1e
100765fd9:  ja       0x100765fd0
      ...
100765fe2:  cmp      ecx, 7
100765fe5:  jb       0x100766025
100765fe7:  add      eax, -0x1d
100765fea:  movsd    xmm2, qword ptr [rip + 0x73ee1e]    ; -> 0x100ea4e10 [__literals] f64 256.0 (0x4070000000000000) hi64 0x4052c00000000000
100765ff2:  nop      word ptr cs:[rax + rax]
100766000:  mulsd    xmm1, xmm2
100766004:  mulsd    xmm1, xmm2
100766008:  mulsd    xmm1, xmm2
10076600c:  mulsd    xmm1, xmm2
100766010:  mulsd    xmm1, xmm2
100766014:  mulsd    xmm1, xmm2
100766018:  mulsd    xmm1, xmm2
10076601c:  mulsd    xmm1, xmm2
100766020:  add      eax, 8
100766023:  jne      0x100766000
100766025:  movsd    qword ptr [rbp - 0xf8], xmm1
10076602d:  movapd   xmm2, xmm0
100766031:  subsd    xmm2, xmm1
100766035:  andpd    xmm2, xmmword ptr [rip + 0x73e173]    ; -> 0x100ea41b0 [__literals] f64 nan (0x7fffffffffffffff) hi64 0x7fffffffffffffff
10076603d:  mulsd    xmm0, qword ptr [rip + 0x73ed0b]    ; -> 0x100ea4d50 [__literals] f64 0.5 (0x3fe0000000000000) hi64 0x3fa0000000000000
100766045:  ucomisd  xmm2, xmm0
100766049:  ja       0x1007663a0
10076604f:  cmp      byte ptr [rip + 0x968ceb], 1    ; -> 0x1010ced41 [__data]
100766056:  jne      0x100766295
