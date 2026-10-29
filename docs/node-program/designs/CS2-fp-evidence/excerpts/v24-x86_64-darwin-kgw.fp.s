; function 0x1004ef250-0x1004eff60 plt=['assert_rtn', 'pow', 'stack_chk_fail'] sym=__Z19GetNextWorkRequiredPK11CBlockIndexPK12CBlockHeaderRKN9Consensus6ParamsE
      ...
1004ef3ba:  movaps   xmmword ptr [rbp - 0x90], xmm0
1004ef3c1:  mov      rdx, qword ptr [rbx + 0x238]
1004ef3c8:  xorps    xmm0, xmm0
1004ef3cb:  cvtsi2sd xmm0, rdx
1004ef3d0:  mulsd    xmm0, qword ptr [rip + 0xad49a8]    ; -> 0x100fc3d80 [__literals] f64 0.025 (0x3f9999999999999a) hi64 0x403c333333333333
1004ef3d8:  mov      r8, qword ptr [rbx + 0x230]
1004ef3df:  cvttsd2si rsi, xmm0
1004ef3e4:  mov      rdi, rsi
1004ef3e7:  subsd    xmm0, qword ptr [rip + 0xad4939]    ; -> 0x100fc3d28 [__literals] f64 9.223372036854776e+18 (0x43e0000000000000) hi64 0x3fa0000000000000
1004ef3ef:  cvttsd2si rax, xmm0
1004ef3f4:  sar      rdi, 0x3f
1004ef3f8:  and      rax, rdi
1004ef3fb:  or       rax, rsi
      ...
1004efc22:  mov      dword ptr [rbp - 0x124], r11d
1004efc29:  mov      qword ptr [rbp - 0xe8], rdi
1004efc30:  movq     xmm1, rcx
1004efc35:  punpckldq xmm1, xmmword ptr [rip + 0xad2473]    ; -> 0x100fc20b0 [__literals] f64 1.9342817955203666e+25 (0x4530000043300000) hi64 0x0000000000000000
1004efc3d:  subpd    xmm1, xmmword ptr [rip + 0xad247b]    ; -> 0x100fc20c0 [__literals] f64 4503599627370496.0 (0x4330000000000000) hi64 0x4530000000000000
1004efc45:  movapd   xmm0, xmm1
1004efc49:  unpckhpd xmm0, xmm1
1004efc4d:  addsd    xmm0, xmm1
1004efc51:  divsd    xmm0, qword ptr [rip + 0xad412f]    ; -> 0x100fc3d88 [__literals] f64 28.2 (0x403c333333333333) hi64 0xbff3a5e353f7ced9
1004efc59:  movsd    xmm1, qword ptr [rip + 0xad412f]    ; -> 0x100fc3d90 [__literals] f64 -1.228 (0xbff3a5e353f7ced9) hi64 0x3fe6ab367a0f9097
1004efc61:  call     0x100f2957e    <pow@plt>
1004efc66:  mulsd    xmm0, qword ptr [rip + 0xad412a]    ; -> 0x100fc3d98 [__literals] f64 0.7084 (0x3fe6ab367a0f9097) hi64 0x4024000000000000
1004efc6e:  test     r12, r12
1004efc71:  jne      0x1004efc81
1004efc73:  movsd    xmm2, qword ptr [rip + 0xad3fc5]    ; -> 0x100fc3c40 [__literals] f64 1.0 (0x3ff0000000000000) hi64 0x3fe51eb851eb851f
1004efc7b:  movapd   xmm1, xmm2
1004efc7f:  jmp      0x1004efc9a
1004efc81:  xorps    xmm1, xmm1
1004efc84:  cvtsi2sd xmm1, r12
1004efc89:  cvtsi2sd xmm2, r13
1004efc8e:  divsd    xmm1, xmm2
1004efc92:  movsd    xmm2, qword ptr [rip + 0xad3fa6]    ; -> 0x100fc3c40 [__literals] f64 1.0 (0x3ff0000000000000) hi64 0x3fe51eb851eb851f
1004efc9a:  test     r14, r14
1004efc9d:  mov      rcx, qword ptr [rbp - 0x110]
1004efca4:  mov      rdx, qword ptr [rbp - 0x100]
      ...
1004efcc7:  mov      r8, qword ptr [rbp - 0x140]
1004efcce:  jg       0x1004efcd4
1004efcd0:  movapd   xmm1, xmm2
1004efcd4:  addsd    xmm0, xmm2
1004efcd8:  divsd    xmm2, xmm0
1004efcdc:  ucomisd  xmm2, xmm1
1004efce0:  jae      0x1004efe4b
1004efce6:  ucomisd  xmm1, xmm0
1004efcea:  jae      0x1004efe4b
1004efcf0:  mov      rdx, qword ptr [rdx + 8]
1004efcf4:  test     rdx, rdx
