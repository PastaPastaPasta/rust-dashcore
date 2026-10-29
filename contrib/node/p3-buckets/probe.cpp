// Bucket-count probe for blockencodings.cpp's `std::unordered_map<uint64_t, uint16_t> shorttxids(n)`.
// For every n in 0..=65535 prints: n, bucket_count() after construction, bucket_count() after n
// distinct inserts. Build statically so the toolchain's own libstdc++ answers.
#include <cstdint>
#include <cstdio>
#include <unordered_map>

int main()
{
    for (uint64_t n = 0; n <= 65535; ++n) {
        std::unordered_map<uint64_t, uint16_t> m(n);
        const auto before = m.bucket_count();
        for (uint64_t i = 0; i < n; ++i) m[i * 0x9E3779B97F4A7C15ULL] = uint16_t(i);
        std::printf("%llu %zu %zu\n", (unsigned long long)n, before, m.bucket_count());
    }
    std::printf("__GNUC__ %d.%d.%d _GLIBCXX_RELEASE %d\n", __GNUC__, __GNUC_MINOR__, __GNUC_PATCHLEVEL__, _GLIBCXX_RELEASE);
}
