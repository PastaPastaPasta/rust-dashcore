// The FZ1 window probe (fuzzctl selftest, `RealWindowStop`): a two-edge callback, linked against
// the locked libFuzzer archive, whose second edge appears after AFTER calls. When libFuzzer
// reports that discovery (ReportNewCoverage), a vfprintf wrapper spins SPIN seconds of CPU time
// before the NEW line's first write, so run.sh's certify driver, which stops on CPU time, ends
// the window while the discovery is neither printed, saved nor counted (Sol r3 B1's pre-header
// probe). Its stop file must let libFuzzer publish the discovery before it stops, or the window
// must fail. With AFTER past the window's reach it is a flat window. The wrapper changes neither
// libFuzzer's coverage bookkeeping nor how it stops.
//
//   c++ -O1 -DAFTER=N -DSPIN=S window_probe.cpp <libfuzzer.a> -Wl,--wrap=vfprintf \
//       -Wl,--wrap=__vfprintf_chk -lpthread
#include <cstdarg>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <ctime>

static unsigned char counters[2];
static uintptr_t pcs[4];
static uint64_t calls;

extern "C" void __sanitizer_cov_8bit_counters_init(unsigned char *, unsigned char *);
extern "C" void __sanitizer_cov_pcs_init(const uintptr_t *, const uintptr_t *);
extern "C" int __real_vfprintf(FILE *, const char *, va_list);

static void edge2() {}

extern "C" int LLVMFuzzerTestOneInput(const uint8_t *, size_t) {
    counters[0] = 1;
    if (++calls > AFTER) counters[1] = 1;
    return 0;
}

extern "C" int LLVMFuzzerInitialize(int *, char ***) {
    pcs[0] = (uintptr_t)&LLVMFuzzerTestOneInput;
    pcs[1] = 1;
    pcs[2] = (uintptr_t)&edge2;
    pcs[3] = 1;  // a function entry: libFuzzer prints a NEW_FUNC line for it
    __sanitizer_cov_8bit_counters_init(counters, counters + 2);
    __sanitizer_cov_pcs_init(pcs, pcs + 4);
    return 0;
}

extern "C" size_t LLVMFuzzerCustomMutator(uint8_t *data, size_t, size_t, unsigned) {
    data[0] = 'a';
    return 1;
}

extern "C" int __wrap_vfprintf(FILE *f, const char *fmt, va_list args) {
    if (!strcmp(fmt, "#%zd\t%s")) {
        va_list copy;
        va_copy(copy, args);
        (void)va_arg(copy, size_t);
        if (!strncmp(va_arg(copy, const char *), "NEW", 3)) {
            const clock_t end = clock() + SPIN * CLOCKS_PER_SEC;  // CPU time: the driver's stop lands here
            while (clock() < end) {
            }
        }
        va_end(copy);
    }
    return __real_vfprintf(f, fmt, args);
}

extern "C" int __wrap___vfprintf_chk(FILE *f, int, const char *fmt, va_list args) {
    return __wrap_vfprintf(f, fmt, args);
}
