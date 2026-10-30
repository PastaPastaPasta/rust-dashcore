; function 0x140196210-0x140196e64 plt=['_assert', 'memmove']
      ...
140196236:  movups   xmmword ptr [rbp + 0x4f0], xmm7
14019623d:  movups   xmmword ptr [rbp + 0x500], xmm8
140196245:  movups   xmmword ptr [rbp + 0x510], xmm9
14019624d:  mov      rdi, qword ptr [rip + 0x1070a1c]    ; -> 0x141206c70 [.rdata] f64 2.6627069333e-314 (0x00000001413b6510) hi64 0x0000000000000000
140196254:  mov      rax, qword ptr [rdi]
140196257:  mov      qword ptr [rbp + 0x4d8], rax
14019625e:  xor      eax, eax
      ...
140196398:  xor      edx, edx
14019639a:  mov      rcx, rdi
14019639d:  call     0x1408bc480    <fn 0x1408bc480>
1401963a2:  mov      rsi, qword ptr [rip + 0x10708c7]    ; -> 0x141206c70 [.rdata] f64 2.6627069333e-314 (0x00000001413b6510) hi64 0x0000000000000000
1401963a9:  mov      rdx, qword ptr [rbp + 0x4d8]
1401963b0:  sub      rdx, qword ptr [rsi]
1401963b3:  jne      0x140196e04
      ...
140196438:  test     ecx, ecx
14019643a:  je       0x140196b59
140196440:  pxor     xmm0, xmm0
140196444:  movsd    xmm1, qword ptr [rip + 0xe54804]    ; -> 0x140feac50 [.rdata] f64 9.223372036854776e+18 (0x43e0000000000000) hi64 0xbff3a5e353f7ced9
14019644c:  cvtsi2sd xmm0, r10
140196451:  mulsd    xmm0, qword ptr [rip + 0xe547ef]    ; -> 0x140feac48 [.rdata] f64 0.025 (0x3f9999999999999a) hi64 0x43e0000000000000
140196459:  comisd   xmm0, xmm1
14019645d:  jae      0x140196b7a
140196463:  cvttsd2si rax, xmm0
140196468:  xor      edx, edx
14019646a:  div      r15
14019646d:  mov      qword ptr [rbp - 0x38], rax
      ...
1401964a8:  mov      qword ptr [rbp - 0x40], rax
1401964ac:  mov      rbx, r9
1401964af:  mov      r13, r8
1401964b2:  movsd    xmm8, qword ptr [rip + 0xe54785]    ; -> 0x140feac40 [.rdata] f64 1.0 (0x3ff0000000000000) hi64 0x3f9999999999999a
1401964bb:  div      r15
1401964be:  movsd    xmm7, qword ptr [rip + 0xe54792]    ; -> 0x140feac58 [.rdata] f64 -1.228 (0xbff3a5e353f7ced9) hi64 0x403c333333333333
1401964c6:  movsd    xmm6, qword ptr [rip + 0xe54792]    ; -> 0x140feac60 [.rdata] f64 28.2 (0x403c333333333333) hi64 0x3fe6ab367a0f9097
1401964ce:  mov      qword ptr [rbp - 0x50], rax
1401964d2:  jmp      0x1401965ba
1401964d7:  nop      word ptr [rax + rax]
      ...
14019652e:  je       0x140196548
140196530:  pxor     xmm9, xmm9
140196535:  pxor     xmm0, xmm0
140196539:  cvtsi2sd xmm9, r14
14019653e:  cvtsi2sd xmm0, rdi
140196543:  divsd    xmm9, xmm0
140196548:  test     rsi, rsi
14019654b:  js       0x140196b90
140196551:  pxor     xmm0, xmm0
140196555:  cvtsi2sd xmm0, rsi
14019655a:  movupd   xmm1, xmm7
14019655e:  divsd    xmm0, xmm6
140196562:  call     0x140c5d170    <fn 0x140c5d170>
140196567:  mov      rax, qword ptr [rbp - 0x38]
14019656b:  cmp      rsi, rax
14019656e:  jb       0x14019659e
140196570:  mulsd    xmm0, qword ptr [rip + 0xe546f0]    ; -> 0x140feac68 [.rdata] f64 0.7084 (0x3fe6ab367a0f9097) hi64 0x0000000000000000
140196578:  movsd    xmm1, qword ptr [rip + 0xe546c0]    ; -> 0x140feac40 [.rdata] f64 1.0 (0x3ff0000000000000) hi64 0x3f9999999999999a
140196580:  addsd    xmm0, xmm1
140196584:  divsd    xmm1, xmm0
140196588:  comisd   xmm1, xmm9
14019658d:  jae      0x140196a8f
140196593:  comisd   xmm9, xmm0
140196598:  jae      0x140196a8f
14019659e:  mov      rbx, qword ptr [rbx + 8]
1401965a2:  test     rbx, rbx
      ...
140196b6d:  mov      rcx, rsi
140196b70:  call     0x1408bc480    <fn 0x1408bc480>
140196b75:  jmp      0x1401963a2
140196b7a:  subsd    xmm0, xmm1
140196b7e:  cvttsd2si rax, xmm0
140196b83:  btc      rax, 0x3f
140196b88:  jmp      0x140196468
140196b8d:  nop      dword ptr [rax]
      ...
140196b9a:  shr      rax, 1
140196b9d:  and      ecx, 1
140196ba0:  or       rax, rcx
140196ba3:  cvtsi2sd xmm0, rax
140196ba8:  addsd    xmm0, xmm0
140196bac:  jmp      0x14019655a
140196bb1:  movdqu   xmm1, xmmword ptr [rbp + 0x70]
140196bb6:  movdqu   xmm5, xmmword ptr [rbp + 0x90]
      ...
140196df7:  lea      rcx, [rip + 0xe53dea]    ; -> 0x140feabe8 [.rdata] str 'pindexLast != nullptr' f64 5.003336424426132e+160 (0x614c7865646e6970) hi64 0x756e203d21207473
140196dfe:  call     qword ptr [rip + 0x12235ec]    <_assert@plt>
140196e04:  call     0x140c4d7d0    <fn 0x140c4d7d0>
140196e09:  mov      rdi, qword ptr [rip + 0x106fe60]    ; -> 0x141206c70 [.rdata] f64 2.6627069333e-314 (0x00000001413b6510) hi64 0x0000000000000000
140196e10:  mov      rax, qword ptr [rbp + 0x4d8]
140196e17:  sub      rax, qword ptr [rdi]
140196e1a:  jne      0x140196e04
      ...
140196e22:  lea      rdx, [rip + 0xe53db7]    ; -> 0x140feabe0 [.rdata] str 'pow.cpp' f64 1.4631207581263282e-306 (0x007070632e776f70) hi64 0x614c7865646e6970
140196e29:  lea      rcx, [rip + 0xe53dec]    ; -> 0x140feac1c [.rdata] str 'pblock != nullptr' f64 4.012838790912533e-149 (0x21206b636f6c6270) hi64 0x74706c6c756e203d
140196e30:  call     qword ptr [rip + 0x12235ba]    <_assert@plt>
140196e36:  mov      rdi, qword ptr [rip + 0x106fe33]    ; -> 0x141206c70 [.rdata] f64 2.6627069333e-314 (0x00000001413b6510) hi64 0x0000000000000000
140196e3d:  mov      rax, qword ptr [rbp + 0x4d8]
140196e44:  sub      rax, qword ptr [rdi]
140196e47:  jne      0x140196e04
