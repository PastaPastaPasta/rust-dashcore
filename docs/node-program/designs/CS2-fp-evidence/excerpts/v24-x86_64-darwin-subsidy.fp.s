; function 0x10081b0c0-0x10081b580 plt=['ZdlPvm', 'assert_rtn', 'memcmp', 'stack_chk_fail'] sym=__ZL21GetBlockSubsidyHelperiiRKN9Consensus6ParamsEb
      ...
10081b1c0:  je       0x10081b1e2
10081b1c2:  and      r12d, 0xffffff
10081b1c9:  xorps    xmm0, xmm0
10081b1cc:  cvtsi2sd xmm0, r12d
10081b1d1:  movsd    xmm1, qword ptr [rip + 0x7a8bff]    ; -> 0x100fc3dd8 [__literals] f64 65535.0 (0x40efffe000000000) hi64 0x3f70000000000000
10081b1d9:  divsd    xmm1, xmm0
10081b1dd:  jmp      0x10081b2b5
10081b1e2:  mov      eax, r12d
10081b1e5:  mov      ecx, r12d
10081b1e8:  and      ecx, 0xffffff
10081b1ee:  xorps    xmm0, xmm0
10081b1f1:  cvtsi2sd xmm0, ecx
10081b1f5:  shr      eax, 0x18
10081b1f8:  movsd    xmm1, qword ptr [rip + 0x7a8bd8]    ; -> 0x100fc3dd8 [__literals] f64 65535.0 (0x40efffe000000000) hi64 0x3f70000000000000
10081b200:  divsd    xmm1, xmm0
10081b204:  cmp      r12d, 0x1cffffff
10081b20b:  ja       0x10081b240
10081b20d:  mov      ecx, 5
      ...
10081b217:  je       0x10081b26d
10081b219:  neg      ecx
10081b21b:  xor      edx, edx
10081b21d:  movsd    xmm0, qword ptr [rip + 0x7a8bc3]    ; -> 0x100fc3de8 [__literals] f64 256.0 (0x4070000000000000) hi64 0x4052c00000000000
10081b225:  nop      word ptr cs:[rax + rax]
10081b230:  mulsd    xmm1, xmm0
10081b234:  dec      edx
10081b236:  cmp      ecx, edx
10081b238:  jne      0x10081b230
      ...
10081b240:  cmp      r12d, 0x1e000000
10081b247:  jb       0x10081b2b5
10081b249:  inc      eax
10081b24b:  movsd    xmm0, qword ptr [rip + 0x7a8b8d]    ; -> 0x100fc3de0 [__literals] f64 0.00390625 (0x3f70000000000000) hi64 0x4070000000000000
10081b253:  nop      word ptr cs:[rax + rax]
10081b260:  mulsd    xmm1, xmm0
10081b264:  dec      eax
10081b266:  cmp      eax, 0x1e
10081b269:  ja       0x10081b260
      ...
10081b272:  cmp      eax, 7
10081b275:  jb       0x10081b2b5
10081b277:  add      ecx, -0x1d
10081b27a:  movsd    xmm0, qword ptr [rip + 0x7a8b66]    ; -> 0x100fc3de8 [__literals] f64 256.0 (0x4070000000000000) hi64 0x4052c00000000000
10081b282:  nop      word ptr cs:[rax + rax]
10081b290:  mulsd    xmm1, xmm0
10081b294:  mulsd    xmm1, xmm0
10081b298:  mulsd    xmm1, xmm0
10081b29c:  mulsd    xmm1, xmm0
10081b2a0:  mulsd    xmm1, xmm0
10081b2a4:  mulsd    xmm1, xmm0
10081b2a8:  mulsd    xmm1, xmm0
10081b2ac:  mulsd    xmm1, xmm0
10081b2b0:  add      ecx, 8
10081b2b3:  jne      0x10081b290
10081b2b5:  movsd    qword ptr [rbp - 0x58], xmm1
      ...
10081b3ac:  jle      0x10081b493
10081b3b2:  cmp      r15d, 0x1558
10081b3b9:  jg       0x10081b3f8
10081b3bb:  addsd    xmm1, qword ptr [rip + 0x7a887d]    ; -> 0x100fc3c40 [__literals] f64 1.0 (0x3ff0000000000000) hi64 0x3fe51eb851eb851f
10081b3c3:  mulsd    xmm1, xmm1
10081b3c7:  movsd    xmm0, qword ptr [rip + 0x7a8a51]    ; -> 0x100fc3e20 [__literals] f64 1111.0 (0x40915c0000000000) hi64 0x3fe62e42fefa39ef
10081b3cf:  divsd    xmm0, xmm1
10081b3d3:  cvttsd2si rcx, xmm0
10081b3d8:  mov      eax, 0x1f4
10081b3dd:  cmp      rcx, 0x1f4
10081b3e4:  jg       0x10081b493
      ...
10081b3ff:  jb       0x10081b418
10081b401:  cmp      r15d, 0x5dbf
10081b408:  ja       0x10081b456
10081b40a:  movsd    xmm0, qword ptr [rip + 0x7a89de]    ; -> 0x100fc3df0 [__literals] f64 75.0 (0x4052c00000000000) hi64 0x40a4500000000000
10081b412:  ucomisd  xmm0, xmm1
10081b416:  jb       0x10081b456
10081b418:  addsd    xmm1, qword ptr [rip + 0x7a89e8]    ; -> 0x100fc3e08 [__literals] f64 51.0 (0x4049800000000000) hi64 0x4018000000000000
10081b420:  divsd    xmm1, qword ptr [rip + 0x7a89e8]    ; -> 0x100fc3e10 [__literals] f64 6.0 (0x4018000000000000) hi64 0x40c5b38000000000
10081b428:  mulsd    xmm1, xmm1
10081b42c:  movsd    xmm0, qword ptr [rip + 0x7a89e4]    ; -> 0x100fc3e18 [__literals] f64 11111.0 (0x40c5b38000000000) hi64 0x40915c0000000000
10081b434:  divsd    xmm0, xmm1
10081b438:  cvttsd2si rcx, xmm0
10081b43d:  mov      eax, 0x1f4
10081b442:  cmp      rcx, 0x1f4
10081b449:  jg       0x10081b493
10081b44b:  cmp      rcx, 0x1a
10081b44f:  mov      eax, 0x19
10081b454:  jmp      0x10081b48f
10081b456:  addsd    xmm1, qword ptr [rip + 0x7a899a]    ; -> 0x100fc3df8 [__literals] f64 2600.0 (0x40a4500000000000) hi64 0x4140f44700000000
10081b45e:  divsd    xmm1, qword ptr [rip + 0x7a8952]    ; -> 0x100fc3db8 [__literals] f64 9.0 (0x4022000000000000) hi64 0x3ef0000000000000
10081b466:  mulsd    xmm1, xmm1
10081b46a:  movsd    xmm0, qword ptr [rip + 0x7a898e]    ; -> 0x100fc3e00 [__literals] f64 2222222.0 (0x4140f44700000000) hi64 0x4049800000000000
10081b472:  divsd    xmm0, xmm1
10081b476:  cvttsd2si rcx, xmm0
10081b47b:  mov      eax, 0x19
10081b480:  cmp      rcx, 0x19
10081b484:  jg       0x10081b493
