; function 0x14039a440-0x14039b358 plt=['_assert', 'memcmp', 'memcpy']
      ...
14039a45b:  mov      r12, qword ptr [rbp + 0x510]
14039a462:  mov      r15, qword ptr [rbp + 0x518]
14039a469:  mov      rbx, rcx
14039a46c:  mov      rcx, qword ptr [rip + 0xe6c7fd]    ; -> 0x141206c70 [.rdata] f64 2.6627069333e-314 (0x00000001413b6510) hi64 0x0000000000000000
14039a473:  mov      rdi, rdx
14039a476:  mov      r13, r8
14039a479:  mov      rsi, r9
      ...
14039a4d9:  mov      rax, r14
14039a4dc:  mov      qword ptr [rbp + 0x328], r8
14039a4e3:  mov      byte ptr [rax + r8], 0
14039a4e8:  mov      rax, qword ptr [rip + 0xe69361]    ; -> 0x141203850 [.rdata] f64 2.6606376994e-314 (0x0000000140fb7d00) hi64 0x0000000000000000
14039a4ef:  mov      r8, qword ptr [rbp + 0x328]
14039a4f6:  cmp      r8, qword ptr [rax + 8]
14039a4fa:  je       0x14039a5f0
      ...
14039a597:  call     0x140ef31d0    <fn 0x140ef31d0>
14039a59c:  mov      rcx, r12
14039a59f:  call     0x140ef31d0    <fn 0x140ef31d0>
14039a5a4:  mov      rdi, qword ptr [rip + 0xe6c6c5]    ; -> 0x141206c70 [.rdata] f64 2.6627069333e-314 (0x00000001413b6510) hi64 0x0000000000000000
14039a5ab:  mov      rax, qword ptr [rbp + 0x498]
14039a5b2:  sub      rax, qword ptr [rdi]
14039a5b5:  jne      0x14039af74
      ...
14039a640:  call     0x140196210    <fn 0x140196210>
14039a645:  mov      ecx, dword ptr [rbx + 0x48]
14039a648:  pxor     xmm2, xmm2
14039a64c:  movsd    xmm1, qword ptr [rip + 0xc876e4]    ; -> 0x141021d38 [.rdata] f64 65535.0 (0x40efffe000000000) hi64 0x4070000000000000
14039a654:  mov      r8d, ecx
14039a657:  mov      edx, ecx
14039a659:  movupd   xmm0, xmm1
14039a65d:  and      r8d, 0xffffff
14039a664:  shr      edx, 0x18
14039a667:  cvtsi2sd xmm2, r8d
14039a66c:  divsd    xmm0, xmm2
14039a670:  cmp      ecx, 0x1cffffff
14039a676:  ja       0x14039ae80
14039a67c:  movsd    xmm2, qword ptr [rip + 0xc876bc]    ; -> 0x141021d40 [.rdata] f64 256.0 (0x4070000000000000) hi64 0x3f70000000000000
14039a684:  test     dl, 1
14039a687:  jne      0x14039a698
14039a689:  mulsd    xmm0, xmm2
14039a68d:  add      edx, 1
14039a690:  cmp      edx, 0x1d
14039a693:  je       0x14039a6a8
14039a695:  nop      dword ptr [rax]
14039a698:  mulsd    xmm0, xmm2
14039a69c:  add      edx, 2
14039a69f:  mulsd    xmm0, xmm2
14039a6a3:  cmp      edx, 0x1d
14039a6a6:  jne      0x14039a698
14039a6a8:  mov      ecx, eax
      ...
14039a6b0:  movsd    qword ptr [rbp - 0x28], xmm0
14039a6b5:  and      ecx, 0xffffff
14039a6bb:  shr      edx, 0x18
14039a6be:  cvtsi2sd xmm2, ecx
14039a6c2:  divsd    xmm1, xmm2
14039a6c6:  cmp      eax, 0x1cffffff
14039a6cb:  ja       0x14039ae20
14039a6d1:  movsd    xmm2, qword ptr [rip + 0xc87667]    ; -> 0x141021d40 [.rdata] f64 256.0 (0x4070000000000000) hi64 0x3f70000000000000
14039a6d9:  test     dl, 1
14039a6dc:  jne      0x14039a6f0
14039a6de:  mulsd    xmm1, xmm2
14039a6e2:  add      edx, 1
14039a6e5:  cmp      edx, 0x1d
14039a6e8:  je       0x14039a700
14039a6ea:  nop      word ptr [rax + rax]
14039a6f0:  mulsd    xmm1, xmm2
14039a6f4:  add      edx, 2
14039a6f7:  mulsd    xmm1, xmm2
14039a6fb:  cmp      edx, 0x1d
14039a6fe:  jne      0x14039a6f0
14039a700:  movupd   xmm2, xmm0
14039a704:  movsd    qword ptr [rbp - 0x20], xmm1
14039a709:  mulsd    xmm0, qword ptr [rip + 0xc876ff]    ; -> 0x141021e10 [.rdata] f64 0.5 (0x3fe0000000000000) hi64 0x3eb0000000000000
14039a711:  subsd    xmm2, xmm1
14039a715:  andpd    xmm2, xmmword ptr [rip + 0xc876e3]    ; -> 0x141021e00 [.rdata] f64 nan (0x7fffffffffffffff) hi64 0x0000000000000000
14039a71d:  comisd   xmm2, xmm0
14039a721:  ja       0x14039a8e8
14039a727:  cmp      byte ptr [rip + 0xc1a71a], 0    ; -> 0x140fb4e48 [.data]
14039a72e:  je       0x14039a840
      ...
14039a7e8:  call     0x140ef31d0    <fn 0x140ef31d0>
14039a7ed:  mov      rcx, rbx
14039a7f0:  call     0x140ef31d0    <fn 0x140ef31d0>
14039a7f5:  lea      rdx, [rip + 0xc838cd]    ; -> 0x14101e0c9 [.rdata] f64 1.0012693448082085e-89 (0x2d74656e76656400) hi64 0x00736973656e6567
14039a7fc:  mov      rcx, r12
14039a7ff:  call     0x14038a100    <fn 0x14038a100>
14039a804:  lea      rdx, [rip + 0xc83f45]    ; -> 0x14101e750 [.rdata] str 'bad-fork-prior-to-checkpoint' f64 3.787945678424966e+209 (0x6b726f662d646162) hi64 0x742d726f6972702d
      ...
14039aaee:  call     0x140ef31d0    <fn 0x140ef31d0>
14039aaf3:  mov      rcx, rbx
14039aaf6:  call     0x140ef31d0    <fn 0x140ef31d0>
14039aafb:  lea      rdx, [rip + 0xc835c7]    ; -> 0x14101e0c9 [.rdata] f64 1.0012693448082085e-89 (0x2d74656e76656400) hi64 0x00736973656e6567
14039ab02:  mov      rcx, rsi
14039ab05:  call     0x14038a100    <fn 0x14038a100>
14039ab0a:  lea      rdx, [rip + 0xc83bd4]    ; -> 0x14101e6e5 [.rdata] str 'bad-diffbits' f64 1.904591567781016e+185 (0x666669642d646162) hi64 0x636e690073746962
      ...
14039ad16:  mov      rcx, r12
14039ad19:  lea      r9, [rbp - 8]
14039ad1d:  lea      r8, [rbp - 0x10]
14039ad21:  lea      rdx, [rip + 0xc83a48]    ; -> 0x14101e770 [.rdata] str "block's timestamp is too early %d %d" f64 2.285710829667507e-152 (0x2073276b636f6c62) hi64 0x6d617473656d6974
14039ad28:  mov      qword ptr [rbp - 8], rax
14039ad2c:  mov      qword ptr [rbp - 0x10], r13
14039ad30:  call     0x140ccaf70    <fn 0x140ccaf70>
      ...
14039ae1f:  nop      
14039ae20:  cmp      edx, 0x1d
14039ae23:  je       0x14039a700
14039ae29:  movsd    xmm2, qword ptr [rip + 0xc86f17]    ; -> 0x141021d48 [.rdata] f64 0.00390625 (0x3f70000000000000) hi64 0xffffffff00000001
14039ae31:  test     dl, 1
14039ae34:  jne      0x14039ae50
14039ae36:  mulsd    xmm1, xmm2
14039ae3a:  sub      edx, 1
14039ae3d:  cmp      edx, 0x1d
14039ae40:  je       0x14039a700
14039ae46:  nop      word ptr cs:[rax + rax]
14039ae50:  mulsd    xmm1, xmm2
14039ae54:  sub      edx, 2
14039ae57:  mulsd    xmm1, xmm2
14039ae5b:  cmp      edx, 0x1d
14039ae5e:  je       0x14039a700
14039ae64:  mulsd    xmm1, xmm2
14039ae68:  sub      edx, 2
14039ae6b:  mulsd    xmm1, xmm2
14039ae6f:  cmp      edx, 0x1d
14039ae72:  jne      0x14039ae50
14039ae74:  jmp      0x14039a700
14039ae79:  nop      dword ptr [rax]
14039ae80:  cmp      edx, 0x1d
14039ae83:  je       0x14039a6a8
14039ae89:  movsd    xmm2, qword ptr [rip + 0xc86eb7]    ; -> 0x141021d48 [.rdata] f64 0.00390625 (0x3f70000000000000) hi64 0xffffffff00000001
14039ae91:  test     dl, 1
14039ae94:  jne      0x14039aeb0
14039ae96:  mulsd    xmm0, xmm2
14039ae9a:  sub      edx, 1
14039ae9d:  cmp      edx, 0x1d
14039aea0:  je       0x14039a6a8
14039aea6:  nop      word ptr cs:[rax + rax]
14039aeb0:  mulsd    xmm0, xmm2
14039aeb4:  sub      edx, 2
14039aeb7:  mulsd    xmm0, xmm2
14039aebb:  cmp      edx, 0x1d
14039aebe:  je       0x14039a6a8
14039aec4:  mulsd    xmm0, xmm2
14039aec8:  sub      edx, 2
14039aecb:  mulsd    xmm0, xmm2
14039aecf:  cmp      edx, 0x1d
14039aed2:  jne      0x14039aeb0
14039aed4:  jmp      0x14039a6a8
      ...
14039aefc:  jmp      0x14039aadd
14039af01:  mov      eax, dword ptr [rbx]
14039af03:  jmp      0x14039a870
14039af08:  mov      rdi, qword ptr [rip + 0xe6bd61]    ; -> 0x141206c70 [.rdata] f64 2.6627069333e-314 (0x00000001413b6510) hi64 0x0000000000000000
14039af0f:  mov      rax, qword ptr [rbp + 0x498]
14039af16:  sub      rax, qword ptr [rdi]
14039af19:  jne      0x14039af74
      ...
14039af50:  call     0x140ef31d0    <fn 0x140ef31d0>
14039af55:  mov      rcx, rbx
14039af58:  call     0x140ef31d0    <fn 0x140ef31d0>
14039af5d:  mov      rdi, qword ptr [rip + 0xe6bd0c]    ; -> 0x141206c70 [.rdata] f64 2.6627069333e-314 (0x00000001413b6510) hi64 0x0000000000000000
14039af64:  mov      rax, qword ptr [rbp + 0x498]
14039af6b:  sub      rax, qword ptr [rdi]
14039af6e:  je       0x14039b00d
14039af74:  call     0x140c4d7d0    <fn 0x140c4d7d0>
14039af79:  mov      rdi, qword ptr [rip + 0xe6bcf0]    ; -> 0x141206c70 [.rdata] f64 2.6627069333e-314 (0x00000001413b6510) hi64 0x0000000000000000
14039af80:  mov      rdx, qword ptr [rbp + 0x498]
14039af87:  sub      rdx, qword ptr [rdi]
14039af8a:  jne      0x14039af74
14039af8c:  mov      ecx, eax
14039af8e:  call     0x140fb17b0    <fn 0x140fb17b0>
14039af93:  mov      rdi, qword ptr [rip + 0xe6bcd6]    ; -> 0x141206c70 [.rdata] f64 2.6627069333e-314 (0x00000001413b6510) hi64 0x0000000000000000
14039af9a:  mov      rdx, qword ptr [rbp + 0x498]
14039afa1:  sub      rdx, qword ptr [rdi]
14039afa4:  jne      0x14039af74
14039afa6:  mov      ecx, eax
14039afa8:  call     0x140fb17b0    <fn 0x140fb17b0>
14039afad:  mov      rdi, qword ptr [rip + 0xe6bcbc]    ; -> 0x141206c70 [.rdata] f64 2.6627069333e-314 (0x00000001413b6510) hi64 0x0000000000000000
14039afb4:  mov      rax, qword ptr [rbp + 0x498]
14039afbb:  sub      rax, qword ptr [rdi]
14039afbe:  jne      0x14039af74
      ...
14039afd0:  mov      rbx, rax
14039afd3:  mov      rcx, rsi
14039afd6:  call     0x140ef31d0    <fn 0x140ef31d0>
14039afdb:  mov      rdi, qword ptr [rip + 0xe6bc8e]    ; -> 0x141206c70 [.rdata] f64 2.6627069333e-314 (0x00000001413b6510) hi64 0x0000000000000000
14039afe2:  mov      rax, qword ptr [rbp + 0x498]
14039afe9:  sub      rax, qword ptr [rdi]
14039afec:  jne      0x14039af74
      ...
14039b022:  mov      rbx, rax
14039b025:  mov      rcx, r12
14039b028:  call     0x140ef31d0    <fn 0x140ef31d0>
14039b02d:  mov      rdi, qword ptr [rip + 0xe6bc3c]    ; -> 0x141206c70 [.rdata] f64 2.6627069333e-314 (0x00000001413b6510) hi64 0x0000000000000000
14039b034:  mov      rax, qword ptr [rbp + 0x498]
14039b03b:  sub      rax, qword ptr [rdi]
14039b03e:  je       0x14039afee
