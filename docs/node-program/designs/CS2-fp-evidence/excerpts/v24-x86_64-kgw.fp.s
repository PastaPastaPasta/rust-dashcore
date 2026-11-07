; function 0x4ec8b0-0x4ed3ff plt=['__assert_fail', '__stack_chk_fail', 'memmove', 'pow']
      ...
  4ecaa2:  test     esi, esi
  4ecaa4:  je       0x4ed0b0
  4ecaaa:  pxor     xmm0, xmm0
  4ecaae:  movsd    xmm1, qword ptr [rip + 0xe543e2]    ; -> 0x1340e98 [.rodata] f64 9.223372036854776e+18 (0x43e0000000000000) hi64 0x0000000100000001
  4ecab6:  cvtsi2sd xmm0, r8
  4ecabb:  mulsd    xmm0, qword ptr [rip + 0xe544e5]    ; -> 0x1340fa8 [.rodata] f64 0.025 (0x3f9999999999999a) hi64 0xbff3a5e353f7ced9
  4ecac3:  comisd   xmm0, xmm1
  4ecac7:  jae      0x4ed169
  4ecacd:  cvttsd2si rax, xmm0
  4ecad2:  xor      edx, edx
  4ecad4:  div      rdi
  4ecad7:  mov      rcx, rax
      ...
  4ecb65:  mov      rbx, qword ptr [rdi + 0x230]
  4ecb6c:  mov      eax, dword ptr [rax + 0x78]
  4ecb6f:  movdqa   xmm7, xmmword ptr [rbp - 0x490]
  4ecb77:  movsd    xmm2, qword ptr [rip + 0xe543a9]    ; -> 0x1340f28 [.rodata] f64 1.0 (0x3ff0000000000000) hi64 0x3fe0000000000000
  4ecb7f:  movaps   xmmword ptr [rbp - 0x480], xmm6
  4ecb86:  imul     rbx, r12
  4ecb8a:  sub      rax, rdx
      ...
  4ecba8:  je       0x4ecbc0
  4ecbaa:  pxor     xmm2, xmm2
  4ecbae:  pxor     xmm0, xmm0
  4ecbb2:  cvtsi2sd xmm2, rbx
  4ecbb7:  cvtsi2sd xmm0, r13
  4ecbbc:  divsd    xmm2, xmm0
  4ecbc0:  test     r12, r12
  4ecbc3:  js       0x4ed180
  4ecbc9:  pxor     xmm0, xmm0
  4ecbcd:  cvtsi2sd xmm0, r12
  4ecbd2:  movsd    xmm1, qword ptr [rip + 0xe543d6]    ; -> 0x1340fb0 [.rodata] f64 -1.228 (0xbff3a5e353f7ced9) hi64 0x403c333333333333
  4ecbda:  movsd    qword ptr [rbp - 0x550], xmm2
  4ecbe2:  divsd    xmm0, qword ptr [rip + 0xe543ce]    ; -> 0x1340fb8 [.rodata] f64 28.2 (0x403c333333333333) hi64 0x3fe6ab367a0f9097
  4ecbea:  call     0x7e330    <pow@plt>
  4ecbef:  mov      rax, qword ptr [rbp - 0x570]
  4ecbf6:  cmp      r12, rax
  4ecbf9:  jb       0x4ecc2f
  4ecbfb:  mulsd    xmm0, qword ptr [rip + 0xe543bd]    ; -> 0x1340fc0 [.rodata] f64 0.7084 (0x3fe6ab367a0f9097) hi64 0x4024000000000000
  4ecc03:  movsd    xmm1, qword ptr [rip + 0xe5431d]    ; -> 0x1340f28 [.rodata] f64 1.0 (0x3ff0000000000000) hi64 0x3fe0000000000000
  4ecc0b:  movsd    xmm2, qword ptr [rbp - 0x550]
  4ecc13:  addsd    xmm0, xmm1
  4ecc17:  divsd    xmm1, xmm0
  4ecc1b:  comisd   xmm1, xmm2
  4ecc1f:  jae      0x4ed0e0
  4ecc25:  comisd   xmm2, xmm0
  4ecc29:  jae      0x4ed0e0
  4ecc2f:  mov      r15, qword ptr [r15 + 8]
  4ecc33:  test     r15, r15
      ...
  4ed15c:  mov      rdi, rbx
  4ed15f:  call     0xdb2c60    <fn 0xdb2c60>
  4ed164:  jmp      0x4eca22
  4ed169:  subsd    xmm0, xmm1
  4ed16d:  cvttsd2si rax, xmm0
  4ed172:  btc      rax, 0x3f
  4ed177:  jmp      0x4ecad2
  4ed17c:  nop      dword ptr [rax]
      ...
  4ed18a:  shr      rax, 1
  4ed18d:  and      ecx, 1
  4ed190:  or       rax, rcx
  4ed193:  cvtsi2sd xmm0, rax
  4ed198:  addsd    xmm0, xmm0
  4ed19c:  jmp      0x4ecbd2
  4ed1a1:  movdqa   xmm1, xmmword ptr [rbp - 0x4a0]
  4ed1a9:  pcmpeqd  xmm0, xmm0
