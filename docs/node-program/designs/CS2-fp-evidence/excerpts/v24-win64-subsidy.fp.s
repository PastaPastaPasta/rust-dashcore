; function 0x14041cb50-0x14041d07d plt=['_assert', 'memcmp', 'memcpy']
      ...
14041cb5c:  sub      rsp, 0x78
14041cb60:  lea      rbp, [rsp + 0x60]
14041cb65:  movups   xmmword ptr [rbp], xmm6
14041cb69:  mov      rdi, qword ptr [rip + 0xf6d020]    ; -> 0x141389b90 [.rdata] f64 2.6635442995e-314 (0x0000000141554190) hi64 0x0000000000000000
14041cb70:  mov      r12d, dword ptr [rbp + 0x80]
14041cb77:  mov      rax, qword ptr [rdi]
14041cb7a:  mov      qword ptr [rbp - 8], rax
      ...
14041cb97:  jle      0x14041cd20
14041cb9d:  mov      edx, r15d
14041cba0:  pxor     xmm0, xmm0
14041cba4:  movsd    xmm6, qword ptr [rip + 0xd7dcf4]    ; -> 0x14119a8a0 [.rdata] f64 65535.0 (0x40efffe000000000) hi64 0x4070000000000000
14041cbac:  mov      eax, r15d
14041cbaf:  and      edx, 0xffffff
14041cbb5:  shr      eax, 0x18
14041cbb8:  cvtsi2sd xmm0, edx
14041cbbc:  divsd    xmm6, xmm0
14041cbc0:  cmp      r15d, 0x1cffffff
14041cbc7:  ja       0x14041cf09
14041cbcd:  movsd    xmm0, qword ptr [rip + 0xd7dcd3]    ; -> 0x14119a8a8 [.rdata] f64 256.0 (0x4070000000000000) hi64 0x3f70000000000000
14041cbd5:  test     al, 1
14041cbd7:  jne      0x14041cbe8
14041cbd9:  mulsd    xmm6, xmm0
14041cbdd:  add      eax, 1
14041cbe0:  cmp      eax, 0x1d
14041cbe3:  je       0x14041cbf8
14041cbe5:  nop      dword ptr [rax]
14041cbe8:  mulsd    xmm6, xmm0
14041cbec:  add      eax, 2
14041cbef:  mulsd    xmm6, xmm0
14041cbf3:  cmp      eax, 0x1d
14041cbf6:  jne      0x14041cbe8
14041cbf8:  call     0x140890e60    <fn 0x140890e60>
      ...
14041cc2c:  mov      rax, r14
14041cc2f:  mov      qword ptr [rbp - 0x28], r15
14041cc33:  mov      byte ptr [rax + r15], 0
14041cc38:  mov      rax, qword ptr [rip + 0xf69b61]    ; -> 0x1413867a0 [.rdata] f64 2.6613667506e-314 (0x0000000141120120) hi64 0x0000000000000000
14041cc3f:  mov      r8, qword ptr [rbp - 0x28]
14041cc43:  mov      r15, qword ptr [rbp - 0x30]
14041cc47:  cmp      r8, qword ptr [rax + 8]
      ...
14041cd25:  lea      rcx, [rbp - 0x30]
14041cd29:  lea      rdx, [rax + 0x390]
14041cd30:  call     0x14104efd0    <fn 0x14104efd0>
14041cd35:  mov      rax, qword ptr [rip + 0xf69a54]    ; -> 0x141386790 [.rdata] f64 2.661366782e-314 (0x0000000141120160) hi64 0x0000000000000000
14041cd3c:  mov      r8, qword ptr [rbp - 0x28]
14041cd40:  mov      rcx, qword ptr [rbp - 0x30]
14041cd44:  cmp      r8, qword ptr [rax + 8]
      ...
14041ce5b:  jle      0x14041cec0
14041ce5d:  cmp      r13d, 0x5dbf
14041ce64:  jg       0x14041ce74
14041ce66:  movsd    xmm0, qword ptr [rip + 0xd7da52]    ; -> 0x14119a8c0 [.rdata] f64 75.0 (0x4052c00000000000) hi64 0x4049800000000000
14041ce6e:  comisd   xmm0, xmm6
14041ce72:  jae      0x14041cec0
14041ce74:  addsd    xmm6, qword ptr [rip + 0xd7da64]    ; -> 0x14119a8e0 [.rdata] f64 2600.0 (0x40a4500000000000) hi64 0x4022000000000000
14041ce7c:  divsd    xmm6, qword ptr [rip + 0xd7da64]    ; -> 0x14119a8e8 [.rdata] f64 9.0 (0x4022000000000000) hi64 0x4140f44700000000
14041ce84:  mulsd    xmm6, xmm6
14041ce88:  movsd    xmm0, qword ptr [rip + 0xd7da60]    ; -> 0x14119a8f0 [.rdata] f64 2222222.0 (0x4140f44700000000) hi64 0x000000003f800000
14041ce90:  mov      eax, 5
14041ce95:  divsd    xmm0, xmm6
14041ce99:  cvttsd2si rcx, xmm0
14041ce9e:  cmp      rcx, rax
14041cea1:  cmovl    rcx, rax
14041cea5:  mov      eax, 0x19
      ...
14041ceb1:  imul     rcx, rcx, 0x5f5e100
14041ceb8:  jmp      0x14041cc82
14041cebd:  nop      dword ptr [rax]
14041cec0:  addsd    xmm6, qword ptr [rip + 0xd7da00]    ; -> 0x14119a8c8 [.rdata] f64 51.0 (0x4049800000000000) hi64 0x4018000000000000
14041cec8:  divsd    xmm6, qword ptr [rip + 0xd7da00]    ; -> 0x14119a8d0 [.rdata] f64 6.0 (0x4018000000000000) hi64 0x40c5b38000000000
14041ced0:  mulsd    xmm6, xmm6
14041ced4:  movsd    xmm0, qword ptr [rip + 0xd7d9fc]    ; -> 0x14119a8d8 [.rdata] f64 11111.0 (0x40c5b38000000000) hi64 0x40a4500000000000
14041cedc:  mov      eax, 0x19
14041cee1:  divsd    xmm0, xmm6
14041cee5:  cvttsd2si rcx, xmm0
14041ceea:  cmp      rcx, rax
14041ceed:  cmovl    rcx, rax
14041cef1:  mov      eax, 0x1f4
      ...
14041cf04:  jmp      0x14041cc82
14041cf09:  cmp      eax, 0x1d
14041cf0c:  je       0x14041cbf8
14041cf12:  movsd    xmm0, qword ptr [rip + 0xd7d996]    ; -> 0x14119a8b0 [.rdata] f64 0.00390625 (0x3f70000000000000) hi64 0x40915c0000000000
14041cf1a:  test     al, 1
14041cf1c:  jne      0x14041cf30
14041cf1e:  mulsd    xmm6, xmm0
14041cf22:  sub      eax, 1
14041cf25:  cmp      eax, 0x1d
14041cf28:  je       0x14041cbf8
14041cf2e:  nop      
14041cf30:  mulsd    xmm6, xmm0
14041cf34:  sub      eax, 2
14041cf37:  mulsd    xmm6, xmm0
14041cf3b:  cmp      eax, 0x1d
14041cf3e:  je       0x14041cbf8
14041cf44:  mulsd    xmm6, xmm0
14041cf48:  sub      eax, 2
14041cf4b:  mulsd    xmm6, xmm0
14041cf4f:  cmp      eax, 0x1d
14041cf52:  jne      0x14041cf30
14041cf54:  jmp      0x14041cbf8
14041cf59:  nop      dword ptr [rax]
14041cf60:  addsd    xmm6, qword ptr [rip + 0xd7d930]    ; -> 0x14119a898 [.rdata] f64 1.0 (0x3ff0000000000000) hi64 0x40efffe000000000
14041cf68:  movsd    xmm0, qword ptr [rip + 0xd7d948]    ; -> 0x14119a8b8 [.rdata] f64 1111.0 (0x40915c0000000000) hi64 0x4052c00000000000
14041cf70:  mov      eax, 1
14041cf75:  mulsd    xmm6, xmm6
14041cf79:  divsd    xmm0, xmm6
14041cf7d:  cvttsd2si rcx, xmm0
14041cf82:  test     rcx, rcx
14041cf85:  cmovle   rcx, rax
14041cf89:  mov      eax, 0x1f4
      ...
14041cfd6:  call     0x1410e4a70    <fn 0x1410e4a70>
14041cfdb:  and      r15d, 0xffffff
14041cfe2:  pxor     xmm0, xmm0
14041cfe6:  movsd    xmm6, qword ptr [rip + 0xd7d8b2]    ; -> 0x14119a8a0 [.rdata] f64 65535.0 (0x40efffe000000000) hi64 0x4070000000000000
14041cfee:  cvtsi2sd xmm0, r15d
14041cff3:  divsd    xmm6, xmm0
14041cff7:  jmp      0x14041cbf8
14041cffc:  mov      r11d, dword ptr [rbx + 0x28c]
14041d003:  mov      eax, r11d
