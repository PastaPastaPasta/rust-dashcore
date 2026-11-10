; function 0x725620-0x7264fb plt=['__assert_fail', '__stack_chk_fail', 'memcmp', 'memcpy', 'pthread_mutex_lock', 'pthread_mutex_unlock']
      ...
  725820:  call     0x4ec8b0    <fn 0x4ec8b0>
  725825:  mov      ecx, dword ptr [r12 + 0x48]
  72582a:  pxor     xmm2, xmm2
  72582e:  movsd    xmm1, qword ptr [rip + 0xc1b802]    ; -> 0x1341038 [.rodata] f64 65535.0 (0x40efffe000000000) hi64 0x4070000000000000
  725836:  mov      esi, ecx
  725838:  mov      edx, ecx
  72583a:  movapd   xmm0, xmm1
  72583e:  and      esi, 0xffffff
  725844:  shr      edx, 0x18
  725847:  cvtsi2sd xmm2, esi
  72584b:  divsd    xmm0, xmm2
  72584f:  cmp      ecx, 0x1cffffff
  725855:  ja       0x726260
  72585b:  movsd    xmm2, qword ptr [rip + 0xc1b7dd]    ; -> 0x1341040 [.rodata] f64 256.0 (0x4070000000000000) hi64 0x3f70000000000000
  725863:  test     dl, 1
  725866:  jne      0x725878
  725868:  mulsd    xmm0, xmm2
  72586c:  add      edx, 1
  72586f:  cmp      edx, 0x1d
  725872:  je       0x725888
  725874:  nop      dword ptr [rax]
  725878:  mulsd    xmm0, xmm2
  72587c:  add      edx, 2
  72587f:  mulsd    xmm0, xmm2
  725883:  cmp      edx, 0x1d
  725886:  jne      0x725878
  725888:  mov      ecx, eax
      ...
  725890:  movsd    qword ptr [rbp - 0x4f8], xmm0
  725898:  and      ecx, 0xffffff
  72589e:  shr      edx, 0x18
  7258a1:  cvtsi2sd xmm2, ecx
  7258a5:  divsd    xmm1, xmm2
  7258a9:  cmp      eax, 0x1cffffff
  7258ae:  ja       0x726200
  7258b4:  movsd    xmm2, qword ptr [rip + 0xc1b784]    ; -> 0x1341040 [.rodata] f64 256.0 (0x4070000000000000) hi64 0x3f70000000000000
  7258bc:  test     dl, 1
  7258bf:  jne      0x7258d0
  7258c1:  mulsd    xmm1, xmm2
  7258c5:  add      edx, 1
  7258c8:  cmp      edx, 0x1d
  7258cb:  je       0x7258e0
  7258cd:  nop      dword ptr [rax]
  7258d0:  mulsd    xmm1, xmm2
  7258d4:  add      edx, 2
  7258d7:  mulsd    xmm1, xmm2
  7258db:  cmp      edx, 0x1d
  7258de:  jne      0x7258d0
  7258e0:  movapd   xmm2, xmm0
  7258e4:  movsd    qword ptr [rbp - 0x4f0], xmm1
  7258ec:  mulsd    xmm0, qword ptr [rip + 0xc1b63c]    ; -> 0x1340f30 [.rodata] f64 0.5 (0x3fe0000000000000) hi64 0x408f400000000000
  7258f4:  subsd    xmm2, xmm1
  7258f8:  andpd    xmm2, xmmword ptr [rip + 0xc14a90]    ; -> 0x133a390 [.rodata] f64 nan (0x7fffffffffffffff) hi64 0x0000000000000000
  725900:  comisd   xmm2, xmm0
  725904:  ja       0x725b40
  72590a:  cmp      byte ptr [r13 + 0x59a], 0
  725912:  je       0x7259c0
      ...
  7259d9:  lea      r13, [rbp - 0x330]
  7259e0:  lea      rbx, [rbp - 0x1b0]
  7259e7:  call     0x53ab10    <fn 0x53ab10>
  7259ec:  lea      rsi, [rip + 0xb80ecd]    ; -> 0x12a68c0 [.rodata] str "block's timestamp is too early %d %d" f64 2.285710829667507e-152 (0x2073276b636f6c62) hi64 0x6d617473656d6974
  7259f3:  mov      rdi, r13
  7259f6:  lea      rcx, [rbp - 0x4d8]
  7259fd:  lea      rdx, [rbp - 0x4e0]
      ...
  725e4a:  call     0x258070    <fn 0x258070>
  725e4f:  mov      rdi, r12
  725e52:  call     0x258070    <fn 0x258070>
  725e57:  lea      rsi, [rip + 0xc0846b]    ; -> 0x132e2c9 [.rodata] f64 2.169165637389679e+59 (0x4c4147454c4c4900) hi64 0x4150204853414820
  725e5e:  mov      rdi, rbx
  725e61:  call     0x717a10    <fn 0x717a10>
  725e66:  lea      rsi, [rip + 0xbf644a]    ; -> 0x131c2b7 [.rodata] str 'bad-diffbits' f64 1.904591567781016e+185 (0x666669642d646162) hi64 0x636e690073746962
      ...
  7261ba:  call     0x258070    <fn 0x258070>
  7261bf:  mov      rdi, r12
  7261c2:  call     0x258070    <fn 0x258070>
  7261c7:  lea      rsi, [rip + 0xc080fb]    ; -> 0x132e2c9 [.rodata] f64 2.169165637389679e+59 (0x4c4147454c4c4900) hi64 0x4150204853414820
  7261ce:  mov      rdi, r13
  7261d1:  call     0x717a10    <fn 0x717a10>
  7261d6:  lea      rsi, [rip + 0xbf6105]    ; -> 0x131c2e2 [.rodata] str 'bad-fork-prior-to-checkpoint' f64 3.787945678424966e+209 (0x6b726f662d646162) hi64 0x742d726f6972702d
      ...
  7261fd:  nop      dword ptr [rax]
  726200:  cmp      edx, 0x1d
  726203:  je       0x7258e0
  726209:  movsd    xmm2, qword ptr [rip + 0xc1ae37]    ; -> 0x1341048 [.rodata] f64 0.00390625 (0x3f70000000000000) hi64 0x40915c0000000000
  726211:  test     dl, 1
  726214:  jne      0x726230
  726216:  mulsd    xmm1, xmm2
  72621a:  sub      edx, 1
  72621d:  cmp      edx, 0x1d
  726220:  je       0x7258e0
  726226:  nop      word ptr cs:[rax + rax]
  726230:  mulsd    xmm1, xmm2
  726234:  sub      edx, 2
  726237:  mulsd    xmm1, xmm2
  72623b:  cmp      edx, 0x1d
  72623e:  je       0x7258e0
  726244:  mulsd    xmm1, xmm2
  726248:  sub      edx, 2
  72624b:  mulsd    xmm1, xmm2
  72624f:  cmp      edx, 0x1d
  726252:  jne      0x726230
  726254:  jmp      0x7258e0
  726259:  nop      dword ptr [rax]
  726260:  cmp      edx, 0x1d
  726263:  je       0x725888
  726269:  movsd    xmm2, qword ptr [rip + 0xc1add7]    ; -> 0x1341048 [.rodata] f64 0.00390625 (0x3f70000000000000) hi64 0x40915c0000000000
  726271:  test     dl, 1
  726274:  jne      0x726290
  726276:  mulsd    xmm0, xmm2
  72627a:  sub      edx, 1
  72627d:  cmp      edx, 0x1d
  726280:  je       0x725888
  726286:  nop      word ptr cs:[rax + rax]
  726290:  mulsd    xmm0, xmm2
  726294:  sub      edx, 2
  726297:  mulsd    xmm0, xmm2
  72629b:  cmp      edx, 0x1d
  72629e:  je       0x725888
  7262a4:  mulsd    xmm0, xmm2
  7262a8:  sub      edx, 2
  7262ab:  mulsd    xmm0, xmm2
  7262af:  cmp      edx, 0x1d
  7262b2:  jne      0x726290
  7262b4:  jmp      0x725888
