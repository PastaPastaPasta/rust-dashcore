; function 0x14038dab0-0x14038dfcb plt=['_assert', 'memcmp', 'memcpy']
      ...
14038dabc:  sub      rsp, 0x78
14038dac0:  lea      rbp, [rsp + 0x60]
14038dac5:  movups   xmmword ptr [rbp], xmm6
14038dac9:  mov      rdi, qword ptr [rip + 0xe791a0]    ; -> 0x141206c70 [.rdata] f64 2.6627069333e-314 (0x00000001413b6510) hi64 0x0000000000000000
14038dad0:  mov      r12d, dword ptr [rbp + 0x80]
14038dad7:  mov      rax, qword ptr [rdi]
14038dada:  mov      qword ptr [rbp - 8], rax
      ...
14038daf3:  jle      0x14038dc90
14038daf9:  mov      edx, r14d
14038dafc:  pxor     xmm0, xmm0
14038db00:  movsd    xmm6, qword ptr [rip + 0xc94230]    ; -> 0x141021d38 [.rdata] f64 65535.0 (0x40efffe000000000) hi64 0x4070000000000000
14038db08:  mov      eax, r14d
14038db0b:  and      edx, 0xffffff
14038db11:  shr      eax, 0x18
14038db14:  cvtsi2sd xmm0, edx
14038db18:  divsd    xmm6, xmm0
14038db1c:  cmp      r14d, 0x1cffffff
14038db23:  ja       0x14038de69
14038db29:  movsd    xmm0, qword ptr [rip + 0xc9420f]    ; -> 0x141021d40 [.rdata] f64 256.0 (0x4070000000000000) hi64 0x3f70000000000000
14038db31:  test     al, 1
14038db33:  jne      0x14038db48
14038db35:  mulsd    xmm6, xmm0
14038db39:  add      eax, 1
14038db3c:  cmp      eax, 0x1d
14038db3f:  je       0x14038db58
14038db41:  nop      dword ptr [rax]
14038db48:  mulsd    xmm6, xmm0
14038db4c:  add      eax, 2
14038db4f:  mulsd    xmm6, xmm0
14038db53:  cmp      eax, 0x1d
14038db56:  jne      0x14038db48
14038db58:  call     0x14077d480    <fn 0x14077d480>
      ...
14038db90:  mov      rax, r15
14038db93:  mov      qword ptr [rbp - 0x28], r14
14038db97:  mov      byte ptr [rax + r14], 0
14038db9c:  mov      rax, qword ptr [rip + 0xe75cbd]    ; -> 0x141203860 [.rdata] f64 2.660637668e-314 (0x0000000140fb7cc0) hi64 0x0000000000000000
14038dba3:  mov      r8, qword ptr [rbp - 0x28]
14038dba7:  mov      r14, qword ptr [rbp - 0x30]
14038dbab:  cmp      r8, qword ptr [rax + 8]
      ...
14038dc99:  lea      rdx, [rax + 0x328]
14038dca0:  mov      rcx, r15
14038dca3:  call     0x140ef6bf0    <fn 0x140ef6bf0>
14038dca8:  mov      rax, qword ptr [rip + 0xe75ba1]    ; -> 0x141203850 [.rdata] f64 2.6606376994e-314 (0x0000000140fb7d00) hi64 0x0000000000000000
14038dcaf:  mov      r8, qword ptr [rbp - 0x28]
14038dcb3:  cmp      r8, qword ptr [rax + 8]
14038dcb7:  je       0x14038df08
      ...
14038ddbb:  jle      0x14038de20
14038ddbd:  cmp      r13d, 0x5dbf
14038ddc4:  jg       0x14038ddd4
14038ddc6:  movsd    xmm0, qword ptr [rip + 0xc93f92]    ; -> 0x141021d60 [.rdata] f64 75.0 (0x4052c00000000000) hi64 0x4049800000000000
14038ddce:  comisd   xmm0, xmm6
14038ddd2:  jae      0x14038de20
14038ddd4:  addsd    xmm6, qword ptr [rip + 0xc93fa4]    ; -> 0x141021d80 [.rdata] f64 2600.0 (0x40a4500000000000) hi64 0x4022000000000000
14038dddc:  divsd    xmm6, qword ptr [rip + 0xc93fa4]    ; -> 0x141021d88 [.rdata] f64 9.0 (0x4022000000000000) hi64 0x4140f44700000000
14038dde4:  mulsd    xmm6, xmm6
14038dde8:  movsd    xmm0, qword ptr [rip + 0xc93fa0]    ; -> 0x141021d90 [.rdata] f64 2222222.0 (0x4140f44700000000) hi64 0x0000000000000000
14038ddf0:  mov      eax, 5
14038ddf5:  divsd    xmm0, xmm6
14038ddf9:  cvttsd2si rcx, xmm0
14038ddfe:  cmp      rcx, rax
14038de01:  cmovl    rcx, rax
14038de05:  mov      eax, 0x19
      ...
14038de11:  imul     rcx, rcx, 0x5f5e100
14038de18:  jmp      0x14038dbe6
14038de1d:  nop      dword ptr [rax]
14038de20:  addsd    xmm6, qword ptr [rip + 0xc93f40]    ; -> 0x141021d68 [.rdata] f64 51.0 (0x4049800000000000) hi64 0x4018000000000000
14038de28:  divsd    xmm6, qword ptr [rip + 0xc93f40]    ; -> 0x141021d70 [.rdata] f64 6.0 (0x4018000000000000) hi64 0x40c5b38000000000
14038de30:  mulsd    xmm6, xmm6
14038de34:  movsd    xmm0, qword ptr [rip + 0xc93f3c]    ; -> 0x141021d78 [.rdata] f64 11111.0 (0x40c5b38000000000) hi64 0x40a4500000000000
14038de3c:  mov      eax, 0x19
14038de41:  divsd    xmm0, xmm6
14038de45:  cvttsd2si rcx, xmm0
14038de4a:  cmp      rcx, rax
14038de4d:  cmovl    rcx, rax
14038de51:  mov      eax, 0x1f4
      ...
14038de64:  jmp      0x14038dbe6
14038de69:  cmp      eax, 0x1d
14038de6c:  je       0x14038db58
14038de72:  movsd    xmm0, qword ptr [rip + 0xc93ece]    ; -> 0x141021d48 [.rdata] f64 0.00390625 (0x3f70000000000000) hi64 0xffffffff00000001
14038de7a:  test     al, 1
14038de7c:  jne      0x14038de90
14038de7e:  mulsd    xmm6, xmm0
14038de82:  sub      eax, 1
14038de85:  cmp      eax, 0x1d
14038de88:  je       0x14038db58
14038de8e:  nop      
14038de90:  mulsd    xmm6, xmm0
14038de94:  sub      eax, 2
14038de97:  mulsd    xmm6, xmm0
14038de9b:  cmp      eax, 0x1d
14038de9e:  je       0x14038db58
14038dea4:  mulsd    xmm6, xmm0
14038dea8:  sub      eax, 2
14038deab:  mulsd    xmm6, xmm0
14038deaf:  cmp      eax, 0x1d
14038deb2:  jne      0x14038de90
14038deb4:  jmp      0x14038db58
14038deb9:  nop      dword ptr [rax]
14038dec0:  addsd    xmm6, qword ptr [rip + 0xc93e68]    ; -> 0x141021d30 [.rdata] f64 1.0 (0x3ff0000000000000) hi64 0x40efffe000000000
14038dec8:  movsd    xmm0, qword ptr [rip + 0xc93e88]    ; -> 0x141021d58 [.rdata] f64 1111.0 (0x40915c0000000000) hi64 0x4052c00000000000
14038ded0:  mov      eax, 1
14038ded5:  mulsd    xmm6, xmm6
14038ded9:  divsd    xmm0, xmm6
14038dedd:  cvttsd2si rcx, xmm0
14038dee2:  test     rcx, rcx
14038dee5:  cmovle   rcx, rax
14038dee9:  mov      eax, 0x1f4
      ...
14038df24:  and      r14d, 0xffffff
14038df2b:  call     0x140ef31d0    <fn 0x140ef31d0>
14038df30:  pxor     xmm0, xmm0
14038df34:  movsd    xmm6, qword ptr [rip + 0xc93dfc]    ; -> 0x141021d38 [.rdata] f64 65535.0 (0x40efffe000000000) hi64 0x4070000000000000
14038df3c:  cvtsi2sd xmm0, r14d
14038df41:  divsd    xmm6, xmm0
14038df45:  jmp      0x14038db58
14038df4a:  mov      r11d, dword ptr [rbx + 0x244]
14038df51:  mov      eax, r11d
