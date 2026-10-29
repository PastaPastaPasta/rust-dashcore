// The FZ1 slow-stop probe (fuzzctl selftest, `RealWindowStop`): a one-edge callback, linked
// against the locked libFuzzer archive, whose calls after the AFTER-th (the first SLOW_CALLS of
// them, default all) each spin half a second of CPU time and then sleep SLEEP seconds (Sol r4
// B1's grace target). run.sh's certify driver, which stops on CPU time, ends the window during a
// spin; libFuzzer reads its stop file only after that input (and the rest of its mutation
// round), so it exits at least about SLEEP seconds after the stop. With one call of more than
// 10 s it is an ordinary slow unit (Sol r4 m1). Nothing of libFuzzer's is wrapped.
//
//   c++ -O1 -DAFTER=N -DSLEEP=S [-DSLOW_CALLS=C] slow_stop_probe.cpp <libfuzzer.a> -lpthread
#include <cstddef>
#include <cstdint>
#include <ctime>
#include <unistd.h>

#ifndef SLOW_CALLS
#define SLOW_CALLS UINT64_MAX
#endif

static unsigned char counters[1];
static uintptr_t pcs[2];
static uint64_t calls;

extern "C" void __sanitizer_cov_8bit_counters_init(unsigned char *, unsigned char *);
extern "C" void __sanitizer_cov_pcs_init(const uintptr_t *, const uintptr_t *);

extern "C" int LLVMFuzzerTestOneInput(const uint8_t *, size_t) {
    counters[0] = 1;
    if (++calls > AFTER && calls - AFTER <= SLOW_CALLS) {
        const clock_t end = clock() + CLOCKS_PER_SEC / 2;  // CPU time: the driver's stop lands here
        while (clock() < end) {
        }
        sleep(SLEEP);
    }
    return 0;
}

extern "C" int LLVMFuzzerInitialize(int *, char ***) {
    pcs[0] = (uintptr_t)&LLVMFuzzerTestOneInput;
    pcs[1] = 1;
    __sanitizer_cov_8bit_counters_init(counters, counters + 1);
    __sanitizer_cov_pcs_init(pcs, pcs + 2);
    return 0;
}

extern "C" size_t LLVMFuzzerCustomMutator(uint8_t *data, size_t, size_t, unsigned) {
    data[0] = 'a';
    return 1;
}
