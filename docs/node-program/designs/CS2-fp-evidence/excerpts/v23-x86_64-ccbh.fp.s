; function 0x653950-0x6545bd plt=['__assert_fail', '__stack_chk_fail', 'memcmp', 'pthread_mutex_lock', 'pthread_mutex_unlock']
      ...
  653c08:  call     0x45fc40    <fn 0x45fc40>
  653c0d:  mov      ecx, dword ptr [r12 + 0x48]
  653c12:  pxor     xmm2, xmm2
  653c16:  movsd    xmm1, qword ptr [rip + 0xb586ba]    ; -> 0x11ac2d8 [.rodata] f64 65535.0 (0x40efffe000000000) hi64 0x4070000000000000
  653c1e:  mov      esi, ecx
  653c20:  mov      edx, ecx
  653c22:  movapd   xmm0, xmm1
  653c26:  and      esi, 0xffffff
  653c2c:  shr      edx, 0x18
  653c2f:  cvtsi2sd xmm2, esi
  653c33:  divsd    xmm0, xmm2
  653c37:  cmp      ecx, 0x1cffffff
  653c3d:  ja       0x6543c0
  653c43:  movsd    xmm2, qword ptr [rip + 0xb58695]    ; -> 0x11ac2e0 [.rodata] f64 256.0 (0x4070000000000000) hi64 0x3f70000000000000
  653c4b:  test     dl, 1
  653c4e:  jne      0x653c60
  653c50:  mulsd    xmm0, xmm2
  653c54:  add      edx, 1
  653c57:  cmp      edx, 0x1d
  653c5a:  je       0x653c70
  653c5c:  nop      dword ptr [rax]
  653c60:  mulsd    xmm0, xmm2
  653c64:  add      edx, 2
  653c67:  mulsd    xmm0, xmm2
  653c6b:  cmp      edx, 0x1d
  653c6e:  jne      0x653c60
  653c70:  mov      ecx, eax
      ...
  653c78:  movsd    qword ptr [rbp - 0x4f8], xmm0
  653c80:  and      ecx, 0xffffff
  653c86:  shr      edx, 0x18
  653c89:  cvtsi2sd xmm2, ecx
  653c8d:  divsd    xmm1, xmm2
  653c91:  cmp      eax, 0x1cffffff
  653c96:  ja       0x654360
  653c9c:  movsd    xmm2, qword ptr [rip + 0xb5863c]    ; -> 0x11ac2e0 [.rodata] f64 256.0 (0x4070000000000000) hi64 0x3f70000000000000
  653ca4:  test     dl, 1
  653ca7:  jne      0x653cb8
  653ca9:  mulsd    xmm1, xmm2
  653cad:  add      edx, 1
  653cb0:  cmp      edx, 0x1d
  653cb3:  je       0x653cc8
  653cb5:  nop      dword ptr [rax]
  653cb8:  mulsd    xmm1, xmm2
  653cbc:  add      edx, 2
  653cbf:  mulsd    xmm1, xmm2
  653cc3:  cmp      edx, 0x1d
  653cc6:  jne      0x653cb8
  653cc8:  movapd   xmm2, xmm0
  653ccc:  movsd    qword ptr [rbp - 0x4f0], xmm1
  653cd4:  mulsd    xmm0, qword ptr [rip + 0xb584c4]    ; -> 0x11ac1a0 [.rodata] f64 0.5 (0x3fe0000000000000) hi64 0x408f400000000000
  653cdc:  subsd    xmm2, xmm1
  653ce0:  andpd    xmm2, xmmword ptr [rip + 0xb53748]    ; -> 0x11a7430 [.rodata] f64 nan (0x7fffffffffffffff) hi64 0x0000000000000000
  653ce8:  comisd   xmm2, xmm0
  653cec:  jbe      0x653a1a
  653cf2:  lea      r12, [rbp - 0x4a0]
  653cf9:  lea      rsi, [rip + 0xb350e9]    ; -> 0x1188de9 [.rodata] str 'validation.cpp' f64 9.750135703467247e+199 (0x69746164696c6176) hi64 0x6d007070632e6e6f
      ...
  653f2b:  call     0x2288d0    <fn 0x2288d0>
  653f30:  mov      rdi, r12
  653f33:  call     0x2288d0    <fn 0x2288d0>
  653f38:  lea      rsi, [rip + 0xb463d4]    ; -> 0x119a313 [.rodata] f64 2.169165637389679e+59 (0x4c4147454c4c4900) hi64 0x4150204853414820
  653f3f:  mov      rdi, r15
  653f42:  call     0x647c90    <fn 0x647c90>
  653f47:  lea      rsi, [rip + 0xb35174]    ; -> 0x11890c2 [.rodata] str 'bad-diffbits' f64 1.904591567781016e+185 (0x666669642d646162) hi64 0x636e690073746962
      ...
  653fa1:  call     0x4a5d60    <fn 0x4a5d60>
  653fa6:  mov      rdx, qword ptr [rbp - 0x508]
  653fad:  mov      rdi, rbx
  653fb0:  lea      rsi, [rip + 0xac8421]    ; -> 0x111c3d8 [.rodata] str "block's timestamp is too early %d %d" f64 2.285710829667507e-152 (0x2073276b636f6c62) hi64 0x6d617473656d6974
  653fb7:  lea      rcx, [rbp - 0x4d8]
  653fbe:  mov      qword ptr [rbp - 0x4d8], rax
  653fc5:  mov      qword ptr [rbp - 0x4e0], rdx
      ...
  654279:  call     0x2288d0    <fn 0x2288d0>
  65427e:  mov      rdi, r12
  654281:  call     0x2288d0    <fn 0x2288d0>
  654286:  lea      rsi, [rip + 0xb46086]    ; -> 0x119a313 [.rodata] f64 2.169165637389679e+59 (0x4c4147454c4c4900) hi64 0x4150204853414820
  65428d:  mov      rdi, rbx
  654290:  call     0x647c90    <fn 0x647c90>
  654295:  lea      rsi, [rip + 0xb34e51]    ; -> 0x11890ed [.rodata] str 'bad-fork-prior-to-checkpoint' f64 3.787945678424966e+209 (0x6b726f662d646162) hi64 0x742d726f6972702d
      ...
  65435c:  nop      dword ptr [rax]
  654360:  cmp      edx, 0x1d
  654363:  je       0x653cc8
  654369:  movsd    xmm2, qword ptr [rip + 0xb57f77]    ; -> 0x11ac2e8 [.rodata] f64 0.00390625 (0x3f70000000000000) hi64 0xffffffff00000001
  654371:  test     dl, 1
  654374:  jne      0x654390
  654376:  mulsd    xmm1, xmm2
  65437a:  sub      edx, 1
  65437d:  cmp      edx, 0x1d
  654380:  je       0x653cc8
  654386:  nop      word ptr cs:[rax + rax]
  654390:  mulsd    xmm1, xmm2
  654394:  sub      edx, 2
  654397:  mulsd    xmm1, xmm2
  65439b:  cmp      edx, 0x1d
  65439e:  je       0x653cc8
  6543a4:  mulsd    xmm1, xmm2
  6543a8:  sub      edx, 2
  6543ab:  mulsd    xmm1, xmm2
  6543af:  cmp      edx, 0x1d
  6543b2:  jne      0x654390
  6543b4:  jmp      0x653cc8
  6543b9:  nop      dword ptr [rax]
  6543c0:  cmp      edx, 0x1d
  6543c3:  je       0x653c70
  6543c9:  movsd    xmm2, qword ptr [rip + 0xb57f17]    ; -> 0x11ac2e8 [.rodata] f64 0.00390625 (0x3f70000000000000) hi64 0xffffffff00000001
  6543d1:  test     dl, 1
  6543d4:  jne      0x6543f0
  6543d6:  mulsd    xmm0, xmm2
  6543da:  sub      edx, 1
  6543dd:  cmp      edx, 0x1d
  6543e0:  je       0x653c70
  6543e6:  nop      word ptr cs:[rax + rax]
  6543f0:  mulsd    xmm0, xmm2
  6543f4:  sub      edx, 2
  6543f7:  mulsd    xmm0, xmm2
  6543fb:  cmp      edx, 0x1d
  6543fe:  je       0x653c70
  654404:  mulsd    xmm0, xmm2
  654408:  sub      edx, 2
  65440b:  mulsd    xmm0, xmm2
  65440f:  cmp      edx, 0x1d
  654412:  jne      0x6543f0
  654414:  jmp      0x653c70
