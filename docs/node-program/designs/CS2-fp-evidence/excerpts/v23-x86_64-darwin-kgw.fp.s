; function 0x100490ee0-0x100491bf0 plt=['assert_rtn', 'pow', 'stack_chk_fail'] sym=__Z19GetNextWorkRequiredPK11CBlockIndexPK12CBlockHeaderRKN9Consensus6ParamsE
      ...
10049104a:  movaps   xmmword ptr [rbp - 0x90], xmm0
100491051:  mov      rdx, qword ptr [rbx + 0x1f0]
100491058:  xorps    xmm0, xmm0
10049105b:  cvtsi2sd xmm0, rdx
100491060:  mulsd    xmm0, qword ptr [rip + 0xa13d40]    ; -> 0x100ea4da8 [__literals] f64 0.025 (0x3f9999999999999a) hi64 0x43e0000000000000
100491068:  mov      r8, qword ptr [rbx + 0x1e8]
10049106f:  cvttsd2si rsi, xmm0
100491074:  mov      rdi, rsi
100491077:  subsd    xmm0, qword ptr [rip + 0xa13d31]    ; -> 0x100ea4db0 [__literals] f64 9.223372036854776e+18 (0x43e0000000000000) hi64 0x403c333333333333
10049107f:  cvttsd2si rax, xmm0
100491084:  sar      rdi, 0x3f
100491088:  and      rax, rdi
10049108b:  or       rax, rsi
      ...
1004918b2:  mov      dword ptr [rbp - 0x124], r11d
1004918b9:  mov      qword ptr [rbp - 0xe8], rdi
1004918c0:  movq     xmm1, rcx
1004918c5:  punpckldq xmm1, xmmword ptr [rip + 0xa12563]    ; -> 0x100ea3e30 [__literals] f64 1.9342817955203666e+25 (0x4530000043300000) hi64 0x0000000000000000
1004918cd:  subpd    xmm1, xmmword ptr [rip + 0xa1256b]    ; -> 0x100ea3e40 [__literals] f64 4503599627370496.0 (0x4330000000000000) hi64 0x4530000000000000
1004918d5:  movapd   xmm0, xmm1
1004918d9:  unpckhpd xmm0, xmm1
1004918dd:  addsd    xmm0, xmm1
1004918e1:  divsd    xmm0, qword ptr [rip + 0xa134cf]    ; -> 0x100ea4db8 [__literals] f64 28.2 (0x403c333333333333) hi64 0xbff3a5e353f7ced9
1004918e9:  movsd    xmm1, qword ptr [rip + 0xa134cf]    ; -> 0x100ea4dc0 [__literals] f64 -1.228 (0xbff3a5e353f7ced9) hi64 0x3fe6ab367a0f9097
1004918f1:  call     0x100e1906a    <pow@plt>
1004918f6:  mulsd    xmm0, qword ptr [rip + 0xa134ca]    ; -> 0x100ea4dc8 [__literals] f64 0.7084 (0x3fe6ab367a0f9097) hi64 0x4024000000000000
1004918fe:  test     r12, r12
100491901:  jne      0x100491911
100491903:  movsd    xmm2, qword ptr [rip + 0xa13375]    ; -> 0x100ea4c80 [__literals] f64 1.0 (0x3ff0000000000000) hi64 0x3fe51eb851eb851f
10049190b:  movapd   xmm1, xmm2
10049190f:  jmp      0x10049192a
100491911:  xorps    xmm1, xmm1
100491914:  cvtsi2sd xmm1, r12
100491919:  cvtsi2sd xmm2, r13
10049191e:  divsd    xmm1, xmm2
100491922:  movsd    xmm2, qword ptr [rip + 0xa13356]    ; -> 0x100ea4c80 [__literals] f64 1.0 (0x3ff0000000000000) hi64 0x3fe51eb851eb851f
10049192a:  test     r14, r14
10049192d:  mov      rcx, qword ptr [rbp - 0x110]
100491934:  mov      rdx, qword ptr [rbp - 0x100]
      ...
100491957:  mov      r8, qword ptr [rbp - 0x140]
10049195e:  jg       0x100491964
100491960:  movapd   xmm1, xmm2
100491964:  addsd    xmm0, xmm2
100491968:  divsd    xmm2, xmm0
10049196c:  ucomisd  xmm2, xmm1
100491970:  jae      0x100491adb
100491976:  ucomisd  xmm1, xmm0
10049197a:  jae      0x100491adb
100491980:  mov      rdx, qword ptr [rdx + 8]
100491984:  test     rdx, rdx
