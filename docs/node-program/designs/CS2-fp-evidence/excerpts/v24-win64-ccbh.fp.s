; function 0x140429f10-0x14042afc9 plt=['_assert', 'memcmp', 'memcpy']
      ...
140429f23:  lea      rbp, [rsp + 0x80]
140429f2b:  mov      rdi, qword ptr [rbp + 0x510]
140429f32:  mov      r13, rcx
140429f35:  mov      rcx, qword ptr [rip + 0xf5fc54]    ; -> 0x141389b90 [.rdata] f64 2.6635442995e-314 (0x0000000141554190) hi64 0x0000000000000000
140429f3c:  mov      rbx, rdx
140429f3f:  mov      r14, r8
140429f42:  mov      rsi, r9
      ...
140429fa3:  mov      rax, r12
140429fa6:  mov      qword ptr [rbp + 0x328], r15
140429fad:  mov      byte ptr [rax + r15], 0
140429fb2:  mov      rax, qword ptr [rip + 0xf5c7d7]    ; -> 0x141386790 [.rdata] f64 2.661366782e-314 (0x0000000141120160) hi64 0x0000000000000000
140429fb9:  mov      r8, qword ptr [rbp + 0x328]
140429fc0:  cmp      r8, qword ptr [rax + 8]
140429fc4:  je       0x14042a0c0
      ...
14042a067:  call     0x14104b720    <fn 0x14104b720>
14042a06c:  mov      rcx, rdi
14042a06f:  call     0x14104b720    <fn 0x14104b720>
14042a074:  mov      rsi, qword ptr [rip + 0xf5fb15]    ; -> 0x141389b90 [.rdata] f64 2.6635442995e-314 (0x0000000141554190) hi64 0x0000000000000000
14042a07b:  mov      rax, qword ptr [rbp + 0x498]
14042a082:  sub      rax, qword ptr [rsi]
14042a085:  jne      0x14042ac0d
      ...
14042a111:  call     0x1401dd730    <fn 0x1401dd730>
14042a116:  mov      ecx, dword ptr [r13 + 0x48]
14042a11a:  pxor     xmm2, xmm2
14042a11e:  movsd    xmm1, qword ptr [rip + 0xd7077a]    ; -> 0x14119a8a0 [.rdata] f64 65535.0 (0x40efffe000000000) hi64 0x4070000000000000
14042a126:  mov      r8d, ecx
14042a129:  mov      edx, ecx
14042a12b:  movupd   xmm0, xmm1
14042a12f:  and      r8d, 0xffffff
14042a136:  shr      edx, 0x18
14042a139:  cvtsi2sd xmm2, r8d
14042a13e:  divsd    xmm0, xmm2
14042a142:  cmp      ecx, 0x1cffffff
14042a148:  ja       0x14042aa40
14042a14e:  movsd    xmm2, qword ptr [rip + 0xd70752]    ; -> 0x14119a8a8 [.rdata] f64 256.0 (0x4070000000000000) hi64 0x3f70000000000000
14042a156:  test     dl, 1
14042a159:  jne      0x14042a170
14042a15b:  mulsd    xmm0, xmm2
14042a15f:  add      edx, 1
14042a162:  cmp      edx, 0x1d
14042a165:  je       0x14042a180
14042a167:  nop      word ptr [rax + rax]
14042a170:  mulsd    xmm0, xmm2
14042a174:  add      edx, 2
14042a177:  mulsd    xmm0, xmm2
14042a17b:  cmp      edx, 0x1d
14042a17e:  jne      0x14042a170
14042a180:  mov      ecx, eax
      ...
14042a188:  movsd    qword ptr [rbp - 0x28], xmm0
14042a18d:  and      ecx, 0xffffff
14042a193:  shr      edx, 0x18
14042a196:  cvtsi2sd xmm2, ecx
14042a19a:  divsd    xmm1, xmm2
14042a19e:  cmp      eax, 0x1cffffff
14042a1a3:  ja       0x14042a9e0
14042a1a9:  movsd    xmm2, qword ptr [rip + 0xd706f7]    ; -> 0x14119a8a8 [.rdata] f64 256.0 (0x4070000000000000) hi64 0x3f70000000000000
14042a1b1:  test     dl, 1
14042a1b4:  jne      0x14042a1c8
14042a1b6:  mulsd    xmm1, xmm2
14042a1ba:  add      edx, 1
14042a1bd:  cmp      edx, 0x1d
14042a1c0:  je       0x14042a1d8
14042a1c2:  nop      word ptr [rax + rax]
14042a1c8:  mulsd    xmm1, xmm2
14042a1cc:  add      edx, 2
14042a1cf:  mulsd    xmm1, xmm2
14042a1d3:  cmp      edx, 0x1d
14042a1d6:  jne      0x14042a1c8
14042a1d8:  movupd   xmm2, xmm0
14042a1dc:  movsd    qword ptr [rbp - 0x20], xmm1
14042a1e1:  mulsd    xmm0, qword ptr [rip + 0xd70737]    ; -> 0x14119a920 [.rdata] f64 0.5 (0x3fe0000000000000) hi64 0x3fe62e42fefa39ef
14042a1e9:  subsd    xmm2, xmm1
14042a1ed:  andpd    xmm2, xmmword ptr [rip + 0xd7071b]    ; -> 0x14119a910 [.rdata] f64 nan (0x7fffffffffffffff) hi64 0x0000000000000000
14042a1f5:  comisd   xmm2, xmm0
14042a1f9:  ja       0x14042a440
14042a1ff:  cmp      byte ptr [rsi + 0x59a], 0
14042a206:  je       0x14042a320
      ...
14042a2c3:  call     0x14104b720    <fn 0x14104b720>
14042a2c8:  mov      rcx, r13
14042a2cb:  call     0x14104b720    <fn 0x14104b720>
14042a2d0:  lea      rdx, [rip + 0xd6b8d8]    ; -> 0x141195baf [.rdata] f64 5.039370974908412e+223 (0x6e616d2d6e6f6e00) hi64 0x732d79726f746164
14042a2d7:  mov      rcx, rdi
14042a2da:  call     0x14041a0c0    <fn 0x14041a0c0>
14042a2df:  lea      rdx, [rip + 0xd6c31a]    ; -> 0x141196600 [.rdata] str 'bad-fork-prior-to-checkpoint' f64 3.787945678424966e+209 (0x6b726f662d646162) hi64 0x742d726f6972702d
      ...
14042a34b:  mov      rcx, rdi
14042a34e:  lea      r9, [rbp - 8]
14042a352:  lea      r8, [rbp - 0x10]
14042a356:  lea      rdx, [rip + 0xd6c2c3]    ; -> 0x141196620 [.rdata] str "block's timestamp is too early %d %d" f64 2.285710829667507e-152 (0x2073276b636f6c62) hi64 0x6d617473656d6974
14042a35d:  mov      qword ptr [rbp - 8], rax
14042a361:  mov      qword ptr [rbp - 0x10], r14
14042a365:  call     0x140df8020    <fn 0x140df8020>
      ...
14042a5a9:  lea      rdx, [rbp + 0x1a8]
14042a5b0:  mov      rcx, rsi
14042a5b3:  call     0x140f18af0    <fn 0x140f18af0>
14042a5b8:  mov      rax, qword ptr [rip + 0xf5d2c1]    ; -> 0x141387880 [.rdata] f64 2.662710048e-314 (0x00000001413b7db0) hi64 0x0000000000000000
14042a5bf:  mov      rdi, qword ptr [rip + 0xf5d25a]    ; -> 0x141387820 [.rdata] f64 2.6627092574e-314 (0x00000001413b7770) hi64 0x0000000000000000
14042a5c6:  mov      rcx, qword ptr [rbp + 0x1f0]
14042a5cd:  lea      rdx, [rax + 0x40]
14042a5d1:  add      rax, 0x18
      ...
14042a601:  mov      rax, qword ptr [rbp + 0x200]
14042a608:  lea      rdx, [rax + 1]
14042a60c:  call     0x1410e4a70    <fn 0x1410e4a70>
14042a611:  mov      rax, qword ptr [rip + 0xf5d4e8]    ; -> 0x141387b00 [.rdata] f64 2.662712119e-314 (0x00000001413b8e10) hi64 0x0000000000000000
14042a618:  lea      rcx, [rbp + 0x1e0]
14042a61f:  lea      r15, [rbp + 0x70]
14042a623:  add      rax, 0x10
14042a627:  mov      qword ptr [rbp + 0x1a8], rax
14042a62e:  call     0x140ff9080    <fn 0x140ff9080>
14042a633:  mov      rax, qword ptr [rip + 0xf5ce46]    ; -> 0x141387480 [.rdata] f64 2.662680214e-314 (0x00000001413a91d0) hi64 0x0000000000000000
14042a63a:  lea      rcx, [rbp + 0x210]
14042a641:  mov      rdx, qword ptr [rax + 8]
14042a645:  mov      rax, qword ptr [rax + 0x10]
14042a649:  mov      qword ptr [rbp + 0x1a0], rdx
14042a650:  mov      rdx, qword ptr [rdx - 0x18]
14042a654:  mov      qword ptr [rbp + rdx + 0x1a0], rax
14042a65c:  mov      rax, qword ptr [rip + 0xf5d6ed]    ; -> 0x141387d50 [.rdata] f64 2.6627170676e-314 (0x00000001413bb530) hi64 0x0000000000000000
14042a663:  add      rax, 0x10
14042a667:  mov      qword ptr [rbp + 0x210], rax
14042a66e:  call     0x1410b71d0    <fn 0x1410b71d0>
      ...
14042a6f5:  call     0x14104b720    <fn 0x14104b720>
14042a6fa:  mov      rcx, r13
14042a6fd:  call     0x14104b720    <fn 0x14104b720>
14042a702:  lea      rdx, [rip + 0xd6b4a6]    ; -> 0x141195baf [.rdata] f64 5.039370974908412e+223 (0x6e616d2d6e6f6e00) hi64 0x732d79726f746164
14042a709:  mov      rcx, rsi
14042a70c:  call     0x14041a0c0    <fn 0x14041a0c0>
14042a711:  lea      rdx, [rip + 0xd6be7d]    ; -> 0x141196595 [.rdata] str 'bad-diffbits' f64 1.904591567781016e+185 (0x666669642d646162) hi64 0x636e690073746962
      ...
14042a898:  lea      rdx, [rbp + 0x328]
14042a89f:  mov      rcx, r15
14042a8a2:  call     0x140f18af0    <fn 0x140f18af0>
14042a8a7:  mov      rax, qword ptr [rip + 0xf5cfd2]    ; -> 0x141387880 [.rdata] f64 2.662710048e-314 (0x00000001413b7db0) hi64 0x0000000000000000
14042a8ae:  mov      rdi, qword ptr [rip + 0xf5cf6b]    ; -> 0x141387820 [.rdata] f64 2.6627092574e-314 (0x00000001413b7770) hi64 0x0000000000000000
14042a8b5:  mov      rcx, qword ptr [rbp + 0x370]
14042a8bc:  lea      rdx, [rax + 0x40]
14042a8c0:  add      rax, 0x18
      ...
14042a8f0:  mov      rax, qword ptr [rbp + 0x380]
14042a8f7:  lea      rdx, [rax + 1]
14042a8fb:  call     0x1410e4a70    <fn 0x1410e4a70>
14042a900:  mov      rax, qword ptr [rip + 0xf5d1f9]    ; -> 0x141387b00 [.rdata] f64 2.662712119e-314 (0x00000001413b8e10) hi64 0x0000000000000000
14042a907:  lea      rcx, [rbp + 0x360]
14042a90e:  lea      rsi, [rbp + 0x50]
14042a912:  add      rax, 0x10
14042a916:  mov      qword ptr [rbp + 0x328], rax
14042a91d:  call     0x140ff9080    <fn 0x140ff9080>
14042a922:  mov      rax, qword ptr [rip + 0xf5cb57]    ; -> 0x141387480 [.rdata] f64 2.662680214e-314 (0x00000001413a91d0) hi64 0x0000000000000000
14042a929:  lea      rcx, [rbp + 0x390]
14042a930:  mov      rdx, qword ptr [rax + 8]
14042a934:  mov      rax, qword ptr [rax + 0x10]
14042a938:  mov      qword ptr [rbp + 0x320], rdx
14042a93f:  mov      rdx, qword ptr [rdx - 0x18]
14042a943:  mov      qword ptr [rbp + rdx + 0x320], rax
14042a94b:  mov      rax, qword ptr [rip + 0xf5d3fe]    ; -> 0x141387d50 [.rdata] f64 2.6627170676e-314 (0x00000001413bb530) hi64 0x0000000000000000
14042a952:  add      rax, 0x10
14042a956:  mov      qword ptr [rbp + 0x390], rax
14042a95d:  call     0x1410b71d0    <fn 0x1410b71d0>
      ...
14042a9df:  nop      
14042a9e0:  cmp      edx, 0x1d
14042a9e3:  je       0x14042a1d8
14042a9e9:  movsd    xmm2, qword ptr [rip + 0xd6febf]    ; -> 0x14119a8b0 [.rdata] f64 0.00390625 (0x3f70000000000000) hi64 0x40915c0000000000
14042a9f1:  test     dl, 1
14042a9f4:  jne      0x14042aa10
14042a9f6:  mulsd    xmm1, xmm2
14042a9fa:  sub      edx, 1
14042a9fd:  cmp      edx, 0x1d
14042aa00:  je       0x14042a1d8
14042aa06:  nop      word ptr cs:[rax + rax]
14042aa10:  mulsd    xmm1, xmm2
14042aa14:  sub      edx, 2
14042aa17:  mulsd    xmm1, xmm2
14042aa1b:  cmp      edx, 0x1d
14042aa1e:  je       0x14042a1d8
14042aa24:  mulsd    xmm1, xmm2
14042aa28:  sub      edx, 2
14042aa2b:  mulsd    xmm1, xmm2
14042aa2f:  cmp      edx, 0x1d
14042aa32:  jne      0x14042aa10
14042aa34:  jmp      0x14042a1d8
14042aa39:  nop      dword ptr [rax]
14042aa40:  cmp      edx, 0x1d
14042aa43:  je       0x14042a180
14042aa49:  movsd    xmm2, qword ptr [rip + 0xd6fe5f]    ; -> 0x14119a8b0 [.rdata] f64 0.00390625 (0x3f70000000000000) hi64 0x40915c0000000000
14042aa51:  test     dl, 1
14042aa54:  jne      0x14042aa70
14042aa56:  mulsd    xmm0, xmm2
14042aa5a:  sub      edx, 1
14042aa5d:  cmp      edx, 0x1d
14042aa60:  je       0x14042a180
14042aa66:  nop      word ptr cs:[rax + rax]
14042aa70:  mulsd    xmm0, xmm2
14042aa74:  sub      edx, 2
14042aa77:  mulsd    xmm0, xmm2
14042aa7b:  cmp      edx, 0x1d
14042aa7e:  je       0x14042a180
14042aa84:  mulsd    xmm0, xmm2
14042aa88:  sub      edx, 2
14042aa8b:  mulsd    xmm0, xmm2
14042aa8f:  cmp      edx, 0x1d
14042aa92:  jne      0x14042aa70
14042aa94:  jmp      0x14042a180
      ...
14042ab68:  jne      0x14042ab07
14042ab6a:  mov      eax, dword ptr [r13]
14042ab6e:  jmp      0x14042a423
14042ab73:  mov      rbx, qword ptr [rip + 0xf5f016]    ; -> 0x141389b90 [.rdata] f64 2.6635442995e-314 (0x0000000141554190) hi64 0x0000000000000000
14042ab7a:  mov      rax, qword ptr [rbp + 0x498]
14042ab81:  sub      rax, qword ptr [rbx]
14042ab84:  jne      0x14042ac0d
      ...
14042abe9:  call     0x14104b720    <fn 0x14104b720>
14042abee:  mov      rcx, r13
14042abf1:  call     0x14104b720    <fn 0x14104b720>
14042abf6:  mov      rdi, qword ptr [rip + 0xf5ef93]    ; -> 0x141389b90 [.rdata] f64 2.6635442995e-314 (0x0000000141554190) hi64 0x0000000000000000
14042abfd:  mov      rax, qword ptr [rbp + 0x498]
14042ac04:  sub      rax, qword ptr [rdi]
14042ac07:  je       0x14042ae6a
14042ac0d:  call     0x140d81650    <fn 0x140d81650>
14042ac12:  mov      rbx, qword ptr [rip + 0xf5ef77]    ; -> 0x141389b90 [.rdata] f64 2.6635442995e-314 (0x0000000141554190) hi64 0x0000000000000000
14042ac19:  mov      rax, qword ptr [rbp + 0x498]
14042ac20:  sub      rax, qword ptr [rbx]
14042ac23:  jne      0x14042ac0d
14042ac25:  lea      rcx, [rip + 0xd6a16c]    ; -> 0x141194d98 [.rdata] str 'basic_string::_M_create' f64 8.876965363392007e+252 (0x74735f6369736162) hi64 0x4d5f3a3a676e6972
14042ac2c:  call     0x1411198d0    <fn 0x1411198d0>
14042ac31:  mov      rbx, qword ptr [rip + 0xf5ef58]    ; -> 0x141389b90 [.rdata] f64 2.6635442995e-314 (0x0000000141554190) hi64 0x0000000000000000
14042ac38:  mov      rdx, qword ptr [rbp + 0x498]
14042ac3f:  sub      rdx, qword ptr [rbx]
14042ac42:  jne      0x14042ac0d
14042ac44:  mov      ecx, eax
14042ac46:  call     0x141119c80    <fn 0x141119c80>
14042ac4b:  mov      rbx, qword ptr [rip + 0xf5ef3e]    ; -> 0x141389b90 [.rdata] f64 2.6635442995e-314 (0x0000000141554190) hi64 0x0000000000000000
14042ac52:  mov      rdx, qword ptr [rbp + 0x498]
14042ac59:  sub      rdx, qword ptr [rbx]
14042ac5c:  jne      0x14042ac0d
      ...
14042ae46:  call     0x14104b720    <fn 0x14104b720>
14042ae4b:  mov      rcx, rsi
14042ae4e:  call     0x14104b720    <fn 0x14104b720>
14042ae53:  mov      rdi, qword ptr [rip + 0xf5ed36]    ; -> 0x141389b90 [.rdata] f64 2.6635442995e-314 (0x0000000141554190) hi64 0x0000000000000000
14042ae5a:  mov      rax, qword ptr [rbp + 0x498]
14042ae61:  sub      rax, qword ptr [rdi]
14042ae64:  jne      0x14042ac0d
      ...
14042ae97:  call     0x14104b720    <fn 0x14104b720>
14042ae9c:  mov      rcx, rdi
14042ae9f:  call     0x14104b720    <fn 0x14104b720>
14042aea4:  mov      rdi, qword ptr [rip + 0xf5ece5]    ; -> 0x141389b90 [.rdata] f64 2.6635442995e-314 (0x0000000141554190) hi64 0x0000000000000000
14042aeab:  mov      rax, qword ptr [rbp + 0x498]
14042aeb2:  sub      rax, qword ptr [rdi]
14042aeb5:  je       0x14042ae6a
