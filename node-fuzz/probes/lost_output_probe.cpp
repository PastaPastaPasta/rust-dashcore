// The FZ1 lost-output probe (fuzzctl selftest, `RealWindowStop`; Sol r3 B2's probe): a callback,
// linked against the locked libFuzzer archive, with ten more edges, the i-th after i * 100000
// calls. A vfprintf wrapper fails every status line printed after 100000 executions (ENOSPC, as
// on a full disk; libFuzzer ignores its write errors, so the discoveries' NEW lines and the DONE
// line are lost) and writes the final stats' count of units added short (11 becomes "1", no
// newline). The log must not certify. The wrapper changes neither libFuzzer's coverage
// bookkeeping nor how it stops.
//
//   c++ -O1 lost_output_probe.cpp <libfuzzer.a> -Wl,--wrap=vfprintf -Wl,--wrap=__vfprintf_chk -lpthread
#include <cerrno>
#include <cstdarg>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>

static unsigned char counters[11];
static uintptr_t pcs[22];
static uint64_t calls;
static bool dropping, lost;

extern "C" void __sanitizer_cov_8bit_counters_init(unsigned char *, unsigned char *);
extern "C" void __sanitizer_cov_pcs_init(const uintptr_t *, const uintptr_t *);
extern "C" int __real_vfprintf(FILE *, const char *, va_list);

static void edge() {}

extern "C" int LLVMFuzzerTestOneInput(const uint8_t *, size_t) {
    counters[0] = 1;
    ++calls;
    for (unsigned i = 1; i < 11; ++i)
        if (calls > i * 100000) counters[i] = 1;
    return 0;
}

extern "C" int LLVMFuzzerInitialize(int *, char ***) {
    for (unsigned i = 0; i < 11; ++i) {
        pcs[2 * i] = (uintptr_t)&edge + i;
        pcs[2 * i + 1] = 0;
    }
    __sanitizer_cov_8bit_counters_init(counters, counters + 11);
    __sanitizer_cov_pcs_init(pcs, pcs + 22);
    return 0;
}

extern "C" size_t LLVMFuzzerCustomMutator(uint8_t *data, size_t, size_t, unsigned) {
    data[0] = 'a';
    return 1;
}

extern "C" int __wrap_vfprintf(FILE *f, const char *fmt, va_list args) {
    if (!strncmp(fmt, "stat::", 6)) dropping = false;
    if (!strcmp(fmt, "#%zd\t%s")) {
        va_list copy;
        va_copy(copy, args);
        dropping = va_arg(copy, size_t) > 100000;
        va_end(copy);
        lost |= dropping;
    }
    if (dropping) {  // to the line's end: PrintStats' End, or PrintStatusForNewUnit's "\n"
        bool end = !strcmp(fmt, "\n");
        if (!strcmp(fmt, "%s")) {
            va_list copy;
            va_copy(copy, args);
            end = !strcmp(va_arg(copy, const char *), "\n");
            va_end(copy);
        }
        dropping = !end;
        errno = ENOSPC;
        return -1;
    }
    if (lost && !strcmp(fmt, "stat::new_units_added:          %zd\n")) {
        fputs("stat::new_units_added:          1", f);  // a short write of the real count
        errno = ENOSPC;
        return -1;
    }
    return __real_vfprintf(f, fmt, args);
}

extern "C" int __wrap___vfprintf_chk(FILE *f, int, const char *fmt, va_list args) {
    return __wrap_vfprintf(f, fmt, args);
}
