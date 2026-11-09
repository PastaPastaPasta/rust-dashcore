; function 0x100836f40-0x1008380e0 plt=['Unwind_Resume', 'ZNSt3__112basic_stringIcNS_11char_traitsIcEENS_9allocatorIcEEE6appendEPKc', 'ZNSt3__15mutex4lockEv', 'ZNSt3__15mutex6unlockEv', 'ZNSt3__16__sortIRNS_6__lessIxxEEPxEEvT0_S5_T_', 'ZdlPvm', 'Znwm', 'assert_rtn', 'cxa_begin_catch', 'cxa_end_catch', 'memcmp', 'stack_chk_fail'] sym=__ZL26ContextualCheckBlockHeaderRK12CBlockHeaderR20BlockValidationStateRN4node12BlockManagerERK17ChainstateManagerPK11CBlockIndex
      ...
100837091:  shr      ecx, 0x18
100837094:  mov      esi, edx
100837096:  and      esi, 0xffffff
10083709c:  cvtsi2sd xmm2, esi
1008370a0:  movsd    xmm1, qword ptr [rip + 0x78cd30]    ; -> 0x100fc3dd8 [__literals] f64 65535.0 (0x40efffe000000000) hi64 0x3f70000000000000
1008370a8:  movapd   xmm0, xmm1
1008370ac:  divsd    xmm0, xmm2
1008370b0:  cmp      edx, 0x1cffffff
1008370b6:  ja       0x10083717c
1008370bc:  mov      edx, 5
      ...
1008370c6:  je       0x10083719d
1008370cc:  neg      edx
1008370ce:  xor      esi, esi
1008370d0:  movsd    xmm2, qword ptr [rip + 0x78cd10]    ; -> 0x100fc3de8 [__literals] f64 256.0 (0x4070000000000000) hi64 0x4052c00000000000
1008370d8:  nop      dword ptr [rax + rax]
1008370e0:  mulsd    xmm0, xmm2
1008370e4:  dec      esi
1008370e6:  cmp      edx, esi
1008370e8:  jne      0x1008370e0
      ...
10083717c:  cmp      edx, 0x1e000000
100837182:  jb       0x1008371e5
100837184:  inc      ecx
100837186:  movsd    xmm2, qword ptr [rip + 0x78cc52]    ; -> 0x100fc3de0 [__literals] f64 0.00390625 (0x3f70000000000000) hi64 0x4070000000000000
10083718e:  nop      
100837190:  mulsd    xmm0, xmm2
100837194:  dec      ecx
100837196:  cmp      ecx, 0x1e
100837199:  ja       0x100837190
      ...
1008371a2:  cmp      ecx, 7
1008371a5:  jb       0x1008371e5
1008371a7:  add      edx, -0x1d
1008371aa:  movsd    xmm2, qword ptr [rip + 0x78cc36]    ; -> 0x100fc3de8 [__literals] f64 256.0 (0x4070000000000000) hi64 0x4052c00000000000
1008371b2:  nop      word ptr cs:[rax + rax]
1008371c0:  mulsd    xmm0, xmm2
1008371c4:  mulsd    xmm0, xmm2
1008371c8:  mulsd    xmm0, xmm2
1008371cc:  mulsd    xmm0, xmm2
1008371d0:  mulsd    xmm0, xmm2
1008371d4:  mulsd    xmm0, xmm2
1008371d8:  mulsd    xmm0, xmm2
1008371dc:  mulsd    xmm0, xmm2
1008371e0:  add      edx, 8
1008371e3:  jne      0x1008371c0
1008371e5:  movsd    qword ptr [rbp - 0xf0], xmm0
      ...
1008371f2:  mov      edx, eax
1008371f4:  and      edx, 0xffffff
1008371fa:  xorps    xmm2, xmm2
1008371fd:  cvtsi2sd xmm2, edx
100837201:  divsd    xmm1, xmm2
100837205:  cmp      eax, 0x1cffffff
10083720a:  ja       0x100837240
10083720c:  mov      eax, 5
      ...
100837216:  je       0x10083726d
100837218:  neg      eax
10083721a:  xor      edx, edx
10083721c:  movsd    xmm2, qword ptr [rip + 0x78cbc4]    ; -> 0x100fc3de8 [__literals] f64 256.0 (0x4070000000000000) hi64 0x4052c00000000000
100837224:  nop      word ptr cs:[rax + rax]
100837230:  mulsd    xmm1, xmm2
100837234:  dec      edx
100837236:  cmp      eax, edx
100837238:  jne      0x100837230
      ...
100837240:  cmp      eax, 0x1e000000
100837245:  jb       0x1008372b5
100837247:  inc      ecx
100837249:  movsd    xmm2, qword ptr [rip + 0x78cb8f]    ; -> 0x100fc3de0 [__literals] f64 0.00390625 (0x3f70000000000000) hi64 0x4070000000000000
100837251:  nop      word ptr cs:[rax + rax]
100837260:  mulsd    xmm1, xmm2
100837264:  dec      ecx
100837266:  cmp      ecx, 0x1e
100837269:  ja       0x100837260
      ...
100837272:  cmp      ecx, 7
100837275:  jb       0x1008372b5
100837277:  add      eax, -0x1d
10083727a:  movsd    xmm2, qword ptr [rip + 0x78cb66]    ; -> 0x100fc3de8 [__literals] f64 256.0 (0x4070000000000000) hi64 0x4052c00000000000
100837282:  nop      word ptr cs:[rax + rax]
100837290:  mulsd    xmm1, xmm2
100837294:  mulsd    xmm1, xmm2
100837298:  mulsd    xmm1, xmm2
10083729c:  mulsd    xmm1, xmm2
1008372a0:  mulsd    xmm1, xmm2
1008372a4:  mulsd    xmm1, xmm2
1008372a8:  mulsd    xmm1, xmm2
1008372ac:  mulsd    xmm1, xmm2
1008372b0:  add      eax, 8
1008372b3:  jne      0x100837290
1008372b5:  movsd    qword ptr [rbp - 0xf8], xmm1
1008372bd:  movapd   xmm2, xmm0
1008372c1:  subsd    xmm2, xmm1
1008372c5:  andpd    xmm2, xmmword ptr [rip + 0x78b143]    ; -> 0x100fc2410 [__literals] f64 nan (0x7fffffffffffffff) hi64 0x7fffffffffffffff
1008372cd:  mulsd    xmm0, qword ptr [rip + 0x78ca43]    ; -> 0x100fc3d18 [__literals] f64 0.5 (0x3fe0000000000000) hi64 0x3fc999999999999a
1008372d5:  ucomisd  xmm2, xmm0
1008372d9:  ja       0x100837659
1008372df:  cmp      byte ptr [r13 + 0x2e2], 1
1008372e7:  jne      0x100837515
