/*
 * Replicates the floating-point part of Dash Core's KimotoGravityWell loop
 * (src/pow.cpp:56-58) for every reachable PastBlocksMass (1..4032), using the
 * pow() of the libm this process is linked against at run time.
 *
 * Output per mass m (hex bit patterns):
 *   m x=fl(m/28.2) p=pow(x,-1.228) d_sep=1+fl(0.7084*p) d_fma=fma(p,0.7084,1)
 *   s_sep=1/d_sep s_fma=1/d_fma
 *
 * d_sep is what the x86_64 release binaries compute (mulsd + addsd);
 * d_fma is what the aarch64 release binaries compute (fmadd).
 * Build with -ffp-contract=off so d_sep is not contracted.
 */
#include <inttypes.h>
#include <math.h>
#include <stdio.h>
#include <string.h>

static uint64_t bits(double d)
{
    uint64_t u;
    memcpy(&u, &d, sizeof u);
    return u;
}

int main(void)
{
    /* volatile: keep the compiler from folding anything at build time */
    volatile double c282 = 28.2, cy = -1.228, c7084 = 0.7084, one = 1.0;
    for (unsigned m = 1; m <= 4032; m++) {
        volatile double mm = (double)m;
        double x = mm / c282;
        double p = pow(x, cy);
        volatile double prod = c7084 * p;
        double d_sep = one + prod;
        double d_fma = fma(p, c7084, one);
        double s_sep = one / d_sep;
        double s_fma = one / d_fma;
        printf("%u %016" PRIx64 " %016" PRIx64 " %016" PRIx64 " %016" PRIx64 " %016" PRIx64 " %016" PRIx64 "\n",
               m, bits(x), bits(p), bits(d_sep), bits(d_fma), bits(s_sep), bits(s_fma));
    }
    return 0;
}
