; function 0x1401dd730-0x1401de384 plt=['_assert', 'memmove']
      ...
1401dd756:  movups   xmmword ptr [rbp + 0x4f0], xmm7
1401dd75d:  movups   xmmword ptr [rbp + 0x500], xmm8
1401dd765:  movups   xmmword ptr [rbp + 0x510], xmm9
1401dd76d:  mov      rdi, qword ptr [rip + 0x11ac41c]    ; -> 0x141389b90 [.rdata] f64 2.6635442995e-314 (0x0000000141554190) hi64 0x0000000000000000
1401dd774:  mov      rax, qword ptr [rdi]
1401dd777:  mov      qword ptr [rbp + 0x4d8], rax
1401dd77e:  xor      eax, eax
      ...
1401dd8b8:  xor      edx, edx
1401dd8ba:  mov      rcx, rdi
1401dd8bd:  call     0x1409ce6c0    <fn 0x1409ce6c0>
1401dd8c2:  mov      rsi, qword ptr [rip + 0x11ac2c7]    ; -> 0x141389b90 [.rdata] f64 2.6635442995e-314 (0x0000000141554190) hi64 0x0000000000000000
1401dd8c9:  mov      rdx, qword ptr [rbp + 0x4d8]
1401dd8d0:  sub      rdx, qword ptr [rsi]
1401dd8d3:  jne      0x1401de324
      ...
1401dd958:  test     ecx, ecx
1401dd95a:  je       0x1401de079
1401dd960:  pxor     xmm0, xmm0
1401dd964:  movsd    xmm1, qword ptr [rip + 0xf7c014]    ; -> 0x141159980 [.rdata] f64 9.223372036854776e+18 (0x43e0000000000000) hi64 0xbff3a5e353f7ced9
1401dd96c:  cvtsi2sd xmm0, r10
1401dd971:  mulsd    xmm0, qword ptr [rip + 0xf7bfff]    ; -> 0x141159978 [.rdata] f64 0.025 (0x3f9999999999999a) hi64 0x43e0000000000000
1401dd979:  comisd   xmm0, xmm1
1401dd97d:  jae      0x1401de09a
1401dd983:  cvttsd2si rax, xmm0
1401dd988:  xor      edx, edx
1401dd98a:  div      r15
1401dd98d:  mov      qword ptr [rbp - 0x38], rax
      ...
1401dd9c8:  mov      qword ptr [rbp - 0x40], rax
1401dd9cc:  mov      rbx, r9
1401dd9cf:  mov      r13, r8
1401dd9d2:  movsd    xmm8, qword ptr [rip + 0xf7bf95]    ; -> 0x141159970 [.rdata] f64 1.0 (0x3ff0000000000000) hi64 0x3f9999999999999a
1401dd9db:  div      r15
1401dd9de:  movsd    xmm7, qword ptr [rip + 0xf7bfa2]    ; -> 0x141159988 [.rdata] f64 -1.228 (0xbff3a5e353f7ced9) hi64 0x403c333333333333
1401dd9e6:  movsd    xmm6, qword ptr [rip + 0xf7bfa2]    ; -> 0x141159990 [.rdata] f64 28.2 (0x403c333333333333) hi64 0x3fe6ab367a0f9097
1401dd9ee:  mov      qword ptr [rbp - 0x50], rax
1401dd9f2:  jmp      0x1401ddada
1401dd9f7:  nop      word ptr [rax + rax]
      ...
1401dda4e:  je       0x1401dda68
1401dda50:  pxor     xmm9, xmm9
1401dda55:  pxor     xmm0, xmm0
1401dda59:  cvtsi2sd xmm9, r14
1401dda5e:  cvtsi2sd xmm0, rdi
1401dda63:  divsd    xmm9, xmm0
1401dda68:  test     rsi, rsi
1401dda6b:  js       0x1401de0b0
1401dda71:  pxor     xmm0, xmm0
1401dda75:  cvtsi2sd xmm0, rsi
1401dda7a:  movupd   xmm1, xmm7
1401dda7e:  divsd    xmm0, xmm6
1401dda82:  call     0x140d90ff0    <fn 0x140d90ff0>
1401dda87:  mov      rax, qword ptr [rbp - 0x38]
1401dda8b:  cmp      rsi, rax
1401dda8e:  jb       0x1401ddabe
1401dda90:  mulsd    xmm0, qword ptr [rip + 0xf7bf00]    ; -> 0x141159998 [.rdata] f64 0.7084 (0x3fe6ab367a0f9097) hi64 0x2e6e696168632f2e
1401dda98:  movsd    xmm1, qword ptr [rip + 0xf7bed0]    ; -> 0x141159970 [.rdata] f64 1.0 (0x3ff0000000000000) hi64 0x3f9999999999999a
1401ddaa0:  addsd    xmm0, xmm1
1401ddaa4:  divsd    xmm1, xmm0
1401ddaa8:  comisd   xmm1, xmm9
1401ddaad:  jae      0x1401ddfaf
1401ddab3:  comisd   xmm9, xmm0
1401ddab8:  jae      0x1401ddfaf
1401ddabe:  mov      rbx, qword ptr [rbx + 8]
1401ddac2:  test     rbx, rbx
      ...
1401de08d:  mov      rcx, rsi
1401de090:  call     0x1409ce6c0    <fn 0x1409ce6c0>
1401de095:  jmp      0x1401dd8c2
1401de09a:  subsd    xmm0, xmm1
1401de09e:  cvttsd2si rax, xmm0
1401de0a3:  btc      rax, 0x3f
1401de0a8:  jmp      0x1401dd988
1401de0ad:  nop      dword ptr [rax]
      ...
1401de0ba:  shr      rax, 1
1401de0bd:  and      ecx, 1
1401de0c0:  or       rax, rcx
1401de0c3:  cvtsi2sd xmm0, rax
1401de0c8:  addsd    xmm0, xmm0
1401de0cc:  jmp      0x1401dda7a
1401de0d1:  movdqu   xmm1, xmmword ptr [rbp + 0x70]
1401de0d6:  movdqu   xmm5, xmmword ptr [rbp + 0x90]
      ...
1401de317:  lea      rcx, [rip + 0xf7b5fa]    ; -> 0x141159918 [.rdata] str 'pindexLast != nullptr' f64 5.003336424426132e+160 (0x614c7865646e6970) hi64 0x756e203d21207473
1401de31e:  call     qword ptr [rip + 0x13790c4]    <_assert@plt>
1401de324:  call     0x140d81650    <fn 0x140d81650>
1401de329:  mov      rdi, qword ptr [rip + 0x11ab860]    ; -> 0x141389b90 [.rdata] f64 2.6635442995e-314 (0x0000000141554190) hi64 0x0000000000000000
1401de330:  mov      rax, qword ptr [rbp + 0x4d8]
1401de337:  sub      rax, qword ptr [rdi]
1401de33a:  jne      0x1401de324
      ...
1401de342:  lea      rdx, [rip + 0xf7b5c7]    ; -> 0x141159910 [.rdata] str 'pow.cpp' f64 1.4631207581263282e-306 (0x007070632e776f70) hi64 0x614c7865646e6970
1401de349:  lea      rcx, [rip + 0xf7b5fc]    ; -> 0x14115994c [.rdata] str 'pblock != nullptr' f64 4.012838790912533e-149 (0x21206b636f6c6270) hi64 0x74706c6c756e203d
1401de350:  call     qword ptr [rip + 0x1379092]    <_assert@plt>
1401de356:  mov      rdi, qword ptr [rip + 0x11ab833]    ; -> 0x141389b90 [.rdata] f64 2.6635442995e-314 (0x0000000141554190) hi64 0x0000000000000000
1401de35d:  mov      rax, qword ptr [rbp + 0x4d8]
1401de364:  sub      rax, qword ptr [rdi]
1401de367:  jne      0x1401de324
