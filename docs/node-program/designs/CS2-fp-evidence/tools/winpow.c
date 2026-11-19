/*
 * Runs the `pow` routine statically linked into an official win64 dashd.exe, natively on
 * Linux x86_64, over KGW's whole `pow` domain (m = 1..4032; report §4.1).
 *
 *   winpow DASHD_EXE POW_VA FCW_HEX   ->  m hex(fl(m/28.2)) hex(pow(fl(m/28.2), fl(-1.228)))
 *
 * The PE image is mapped at its preferred ImageBase, so no relocations are applied. Only the
 * routine's normal-input path runs: x87 code plus internal helpers (log2l, exp2l and a
 * classification helper); the IAT is not populated, so any import call would fault. POW_VA is
 * the routine KGW calls (CS2-fp-release-binaries.md §3.2; reproduce.sh takes it from the KGW
 * listing). FCW_HEX is the x87 control word to run under: 0x37F (64-bit precision, what the
 * mingw-w64 CRT's fninit gives the main thread) or 0x27F (53-bit precision, the Windows x64
 * default). The control word is restored to its value before each call, and checked after.
 *
 * Adapted from the CS2 r1 Opus reviewer's winpow.c (/work/scratch/cs2-review-opus/), with the
 * routine address taken as an argument instead of hard-coded.
 */
#define _GNU_SOURCE
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>

typedef double(__attribute__((ms_abi)) * pow_fn)(double, double);

static uint64_t bits(double d)
{
    uint64_t u;
    memcpy(&u, &d, 8);
    return u;
}
static uint16_t rd16(const unsigned char* p) { uint16_t v; memcpy(&v, p, 2); return v; }
static uint32_t rd32(const unsigned char* p) { uint32_t v; memcpy(&v, p, 4); return v; }
static uint64_t rd64(const unsigned char* p) { uint64_t v; memcpy(&v, p, 8); return v; }

int main(int argc, char** argv)
{
    if (argc != 4) {
        fprintf(stderr, "usage: winpow DASHD_EXE POW_VA FCW_HEX\n");
        return 2;
    }
    FILE* f = fopen(argv[1], "rb");
    if (!f) { perror("open"); return 1; }
    fseek(f, 0, SEEK_END);
    long n = ftell(f);
    fseek(f, 0, SEEK_SET);
    unsigned char* img = malloc(n);
    if (fread(img, 1, n, f) != (size_t)n) return 1;
    fclose(f);
    uint32_t pe = rd32(img + 0x3c);
    uint16_t nsec = rd16(img + pe + 6), optsz = rd16(img + pe + 20);
    const unsigned char* opt = img + pe + 24;
    uint64_t base = rd64(opt + 24);
    uint32_t size = rd32(opt + 56);
    void* m = mmap((void*)base, size, PROT_READ | PROT_WRITE | PROT_EXEC,
                   MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0);
    if (m != (void*)base) { perror("mmap"); return 1; }
    const unsigned char* sh = opt + optsz;
    for (int i = 0; i < nsec; i++, sh += 40) {
        uint32_t vsz = rd32(sh + 8), va = rd32(sh + 12), rsz = rd32(sh + 16), roff = rd32(sh + 20);
        uint32_t c = rsz < vsz ? rsz : vsz;
        if (roff && c) memcpy((char*)base + va, img + roff, c);
    }
    uint64_t va = strtoull(argv[2], 0, 16);
    if (va < base || va >= base + size) { fprintf(stderr, "POW_VA outside the image\n"); return 1; }
    pow_fn p = (pow_fn)(uintptr_t)va;
    unsigned short fcw = (unsigned short)strtoul(argv[3], 0, 16), now;
    volatile double k = 28.2, y = -1.228;
    for (unsigned mm = 1; mm <= 4032; mm++) {
        __asm__ volatile("fldcw %0" : : "m"(fcw));
        volatile double md = (double)mm;
        double x = md / k;
        double r = p(x, y);
        __asm__ volatile("fnstcw %0" : "=m"(now));
        if (now != fcw) { fprintf(stderr, "control word changed to %04x at m=%u\n", now, mm); return 1; }
        printf("%u %016" PRIx64 " %016" PRIx64 "\n", mm, bits(x), bits(r));
    }
    return 0;
}
