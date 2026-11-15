; function 0x45fc40-0x46078f plt=['__assert_fail', '__stack_chk_fail', 'memmove', 'pow']
      ...
  45fe32:  test     esi, esi
  45fe34:  je       0x460440
  45fe3a:  pxor     xmm0, xmm0
  45fe3e:  movsd    xmm1, qword ptr [rip + 0xd4c2ca]    ; -> 0x11ac110 [.rodata] f64 9.223372036854776e+18 (0x43e0000000000000) hi64 0x000000000000012b
  45fe46:  cvtsi2sd xmm0, r8
  45fe4b:  mulsd    xmm0, qword ptr [rip + 0xd4c3d5]    ; -> 0x11ac228 [.rodata] f64 0.025 (0x3f9999999999999a) hi64 0xbff3a5e353f7ced9
  45fe53:  comisd   xmm0, xmm1
  45fe57:  jae      0x4604f9
  45fe5d:  cvttsd2si rax, xmm0
  45fe62:  xor      edx, edx
  45fe64:  div      rdi
  45fe67:  mov      rcx, rax
      ...
  45fef5:  mov      rbx, qword ptr [rdi + 0x1e8]
  45fefc:  mov      eax, dword ptr [rax + 0x78]
  45feff:  movdqa   xmm7, xmmword ptr [rbp - 0x490]
  45ff07:  movsd    xmm2, qword ptr [rip + 0xd4c281]    ; -> 0x11ac190 [.rodata] f64 1.0 (0x3ff0000000000000) hi64 0xffffffff00000000
  45ff0f:  movaps   xmmword ptr [rbp - 0x480], xmm6
  45ff16:  imul     rbx, r12
  45ff1a:  sub      rax, rdx
      ...
  45ff38:  je       0x45ff50
  45ff3a:  pxor     xmm2, xmm2
  45ff3e:  pxor     xmm0, xmm0
  45ff42:  cvtsi2sd xmm2, rbx
  45ff47:  cvtsi2sd xmm0, r13
  45ff4c:  divsd    xmm2, xmm0
  45ff50:  test     r12, r12
  45ff53:  js       0x460510
  45ff59:  pxor     xmm0, xmm0
  45ff5d:  cvtsi2sd xmm0, r12
  45ff62:  movsd    xmm1, qword ptr [rip + 0xd4c2c6]    ; -> 0x11ac230 [.rodata] f64 -1.228 (0xbff3a5e353f7ced9) hi64 0x403c333333333333
  45ff6a:  movsd    qword ptr [rbp - 0x550], xmm2
  45ff72:  divsd    xmm0, qword ptr [rip + 0xd4c2be]    ; -> 0x11ac238 [.rodata] f64 28.2 (0x403c333333333333) hi64 0x3fe6ab367a0f9097
  45ff7a:  call     0x7b330    <pow@plt>
  45ff7f:  mov      rax, qword ptr [rbp - 0x570]
  45ff86:  cmp      r12, rax
  45ff89:  jb       0x45ffbf
  45ff8b:  mulsd    xmm0, qword ptr [rip + 0xd4c2ad]    ; -> 0x11ac240 [.rodata] f64 0.7084 (0x3fe6ab367a0f9097) hi64 0x4024000000000000
  45ff93:  movsd    xmm1, qword ptr [rip + 0xd4c1f5]    ; -> 0x11ac190 [.rodata] f64 1.0 (0x3ff0000000000000) hi64 0xffffffff00000000
  45ff9b:  movsd    xmm2, qword ptr [rbp - 0x550]
  45ffa3:  addsd    xmm0, xmm1
  45ffa7:  divsd    xmm1, xmm0
  45ffab:  comisd   xmm1, xmm2
  45ffaf:  jae      0x460470
  45ffb5:  comisd   xmm2, xmm0
  45ffb9:  jae      0x460470
  45ffbf:  mov      r15, qword ptr [r15 + 8]
  45ffc3:  test     r15, r15
      ...
  4604ec:  mov      rdi, rbx
  4604ef:  call     0xc54c40    <fn 0xc54c40>
  4604f4:  jmp      0x45fdb2
  4604f9:  subsd    xmm0, xmm1
  4604fd:  cvttsd2si rax, xmm0
  460502:  btc      rax, 0x3f
  460507:  jmp      0x45fe62
  46050c:  nop      dword ptr [rax]
      ...
  46051a:  shr      rax, 1
  46051d:  and      ecx, 1
  460520:  or       rax, rcx
  460523:  cvtsi2sd xmm0, rax
  460528:  addsd    xmm0, xmm0
  46052c:  jmp      0x45ff62
  460531:  movdqa   xmm1, xmmword ptr [rbp - 0x4a0]
  460539:  pcmpeqd  xmm0, xmm0
