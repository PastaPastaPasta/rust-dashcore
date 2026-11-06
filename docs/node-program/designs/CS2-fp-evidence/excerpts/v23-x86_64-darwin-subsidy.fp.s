; function 0x10074a450-0x10074aa50 plt=['ZdlPvm', 'Znwm', 'assert_rtn', 'memcmp', 'memmove', 'stack_chk_fail'] sym=__ZL21GetBlockSubsidyHelperiiRKN9Consensus6ParamsEb
      ...
10074a5b1:  mov      ecx, r12d
10074a5b4:  and      ecx, 0xffffff
10074a5ba:  xorps    xmm0, xmm0
10074a5bd:  cvtsi2sd xmm0, ecx
10074a5c1:  shr      eax, 0x18
10074a5c4:  movsd    xmm1, qword ptr [rip + 0x75a834]    ; -> 0x100ea4e00 [__literals] f64 65535.0 (0x40efffe000000000) hi64 0x3f70000000000000
10074a5cc:  divsd    xmm1, xmm0
10074a5d0:  cmp      r12d, 0x1cffffff
10074a5d7:  ja       0x10074a610
10074a5d9:  mov      ecx, 5
      ...
10074a5e3:  je       0x10074a640
10074a5e5:  neg      ecx
10074a5e7:  xor      edx, edx
10074a5e9:  movsd    xmm0, qword ptr [rip + 0x75a81f]    ; -> 0x100ea4e10 [__literals] f64 256.0 (0x4070000000000000) hi64 0x4052c00000000000
10074a5f1:  nop      word ptr cs:[rax + rax]
10074a600:  mulsd    xmm1, xmm0
10074a604:  dec      edx
10074a606:  cmp      ecx, edx
10074a608:  jne      0x10074a600
      ...
10074a610:  cmp      r12d, 0x1e000000
10074a617:  jb       0x10074a6ee
10074a61d:  inc      eax
10074a61f:  movsd    xmm0, qword ptr [rip + 0x75a7e1]    ; -> 0x100ea4e08 [__literals] f64 0.00390625 (0x3f70000000000000) hi64 0x4070000000000000
10074a627:  nop      word ptr [rax + rax]
10074a630:  mulsd    xmm1, xmm0
10074a634:  dec      eax
10074a636:  cmp      eax, 0x1e
10074a639:  ja       0x10074a630
      ...
10074a645:  cmp      eax, 7
10074a648:  jb       0x10074a6ee
10074a64e:  add      ecx, -0x1d
10074a651:  movsd    xmm0, qword ptr [rip + 0x75a7b7]    ; -> 0x100ea4e10 [__literals] f64 256.0 (0x4070000000000000) hi64 0x4052c00000000000
10074a659:  nop      dword ptr [rax]
10074a660:  mulsd    xmm1, xmm0
10074a664:  mulsd    xmm1, xmm0
10074a668:  mulsd    xmm1, xmm0
10074a66c:  mulsd    xmm1, xmm0
10074a670:  mulsd    xmm1, xmm0
10074a674:  mulsd    xmm1, xmm0
10074a678:  mulsd    xmm1, xmm0
10074a67c:  mulsd    xmm1, xmm0
10074a680:  add      ecx, 8
10074a683:  jne      0x10074a660
10074a685:  jmp      0x10074a6ee
      ...
10074a6ce:  call     0x100e18f56    <ZdlPvm@plt>
10074a6d3:  and      r12d, 0xffffff
10074a6da:  xorps    xmm0, xmm0
10074a6dd:  cvtsi2sd xmm0, r12d
10074a6e2:  movsd    xmm1, qword ptr [rip + 0x75a716]    ; -> 0x100ea4e00 [__literals] f64 65535.0 (0x40efffe000000000) hi64 0x3f70000000000000
10074a6ea:  divsd    xmm1, xmm0
10074a6ee:  movsd    qword ptr [rbp - 0x58], xmm1
10074a6f3:  call     0x1009da6b0    <fn 0x1009da6b0 __Z6Paramsv>
10074a6f8:  test     byte ptr [rax + 0x328], 1
      ...
10074a877:  jle      0x10074a95e
10074a87d:  cmp      r15d, 0x1558
10074a884:  jg       0x10074a8c3
10074a886:  addsd    xmm1, qword ptr [rip + 0x75a3f2]    ; -> 0x100ea4c80 [__literals] f64 1.0 (0x3ff0000000000000) hi64 0x3fe51eb851eb851f
10074a88e:  mulsd    xmm1, xmm1
10074a892:  movsd    xmm0, qword ptr [rip + 0x75a5ae]    ; -> 0x100ea4e48 [__literals] f64 1111.0 (0x40915c0000000000) hi64 0x3fe62e42fefa39ef
10074a89a:  divsd    xmm0, xmm1
10074a89e:  cvttsd2si rcx, xmm0
10074a8a3:  mov      eax, 0x1f4
10074a8a8:  cmp      rcx, 0x1f4
10074a8af:  jg       0x10074a95e
      ...
10074a8ca:  jb       0x10074a8e3
10074a8cc:  cmp      r15d, 0x5dbf
10074a8d3:  ja       0x10074a921
10074a8d5:  movsd    xmm0, qword ptr [rip + 0x75a53b]    ; -> 0x100ea4e18 [__literals] f64 75.0 (0x4052c00000000000) hi64 0x40a4500000000000
10074a8dd:  ucomisd  xmm0, xmm1
10074a8e1:  jb       0x10074a921
10074a8e3:  addsd    xmm1, qword ptr [rip + 0x75a545]    ; -> 0x100ea4e30 [__literals] f64 51.0 (0x4049800000000000) hi64 0x4018000000000000
10074a8eb:  divsd    xmm1, qword ptr [rip + 0x75a545]    ; -> 0x100ea4e38 [__literals] f64 6.0 (0x4018000000000000) hi64 0x40c5b38000000000
10074a8f3:  mulsd    xmm1, xmm1
10074a8f7:  movsd    xmm0, qword ptr [rip + 0x75a541]    ; -> 0x100ea4e40 [__literals] f64 11111.0 (0x40c5b38000000000) hi64 0x40915c0000000000
10074a8ff:  divsd    xmm0, xmm1
10074a903:  cvttsd2si rcx, xmm0
10074a908:  mov      eax, 0x1f4
10074a90d:  cmp      rcx, 0x1f4
10074a914:  jg       0x10074a95e
10074a916:  cmp      rcx, 0x1a
10074a91a:  mov      eax, 0x19
10074a91f:  jmp      0x10074a95a
10074a921:  addsd    xmm1, qword ptr [rip + 0x75a4f7]    ; -> 0x100ea4e20 [__literals] f64 2600.0 (0x40a4500000000000) hi64 0x4140f44700000000
10074a929:  divsd    xmm1, qword ptr [rip + 0x75a4b7]    ; -> 0x100ea4de8 [__literals] f64 9.0 (0x4022000000000000) hi64 0x3ef0000000000000
10074a931:  mulsd    xmm1, xmm1
10074a935:  movsd    xmm0, qword ptr [rip + 0x75a4eb]    ; -> 0x100ea4e28 [__literals] f64 2222222.0 (0x4140f44700000000) hi64 0x4049800000000000
10074a93d:  divsd    xmm0, xmm1
10074a941:  cvttsd2si rcx, xmm0
10074a946:  mov      eax, 0x19
10074a94b:  cmp      rcx, 0x19
10074a94f:  jg       0x10074a95e
