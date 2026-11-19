; function 0x64abf0-0x64b145 plt=['__assert_fail', '__stack_chk_fail', 'memcmp', 'memcpy']
      ...
  64ac22:  jle      0x64adc0
  64ac28:  mov      edx, r14d
  64ac2b:  pxor     xmm1, xmm1
  64ac2f:  movsd    xmm0, qword ptr [rip + 0xb616a1]    ; -> 0x11ac2d8 [.rodata] f64 65535.0 (0x40efffe000000000) hi64 0x4070000000000000
  64ac37:  mov      eax, r14d
  64ac3a:  and      edx, 0xffffff
  64ac40:  shr      eax, 0x18
  64ac43:  cvtsi2sd xmm1, edx
  64ac47:  divsd    xmm0, xmm1
  64ac4b:  cmp      r14d, 0x1cffffff
  64ac52:  ja       0x64afd9
  64ac58:  movsd    xmm1, qword ptr [rip + 0xb61680]    ; -> 0x11ac2e0 [.rodata] f64 256.0 (0x4070000000000000) hi64 0x3f70000000000000
  64ac60:  test     al, 1
  64ac62:  jne      0x64ac70
  64ac64:  mulsd    xmm0, xmm1
  64ac68:  add      eax, 1
  64ac6b:  cmp      eax, 0x1d
  64ac6e:  je       0x64ac80
  64ac70:  mulsd    xmm0, xmm1
  64ac74:  add      eax, 2
  64ac77:  mulsd    xmm0, xmm1
  64ac7b:  cmp      eax, 0x1d
  64ac7e:  jne      0x64ac70
  64ac80:  movsd    qword ptr [rbp - 0x68], xmm0
      ...
  64af2a:  jle      0x64af90
  64af2c:  cmp      r13d, 0x5dbf
  64af33:  jg       0x64af43
  64af35:  movsd    xmm1, qword ptr [rip + 0xb613c3]    ; -> 0x11ac300 [.rodata] f64 75.0 (0x4052c00000000000) hi64 0x4049800000000000
  64af3d:  comisd   xmm1, xmm0
  64af41:  jae      0x64af90
  64af43:  addsd    xmm0, qword ptr [rip + 0xb613d5]    ; -> 0x11ac320 [.rodata] f64 2600.0 (0x40a4500000000000) hi64 0x4140f44700000000
  64af4b:  divsd    xmm0, qword ptr [rip + 0xb6130d]    ; -> 0x11ac260 [.rodata] f64 9.0 (0x4022000000000000) hi64 0x0000000400000003
  64af53:  mulsd    xmm0, xmm0
  64af57:  movsd    xmm1, qword ptr [rip + 0xb613c9]    ; -> 0x11ac328 [.rodata] f64 2222222.0 (0x4140f44700000000) hi64 0x0000025700000255
  64af5f:  mov      eax, 5
  64af64:  divsd    xmm1, xmm0
  64af68:  cvttsd2si rcx, xmm1
  64af6d:  cmp      rcx, rax
  64af70:  cmovl    rcx, rax
  64af74:  mov      eax, 0x19
      ...
  64af80:  imul     rcx, rcx, 0x5f5e100
  64af87:  jmp      0x64ad21
  64af8c:  nop      dword ptr [rax]
  64af90:  addsd    xmm0, qword ptr [rip + 0xb61370]    ; -> 0x11ac308 [.rodata] f64 51.0 (0x4049800000000000) hi64 0x4018000000000000
  64af98:  divsd    xmm0, qword ptr [rip + 0xb61370]    ; -> 0x11ac310 [.rodata] f64 6.0 (0x4018000000000000) hi64 0x40c5b38000000000
  64afa0:  mulsd    xmm0, xmm0
  64afa4:  movsd    xmm1, qword ptr [rip + 0xb6136c]    ; -> 0x11ac318 [.rodata] f64 11111.0 (0x40c5b38000000000) hi64 0x40a4500000000000
  64afac:  mov      eax, 0x19
  64afb1:  divsd    xmm1, xmm0
  64afb5:  cvttsd2si rcx, xmm1
  64afba:  cmp      rcx, rax
  64afbd:  cmovl    rcx, rax
  64afc1:  mov      eax, 0x1f4
      ...
  64afd4:  jmp      0x64ad21
  64afd9:  cmp      eax, 0x1d
  64afdc:  je       0x64ac80
  64afe2:  movsd    xmm1, qword ptr [rip + 0xb612fe]    ; -> 0x11ac2e8 [.rodata] f64 0.00390625 (0x3f70000000000000) hi64 0xffffffff00000001
  64afea:  test     al, 1
  64afec:  jne      0x64b000
  64afee:  mulsd    xmm0, xmm1
  64aff2:  sub      eax, 1
  64aff5:  cmp      eax, 0x1d
  64aff8:  je       0x64ac80
  64affe:  nop      
  64b000:  mulsd    xmm0, xmm1
  64b004:  sub      eax, 2
  64b007:  mulsd    xmm0, xmm1
  64b00b:  cmp      eax, 0x1d
  64b00e:  je       0x64ac80
  64b014:  mulsd    xmm0, xmm1
  64b018:  sub      eax, 2
  64b01b:  mulsd    xmm0, xmm1
  64b01f:  cmp      eax, 0x1d
  64b022:  jne      0x64b000
  64b024:  jmp      0x64ac80
  64b029:  nop      dword ptr [rax]
  64b030:  addsd    xmm0, qword ptr [rip + 0xb61158]    ; -> 0x11ac190 [.rodata] f64 1.0 (0x3ff0000000000000) hi64 0xffffffff00000000
  64b038:  movsd    xmm1, qword ptr [rip + 0xb612b8]    ; -> 0x11ac2f8 [.rodata] f64 1111.0 (0x40915c0000000000) hi64 0x4052c00000000000
  64b040:  mov      eax, 1
  64b045:  mulsd    xmm0, xmm0
  64b049:  divsd    xmm1, xmm0
  64b04d:  cvttsd2si rcx, xmm1
  64b052:  test     rcx, rcx
  64b055:  cmovle   rcx, rax
  64b059:  mov      eax, 0x1f4
      ...
  64b098:  and      r14d, 0xffffff
  64b09f:  call     0x2288d0    <fn 0x2288d0>
  64b0a4:  pxor     xmm1, xmm1
  64b0a8:  movsd    xmm0, qword ptr [rip + 0xb61228]    ; -> 0x11ac2d8 [.rodata] f64 65535.0 (0x40efffe000000000) hi64 0x4070000000000000
  64b0b0:  cvtsi2sd xmm1, r14d
  64b0b5:  divsd    xmm0, xmm1
  64b0b9:  jmp      0x64ac80
  64b0be:  mov      r9d, dword ptr [rbx + 0x244]
  64b0c5:  mov      eax, r9d
