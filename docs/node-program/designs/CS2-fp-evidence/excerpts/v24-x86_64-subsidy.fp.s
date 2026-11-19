; function 0x719e10-0x71a387 plt=['__assert_fail', '__stack_chk_fail', 'memcmp', 'memcpy']
      ...
  719e46:  jle      0x719fe8
  719e4c:  mov      edx, r15d
  719e4f:  pxor     xmm1, xmm1
  719e53:  movsd    xmm0, qword ptr [rip + 0xc271dd]    ; -> 0x1341038 [.rodata] f64 65535.0 (0x40efffe000000000) hi64 0x4070000000000000
  719e5b:  mov      eax, r15d
  719e5e:  and      edx, 0xffffff
  719e64:  shr      eax, 0x18
  719e67:  cvtsi2sd xmm1, edx
  719e6b:  divsd    xmm0, xmm1
  719e6f:  cmp      r15d, 0x1cffffff
  719e76:  ja       0x71a209
  719e7c:  movsd    xmm1, qword ptr [rip + 0xc271bc]    ; -> 0x1341040 [.rodata] f64 256.0 (0x4070000000000000) hi64 0x3f70000000000000
  719e84:  test     al, 1
  719e86:  jne      0x719e98
  719e88:  mulsd    xmm0, xmm1
  719e8c:  add      eax, 1
  719e8f:  cmp      eax, 0x1d
  719e92:  je       0x719ea8
  719e94:  nop      dword ptr [rax]
  719e98:  mulsd    xmm0, xmm1
  719e9c:  add      eax, 2
  719e9f:  mulsd    xmm0, xmm1
  719ea3:  cmp      eax, 0x1d
  719ea6:  jne      0x719e98
  719ea8:  movsd    qword ptr [rbp - 0x68], xmm0
      ...
  71a15a:  jle      0x71a1c0
  71a15c:  cmp      r13d, 0x5dbf
  71a163:  jg       0x71a173
  71a165:  movsd    xmm1, qword ptr [rip + 0xc26eeb]    ; -> 0x1341058 [.rodata] f64 75.0 (0x4052c00000000000) hi64 0x4049800000000000
  71a16d:  comisd   xmm1, xmm0
  71a171:  jae      0x71a1c0
  71a173:  addsd    xmm0, qword ptr [rip + 0xc26efd]    ; -> 0x1341078 [.rodata] f64 2600.0 (0x40a4500000000000) hi64 0x4140f44700000000
  71a17b:  divsd    xmm0, qword ptr [rip + 0xc26e5d]    ; -> 0x1340fe0 [.rodata] f64 9.0 (0x4022000000000000) hi64 0x800b800a80098008
  71a183:  mulsd    xmm0, xmm0
  71a187:  movsd    xmm1, qword ptr [rip + 0xc26ef1]    ; -> 0x1341080 [.rodata] f64 2222222.0 (0x4140f44700000000) hi64 0xffffffff00000001
  71a18f:  mov      eax, 5
  71a194:  divsd    xmm1, xmm0
  71a198:  cvttsd2si rcx, xmm1
  71a19d:  cmp      rcx, rax
  71a1a0:  cmovl    rcx, rax
  71a1a4:  mov      eax, 0x19
      ...
  71a1b0:  imul     rcx, rcx, 0x5f5e100
  71a1b7:  jmp      0x719f45
  71a1bc:  nop      dword ptr [rax]
  71a1c0:  addsd    xmm0, qword ptr [rip + 0xc26e98]    ; -> 0x1341060 [.rodata] f64 51.0 (0x4049800000000000) hi64 0x4018000000000000
  71a1c8:  divsd    xmm0, qword ptr [rip + 0xc26e98]    ; -> 0x1341068 [.rodata] f64 6.0 (0x4018000000000000) hi64 0x40c5b38000000000
  71a1d0:  mulsd    xmm0, xmm0
  71a1d4:  movsd    xmm1, qword ptr [rip + 0xc26e94]    ; -> 0x1341070 [.rodata] f64 11111.0 (0x40c5b38000000000) hi64 0x40a4500000000000
  71a1dc:  mov      eax, 0x19
  71a1e1:  divsd    xmm1, xmm0
  71a1e5:  cvttsd2si rcx, xmm1
  71a1ea:  cmp      rcx, rax
  71a1ed:  cmovl    rcx, rax
  71a1f1:  mov      eax, 0x1f4
      ...
  71a204:  jmp      0x719f45
  71a209:  cmp      eax, 0x1d
  71a20c:  je       0x719ea8
  71a212:  movsd    xmm1, qword ptr [rip + 0xc26e2e]    ; -> 0x1341048 [.rodata] f64 0.00390625 (0x3f70000000000000) hi64 0x40915c0000000000
  71a21a:  test     al, 1
  71a21c:  jne      0x71a230
  71a21e:  mulsd    xmm0, xmm1
  71a222:  sub      eax, 1
  71a225:  cmp      eax, 0x1d
  71a228:  je       0x719ea8
  71a22e:  nop      
  71a230:  mulsd    xmm0, xmm1
  71a234:  sub      eax, 2
  71a237:  mulsd    xmm0, xmm1
  71a23b:  cmp      eax, 0x1d
  71a23e:  je       0x719ea8
  71a244:  mulsd    xmm0, xmm1
  71a248:  sub      eax, 2
  71a24b:  mulsd    xmm0, xmm1
  71a24f:  cmp      eax, 0x1d
  71a252:  jne      0x71a230
  71a254:  jmp      0x719ea8
  71a259:  nop      dword ptr [rax]
  71a260:  addsd    xmm0, qword ptr [rip + 0xc26cc0]    ; -> 0x1340f28 [.rodata] f64 1.0 (0x3ff0000000000000) hi64 0x3fe0000000000000
  71a268:  movsd    xmm1, qword ptr [rip + 0xc26de0]    ; -> 0x1341050 [.rodata] f64 1111.0 (0x40915c0000000000) hi64 0x4052c00000000000
  71a270:  mov      eax, 1
  71a275:  mulsd    xmm0, xmm0
  71a279:  divsd    xmm1, xmm0
  71a27d:  cvttsd2si rcx, xmm1
  71a282:  test     rcx, rcx
  71a285:  cmovle   rcx, rax
  71a289:  mov      eax, 0x1f4
      ...
  71a2da:  call     0x1184da0    <fn 0x1184da0>
  71a2df:  and      r15d, 0xffffff
  71a2e6:  pxor     xmm1, xmm1
  71a2ea:  movsd    xmm0, qword ptr [rip + 0xc26d46]    ; -> 0x1341038 [.rodata] f64 65535.0 (0x40efffe000000000) hi64 0x4070000000000000
  71a2f2:  cvtsi2sd xmm1, r15d
  71a2f7:  divsd    xmm0, xmm1
  71a2fb:  jmp      0x719ea8
  71a300:  mov      r9d, dword ptr [rbx + 0x28c]
  71a307:  mov      eax, r9d
